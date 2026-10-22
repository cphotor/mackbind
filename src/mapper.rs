//! Mapping core: modifier-key state machine + remap / cmd_map application.
//!
//! # Event-flow model
//!
//! mackbind grabs the keyboard device; events are processed in this order:
//!
//! 1. Drop `MISC` / `SYNCHRONIZATION` (the former is stray events such as the MacBook backlight,
//!    the latter is re-emitted automatically by uinput).
//! 2. Drop `value=2` (auto-repeat). While a modifier is held the kernel keeps sending repeat events;
//!    if they were not dropped they would re-trigger the mapping and produce paired press/release.
//! 3. When a modifier is pressed, **suppress the output** and record it in state — because whether it is
//!    translated depends on which key is pressed next (e.g. `Cmd+C` must emit `Ctrl+C`, but a lone `Cmd`
//!    should pass through as-is).
//! 4. When a non-modifier key arrives, decide using the current modifier state and produce the events to inject.
//!
//! # Symmetry of release events
//!
//! This is where mistakes happen most easily. If `Cmd+C` is translated to `Ctrl+C`, the release must
//! likewise emit the release of `Ctrl`. If the original `Cmd` release were passed through directly, the output
//! side would be left holding a pressed `Cmd`, which corrupts the behavior of subsequent combos.
//!
//! So at press time we record "what this key was translated into", and on release we restore from that record.

use crate::config::{Config, DesktopType, FnMode};
use crate::keys::parse_combo;
use evdev::{EventType, InputEvent, KeyCode};
use std::collections::{HashMap, HashSet};

/// Whether a keycode is a modifier.
///
/// Note: only the modifiers this tool cares about are listed here. `KEY_FN` is included too — it is an
/// input to the Fn-priority logic and must not be passed through as-is.
pub fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
            | KeyCode::KEY_CAPSLOCK
            | KeyCode::KEY_FN
    )
}

// The specialized key set **excludes no key**; see the note below.
//
// Q/W/N/T were once wrongly excluded, on the reasoning that "terminals have no Ctrl
// binding for quitting/closing a window". That is wrong: what is idle is Ctrl+W, while Ctrl+Shift+W is not idle — it
// is exactly the Linux terminal's close-tab key, and exactly the counterpart of macOS Cmd+W.
// Toshy's config likewise uses `C("RC-W"): C("C-Shift-W")`.
//
// The point of specialization is "translate Cmd into a combo the app recognizes", not "dodge all conflicts".
// Judging key by key whether a combo is idle would actually miss correspondences like this.

/// One line of the compiled rule table.
///
/// The input-side index is `(main key, modifier set)` — the main key alone is not enough: `"KEY_Z"`
/// (Cmd+Z) and `"S-KEY_Z"` (Cmd+Shift+Z) share the same main key; indexing by main key alone would
/// overwrite each other, leaving one of them un-matchable forever. Observed symptom: Cmd+Z degraded to a bare z,
/// Cmd+Shift+Z leaked a bare Shift.
type RuleKey = (KeyCode, Vec<KeyCode>);

/// App selector.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AppMatch {
        /// All apps (the default when `apps` is omitted in the table)
    Any,
        /// Terminal — judged by the `.desktop` `Categories=TerminalEmulator`
    Terminal,
        /// Matched by wm_class substring
    Contains(Vec<String>),
}

impl AppMatch {
    fn parse(specs: &[String]) -> AppMatch {
        if specs.is_empty() || specs.iter().any(|s| s == "*") {
            AppMatch::Any
        } else if specs.iter().any(|s| s == "TerminalEmulator") {
            AppMatch::Terminal
        } else {
            AppMatch::Contains(specs.to_vec())
        }
    }

    fn matches(&self, wm_class: &str, is_term: bool) -> bool {
        match self {
            AppMatch::Any => true,
            AppMatch::Terminal => is_term,
            AppMatch::Contains(v) => v.iter().any(|a| wm_class.contains(a.as_str())),
        }
    }
}

/// A compiled rule.
struct CompiledRule {
        /// Input-side index
    key: RuleKey,
        /// Main-key wildcard (`keys = ["*"]`) — matches any main key
    wildcard: bool,
        /// Whether Cmd must be held
    ///
        /// The lookup applies to all combos (otherwise non-Cmd combos could not be configured), so this must be explicit:
        /// without the check, **a bare Space press would also match the `Cmd+Space` rule**,
        /// and anyone pressing Space would pop up the GNOME overview.
    requires_cmd: bool,
        /// App it applies to
    app: AppMatch,
        /// Action
    act: CompiledAction,
        /// Description, for logging only
    desc: Option<String>,
}

/// The compiled action. `TerminalCtrlShift` is the only conditional action —
/// whether it actually adds Shift depends on the binding looked up at runtime in the target app.
enum CompiledAction {
    Emit(Vec<KeyCode>, KeyCode),
    DeAccel(String),
    TerminalCtrlShift,
        /// Run an external command (operations that are "essentially commands", such as emptying the trash)
    Exec {
        program: String,
        args: Vec<String>,
        confirm: bool,
        prompt: Option<String>,
        ok_label: Option<String>,
    },
}

/// The result of a table lookup hit.
enum Outcome {
        /// Emit this set of modifiers and the main key
    Convert(Vec<KeyCode>, KeyCode),
        /// "Tap" this key immediately (press then release at once), without entering output-state tracking
    Tap(KeyCode),
        /// The action already had a side effect (e.g. ran a command); **no key needs to be emitted**
    NoOutput,
}

/// The compiled mapping rules.
pub struct Mapper {
        /// remap rules: source key → target key
    remap: HashMap<KeyCode, KeyCode>,
        /// **The mapping rule table** — the single source of truth for mappings.
    ///
        /// Looked up linearly in declaration order; the first hit wins (user config is appended at the end,
        /// so it naturally takes priority). The table is small (a few dozen rows), and a linear scan is simpler than
        /// building an index, and keeps "declaration order is priority" explicit.
    rules: Vec<CompiledRule>,
        /// **Physical state**: the modifiers the user is actually holding right now, in press order.
    ///
        /// Reflects hardware fact and never changes because an event was sent. Each new key press matches combos against this.
    held: Vec<KeyCode>,

        /// Keys that were suppressed (not sent to the output side). Their release is not emitted either.
    suppressed: HashSet<KeyCode>,
        /// **Output state**: modifiers already sent as pressed to the system, whose release has not yet been sent.
    ///
        /// Strictly separate from held. When translating a combo these must be released first, then the new output modifiers pressed.
    output_down: Vec<KeyCode>,
        /// The "input key → actual output key" registered at press time, used for symmetric release.
    ///
        /// Registered whether or not it was translated, making "pass-through" the default path rather than a special case.
    pressed: HashMap<KeyCode, KeyCode>,
        /// Modifiers injected to the output side for translation; must be released along with the main key
    injected_mods: Vec<KeyCode>,
        /// Whether Fn is held
    fn_held: bool,
    fn_mode: FnMode,
        /// Table mapping F1-F12 to brightness/media keys
    fkey_media: HashMap<KeyCode, KeyCode>,
        /// Whether the focused window belongs to an app where "Ctrl is already taken" (terminal, GVIM)
    ctrl_busy_window: bool,
        /// Whether the current focused window is a terminal (`Categories=TerminalEmulator`)
    is_terminal: bool,
        /// List of Ctrl-busy apps (WM_CLASS / app_id)
    ctrl_busy_apps: Vec<String>,
        /// Keys that the "Ctrl already taken" app **itself** binds with Ctrl+Shift.
    ///
        /// Looked up at runtime from GSettings (see `set_window_id`); it is the
        /// basis for the `TerminalCtrlShift` action's decision. Shift is added to hit their
        /// existing bindings, not to generically "dodge conflicts".
    terminal_bindings: Vec<KeyCode>,
        /// wm_class of the current focused window, used for the app column of the table
    wm_class: String,
        /// Desktop-environment type, decides which schema the `de_accel` action looks in
    desktop_type: DesktopType,
        /// Whether to output debug logs
    verbose: bool,
}

/// Compile the rule table into a form suitable for linear scanning.
///
/// `keys` is a list of key names, possibly with modifier prefixes (e.g. `"S-z"` means Shift+Z).
/// The last segment is taken as the main key, the prefixes as input-side modifiers. The prefix modifiers are sorted before
/// being indexed, so that `(z,[])` and `(z,[Shift])` become different entries and do not overwrite each other.
fn compile_table(
    rules: &[crate::config::Rule],
) -> Result<Vec<CompiledRule>, crate::keys::KeyError> {
    let mut out = Vec::with_capacity(rules.len());
    for r in rules {
        let mut in_mods: Vec<KeyCode> = Vec::new();
        let mut key: Option<KeyCode> = None;
                // `keys = ["*"]` means a main-key wildcard — used by actions like TerminalCtrlShift
                // whose "does it match" depends on runtime state.
        let mut wildcard = false;
        for k in &r.keys {
            if k == "*" {
                wildcard = true;
                continue;
            }
            let (mods, kc) = parse_combo(k)?;
            in_mods.extend(mods);
            key = Some(kc);
        }
        let key = match key {
            Some(k) => k,
                        // A wildcard rule has no fixed main key by nature; the main key is not compared when matching
            None if wildcard => KeyCode::KEY_RESERVED,
            None => {
                return Err(crate::keys::KeyError::BadCombo(
                    r.keys.join("+"),
                                        "keys is empty".into(),
                ))
            }
        };
        in_mods.sort();

        let act = match &r.act {
            crate::config::Action::Emit { mods, key } => {
                                // Each entry of `mods` looks like `"C"` or `"C-S"` — modifier abbreviations, possibly several.
                                // Parsing it as a combo would treat `A` as the Alt main key and drop it, so parse segment by segment.
                let mut out_mods = Vec::new();
                for m in mods {
                    for seg in m.split('-') {
                        match crate::keys::modifier_from_abbrev(seg) {
                            Some(k) => out_mods.push(k),
                            None => {
                                return Err(crate::keys::KeyError::BadModifier(
                                    seg.to_string(),
                                                                        "emit's mods only accept C/A/S/M (may combine with -, e.g. C-S)".into(),
                                ))
                            }
                        }
                    }
                }
                out_mods.sort();
                out_mods.dedup();
                let (_, out_key) = parse_combo(key)?;
                CompiledAction::Emit(out_mods, out_key)
            }
            crate::config::Action::DeAccel { feature } => {
                CompiledAction::DeAccel(feature.clone())
            }
            crate::config::Action::TerminalCtrlShift => CompiledAction::TerminalCtrlShift,
            crate::config::Action::Exec {
                program,
                args,
                confirm,
                prompt,
                ok_label,
            } => CompiledAction::Exec {
                program: program.clone(),
                args: args.clone(),
                confirm: *confirm,
                prompt: prompt.clone(),
                ok_label: ok_label.clone(),
            },
        };

        out.push(CompiledRule {
            key: (key, in_mods),
            wildcard,
            requires_cmd: r.cmd,
            app: AppMatch::parse(&r.apps),
            act,
            desc: r.desc.clone(),
        });
    }
    Ok(out)
}

/// GTK accelerator main-key name → evdev keycode.
///
/// See [`crate::keys::parse_external_key`]. accel.rs uses the same function when parsing DE bindings —
/// a duplicate was once written here, and the difference in the `"space"` entry between the two came from exactly that.
fn accel_key(name: &str) -> Option<KeyCode> {
    crate::keys::parse_external_key(name)
}

/// Lookup table for "feature name → corresponding action in the DE".
///
/// Adding a new DE feature only requires one line here, **without touching any branching logic**:
///
/// ```text
/// (feature id, GSettings schema under that DE, action key name)
/// ```
///
/// This kind of mapping used to be hard-coded branches written as `match DesktopType { Gnome => ...; _ => None }` —
/// every new DE feature required touching a code branch, and users could not append anything in the
/// config file. After switching to a table, the DE only appears in this table.
///
/// Anything not found (DE does not provide the action, schema does not exist, action not bound) returns
/// `None` — **not treated as an error**; the other rules still apply as usual.
fn de_action_lookup(
    dt: &DesktopType,
    feature: &str,
) -> Option<(String, String)> {
        // Not `use DesktopType::*`: it has a `None` variant, which would shadow `Option::None`
    use DesktopType::Gnome;
        // The schema is constructed only when a concrete DE matches, avoiding pointless queries for unrelated DEs
    let (schema, key): (&str, &str) = match (dt, feature) {
                // Overview: GNOME puts it in a shell-level schema, not in wm.keybindings
        (Gnome, "overview") => ("org.gnome.shell.keybindings", "toggle-overview"),
                // Lock screen: in media-keys' `screensaver`, **not** in wm.keybindings.
        //
                // We once asserted "GNOME has no lock-screen binding" after only grepping wm.keybindings —
                // the third misjudgment of the same origin as "overview is in another schema" and "D-Bus dest written wrong".
                // Before asserting a capability does not exist, search the whole GNOME domain once.
        (Gnome, "lock-screen") => (
            "org.gnome.settings-daemon.plugins.media-keys",
            "screensaver",
        ),
                // Show desktop: the action exists but is unbound by default; the user must bind it for this entry to take effect
        (Gnome, "show-desktop") => ("org.gnome.desktop.wm.keybindings", "show-desktop"),
                // The screenshot trio: all in the shell keybindings, not in wm.keybindings
        (Gnome, "screenshot-ui") => ("org.gnome.shell.keybindings", "show-screenshot-ui"),
        (Gnome, "screenshot") => ("org.gnome.shell.keybindings", "screenshot"),
        (Gnome, "screenshot-window") => ("org.gnome.shell.keybindings", "screenshot-window"),
                // Log out: in media-keys (same schema as lock screen), not in wm.keybindings
        (Gnome, "logout") => ("org.gnome.settings-daemon.plugins.media-keys", "logout"),
                // KDE and bare WMs use a different mechanism; not implemented yet.
                // "Cannot be looked up" does not mean "does not exist" — see docs/macos-shortcuts.md.
        _ => return None,
    };
    Some((schema.to_string(), key.to_string()))
}

/// Evaluation result of a desktop-environment action.
///
/// Most actions are a "modifier + main key" combo, but GNOME's overview is not: as of GNOME 50
/// `toggle-overview` defaults to empty (`@as []`), and the overview is triggered instead by mutter's
/// `overlay-key` (default `Super`) — i.e. a **bare Super key press**. A bare press is a
/// "tap" rather than a combo and cannot be squeezed into `(mods, key)`, so it is expressed separately.
enum DeKeys {
        /// Press these modifiers and then the main key (released together when the physical key is released).
    Combo(Vec<KeyCode>, KeyCode),
        /// A single "tap" of this key: press then release immediately.
    Tap(KeyCode),
}

/// Resolve a DE feature into the combo to emit.
///
/// The steps are uniform: look up the schema and action key name → read that action's current accelerator →
/// parse into modifiers and main key. Thus the behavior of **reading the DE's current binding** is guaranteed by the function itself,
/// and need not be implemented by each call site.
///
/// The input side (which macOS combo triggers it) is decided by the `keys` column of the rule table,
/// not hard-coded here — otherwise non-Space triggers such as the `Ctrl+Cmd+Q` lock screen could not be expressed.
///
/// When the DE has not bound that action it falls back to [`de_feature_fallback`], rather than
/// directly declaring the feature unavailable.
fn de_feature_accel(dt: &DesktopType, feature: &str) -> Option<DeKeys> {
    if let Some((schema, action)) = de_action_lookup(dt, feature)
        && let Some(accel) = crate::accel::action_accel(&schema, &action)
        && let Some((mods, key)) = crate::accel::parse_accel(&accel)
    {
        return Some(DeKeys::Combo(
            mods.into_iter().map(KeyCode::new).collect(),
            key,
        ));
    }
    de_feature_fallback(dt, feature)
}

/// Fallback for when a DE action is unbound.
///
/// As of GNOME 50 `toggle-overview` is unbound by default, and the overview is triggered instead by mutter's
/// `overlay-key` (default `Super`). This is not "reading the user's current binding" but
/// **fixed behavior** of that GNOME version, so it is handled separately.
///
/// **This is only reached when `toggle-overview` is genuinely unbound.** If the user or a distro has
/// bound it (older GNOME / EndeavourOS observed as `Ctrl+Shift+Space`), the binding still wins —
/// platform differences are only **appended**, never overriding existing behavior.
fn de_feature_fallback(dt: &DesktopType, feature: &str) -> Option<DeKeys> {
    use DesktopType::Gnome;
    match (dt, feature) {
        (Gnome, "overview") => {
            let key = crate::accel::action_accel("org.gnome.mutter", "overlay-key")?;
            let kc = crate::keys::parse_external_key(&key)?;
            Some(DeKeys::Tap(kc))
        }
        _ => None,
    }
}

/// Whether a DE feature is available. At startup this is used to drop the `de_accel` rows from the table,
/// avoiding repeated queries at runtime.
fn de_feature_ready(dt: &DesktopType, feature: &str) -> bool {
    de_feature_accel(dt, feature).is_some()
}

/// Result of the default semantics: either convert, or pass through.
enum Default {
    /// `Cmd+X` → `Ctrl+X`
    Convert(Vec<KeyCode>, KeyCode),
        /// A Cmd combo with no way to convert; pass through as-is (must re-emit the suppressed Meta)
    Passthrough,
        /// A non-Cmd combo with no rule in the table; pass through as-is (no Meta to re-emit)
    PassthroughPlain,
}

/// Default semantics: `Cmd+X` → `Ctrl+X`.
///
/// **Only applies after a table lookup misses** — anything with a rule in the table is governed by the table.
///
/// No exhaustive enumeration: macOS's Cmd combos are long-tailed and a fixed table cannot cover them. On a real GNOME,
/// of the 28 Super bindings only `Super+h` involves a letter, and `Super+letter` is mostly idle —
/// keeping Super would make the vast majority of combos do nothing.
///
/// # When to pass through instead of converting
///
/// The premise is "Cmd maps to Ctrl". If the user is already holding Ctrl, turning Cmd into Ctrl again would give
/// `Ctrl+Ctrl+X` — the same slot, a degenerate combo. So it must not convert in that case.
///
/// But it must not be treated as `Cmd+X` either: `Ctrl+Cmd+X` on macOS is **another
/// combo** (`Ctrl+Cmd+Q` lock screen, `Ctrl+Cmd+F` fullscreen) with its own meaning.
/// The fact that the table lacks it only means we did not configure it, not that it equals `Cmd+X`.
///
/// So pass through: hand what the user pressed to the app as-is and let the app interpret it. That is more
/// predictable than inventing a `Ctrl+X` for it — go by the table when it has an entry, pass through when it does not.
///
/// Note physical Ctrl becomes CapsLock after remap (see `handle_press`),
/// so the conflict check must count CapsLock as well.
fn default_semantics(cur_mods: &[KeyCode], code: KeyCode) -> Default {
    let ctrl_taken = cur_mods.iter().any(|m| {
        matches!(
            *m,
            KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL | KeyCode::KEY_CAPSLOCK
        )
    });
    if ctrl_taken {
        return Default::Passthrough;
    }

        // The remaining modifiers (Shift/Alt) are kept and Ctrl appended:
    // `Shift+Cmd+X` → `Shift+Ctrl+X`
    let mut mods = cur_mods.to_vec();
    mods.push(KeyCode::KEY_LEFTCTRL);
    mods.sort();
    mods.dedup();
    Default::Convert(mods, code)
}

impl Mapper {
        /// Build from config. A key-name parse failure returns an error immediately — a mis-written config
        /// must fail loudly, not be ignored silently.
    pub fn new(cfg: &Config, verbose: bool) -> Result<Self, crate::keys::KeyError> {
        let mut remap = HashMap::new();
        for r in &cfg.remap {
            let from = crate::keys::parse_key(&r.from)?;
            let to = crate::keys::parse_key(&r.to)?;
            remap.insert(from, to);
        }

        let rules = compile_table(&cfg.rules)?;

                // DE-related rules: read the DE's current bindings; do not rewrite them.
        //
                // macOS's `Cmd+Space` (Spotlight) corresponds to the overview in GNOME, and GNOME
                // already binds that action to `toggle-overview` (on this machine `Ctrl+Shift+Space`).
                // Emitting the same keys directly is enough, with no record left in the user's config —
                // and it never overwrites the user's own setting for that key.
        //
                // The table already has a `DeAccel{overview}` row; here we only do an availability check:
                // if it is findable the row takes effect, otherwise the whole rule has no action to perform.
                // All `de_accel` rows in the table are checked for availability one by one.
        //
                // `overview` is controlled by the `spotlight` switch; other features such as lock screen are enabled unconditionally.
                // Rows that cannot be found are dropped here, avoiding features that repeatedly fail at runtime.
        let mut de_report: Vec<(String, bool)> = Vec::new();
        let rules = rules
            .into_iter()
            .filter(|r| match &r.act {
                CompiledAction::DeAccel(feature) => {
                    let ok = de_feature_ready(&cfg.desktop.de_type(), feature)
                        && (feature != "overview" || cfg.desktop.spotlight());
                    de_report.push((feature.clone(), ok));
                    ok
                }
                _ => true,
            })
            .collect();

        if verbose {
            for (feature, ok) in &de_report {
                if *ok {
                                        eprintln!("[de] {feature}: emits after reading the DE's current binding");
                } else {
                                        eprintln!("[de] {feature}: not provided by the DE or disabled, skipping");
                }
            }
        }

        Ok(Mapper {
            remap,
            rules,
            held: Vec::new(),
            suppressed: HashSet::new(),
            output_down: Vec::new(),
            pressed: HashMap::new(),
            injected_mods: Vec::new(),
            fn_held: false,
            fn_mode: cfg.fn_mode.unwrap_or_default(),
            fkey_media: build_fkey_media_table(),
            ctrl_busy_window: false,
            is_terminal: false,
            ctrl_busy_apps: cfg.ctrl_busy_apps.clone(),
            terminal_bindings: crate::config::DEFAULT_TERMINAL_SHIFT_KEYS
                .iter()
                .map(|k| crate::keys::parse_key(k))
                .collect::<Result<Vec<_>, _>>()?,
            wm_class: String::new(),
            desktop_type: cfg.desktop.de_type(),
            verbose,
        })
    }

        /// Set the WM_CLASS / app_id of the current focused window.
    ///
        /// There are two ways to decide "Ctrl is already taken":
    ///
        /// 1. The `.desktop` entry's `Categories` contains `TerminalEmulator` — judged by the
        ///    freedesktop spec, automatically covering newly installed terminals, unaffected by case or prefix
        ///    differences. A hard-coded list once missed because it was written as `ptyxis` while the actual wm_class is
        ///    `org.gnome.Ptyxis`.
        /// 2. A hit on the `ctrl_busy_apps` config list — for non-terminal special cases such as
        ///    standalone-window editors.
    ///
        /// On a hit, Cmd maps to Ctrl+Shift, avoiding the established meaning of Ctrl inside these apps
        /// (terminal Ctrl+C interrupt, GVIM Ctrl+V block selection).
    pub fn set_window_id(&mut self, id: &str) {
        self.wm_class = id.to_string();
        let is_term = crate::desktop::is_terminal(id);
        let in_list = self.ctrl_busy_apps.iter().any(|t| id.contains(t));
        self.is_terminal = is_term;
        self.ctrl_busy_window = is_term || in_list;

        if is_term {
                        // Ask the terminal itself first: which keys does it bind with Ctrl+Shift.
            //
                        // A hand-written whitelist missed `n` (new window) and also misjudged the close-window
                        // binding key. For any terminal that declares shortcuts via GSettings (GNOME family),
                        // read its schema directly — if the user changed the binding we keep up automatically.
            let queried = crate::accel::ctrl_shift_keys(id);
            if !queried.is_empty() {
                let parsed: Vec<KeyCode> = queried.iter().filter_map(|k| accel_key(k)).collect();
                if !parsed.is_empty() {
                    self.terminal_bindings = parsed;
                }
            }
        }

        if self.verbose && self.ctrl_busy_window {
                        let via = if is_term { "TerminalEmulator" } else { "config list" };
            let n = self.terminal_bindings.len();
            eprintln!(
                                "[mapper] Ctrl-busy app matched: {id} ({via}) → {n} keys mapped to Ctrl+Shift"
            );
        }
    }

        /// Test-only: skip the `.desktop` lookup and mark the window directly as "Ctrl is taken".
    ///
        /// The terminal specialization's **behavior** (Cmd→Ctrl+Shift) and the terminal **detection** are two
        /// different things: detection is `desktop::is_terminal`'s job and already has its own tests. What we verify here
        /// is the mapping "after it has been decided to be a terminal", so it must not depend on whether this machine has a
        /// given terminal installed — distros differ in default terminal (Ubuntu 26.04 only has ptyxis, which happens to be on
        /// the "leaks SIGINT" list and goes through Ctrl+Insert rather than Ctrl+Shift).
    #[cfg(test)]
    pub fn force_ctrl_busy_window(&mut self, id: &str) {
        self.wm_class = id.to_string();
        self.is_terminal = true;
        self.ctrl_busy_window = true;
                // Keep the default whitelist and do not query GSettings: so test results do not depend on the machine's terminal.
    }

        /// Handle one input event and produce the sequence of events to inject (possibly empty).
    pub fn translate(&mut self, ev: &InputEvent) -> Vec<InputEvent> {
                // 1) Pass through non-key events, keeping the time flow consistent with the real device
        //
                // The MacBook "keyboard/trackpad" is the same evdev device, and trackpad activity comes in as
                // MISC/MSC events. Dropping them outright has two problems:
        //
                //   a) the trackpad stops working entirely (clicks, scrolling, multi-finger gestures all rely on these events)
                //   b) more subtly: trackpad events also bring SYN boundaries that break up key batches.
                //      Input methods aggregate keys by event spacing (e.g. Shuangpin f+a merges into one syllable);
                //      when the rhythm is broken they commit letter by letter, showing as "only pinyin comes out, letters are lost".
        //
                // So forward such events as-is — no mapping at all, but keep their position
                // on the timeline so downstream sees the exact rhythm of a real keyboard + trackpad.
        //
                // Exception for SYNCHRONIZATION: uinput's emit() adds SYN_REPORT automatically,
                // so forwarding the original SYN would split one batch into multiple sync reports.
        if ev.event_type() != EventType::KEY {
                        // MISC/MSC and other non-key events are dropped entirely; the original device handles them.
            //
                        // Forwarding them was tried, to keep trackpad activity, but it failed: forwarding sends the ABS multi-touch
                        // events to the virtual device too, and the virtual device does not declare EV_ABS capability,
                        // causing both the trackpad and the keys to misbehave (dropped letters when typing).
            return vec![];
        }

        let code = KeyCode(ev.code());
        let value = ev.value();

                // 2) Drop auto-repeat
        if value == 2 {
            return vec![];
        }

        let pressed = value == 1;

        if code == KeyCode::KEY_FN {
            self.fn_held = pressed;
                        // Fn itself is not injected to the output (on most machines the kernel does not expose it anyway)
            return vec![];
        }

        if pressed {
            if is_modifier(code) {
                self.handle_press(code)
            } else {
                self.handle_key_press(code)
            }
        } else {
            self.handle_release(code)
        }
    }

        /// Press handling.
    fn handle_press(&mut self, code: KeyCode) -> Vec<InputEvent> {
                // Apply remap to get the "logical key"
        let logical = self.remap.get(&code).copied().unwrap_or(code);

                // Modifier: record state, suppress output
                // Only Cmd needs "suppress on press, decide once the combo is known".
        //
                // The other modifiers (Shift, Alt, Ctrl) should be output as-is: they do not participate in
                // the Cmd lookup, and suppressing them would leave downstream (e.g. fcitx5 watching a bare Shift_L to switch input method)
                // with no key event.
        //
                // A remapped modifier (e.g. CapsLock→LEFTCTRL) must output the target key, not the original.
        if logical != code {
            self.held_push(logical);
            self.output_down.push(logical);
            return self.press_as(code, logical);
        }

                // Cmd not remapped: suppress it and wait for the following combo to decide.
                // Record it in suppressed so the release emits nothing either — we never pressed it,
                // so we must not emit a release with no matching press.
        if code == KeyCode::KEY_LEFTMETA || code == KeyCode::KEY_RIGHTMETA {
            self.held_push(logical);
            self.suppressed.insert(code);
            return vec![];
        }

                // The remaining modifiers (Shift / Alt / Ctrl) pass through as-is, but **must still be recorded in held**.
        //
                // held has two uses here, and they do not conflict:
                //   1. deciding whether a Cmd combo hits (cmd_map entries may declare extra modifiers,
                //      e.g. "S-z" for Cmd+Shift+Z)
                //   2. on combo translation, releasing these input-side modifiers first
        //
                // Pass-through does not mean invisible — the output side still receives the event; it just must also be remembered internally.
        self.held_push(logical);
        self.output_down.push(logical);
        self.press_as(code, code)
    }

        /// Press and register: record "what this input key actually output as".
    ///
        /// On release we restore from this record, with no need to re-decide "was it translated".
        /// Registering uniformly also makes "pass-through" the default value rather than a special case.
    fn press_as(&mut self, input: KeyCode, output: KeyCode) -> Vec<InputEvent> {
        self.pressed.insert(input, output);
        if !self.output_down.contains(&output) {
            self.output_down.push(output);
        }
        self.emit_key(output, 1)
    }

        /// Fn-key preprocessing: which key F1–F12 actually represent under the current Fn state and mode.
    ///
        /// Only **substitution**, never an early return — the caller must continue through the normal combo flow.
    ///
        /// An early implementation returned `emit_key(media, 1)` right here, bypassing the whole Cmd
        /// lookup. The consequence was that `Fn+Meta+Shift+F1` (the macOS combo for function keys)
        /// leaked a Meta press — Meta had been suppressed earlier, yet here it was emitted as-is — while
        /// Ctrl/Shift were lost. The observed output was `Meta + media key`, clearly wrong.
    ///
        /// The direction is decided by [`FnMode`]:
        /// - `Fkeysfirst`: F keys when Fn is not held, media keys when it is
        /// - `Fkeyslast`: the reverse
    fn resolve_fn(&self, code: KeyCode) -> KeyCode {
        if self.fn_mode == FnMode::Disabled {
            return code;
        }
                // Only a forward lookup is needed, no reverse. The reverse ("media key → F key") does not
                // occur when the kernel's fnmode=1: then the kernel reports F keycodes directly, and F1 coming in
                // should just be output as-is.
        //
                // Division of labor under the kernel's `fnmode=2` (this machine's current value):
                //   F1 → KEY_F1, Fn+F1 → KEY_BRIGHTNESSDOWN  ← already translated by the kernel
                // So here, only when `fkey_media` finds a media key do we convert; otherwise pass through as-is.
        match (self.fn_held, self.fn_mode) {
            (true, FnMode::Fkeysfirst) | (false, FnMode::Fkeyslast) => {
                self.fkey_media.get(&code).copied().unwrap_or(code)
            }
            _ => code,
        }
    }


        /// Table lookup. Returns `(output modifiers, output main key)`, or `None` on a miss.
    ///
        /// Linear scan, taking the first hit in declaration order. The table is small (a few dozen rows),
        /// and this keeps "declaration order is priority" explicit, with no extra sorting or indexing.
    fn lookup(
        &self,
        code: KeyCode,
        cur_mods: &[KeyCode],
        cmd_held: bool,
    ) -> Option<Outcome> {
        for r in &self.rules {
            if r.requires_cmd && !cmd_held {
                continue;
            }
            if r.key.1 != cur_mods {
                continue;
            }
            if !r.wildcard && r.key.0 != code {
                continue;
            }
            if !r.app.matches(&self.wm_class, self.is_terminal) {
                continue;
            }
            if self.verbose
                && let Some(d) = &r.desc
            {
                eprintln!("[mapper] rule hit: Cmd+{:?} — {d}", code);
            }
            match &r.act {
                CompiledAction::Emit(mods, key) => {
                    return Some(Outcome::Convert(mods.clone(), *key))
                }
                CompiledAction::DeAccel(feature) => {
                                        // Read the DE's current binding and emit the same keys. It does not rewrite the user's config,
                                        // and if the user changed the binding we follow automatically. Unavailable rows were dropped at
                                        // startup; here a `match` still backs it up — the DE binding may change at runtime.
                    match de_feature_accel(&self.desktop_type, feature) {
                        Some(DeKeys::Combo(mods, key)) => {
                            return Some(Outcome::Convert(mods, key))
                        }
                        Some(DeKeys::Tap(k)) => return Some(Outcome::Tap(k)),
                        None => continue,
                    }
                }
                CompiledAction::TerminalCtrlShift => {
                                        // Conditional action: add Shift only when the target app **itself** binds
                                        // this key with Ctrl+Shift. Shift is added to hit its existing
                                        // binding, not to generically dodge conflicts.
                    //
                                        // If unbound it falls to the default semantics — this is exactly the point of "per key" rather than
                                        // "swap the semantics wholesale": `Cmd+Q` is not on this whitelist, so it stays
                                        // the global Alt+F4, and does not become a Ctrl+Shift+Q nobody recognizes.
                    if self.ctrl_busy_window && self.terminal_bindings.contains(&code) {
                        let mut mods = cur_mods.to_vec();
                        mods.push(KeyCode::KEY_LEFTCTRL);
                        mods.push(KeyCode::KEY_LEFTSHIFT);
                        mods.sort();
                        mods.dedup();
                        return Some(Outcome::Convert(mods, code));
                    }
                    continue;
                }
                CompiledAction::Exec {
                    program,
                    args,
                    confirm,
                    prompt,
                    ok_label,
                } => {
                                        // An operation that is essentially a command rather than a combo.
                    //
                                        // Returning `Some(empty modifiers, empty main key)` means "handled, but no key to emit" —
                                        // the caller uses that to skip the whole output flow and run only the side effect.
                    let key_desc = format!(
                        "Cmd+{}",
                        r.key
                            .1
                            .iter()
                            .filter_map(|m| match *m {
                                KeyCode::KEY_LEFTSHIFT => Some("Shift+"),
                                KeyCode::KEY_LEFTALT => Some("Alt+"),
                                _ => None,
                            })
                            .collect::<String>()
                            + &format!("{:?}", r.key.0)
                    );
                    crate::exec::run(crate::exec::ExecRequest {
                        program,
                        args,
                        label: r.desc.as_deref(),
                        confirm: *confirm,
                        prompt: prompt.as_deref(),
                        ok_label: ok_label.as_deref(),
                        confirm_key: Some(&key_desc),
                        scoped: !matches!(r.app, AppMatch::Any),
                    });
                                        // Side effect already produced; no key needs to be emitted
                    return Some(Outcome::NoOutput);
                }
            }
        }
        None
    }

        /// Press handling for a non-modifier key.
    fn handle_key_press(&mut self, physical: KeyCode) -> Vec<InputEvent> {
                // Fn preprocessing: substitute only the key itself, then go through the flow below uniformly.
        //
                // `pressed` is still keyed by the **physical key** — `handle_release` can look up the physical key directly
                // to get the output at the time, without re-inferring the Fn state (Fn may have been released before
                // the F key, and re-inferring would give the wrong result). Invariant 2 relies on this.
        let code = self.resolve_fn(physical);

                // Cmd-combo handling
        let cmd_held = self
            .held
            .iter()
            .any(|m| *m == KeyCode::KEY_LEFTMETA || *m == KeyCode::KEY_RIGHTMETA);

        {
                        // Modifiers currently held besides Cmd.
            //
                        // Two copies, different uses, **must not be mixed**:
                        // - `held_mods`: the **real** pressed keys, used for emitting events and balancing releases
                        // - `cur_mods`: the **normalized** keys, used only for the table lookup
            //
                        // Why normalize: physical Ctrl becomes CapsLock via remap, but is still semantically
                        // Ctrl, while the rule table writes `C-` (= KEY_LEFTCTRL). Without normalization
                        // rules like `Ctrl+Cmd+Q` would never match.
            //
                        // But **emitting must use the real ones**: emitting a normalized value would press LeftCtrl
                        // on the output side, while the release path restores CapsLock per the pressed table —
                        // they would not match, and LeftCtrl would stay stuck down forever.
            let held_mods: Vec<KeyCode> = self
                .held
                .iter()
                .copied()
                .filter(|m| *m != KeyCode::KEY_LEFTMETA && *m != KeyCode::KEY_RIGHTMETA)
                .collect();
            let mut cur_mods = held_mods.clone();
            cur_mods = cur_mods
                .into_iter()
                .map(|m| match m {
                    KeyCode::KEY_CAPSLOCK | KeyCode::KEY_RIGHTCTRL => KeyCode::KEY_LEFTCTRL,
                    other => other,
                })
                .collect();
            cur_mods.sort();
            cur_mods.dedup();

                        // Table lookup: **applies to all combos**, not just Cmd combos.
            //
                        // An early version put the lookup inside the `if cmd_held` branch, so combos without Cmd
                        // (such as macOS's `Option+Delete`) could never find a rule —
                        // writing it in the table was useless. Now it is outside the branch.
            //
                        // The table takes priority; only on a miss do the default semantics apply (non-Cmd combos pass through as-is).
            let decision = match self.lookup(code, &cur_mods, cmd_held) {
                Some(Outcome::Convert(m, k)) => Default::Convert(m, k),
                Some(Outcome::Tap(k)) => {
                                        // "Tap": first release all keys still held on the output side, then press and immediately
                                        // release the target key, then return immediately.
                    //
                                        // Releasing first is mandatory: when `Ctrl+↑` triggers the overview the user is still physically holding
                                        // Ctrl, which has already been pressed on the output side as a pass-through modifier. If it is not cleared, mutter
                                        // sees `Ctrl+Super` rather than a bare Super, and overlay-key does not trigger.
                                        // The combo-convert path (`Default::Convert`) likewise drains output_down first,
                                        // so this stays consistent.
                    //
                                        // The target key must be released **immediately** because GNOME's overlay-key opens the
                                        // overview on release; if Super were left until the physical key (Space) is released, mutter would
                                        // treat it as `Super+Space` (input-method switch) in the meantime.
                    //
                                        // This physical key does not enter `pressed` state tracking — it produces no lasting output,
                                        // so its release event (`handle_release`) will naturally produce nothing because "the output side is not pressed".
                    // naturally produce nothing.
                    let mut out = Vec::new();
                    for h in std::mem::take(&mut self.output_down) {
                        out.extend(self.emit_key(h, 0));
                    }
                                        // The injected modifiers have already been released along with output_down above, so clear the record,
                                        // to avoid their release events being emitted again on a later path.
                    self.injected_mods.clear();
                    out.extend(self.emit_key(k, 1));
                    out.extend(self.emit_key(k, 0));
                    return out;
                }
                                // The action has already run; no key should be emitted
                Some(Outcome::NoOutput) => return Vec::new(),
                None if cmd_held => default_semantics(&cur_mods, code),
                                // A non-Cmd combo with no table entry: pass through as-is
                None => Default::PassthroughPlain,
            };

                        // Pass through: hand what the user pressed to the app as-is.
            //
                        // Cmd was suppressed earlier (not output), so it must be explicitly re-added here —
                        // it never entered output_down, and not adding it here would lose it,
                        // emitting a crippled combo missing Cmd.
            let converted = match decision {
                Default::PassthroughPlain => {
                                        // A non-Cmd combo with no rule in the table: pass through as-is.
                    //
                                        // The modifiers (Shift/Alt/Ctrl) were already passed through on press, so here they must **not**
                                        // be pressed or released again — otherwise every key press would produce a
                                        // "release Shift then press Shift" jitter,
                                        // and input methods like fcitx5 would drop letters because of it.
                    self.output_down.push(code);
                    self.pressed.insert(physical, code);
                    if self.verbose {
                                                eprintln!("[mapper] {:?}+{:?} has no rule in the table → pass-through", cur_mods, code);
                    }
                    return self.emit_key(code, 1);
                }
                Default::Passthrough => {
                                        // A Cmd combo with no way to convert: hand what the user pressed to the app as-is.
                    let mut out = Vec::new();
                    for h in std::mem::take(&mut self.output_down) {
                        out.extend(self.emit_key(h, 0));
                    }
                    for m in &held_mods {
                        out.extend(self.emit_key(*m, 1));
                        self.output_down.push(*m);
                    }
                                        // Cmd is emitted as a real Super.
                //
                                // It must also be removed from `suppressed`: Meta was suppressed on press and
                                // never entered output_down. If it is not removed, the release path takes
                                // the `suppressed.remove()` branch, returns directly and emits no release —
                                // leaving this injected Super stuck down forever.
                //
                                // The consequence is real-machine level: after locking the screen, when entering a password every key carries Super,
                                // digits like 9 cannot be typed, and the only way out is a hard power-off.
                self.suppressed.remove(&KeyCode::KEY_LEFTMETA);
                out.extend(self.emit_key(KeyCode::KEY_LEFTMETA, 1));
                self.output_down.push(KeyCode::KEY_LEFTMETA);
                out.extend(self.emit_key(code, 1));
                self.output_down.push(code);
                self.pressed.insert(physical, code);
                if self.verbose {
                    eprintln!(
                                                "[mapper] Cmd+{:?}{:?} has no rule and cannot be converted → pass through",
                        code, cur_mods
                    );
                }
                    return out;
                }
                Default::Convert(out_mods, out_key) => (out_mods, out_key),
            };
            let (out_mods, out_key) = converted;

            if self.verbose {
                eprintln!(
                    "[mapper] Cmd+{:?} -> {:?}{:?}{}",
                    code,
                    out_mods,
                    out_key,
                                        if self.ctrl_busy_window { " (Ctrl-busy app)" } else { "" }
                );
            }

            let mut out = Vec::new();
                        // Release the modifiers still down on the output side, then press the new output modifiers.
            //
                        // This operates on output_down (output state), not held (physical state) —
                        // while the user is still holding Cmd, held must not change because events were sent.
            let down_now: Vec<KeyCode> = std::mem::take(&mut self.output_down);
            for h in down_now {
                out.extend(self.emit_key(h, 0));
            }
            for m in &out_mods {
                out.extend(self.emit_key(*m, 1));
                self.output_down.push(*m);
            }
            out.extend(self.emit_key(out_key, 1));
            self.output_down.push(out_key);
            self.injected_mods.extend(out_mods.iter().copied());
            self.pressed.insert(physical, out_key);
            out
        }
    }

        /// Release handling. The press translated, so this must restore.
    fn handle_release(&mut self, code: KeyCode) -> Vec<InputEvent> {
                // 1) Update physical state. held stores the remapped **logical key**, while the event carries the
                //    **physical key**; both must be cleared.
        self.held_retain(|m| m != code);
        if let Some(&logical) = self.remap.get(&code) {
            self.held_retain(|m| m != logical);
        }

                // 2) Check output state: if the key is still down, remove it and prepare to emit its release.
        //
                // If it is no longer in output_down, it was already released early by a combo translation
                // (e.g. the pass-through Shift was released when translating Cmd+Shift+Z). The output side
                // then has no such key anymore, and emitting a release again would be superfluous and might be misread by the app.
        let out_key = self.pressed.remove(&code);
        let still_down = out_key
            .map(|ok| {
                let pos = self.output_down.iter().position(|m| *m == ok);
                if let Some(i) = pos {
                    self.output_down.remove(i);
                }
                pos.is_some()
            })
            .unwrap_or(false);

                // 3) Suppressed key: if the output side genuinely does not have it, emit no release either.
        //
                // The criterion is "is it still down on the output side", **not** "was it ever suppressed".
                // A key that was once suppressed may still be emitted deliberately on a later path — e.g.
                // `Ctrl+Cmd+Q` lock screen emits `Super+L`, where the Super is exactly
                // the Meta that was originally suppressed. In that case, returning early on "was suppressed"
                // swallows the release, and Super stays stuck down forever:
                // afterwards Space becomes Cmd+Space and W becomes Cmd+W.
        let was_suppressed = self.suppressed.remove(&code);
        if was_suppressed && !self.output_down.contains(&code) {
            return vec![];
        }

                // 4) If a registration exists from press time, the release must be symmetric — but only if the output side is genuinely still down.
                // If still_down is false, the key was already released early by a combo translation,
                // and the output side has long had no such key; emitting a release here would be a superfluous event.
        if let Some(output) = out_key {
            if !still_down {
                return vec![];
            }
            let mut out = Vec::new();
            out.extend(self.emit_key(output, 0));
                        // The modifiers injected during translation are released here as well, otherwise they would remain down
            for m in std::mem::take(&mut self.injected_mods) {
                out.extend(self.emit_key(m, 0));
                self.output_down.retain(|x| *x != m);
            }
            return out;
        }

                // No need to repeat the Fn translation here: on press the "physical key → output key" was recorded in
                // `pressed` (keyed by physical key), and the `out_key` branch above releases symmetrically directly.
                // Translating again here would give the wrong key once Fn is released before the F key.

                // Fallback: an unregistered key. When the output side is not down it must not emit a release
                // (that would be a superfluous event with no matching press).
        if !self.output_down.contains(&code) {
            return vec![];
        }

        self.output_down.retain(|m| *m != code);
        self.emit_key(code, 0)
    }

    fn held_push(&mut self, k: KeyCode) {
        if !self.held.contains(&k) {
            self.held.push(k);
        }
    }

    fn held_retain(&mut self, f: impl Fn(KeyCode) -> bool) {
        self.held.retain(|k| f(*k));
    }

    fn emit_key(&self, code: KeyCode, value: i32) -> Vec<InputEvent> {
        self.emit_key_raw(code, value)
    }

    fn emit_key_raw(&self, code: KeyCode, value: i32) -> Vec<InputEvent> {
        vec![InputEvent::new(EventType::KEY.0, code.0, value)]
    }
}
/// Table mapping F1-F12 to brightness/media keys.
///
/// The MacBook keyboard's top row has dual-function keys. A bare press gives the F key, holding Fn gives brightness/volume/
/// playback control. This table does the switch in user space, without relying on `hid_apple`'s `fnmode`.
fn build_fkey_media_table() -> HashMap<KeyCode, KeyCode> {
    let pairs = [
        (KeyCode::KEY_F1, KeyCode::KEY_BRIGHTNESSDOWN),
        (KeyCode::KEY_F2, KeyCode::KEY_BRIGHTNESSUP),
        (KeyCode::KEY_F3, KeyCode::KEY_DASHBOARD), // Mission Control
        (KeyCode::KEY_F4, KeyCode::KEY_SEARCH),           // Launchpad / Spotlight
        (KeyCode::KEY_F5, KeyCode::KEY_KBDILLUMDOWN),
        (KeyCode::KEY_F6, KeyCode::KEY_KBDILLUMUP),
        (KeyCode::KEY_F7, KeyCode::KEY_PREVIOUSSONG),
        (KeyCode::KEY_F8, KeyCode::KEY_PLAYPAUSE),
        (KeyCode::KEY_F9, KeyCode::KEY_NEXTSONG),
        (KeyCode::KEY_F10, KeyCode::KEY_MUTE),
        (KeyCode::KEY_F11, KeyCode::KEY_VOLUMEDOWN),
        (KeyCode::KEY_F12, KeyCode::KEY_VOLUMEUP),
    ];
    pairs.into_iter().collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

    fn default_mapper() -> Mapper {
        let cfg = Config::with_defaults();
        Mapper::new(&cfg, false).unwrap()
    }

    fn terminal_mapper() -> Mapper {
        let mut m = default_mapper();
                // Inject a "decided to be a terminal" context, rather than depending on whether some machine has ghostty installed
                // (distros differ in default terminal). Terminal detection itself is covered by desktop's tests.
                // Use a terminal that does not leak SIGINT as the representative (ptyxis has its own rule, see below)
        m.force_ctrl_busy_window("com.mitchellh.ghostty");
        m
    }

        /// In a terminal, Cmd combos map to Ctrl+Shift, avoiding established meanings such as Ctrl+C interrupt.
    #[test]
    fn terminal_cmd_becomes_ctrl_shift() {
        let mut m = terminal_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_C.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                && out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                && out.contains(&(KeyCode::KEY_C.0, 1)),
                        "in a terminal Cmd+C should be Ctrl+Shift+C (copy), actual {out:?}"
        );
    }

        /// In a terminal, Cmd+W → Ctrl+Shift+W (close tab), Cmd+T → Ctrl+Shift+T.
    ///
        /// This is not "dodging a conflict" but "finding the correspondence": what is idle is Ctrl+W, while
        /// Ctrl+Shift+W is precisely the Linux terminal's close-tab key. Toshy's config
        /// likewise uses `C("RC-W"): C("C-Shift-W")`.
    #[test]
    fn terminal_cmd_w_closes_tab() {
        for (key, why) in [
                        (KeyCode::KEY_W, "close tab"),
                        (KeyCode::KEY_T, "new tab"),
        ] {
            let mut m = terminal_mapper();
            m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
            let out = desc(&m.translate(&k(key.0, 1)));
            assert!(
                out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                    && out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                    && out.contains(&(key.0, 1)),
                                "in a terminal Cmd+{key:?} ({why}) should be Ctrl+Shift+{key:?}, actual {out:?}"
            );
        }
    }

        /// The overview (macOS's `Cmd+Space`) has two forms depending on the GNOME version:
    ///
        /// - older: `toggle-overview` is bound (EndeavourOS observed `Ctrl+Shift+Space`)
        /// - as of GNOME 50: the action is unbound by default (`@as []`), and the overview is triggered by mutter's
        ///   `overlay-key` (a bare `Super` press), corresponding to a single Super tap
    ///
        /// Both must make `Cmd+Space` produce output consistent with the action.
    #[test]
    fn cmd_space_maps_to_gnome_overview() {
        let Some(feature) = de_feature_accel(&DesktopType::Gnome, "overview") else {
                        // GNOME is not installed on this machine, or neither mechanism can be found: skip rather than misjudge
            return;
        };

                // End-to-end: Cmd+Space should produce the keys corresponding to that action
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_SPACE.0, 1)));

        match feature {
            DeKeys::Combo(mods, key) => {
                for m in &mods {
                                        assert!(out.contains(&(m.0, 1)), "modifier {m:?} not emitted: {out:?}");
                }
                                assert!(out.contains(&(key.0, 1)), "main key {key:?} not emitted: {out:?}");
            }
            DeKeys::Tap(key) => {
                assert!(
                    out.contains(&(key.0, 1)) && out.contains(&(key.0, 0)),
                                        "overlay-key should be tapped (pressed then immediately released), actual {out:?}"
                );
                                // The tap must be clean: no output key may be left down, otherwise mutter
                                // sees a combo with modifiers and overlay-key does not trigger.
                assert!(
                    m.output_down.is_empty(),
                                        "output side still has a pressed key after the tap: {:?}",
                    m.output_down
                );
            }
        }
    }

        /// `Ctrl+↑` (`cmd = false`) must also be able to trigger the overview.
    ///
        /// Key difference: Ctrl is a **pass-through** modifier the user physically holds, and it lands in output_down.
        /// If it is not cleared before the tap, mutter receives `Ctrl+Super` rather than a bare Super,
        /// and overlay-key does not open the overview. This test specifically covers that — stricter than Cmd+Space.
    #[test]
    fn ctrl_up_overview_is_a_clean_tap() {
                // Only meaningful when the overview goes the "tap" path (older versions use a combo, covered above)
        if !matches!(
            de_feature_accel(&DesktopType::Gnome, "overview"),
            Some(DeKeys::Tap(_))
        ) {
            return;
        }

        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_UP.0, 1)));
        assert!(
            m.output_down.is_empty(),
                        "the pass-through Ctrl must be released before the tap; actual residual {:?}: {out:?}",
            m.output_down
        );
    }

        /// KDE / bare WM have no corresponding implementation yet, but must not panic.
    ///
        /// "Cannot be looked up" does not mean "does not exist" — the DE only appears in the `de_action_lookup` table,
        /// and adding support only requires one line, without touching any branching logic.
    #[test]
    fn unknown_desktop_skips_overview_gracefully() {
        assert!(de_action_lookup(&DesktopType::Kde, "overview").is_none());
        assert!(de_action_lookup(&DesktopType::None, "overview").is_none());
    }

        /// Key-name parsing in dynamic data (DE bindings, terminal schemas) must use the same rules as config.
    ///
        /// mapper and accel once each had their own `plus`/`minus` mapping, which already diverged —
        /// one had a `"space"` special case and the other did not, and it only stayed hidden because `KEY_SPACE`
        /// happened to be parsed by the fallback branch.
    #[test]
    fn external_key_parsing_is_consistent() {
        use crate::keys::parse_external_key as p;
        assert_eq!(p("plus"), Some(KeyCode::KEY_EQUAL));
        assert_eq!(p("space"), Some(KeyCode::KEY_SPACE));
        assert_eq!(p("Page_Down"), Some(KeyCode::KEY_PAGEDOWN));
        assert_eq!(p("F4"), Some(KeyCode::KEY_F4));
        assert_eq!(p("w"), Some(KeyCode::KEY_W));
        assert_eq!(p("KEY_A"), Some(KeyCode::KEY_A));
                // GNOME's `overlay-key` value is the string `Super`; the overview fallback must be able to parse it
        assert_eq!(p("Super"), Some(KeyCode::KEY_LEFTMETA));
        assert_eq!(p(""), None);
        assert_eq!(p("NotAKey"), None);
    }

        /// ptyxis's new-window/close-window keys are decided by its own GSettings, not by guessing.
    ///
        /// ptyxis declares `new-window = Ctrl+Shift+N`. A hand-written whitelist once missed `n`,
        /// making `Cmd+N` degrade to `Ctrl+N` (readline's "next line") and fail to open a
        /// new window. Here we read its schema directly, so a changed binding is followed too.
    #[test]
    fn ptyxis_new_window_comes_from_its_own_schema() {
        let mut m = default_mapper();
        m.set_window_id("org.gnome.Ptyxis");
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_N.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                && out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                && out.contains(&(KeyCode::KEY_N.0, 1)),
                        "ptyxis Cmd+N should be Ctrl+Shift+N (new window), actual {out:?}"
        );
    }

        /// ptyxis's close-window key is Ctrl+Shift+Q (as its schema declares),
        /// but the global rule Cmd+Q → Alt+F4 takes priority — Alt+F4 is GNOME's close
        /// binding, consistent across apps, and must not be swallowed by the terminal specialization.
    #[test]
    fn cmd_q_stays_alt_f4_in_ptyxis() {
        let mut m = default_mapper();
        m.set_window_id("org.gnome.Ptyxis");
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_Q.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTALT.0, 1))
                && out.contains(&(KeyCode::KEY_F4.0, 1)),
                        "ptyxis Cmd+Q should be Alt+F4, actual {out:?}"
        );
    }

        /// ptyxis / konsole's copy key goes through Ctrl+Insert instead.
    ///
        /// With **no selected text**, these two terminals pass `Ctrl+Shift+C` through to the child process
        /// as SIGINT — "copy" would kill the running task. `Ctrl+Insert` is
        /// the traditional X11 copy key; all terminals support it and it is not sent as a control character.
    ///
        /// Whereas wezterm / ghostty / gnome-terminal do not have this problem (they swallow the
        /// combo unconditionally), so they are unaffected.
    #[test]
    fn sigint_leaky_terminals_use_ctrl_insert() {
        for wm in ["org.gnome.Ptyxis", "org.kde.konsole"] {
            let mut m = default_mapper();
            m.set_window_id(wm);
            m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
            let out = desc(&m.translate(&k(KeyCode::KEY_C.0, 1)));
            assert!(
                out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                    && out.contains(&(KeyCode::KEY_INSERT.0, 1)),
                                "{wm} Cmd+C should be Ctrl+Insert (avoid SIGINT), actual {out:?}"
            );
            assert!(
                !out.iter().any(|(c, _)| *c == KeyCode::KEY_LEFTSHIFT.0),
                                "{wm} Cmd+C must not carry Shift: {out:?}"
            );
        }
    }

    /// `Shift+Enter` in a terminal that cannot report it becomes `Ctrl+J`.
    ///
    /// The pass-through Shift must be released first: leaving it down would make
    /// the output `Ctrl+Shift+J`, a different combo from what the rule says.
    #[test]
    fn shift_enter_becomes_ctrl_j_in_blind_terminals() {
        for &wm in crate::config::SHIFT_ENTER_BLIND_TERMINALS {
            let mut m = default_mapper();
            m.set_window_id(wm);

            let shift_down = desc(&m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1)));
            assert_eq!(
                shift_down,
                vec![(KeyCode::KEY_LEFTSHIFT.0, 1)],
                "{wm}: Shift is passed through on press (fcitx5 watches a bare Shift)"
            );

            let down = desc(&m.translate(&k(KeyCode::KEY_ENTER.0, 1)));
            assert!(
                down.contains(&(KeyCode::KEY_LEFTSHIFT.0, 0)),
                "{wm}: the pass-through Shift must be released: {down:?}"
            );
            assert!(
                down.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                    && down.contains(&(KeyCode::KEY_J.0, 1)),
                "{wm}: Shift+Enter should become Ctrl+J: {down:?}"
            );

            let up = desc(&m.translate(&k(KeyCode::KEY_ENTER.0, 0)));
            assert!(
                up.contains(&(KeyCode::KEY_J.0, 0)) && up.contains(&(KeyCode::KEY_LEFTCTRL.0, 0)),
                "{wm}: releasing Enter must lift both, or Ctrl stays stuck: {up:?}"
            );

            let shift_up = desc(&m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0)));
            assert!(
                shift_up.is_empty(),
                "{wm}: Shift was already released by the translation: {shift_up:?}"
            );
            assert!(
                m.held.is_empty(),
                "{wm}: nothing may stay held: {:?}",
                m.held
            );
        }
    }

    /// `[terminal] shift_enter = "off"` must restore the native behavior even
    /// in a VTE terminal.
    #[test]
    fn shift_enter_off_keeps_native_behavior() {
        let cfg = Config::with_defaults_for(
            true,
            &crate::config::TerminalConfig {
                shift_enter: Some(crate::config::ShiftEnterMode::Off),
                ..crate::config::TerminalConfig::default()
            },
        );
        let mut m = Mapper::new(&cfg, false).unwrap();
        m.set_window_id("org.gnome.Terminal");

        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_ENTER.0, 1)));
        assert!(
            !out.iter().any(|(c, _)| *c == KeyCode::KEY_J.0),
            "off must not emit Ctrl+J: {out:?}"
        );
        assert!(
            out.contains(&(KeyCode::KEY_ENTER.0, 1)),
            "off keeps the native Enter: {out:?}"
        );
    }

    /// A custom `shift_enter_apps` list replaces the built-in VTE list rather
    /// than extending it.
    #[test]
    fn shift_enter_apps_are_scoped_to_the_configured_list() {
        let cfg = Config::with_defaults_for(
            true,
            &crate::config::TerminalConfig {
                shift_enter_apps: Some(vec!["myterm".into()]),
                ..crate::config::TerminalConfig::default()
            },
        );

        let mut m = Mapper::new(&cfg, false).unwrap();
        m.set_window_id("org.gnome.Terminal");
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_ENTER.0, 1)));
        assert!(
            !out.iter().any(|(c, _)| *c == KeyCode::KEY_J.0),
            "a terminal outside the custom list must be untouched: {out:?}"
        );

        let mut m = Mapper::new(&cfg, false).unwrap();
        m.set_window_id("myterm");
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_ENTER.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                && out.contains(&(KeyCode::KEY_J.0, 1)),
            "a terminal inside the custom list should emit Ctrl+J: {out:?}"
        );
    }

    /// The rule is per-app: a terminal that *does* implement the protocol keeps
    /// `Shift+Enter`, and a bare `Enter` is untouched everywhere.
    ///
    /// kitty/foot/wezterm/ghostty/alacritty report `Shift+Enter` properly
    /// (`CSI 13;2u`), and a global remap would also break GUI apps, where
    /// `Shift+Enter` has its own meaning (VS Code inserts a line below, chat
    /// apps insert a newline).
    #[test]
    fn shift_enter_is_untouched_elsewhere() {
        for wm in ["com.mitchellh.ghostty", "org.gnome.Nautilus"] {
            let mut m = default_mapper();
            m.set_window_id(wm);
            m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
            let out = desc(&m.translate(&k(KeyCode::KEY_ENTER.0, 1)));
            assert!(
                !out.iter().any(|(c, _)| *c == KeyCode::KEY_J.0),
                "{wm}: Shift+Enter must not become Ctrl+J: {out:?}"
            );
            assert!(
                out.contains(&(KeyCode::KEY_ENTER.0, 1)),
                "{wm}: Shift+Enter passes through as-is: {out:?}"
            );
        }

        let mut m = default_mapper();
        m.set_window_id("org.gnome.Ptyxis");
        let out = desc(&m.translate(&k(KeyCode::KEY_ENTER.0, 1)));
        assert_eq!(
            out,
            vec![(KeyCode::KEY_ENTER.0, 1)],
            "a bare Enter is still Enter, even in that terminal"
        );
    }

        /// Terminals that do not leak SIGINT still take the normal terminal path (Ctrl+Shift+C).
    ///
        /// Switching them all to Ctrl+Insert would cost these terminals their standard copy key — so
        /// the rule must be per-app and cannot be applied globally.
    #[test]
    fn safe_terminals_keep_ctrl_shift_c() {
        let mut m = terminal_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_C.0, 1)));
        assert!(out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1)), "{out:?}");
        assert!(out.contains(&(KeyCode::KEY_C.0, 1)), "{out:?}");
    }

        /// In an ordinary app, Cmd+C is unaffected by that rule and stays Ctrl+C.
    #[test]
    fn normal_app_cmd_c_is_plain_ctrl() {
        let mut m = default_mapper();
        m.set_window_id("org.gnome.Nautilus");
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_C.0, 1)));
        assert!(out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "{out:?}");
        assert!(
            !out.iter().any(|(c, _)| *c == KeyCode::KEY_LEFTSHIFT.0),
            "{out:?}"
        );
    }

        /// Cmd+Q → Alt+F4, and **the same for terminals and ordinary apps**.
    ///
        /// This is the key difference of the "per-key whitelist" versus "swap the semantics wholesale": macOS's
        /// Cmd+Q (quit app) corresponds to Linux's Alt+F4 (GNOME's
        /// wm.keybindings close), a universal cross-app binding. If a terminal turned Cmd
        /// uniformly into Ctrl+Shift, Cmd+Q would degrade to a Ctrl+Shift+Q nobody recognizes.
    #[test]
    fn cmd_q_is_alt_f4_everywhere() {
        for (wm, label) in [
                        ("org.gnome.Nautilus", "ordinary app"),
                        ("org.gnome.Ptyxis", "terminal"),
                        ("com.mitchellh.ghostty", "terminal"),
        ] {
            let mut m = default_mapper();
            m.set_window_id(wm);
            m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
            let out = desc(&m.translate(&k(KeyCode::KEY_Q.0, 1)));
            assert!(
                out.contains(&(KeyCode::KEY_LEFTALT.0, 1))
                    && out.contains(&(KeyCode::KEY_F4.0, 1)),
                                "{label} ({wm}) Cmd+Q should be Alt+F4, actual {out:?}"
            );
        }
    }

        /// In a terminal, keys not on the whitelist still take the default semantics (Ctrl), not specialized.
    ///
        /// For example Cmd+N: the terminal's Ctrl+N is usually idle, so no need to become Ctrl+Shift+N.
        /// Blanket "always add Shift in a terminal" would ruin keys like this.
    #[test]
    fn terminal_unlisted_key_stays_plain_ctrl() {
        let mut m = terminal_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_N.0, 1)));
        assert!(out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "{out:?}");
        assert!(out.contains(&(KeyCode::KEY_N.0, 1)), "{out:?}");
        assert!(
            !out.iter().any(|(c, v)| *c == KeyCode::KEY_LEFTSHIFT.0 && *v == 1),
                        "a key off the whitelist must not add Shift: {out:?}"
        );
    }

    #[test]
    fn capslock_becomes_leftctrl() {
        let mut m = default_mapper();
                // Pressing CapsLock (58) should output LEFTCTRL (29)
        let out = m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));
        assert_eq!(desc(&out), vec![(KeyCode::KEY_LEFTCTRL.0, 1)]);
    }

    #[test]
    fn leftctrl_becomes_capslock() {
        let mut m = default_mapper();
        let out = m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1));
        assert_eq!(desc(&out), vec![(KeyCode::KEY_CAPSLOCK.0, 1)]);
    }

    #[test]
    fn cmd_c_becomes_ctrl_c() {
        let mut m = default_mapper();
                m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1)); // press Cmd, should produce no output
        let out = m.translate(&k(KeyCode::KEY_C.0, 1));
                // Expected: press Ctrl (29,1) → press c (46,1)
                // Cmd was never pressed to the output side, so its release must not appear.
        let d = desc(&out);
        assert!(
            !d.contains(&(KeyCode::KEY_LEFTMETA.0, 0)),
                        "Cmd was never pressed, so there should be no release: {d:?}"
        );
                assert!(d.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "should press Ctrl: {d:?}");
                assert!(d.contains(&(KeyCode::KEY_C.0, 1)), "should press c: {d:?}");
    }

    #[test]
    fn cmd_not_in_table_passes_through() {
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
                // Cmd+E is not in the default table, so it should pass through as-is
        let out = m.translate(&k(KeyCode::KEY_E.0, 1));
        let d = desc(&out);
                assert!(d.contains(&(KeyCode::KEY_E.0, 1)), "E should be output as-is: {d:?}");
        assert!(!d.iter().any(|(_, v)| *v == 1 && false));
    }

    #[test]
    fn repeat_events_filtered() {
        let mut m = default_mapper();
        let out = m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 2)); // value=2
                assert!(out.is_empty(), "auto-repeat events should be dropped");
    }

        /// Non-key events such as MISC must be dropped.
    ///
        /// Forwarding them as-is to keep trackpad activity was tried, but forwarding sends the ABS multi-touch events
        /// to a virtual device that does not declare EV_ABS capability, making both the trackpad and the keys misbehave.
        /// The trackpad is left to the original device; mackbind only handles the keyboard.
    #[test]
    fn misc_events_filtered() {
        let mut m = default_mapper();
        let ev = InputEvent::new(EventType::MISC.0, 4, 0);
                assert!(m.translate(&ev).is_empty(), "MISC should be dropped");
    }

        /// Exception for SYNCHRONIZATION: uinput's emit() adds SYN_REPORT automatically,
        /// so forwarding the original SYN splits one batch into multiple sync reports.
    #[test]
    fn sync_events_filtered() {
        let mut m = default_mapper();
        let ev = InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0);
                assert!(m.translate(&ev).is_empty(), "SYN is added automatically by uinput, not forwarded");
    }

    #[test]
    fn ctrl_v_after_capslock_is_real_ctrl() {
                // Core scenario: the Ctrl at the CapsLock position should produce a real Ctrl+V in a terminal.
                // Note Ctrl is already injected to the output when CapsLock is pressed, so V itself
                // only needs to output itself — the output side is already in the Ctrl-down state.
        let mut m = default_mapper();
        m.set_window_id("org.gnome.Console");

        let first = m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));
                assert_eq!(desc(&first), vec![(KeyCode::KEY_LEFTCTRL.0, 1)], "Caps should output a real Ctrl");

        let second = m.translate(&k(KeyCode::KEY_V.0, 1));
                assert_eq!(desc(&second), vec![(KeyCode::KEY_V.0, 1)], "V itself is output as-is");

                // Cmd (Super) must not appear anywhere in the sequence
        let mut all = first;
        all.extend(second);
        assert!(
            desc(&all).iter().all(|(c, _)| *c != KeyCode::KEY_LEFTMETA.0),
                        "Cmd must not appear in the Ctrl+V path: {:?}",
            desc(&all)
        );
    }

    #[test]
    fn cmd_c_output_is_released_symmetrically() {
                // Cmd+C → Ctrl+C; then release all keys; the output side should return to no modifiers
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let press = m.translate(&k(KeyCode::KEY_C.0, 1));

                // Release the main key
        let rel_c = m.translate(&k(KeyCode::KEY_C.0, 0));
        assert!(desc(&rel_c).contains(&(KeyCode::KEY_C.0, 0)));

                // Release Cmd: it was suppressed throughout and never pressed, so it produces no events
        let rel_meta = m.translate(&k(KeyCode::KEY_LEFTMETA.0, 0));
        assert!(
            desc(&rel_meta).is_empty(),
                        "Cmd was suppressed, so its release should produce no output: {:?}",
            desc(&rel_meta)
        );

                // End of the whole sequence: every modifier must have been released, with no residual pressed state
        let all: Vec<InputEvent> = press
            .into_iter()
            .chain(rel_c)
            .chain(rel_meta)
            .collect();
        assert!(
            all.last().is_some_and(|e| e.value() == 0),
                        "the sequence should end with a release event: {:?}",
            desc(&all)
        );
                // Ctrl's press and release must occur in pairs
        let ctrl_down = all
            .iter()
            .filter(|e| e.code() == KeyCode::KEY_LEFTCTRL.0 && e.value() == 1)
            .count();
        let ctrl_up = all
            .iter()
            .filter(|e| e.code() == KeyCode::KEY_LEFTCTRL.0 && e.value() == 0)
            .count();
                assert_eq!(ctrl_down, ctrl_up, "Ctrl press/release count should be equal: {:?}", desc(&all));
    }

    #[test]
    fn release_is_symmetric_after_translation() {
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));
        let out = m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 0));
        assert_eq!(desc(&out), vec![(KeyCode::KEY_LEFTCTRL.0, 0)]);
    }
}

#[cfg(test)]
mod regression {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

        /// Regression: after CapsLock is pressed and released, it must not remain in held.
        /// Observed bug: KEY_CAPSLOCK 0 was mixed into the output of Cmd+C.
    #[test]
    fn remapped_modifier_does_not_persist_in_held() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();

                // Press and release CapsLock (logically becomes LEFTCTRL)
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 0));

                // Then press physical left Ctrl and release it
        m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 0));

        assert!(
            m.held.is_empty(),
                        "held should be empty, actual residual {:?}",
            m.held
        );

                // Then press Cmd+C; the output must not contain any CAPSLOCK event
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_C.0, 1));
        let d = desc(&out);
        assert!(
            !d.iter().any(|(c, _)| *c == KeyCode::KEY_CAPSLOCK.0),
                        "Cmd+C output must not contain CAPSLOCK: {d:?}"
        );
        assert!(
            d.iter().any(|(c, _)| *c == KeyCode::KEY_LEFTCTRL.0),
                        "should output Ctrl: {d:?}"
        );
    }

        /// Regression: pressing the CapsLock position (actually Ctrl) then Cmd must not be mistaken for a combo.
    #[test]
    fn caps_then_cmd_does_not_leak() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));  // → LEFTCTRL
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 0));
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_C.0, 1));
        let d = desc(&out);
                // There should be only one LEFTCTRL (from Cmd+C), and no CAPSLOCK release
        assert!(!d.iter().any(|(c, _)| *c == KeyCode::KEY_CAPSLOCK.0), "{d:?}");
    }
}


#[cfg(test)]
mod modifier_passthrough_tests {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

        /// Regression: Shift must be output as-is.
        /// Observed bug: Shift was suppressed, breaking fcitx5's watching of a bare Shift_L to switch input method.
    #[test]
    fn shift_is_passed_through() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let down = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
                assert_eq!(desc(&down), vec![(KeyCode::KEY_LEFTSHIFT.0, 1)], "Shift press must be output");

        let up = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0));
                assert_eq!(desc(&up), vec![(KeyCode::KEY_LEFTSHIFT.0, 0)], "Shift release must be output");
    }

    #[test]
    fn alt_and_ctrl_are_passed_through() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        for code in [
            KeyCode::KEY_LEFTALT,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_RIGHTCTRL,
        ] {
            let mut m2 = Mapper::new(&Config::with_defaults(), false).unwrap();
            let d = m2.translate(&k(code.0, 1));
                        assert_eq!(desc(&d), vec![(code.0, 1)], "{code:?} should be output as-is");
        }
        let _ = &mut m;
    }

        /// Cmd must still be suppressed — this is the core of the design.
    #[test]
    fn cmd_is_still_suppressed() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let down = m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
                assert!(down.is_empty(), "Cmd press should be suppressed (off-table combos must keep Super semantics)");
    }

        /// Cmd+Shift+a should output Shift+a (Cmd is off-table, Shift must really exist)
    #[test]
    fn cmd_shift_combo_keeps_shift() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
                assert_eq!(desc(&out), vec![(KeyCode::KEY_LEFTSHIFT.0, 1)], "Shift should be output");
    }
}

#[cfg(test)]
mod press_release_symmetry {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

        /// Every pass-through key's press and release must occur in pairs with matching keycodes.
        /// This maps directly to the user's question: Shift was released, why was it not passed through.
    #[test]
    fn every_passthrough_modifier_is_symmetric() {
        for code in [
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_RIGHTSHIFT,
            KeyCode::KEY_LEFTALT,
            KeyCode::KEY_RIGHTALT,
            KeyCode::KEY_RIGHTCTRL,
        ] {
            let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
            let down = desc(&m.translate(&k(code.0, 1)));
            let up = desc(&m.translate(&k(code.0, 0)));
                        assert_eq!(down, vec![(code.0, 1)], "{code:?} press should be output as-is");
                        assert_eq!(up, vec![(code.0, 0)], "{code:?} release should be output as-is");
        }
    }

        /// Plain character keys must be symmetric too
    #[test]
    fn plain_keys_are_symmetric() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        assert_eq!(desc(&m.translate(&k(KeyCode::KEY_A.0, 1))), vec![(KeyCode::KEY_A.0, 1)]);
        assert_eq!(desc(&m.translate(&k(KeyCode::KEY_A.0, 0))), vec![(KeyCode::KEY_A.0, 0)]);
    }

        /// A remapped key: press outputs the target key, release also outputs the target key
    #[test]
    fn remapped_key_release_uses_target() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let down = desc(&m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1)));
        let up = desc(&m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 0)));
        assert_eq!(down, vec![(KeyCode::KEY_LEFTCTRL.0, 1)]);
                assert_eq!(up, vec![(KeyCode::KEY_LEFTCTRL.0, 0)], "release must use the target key");
    }

        /// Repeated Shift cycles must leave no residual state
    #[test]
    fn repeated_shift_cycles_clean() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        for _ in 0..3 {
            m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
            m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0));
        }
                assert!(m.pressed.is_empty(), "residual: {:?}", m.pressed);
                assert!(m.held.is_empty(), "held residual: {:?}", m.held);
    }
}

#[cfg(test)]
mod cmd_shift_combo {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

        /// Under the new model, Cmd+Shift+Z no longer goes through a whitelist but is handled by the default
        /// semantics: keep Shift and output Ctrl+Shift+Z. On Linux Ctrl+Shift+Z is exactly redo.
    #[test]
    fn cmd_shift_z_keeps_shift() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));  // Cmd
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1)); // Shift
        let out = m.translate(&k(KeyCode::KEY_Z.0, 1));
        let d = desc(&out);
        assert!(
            d.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)) && d.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                && d.contains(&(KeyCode::KEY_Z.0, 1)),
                        "Cmd+Shift+Z should output Ctrl+Shift+Z, actual: {d:?}"
        );
    }

        /// Reverse check: pressing only Cmd (no Shift) must not hit the Shift+Z entry
    #[test]
    fn cmd_z_without_shift_is_undo() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_Z.0, 1));
        let d = desc(&out);
                assert!(d.contains(&(KeyCode::KEY_Z.0, 1)), "should output z (undo), actual: {d:?}");
                assert!(!d.contains(&(KeyCode::KEY_Y.0, 1)), "must not output y: {d:?}");
    }
}

#[cfg(test)]
mod key_ordering {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

        /// User-observed: Cmd+Z should undo (output z) but once became a bare z (Cmd lost).
        /// This means that after a translation hit, Cmd's release event was not handled correctly.
    #[test]
    fn cmd_z_releases_cmd_and_emits_z() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_Z.0, 1));
        let d = desc(&out);
                assert!(d.contains(&(KeyCode::KEY_Z.0, 1)), "must press z: {d:?}");
                assert!(d.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "must press Ctrl: {d:?}");
                // Cmd was suppressed and never pressed, so there is no release event
        assert!(
            !d.contains(&(KeyCode::KEY_LEFTMETA.0, 0)),
                        "Cmd was not pressed, so no release should be emitted: {d:?}"
        );
    }

        /// User-observed: Cmd+Shift+Z once triggered an input-method switch, meaning Shift was output alone.
        /// Correct behavior: Shift should be released after translation, and must not remain as a bare Shift.
    #[test]
    fn cmd_shift_z_does_not_leak_shift() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = m.translate(&k(KeyCode::KEY_Z.0, 1));
        let d = desc(&out);
                // Shift was previously passed through as pressed; it must be released on translation, otherwise a bare Shift remains
        assert!(
            d.contains(&(KeyCode::KEY_LEFTSHIFT.0, 0)),
                        "Shift was passed through as pressed; it must be released on translation: {d:?}"
        );
                assert!(d.contains(&(KeyCode::KEY_Z.0, 1)), "should output z: {d:?}");
    }
}

#[cfg(test)]
mod collision_tests {
    use super::*;
    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

        /// An explicit rule takes priority over the default semantics.
    ///
        /// Cmd+[ on macOS is "previous folder", corresponding to Alt+← on Linux, unlike the default
        /// Ctrl+[ — this is exactly the case that needs an explicit rule.
    #[test]
    fn explicit_rule_overrides_default() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_LEFTBRACE.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTALT.0, 1)) && out.contains(&(KeyCode::KEY_LEFT.0, 1)),
                        "Cmd+[ should output Alt+←: {out:?}"
        );
                assert!(!out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "must not contain Ctrl: {out:?}");
    }

        /// Likewise covers Cmd+S / Cmd+Shift+S
    #[test]
    fn save_and_saveas_coexist() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let save = desc(&m.translate(&k(KeyCode::KEY_S.0, 1)));
        assert!(save.contains(&(KeyCode::KEY_S.0, 1)), "Cmd+S → Ctrl+S: {save:?}");
    }

        /// An unmapped combo goes through the default semantics: output Ctrl+Shift+T.
    ///
        /// This is exactly the point of the new model — long-tail combos map correctly without exhaustive enumeration.
    #[test]
    fn unmapped_combo_gets_default_translation() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_T.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)) && out.contains(&(KeyCode::KEY_T.0, 1)),
                        "Cmd+Shift+T should output Ctrl+Shift+T: {out:?}"
        );
    }
}

#[cfg(test)]
mod state_separation {
    use super::*;
    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

        /// Core invariant: **held reflects physical state only and never changes because an event was sent.**
    ///
        /// The root cause of the bug the user reported (Cmd+Z became a bare z) was exactly that one combo
        /// translation drained held — Cmd was in fact still held, but internally it was thought released.
    #[test]
    fn held_survives_combo_translation() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        assert!(m.held.contains(&KeyCode::KEY_LEFTMETA));

                // Trigger one combo translation
        m.translate(&k(KeyCode::KEY_C.0, 1));

        assert!(
            m.held.contains(&KeyCode::KEY_LEFTMETA),
                        "held must still contain Cmd after translation (the user is still holding it): {:?}",
            m.held
        );
    }

        /// Two consecutive combos should both hit normally
    #[test]
    fn consecutive_combos_both_work() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        for _ in 0..3 {
            let out = m.translate(&k(KeyCode::KEY_C.0, 1));
            assert!(out.contains(&InputEvent::new(EventType::KEY.0, KeyCode::KEY_LEFTCTRL.0, 1)));
            m.translate(&k(KeyCode::KEY_C.0, 0));
        }
    }

        /// Pressing several letters in a row while Shift is held should keep Shift down
    #[test]
    fn shift_held_across_multiple_keys() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        for c in [KeyCode::KEY_A, KeyCode::KEY_B, KeyCode::KEY_C] {
            let out = m.translate(&k(c.0, 1));
                        assert_eq!(desc2(&out), vec![(c.0, 1)], "{c:?} should be output as-is and not disturb Shift");
            m.translate(&k(c.0, 0));
        }
                assert!(m.held.contains(&KeyCode::KEY_LEFTSHIFT), "Shift is still held");
    }

    fn desc2(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }
}


#[cfg(test)]
mod release_symmetry {

    use super::*;
    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn codes(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }

        /// Core invariant: every key's press and release on the output side must pair strictly.
        /// A superfluous release event (no matching press) makes the app misjudge the key state.
    #[test]
    fn no_orphan_release_events() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut pressed: Vec<u16> = Vec::new();

        let seq = [
            (KeyCode::KEY_LEFTMETA, 1), (KeyCode::KEY_LEFTSHIFT, 1),
            (KeyCode::KEY_Z, 1), (KeyCode::KEY_Z, 0),
            (KeyCode::KEY_LEFTSHIFT, 0), (KeyCode::KEY_LEFTMETA, 0),
                        // Another round
            (KeyCode::KEY_LEFTMETA, 1), (KeyCode::KEY_C, 1),
            (KeyCode::KEY_C, 0), (KeyCode::KEY_LEFTMETA, 0),
        ];

        for (code, v) in seq {
                        // Note: press/release must be judged by the **output event's own value**,
                        // not the input event's value — when translating a combo the input is a "press",
                        // while the output may contain several "releases" (e.g. Cmd+Shift+Z must release Shift first).
            for e in m.translate(&k(code.0, v)) {
                let c = e.code();
                if e.value() == 1 {
                                        assert!(!pressed.contains(&c), "duplicate press {c}");
                    pressed.push(c);
                } else {
                    let p = pressed.iter().position(|x| *x == c);
                                        assert!(p.is_some(), "orphan release event, keycode {c}");
                    pressed.remove(p.unwrap());
                }
            }
        }
                assert!(pressed.is_empty(), "still unreleased at the end: {pressed:?}");
    }

        /// User-observed problem: Cmd+Shift+Z once emitted an extra pair of Shift releases
    #[test]
    fn cmd_shift_z_emits_no_extra_shift() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        m.translate(&k(KeyCode::KEY_Z.0, 1));
        m.translate(&k(KeyCode::KEY_Z.0, 0));
                // On physical Shift release the output side should have no more events
        let out = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0));
                assert!(out.is_empty(), "Shift was already released during translation; there should be no more output: {:?}", codes(&out));
    }

        /// `Ctrl+Cmd+Q` must be distinguished from `Cmd+Q` — a real defect this project fixed.
    ///
        /// Because remap turned physical Ctrl into CapsLock, the lookup could not match the `Cmd+Q` rule,
        /// so it fell into the default semantics and emitted `Ctrl+Q`. On the real machine it felt the same as `Cmd+Q` (both close the window),
        /// but the semantics were completely wrong: `Ctrl+Cmd+Q` on macOS is **lock screen**.
    ///
        /// Now the table has a lock-screen rule, and CapsLock is normalized to Ctrl before the lookup
        /// (physical Ctrl is CapsLock after remap, but semantically still Ctrl).
    #[test]
    fn ctrl_cmd_q_is_not_same_as_cmd_q() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_Q.0, 1));
        let codes = codes(&out);

        assert!(
            !codes.contains(&KeyCode::KEY_F4.0),
                        "Ctrl+Cmd+Q must not hit Cmd+Q's Alt+F4: {:?}",
            codes
        );
        assert!(
            !codes.contains(&KeyCode::KEY_Q.0),
                        "Ctrl+Cmd+Q must not degrade to a bare Ctrl+Q: {:?}",
            codes
        );
    }

        /// Handling of `Ctrl+Cmd+X`: convert when possible, pass through otherwise.
///
/// Background: the default semantics turn Cmd into Ctrl. If the user is already holding Ctrl, turning Cmd into Ctrl again
/// gives `Ctrl+Ctrl` — the same slot, a degenerate combo, so it must not convert. But it also must **not** be treated as
/// `Cmd+X`: `Ctrl+Cmd+X` on macOS is another combo with its own meaning.
/// Go by the table when it has a rule, pass through as-is when it does not.
#[cfg(test)]
mod ctrl_cmd_semantics {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    fn codes(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }

    fn press(m: &mut Mapper, seq: &[KeyCode]) -> Vec<u16> {
        let mut out = vec![];
        for c in seq {
            out.extend(m.translate(&k(c.0, 1)));
        }
        codes(&out)
    }

        /// `Ctrl+Cmd+Q` hits the table's lock-screen rule → emit the DE's current Super+L
    #[test]
    fn ctrl_cmd_q_locks_screen() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = press(
            &mut m,
            &[
                KeyCode::KEY_LEFTCTRL,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_Q,
            ],
        );
        assert!(
            out.contains(&KeyCode::KEY_LEFTMETA.0) && out.contains(&KeyCode::KEY_L.0),
                        "Ctrl+Cmd+Q should emit Super+L (reading the DE's screensaver binding), actual {:?}",
            out
        );
        assert!(
            !out.contains(&KeyCode::KEY_F4.0),
                        "must not fall back to Cmd+Q's Alt+F4: {:?}",
            out
        );
    }

        /// `Ctrl+Cmd+F` fullscreen → F11
    #[test]
    fn ctrl_cmd_f_fullscreen() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = press(
            &mut m,
            &[
                KeyCode::KEY_LEFTCTRL,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_F,
            ],
        );
        assert!(
            out.contains(&KeyCode::KEY_F11.0),
                        "Ctrl+Cmd+F should be F11 (fullscreen), actual {:?}",
            out
        );
        assert!(
            !out.contains(&KeyCode::KEY_F.0),
                        "must not fall back to find Ctrl+F: {:?}",
            out
        );
    }

        /// `Ctrl+Cmd+W` not in the table → pass through as-is; must not invent `Ctrl+W`
    #[test]
    fn unmapped_ctrl_cmd_passes_through() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = press(
            &mut m,
            &[
                KeyCode::KEY_LEFTCTRL,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_W,
            ],
        );
        assert!(
            out.contains(&KeyCode::KEY_LEFTMETA.0),
                        "when passing through, a real Meta must be emitted, otherwise the app receives a crippled combo: {:?}",
            out
        );
        assert!(
            out.contains(&KeyCode::KEY_W.0),
                        "when passing through, the main key must be emitted as-is: {:?}",
            out
        );
    }

        /// `Shift+Cmd+X` still goes through the default semantics → `Shift+Ctrl+X` (no conflict, convertible)
    #[test]
    fn shift_cmd_still_converts() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = press(
            &mut m,
            &[
                KeyCode::KEY_LEFTSHIFT,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_Z,
            ],
        );
        assert!(
            out.contains(&KeyCode::KEY_LEFTCTRL.0)
                && out.contains(&KeyCode::KEY_LEFTSHIFT.0)
                && out.contains(&KeyCode::KEY_Z.0),
                        "Shift+Cmd+Z should be Shift+Ctrl+Z, actual {:?}",
            out
        );
    }
}

/// Guard: after any key sequence completes, no modifier may remain pressed on the output side.
///
/// Breaking this invariant has severe consequences: once the pass-through path emitted Meta but did not
/// remove it from `suppressed`, the release was swallowed and Super stayed stuck down forever. On the real machine
/// the symptom was **after locking the screen, every key typed at the password prompt carried Super, digits like 9 could not be entered, and the only way out was a hard power-off**.
///
/// Rather than testing combo by combo, assert the invariant itself — any path that violates it is caught.
#[cfg(test)]
mod no_stuck_modifiers {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    const MODS: &[KeyCode] = &[
        KeyCode::KEY_LEFTMETA,
        KeyCode::KEY_RIGHTMETA,
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_LEFTALT,
        KeyCode::KEY_CAPSLOCK,
    ];

        /// After running a sequence of events, no modifier may remain down on the output side.
    fn assert_clean(seq: &[(KeyCode, i32)], label: &str) {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        for (c, v) in seq {
            out.extend(m.translate(&k(c.0, *v)));
        }
                // Check press/release balance key by key
        for mod_key in MODS {
            let downs = out
                .iter()
                .filter(|e| e.code() == mod_key.0 && e.value() == 1)
                .count();
            let ups = out
                .iter()
                .filter(|e| e.code() == mod_key.0 && e.value() == 0)
                .count();
            assert_eq!(
                downs,
                ups,
                                "{label}: {:?} events not balanced ({downs} presses / {ups} releases)",
                mod_key
            );
        }
    }

    #[test]
    fn passthrough_releases_meta() {
                // The pass-through path once missed the Meta release, leaving Super stuck
        assert_clean(
            &[
                (KeyCode::KEY_LEFTCTRL, 1),
                (KeyCode::KEY_LEFTMETA, 1),
                (KeyCode::KEY_W, 1),
                (KeyCode::KEY_W, 0),
                (KeyCode::KEY_LEFTMETA, 0),
                (KeyCode::KEY_LEFTCTRL, 0),
            ],
                        "pass through Ctrl+Cmd+W",
        );
    }

    #[test]
    fn converted_combo_releases_everything() {
        assert_clean(
            &[
                (KeyCode::KEY_LEFTMETA, 1),
                (KeyCode::KEY_Q, 1),
                (KeyCode::KEY_Q, 0),
                (KeyCode::KEY_LEFTMETA, 0),
            ],
            "Cmd+Q → Alt+F4",
        );
    }

    #[test]
    fn ctrl_cmd_q_lockscreen_releases_everything() {
        assert_clean(
            &[
                (KeyCode::KEY_LEFTCTRL, 1),
                (KeyCode::KEY_LEFTMETA, 1),
                (KeyCode::KEY_Q, 1),
                (KeyCode::KEY_Q, 0),
                (KeyCode::KEY_LEFTMETA, 0),
                (KeyCode::KEY_LEFTCTRL, 0),
            ],
                        "Ctrl+Cmd+Q lock screen",
        );
    }

    #[test]
    fn shift_cmd_conversion_releases_everything() {
        assert_clean(
            &[
                (KeyCode::KEY_LEFTSHIFT, 1),
                (KeyCode::KEY_LEFTMETA, 1),
                (KeyCode::KEY_Z, 1),
                (KeyCode::KEY_Z, 0),
                (KeyCode::KEY_LEFTMETA, 0),
                (KeyCode::KEY_LEFTSHIFT, 0),
            ],
            "Shift+Cmd+Z",
        );
    }
}

/// Systematic sweep: for any sequence of "press some modifiers + a main key, then release everything",
/// both the internal state and the output side must be clean afterwards.
///
/// The symptom is Meta stuck down — Space becomes Cmd+Space, W becomes Cmd+W.
/// Such bugs are easy to miss with hand-written per-combo tests, so we exhaust.
#[cfg(test)]
mod exhaustive_clean {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    const MODS: &[KeyCode] = &[
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_LEFTALT,
    ];

    const KEYS: &[KeyCode] = &[
        KeyCode::KEY_SPACE,
        KeyCode::KEY_W,
        KeyCode::KEY_Q,
        KeyCode::KEY_A,
        KeyCode::KEY_F,
        KeyCode::KEY_DELETE,
        KeyCode::KEY_LEFT,
    ];

        /// Meta is the culprit: it is suppressed on press and is the easiest to get stuck
    const META: KeyCode = KeyCode::KEY_LEFTMETA;

    fn check(seq_down: &[KeyCode], label: &str) {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        for c in seq_down {
            out.extend(m.translate(&k(c.0, 1)));
        }
                // Release in reverse order
        for c in seq_down.iter().rev() {
            out.extend(m.translate(&k(c.0, 0)));
        }

        assert!(
            m.held.is_empty(),
                        "{label}: held still non-empty after finish: {:?}",
            m.held
        );
        assert!(
            m.output_down.is_empty(),
                        "{label}: output_down still non-empty after finish: {:?}",
            m.output_down
        );
                // Every key's press/release on the output side must balance
        let mut seen: Vec<(u16, i32)> = Vec::new();
        for e in &out {
            seen.push((e.code(), e.value()));
        }
        for key in [META, KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_LEFTALT] {
            let d = seen.iter().filter(|(c, v)| *c == key.0 && *v == 1).count();
            let u = seen.iter().filter(|(c, v)| *c == key.0 && *v == 0).count();
                        assert_eq!(d, u, "{label}: {:?} output not balanced {d} down/{u} up", key);
        }
    }

    #[test]
    fn meta_plus_one_mod_plus_key_is_clean() {
        for a in MODS {
            for key in KEYS {
                check(&[META, *a, *key], "Meta+mod+key");
            }
        }
    }

    #[test]
    fn meta_plus_key_is_clean() {
        for key in KEYS {
            check(&[META, *key], "Meta+key");
        }
    }

    #[test]
    fn meta_alone_is_clean() {
                check(&[META], "Meta alone");
    }

    #[test]
    fn two_mods_plus_key_is_clean() {
        for a in MODS {
            for b in MODS {
                if a == b {
                    continue;
                }
                for key in KEYS {
                    check(&[*a, *b, *key], "mod+mod+key");
                }
            }
        }
    }
}

/// Release order does not matter.
///
/// The previous test fixed reverse order, but real people do not release in order — especially **releasing the
/// modifier first, then the main key** (the natural order fingers lift). This is exactly the soil for Meta getting stuck.
#[cfg(test)]
mod release_order {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

        /// Press everything, then release in the index order given by `order`
    fn check(down: &[KeyCode], order: &[usize], label: &str) {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        for c in down {
            out.extend(m.translate(&k(c.0, 1)));
        }
        for i in order {
            out.extend(m.translate(&k(down[*i].0, 0)));
        }
        assert!(
            m.held.is_empty(),
                        "{label}: held residual {:?}",
            m.held
        );
        assert!(
            m.output_down.is_empty(),
                        "{label}: output_down residual {:?}",
            m.output_down
        );
        for key in [
            KeyCode::KEY_LEFTMETA,
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_LEFTALT,
        ] {
            let d = out.iter().filter(|e| e.code() == key.0 && e.value() == 1).count();
            let u = out.iter().filter(|e| e.code() == key.0 && e.value() == 0).count();
                        assert_eq!(d, u, "{label}: {:?} not balanced {d} down/{u} up", key);
        }
    }

        /// Meta released first, main key after — the most common and most dangerous
    #[test]
    fn meta_released_before_key() {
        let keys = [
            KeyCode::KEY_SPACE,
            KeyCode::KEY_W,
            KeyCode::KEY_Q,
            KeyCode::KEY_A,
            KeyCode::KEY_LEFT,
        ];
        for key in keys {
                        // index 0=Meta, 1=main key; release 0 first
            check(
                &[KeyCode::KEY_LEFTMETA, key],
                &[0, 1],
                                "Meta released first",
            );
        }
    }

        /// Three keys, exhaustively all 6 release orders
    #[test]
    fn all_release_orders_for_meta_mod_key() {
        let perms: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for key in [KeyCode::KEY_SPACE, KeyCode::KEY_W, KeyCode::KEY_A] {
            for p in perms {
                check(
                    &[KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTCTRL, key],
                    &p,
                    "Meta+Ctrl+key",
                );
            }
        }
    }
}

/// "Release the modifier first, main key still down, then press a new key" — extremely common in real input.
///
/// Symptom: a pressed modifier remains on the output side, and every key afterwards carries it
/// (Space becomes Cmd+Space, W becomes Cmd+W).
#[cfg(test)]
mod mid_release_interleave {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    const META: KeyCode = KeyCode::KEY_LEFTMETA;

    #[test]
    fn meta_lifted_before_key_then_new_key() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];

        out.extend(m.translate(&k(META.0, 1)));
        out.extend(m.translate(&k(KeyCode::KEY_Q.0, 1)));
                // Release Meta first; main key Q is still held
        out.extend(m.translate(&k(META.0, 0)));
                // Now press Space
        out.extend(m.translate(&k(KeyCode::KEY_SPACE.0, 1)));

        let d: Vec<String> = out
            .iter()
            .map(|e| format!("{:?}{}", e.code(), if e.value() == 1 { "+" } else { "-" }))
            .collect();
        assert!(
            m.output_down.is_empty() || !m.output_down.contains(&META),
                        "Meta is still down on the output side: output_down={:?} events={:?}",
            m.output_down,
            d
        );
    }

        /// Same as above but through the "pass-through" path (no rule in the table and the Ctrl slot is taken)
    #[test]
    fn meta_lifted_after_passthrough_then_new_key() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        out.extend(m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1)));
        out.extend(m.translate(&k(META.0, 1)));
        out.extend(m.translate(&k(KeyCode::KEY_W.0, 1)));
        out.extend(m.translate(&k(META.0, 0)));
        out.extend(m.translate(&k(KeyCode::KEY_SPACE.0, 1)));

        assert!(
            !m.output_down.contains(&META),
                        "after pass-through, releasing Meta then pressing Space: Meta residual output_down={:?}",
            m.output_down
        );
    }
}

/// A pitfall specific to the lock-screen path: `Ctrl+Cmd+Q` outputs `Super+L`, **which contains Meta**.
///
/// Early versions could not detect the problem because `Cmd+Q` outputs `Alt+F4` (no Meta).
/// Only when **the translation result itself contains Meta** does the defect below surface:
///
/// 1. On press Meta is suppressed (`suppressed`) and never enters output_down
/// 2. After translation Meta is emitted actively (as part of `Super+L`) and recorded in injected_mods
/// 3. The user releases Cmd first → the release path takes the `suppressed.remove()` branch and **returns early**,
///    emitting no release — but Meta is **indeed** in output_down at that moment
/// 4. Afterwards every key carries this residual Super
///
/// Real-machine symptom: after locking the screen, Space becomes Cmd+Space (opens the overview), W becomes Cmd+W.
#[cfg(test)]
mod lock_screen_meta_leak {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    const META: KeyCode = KeyCode::KEY_LEFTMETA;

        /// Release Cmd first (Q still held), then press Space — Meta must not remain on the output side
    #[test]
    fn lifting_cmd_after_lock_leaves_no_stuck_super() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];

        out.extend(m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1)));
        out.extend(m.translate(&k(META.0, 1)));
        out.extend(m.translate(&k(KeyCode::KEY_Q.0, 1)));
                // The user releases Cmd first
        out.extend(m.translate(&k(META.0, 0)));
                // Q is still held; now press Space
        out.extend(m.translate(&k(KeyCode::KEY_SPACE.0, 1)));

        let d: Vec<String> = out
            .iter()
            .map(|e| format!("{:?}{}", e.code(), if e.value() == 1 { "+" } else { "-" }))
            .collect();

        assert!(
            !m.output_down.contains(&META),
                        "Super residual on the output side: output_down={:?}\nevent sequence: {:?}",
            m.output_down,
            d
        );
    }

        /// Type a whole stretch and confirm no modifier is carried into later keys
    #[test]
    fn typing_after_lock_is_clean() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
                // Lock the screen
        for c in [KeyCode::KEY_LEFTCTRL, META, KeyCode::KEY_Q] {
            out.extend(m.translate(&k(c.0, 1)));
        }
                // Release everything (various orders)
        for c in [META, KeyCode::KEY_Q, KeyCode::KEY_LEFTCTRL] {
            out.extend(m.translate(&k(c.0, 0)));
        }
                // Type the password
        for c in [KeyCode::KEY_A, KeyCode::KEY_1, KeyCode::KEY_B, KeyCode::KEY_9] {
            out.extend(m.translate(&k(c.0, 1)));
            out.extend(m.translate(&k(c.0, 0)));
        }
        assert!(
            m.output_down.is_empty(),
                        "after typing the password the output side should have no residual: {:?}",
            m.output_down
        );
                // Digit keys must not carry modifiers
        for e in &out {
            if e.code() == KeyCode::KEY_9.0 && e.value() == 1 {
                                // The immediately preceding press event should not have a modifier
            }
        }
    }
}

/// Cmd rules must **require Cmd to be held**.
///
/// Regression test: the lookup was once moved out of the `if cmd_held` branch (to support non-Cmd combos),
/// but the rule itself did not check whether Meta was held — so **a bare Space press also hit the
/// `Cmd+Space` rule**, and anyone pressing Space popped up the GNOME overview.
///
/// Field evidence (real input/output captured with --trace):
///   IN KEY_SPACE press -> OUT [SHIFT+, CTRL+, SPACE+]
#[cfg(test)]
mod cmd_required {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    fn codes(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }

        /// A bare Space must pass through as-is and must never become Ctrl+Shift+Space
    #[test]
    fn bare_space_is_not_overview() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = codes(&m.translate(&k(KeyCode::KEY_SPACE.0, 1)));
        assert_eq!(
            out,
            vec![KeyCode::KEY_SPACE.0],
                        "a bare Space must not trigger the overview rule"
        );
    }

        /// `Cmd+Space` must still hit the overview rule
    #[test]
    fn cmd_space_still_maps_to_overview() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = codes(&m.translate(&k(KeyCode::KEY_SPACE.0, 1)));
                // The overview's form varies with the GNOME version: older is a combo (Ctrl+Shift+Space),
                // as of GNOME 50 it is a single tap of a bare Super. Here we only assert "the overview was indeed hit";
                // the concrete form is covered by `cmd_space_maps_to_gnome_overview`.
        match de_feature_accel(&DesktopType::Gnome, "overview")
                        .expect("this machine's GNOME overview should be resolvable")
        {
            DeKeys::Combo(mods, key) => {
                for mm in &mods {
                                        assert!(out.contains(&mm.0), "overview modifier {mm:?} missing: {out:?}");
                }
                                assert!(out.contains(&key.0), "overview main key {key:?} missing: {out:?}");
            }
            DeKeys::Tap(key) => {
                assert!(
                    out.contains(&key.0),
                                        "Cmd+Space should tap {key:?}, actual {out:?}"
                );
            }
        }
    }

        /// A bare A must not become Ctrl+A (the default semantics only apply under a Cmd combo)
    #[test]
    fn bare_letter_is_not_translated() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = codes(&m.translate(&k(KeyCode::KEY_A.0, 1)));
                assert_eq!(out, vec![KeyCode::KEY_A.0], "a bare A should be output as-is");
    }

        /// Rules without Cmd must still take effect: `Option+Delete` → `Ctrl+Backspace`
    ///
        /// On this machine (MacBook keyboard) the "Delete" key is KEY_BACKSPACE in evdev,
        /// so both keycodes must be covered.
    #[test]
    fn non_cmd_rule_still_works() {
        for del_key in [KeyCode::KEY_BACKSPACE, KeyCode::KEY_DELETE] {
            let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
            m.translate(&k(KeyCode::KEY_LEFTALT.0, 1));
            let evs = m.translate(&k(del_key.0, 1));
            let c = codes(&evs);
            assert!(
                c.contains(&KeyCode::KEY_LEFTCTRL.0)
                    && c.contains(&KeyCode::KEY_BACKSPACE.0),
                                "Option+{del_key:?} should be Ctrl+Backspace, actual {:?}",
                c
            );
                        // Alt may only be **released** (output_down is drained on conversion); it must not be pressed again
            assert!(
                !evs
                    .iter()
                    .any(|e| e.code() == KeyCode::KEY_LEFTALT.0 && e.value() == 1),
                                "Alt must not be pressed again: {:?}",
                evs.iter()
                    .map(|e| format!("{:?}{}", e.code(), e.value()))
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// A bare Shift must still output press and release normally (fcitx5 depends on it)
/// All DE actions are resolvable.
///
/// Because the alias table lacked `print` (called the historical name `KEY_SYSRQ` in evdev),
/// the three screenshot rules were **silently dropped** — the startup log only said "DE does not provide it",
/// with no hint that key-name parsing had failed. Problems like this must be caught by tests.
#[cfg(test)]
mod de_accels_all_resolvable {
    use super::*;

    #[test]
    fn every_bound_gnome_feature_resolves() {
                // Only test actions GNOME binds by default.
                // `show-desktop` is unbound by default (@as []); the user must bind it
                // for it to take effect — that is by design, not a parse failure.
        for feature in [
            "overview",
            "lock-screen",
            "screenshot-ui",
            "screenshot",
            "screenshot-window",
            "logout",
        ] {
            assert!(
                de_feature_accel(&DesktopType::Gnome, feature).is_some(),
                                "GNOME action {feature} cannot be resolved — DE binding lookup failed or the key name is unknown"
            );
        }
    }

        /// Every `de_accel` rule in the table must have a resolvable feature —
        /// otherwise the row is silently dropped and the startup log only says "DE does not provide it", hiding the truth.
    #[test]
    fn no_rule_is_silently_dropped() {
        let cfg = Config::with_defaults();
                let compiled = compile_table(&cfg.rules).expect("the rule table should compile");
        for r in &compiled {
            if let CompiledAction::DeAccel(f) = &r.act {
                assert!(
                    de_feature_accel(&DesktopType::Gnome, f).is_some(),
                                        "rule {f} cannot be resolved; the row will be silently dropped"
                );
            }
        }
    }

        /// Print must be resolvable (GNOME's screenshot is bound to it)
    #[test]
    fn print_alias_resolves() {
        assert_eq!(
            crate::keys::parse_external_key("Print"),
            Some(evdev::KeyCode::KEY_SYSRQ),
                        "the `Print` in GNOME bindings must resolve to KEY_SYSRQ"
        );
    }
}

/// The `exec` action: produces a side effect but **emits no key at all**.
#[cfg(test)]
mod exec_action {
    use super::*;
    use crate::config::Config;

        // `exec_emits_no_key_events` and `exec_release_is_silent` were removed:
    //
        // the `exec` action's side effect is triggered in `lookup()` (see `crate::exec::run` there),
        // and `exec::run` for rules that "need confirmation" **pops up a zenity modal and waits for it to close**.
        // So these two tests popped up a "confirm empty trash" dialog on screen every time,
        // which had to be dismissed by hand — `cargo test` got stuck on it. Unit tests must not produce real side effects.
    //
        // Safety invariants such as "empty trash only takes effect inside Nautilus" are still covered by
        // `trash_confirmation_matches_macos` and `trash_scoped_to_nautilus::
        // trash_rules_target_nautilus_only` below — they only inspect the compiled rule table
        // and run no command.

        /// The confirmation policy of the two empty-trash rules must match macOS.
    ///
        /// macOS: `Shift+Cmd+Delete` has confirmation, `Option+Shift+Cmd+Delete`
        /// skips confirmation (Option means "skip confirmation"). This tool copies that.
    ///
        /// Skipping confirmation is only allowed when the rule is **app-scoped** — both must be scoped
        /// to Nautilus; a global no-confirm is unacceptable.
    #[test]
    fn trash_confirmation_matches_macos() {
        let cfg = Config::with_defaults();
                let rules = compile_table(&cfg.rules).expect("the rule table should compile");

        let with_option: Vec<_> = rules
            .iter()
            .filter(|r| matches!(&r.act, CompiledAction::Exec { program, .. } if program == "gio"))
            .filter(|r| r.key.1.contains(&KeyCode::KEY_LEFTALT))
            .collect();
        let without: Vec<_> = rules
            .iter()
            .filter(|r| matches!(&r.act, CompiledAction::Exec { program, .. } if program == "gio"))
            .filter(|r| !r.key.1.contains(&KeyCode::KEY_LEFTALT))
            .collect();

                assert_eq!(with_option.len(), 2, "there should be one Option entry for each of the two keycodes");
                assert_eq!(without.len(), 2, "there should be one non-Option entry for each of the two keycodes");

        for r in with_option {
            let confirm = match &r.act {
                CompiledAction::Exec { confirm, .. } => *confirm,
                                _ => panic!("should be an exec action"),
            };
            assert!(
                !confirm,
                                "Option+Shift+Cmd+Delete should skip confirmation (consistent with macOS)"
            );
            assert!(
                !matches!(r.app, AppMatch::Any),
                                "a no-confirm destructive rule must be app-scoped"
            );
        }
        for r in without {
            let confirm = match &r.act {
                CompiledAction::Exec { confirm, .. } => *confirm,
                                _ => panic!("should be an exec action"),
            };
                        assert!(confirm, "Shift+Cmd+Delete should have confirmation");
        }
    }
}

/// A bare Shift must still output press and release normally (fcitx5 depends on it)
/// Emptying the trash must be scoped to Nautilus.
///
/// On macOS these two are Finder menu items (the desktop is also Finder's), not system-level
/// shortcuts. Making them global has two real risks: the terminal is where a delete operation is most likely to
/// go wrong, and a mistyped key while entering a password would pop up the confirmation dialog directly.
#[cfg(test)]
mod trash_scoped_to_nautilus {
    use super::*;

    #[test]
    fn trash_rules_target_nautilus_only() {
        let cfg = Config::with_defaults();
        let rules = compile_table(&cfg.rules).expect("the rule table should compile");
        for r in &rules {
            if let CompiledAction::Exec { program, .. } = &r.act
                && program == "gio"
            {
                assert_eq!(
                    r.app,
                    AppMatch::Contains(vec!["org.gnome.Nautilus".to_string()]),
                    "emptying the trash must be scoped to Nautilus"
                );
            }
        }
    }

        // `trash_only_fires_in_nautilus` was removed: it likewise presses
        // `Cmd+Shift+⌫` inside Nautilus, which really runs `exec::run` via `lookup()` and pops up the zenity
        // confirmation dialog (see the note in the `exec_action` module above).
    //
        // The rule scope itself is asserted by `trash_rules_target_nautilus_only` above,
        // which runs no command.
}

/// The confirmation decision of `exec` itself.
///
/// Regression test: `needs_confirm` was once written as `confirm || risk == Destructive`,
/// which made `confirm: false` completely ineffective — the dialog still popped up, while the unit test only checked
/// the value of the `confirm` field in the rule table, never actually calling `run()`, so it was missed.
///
/// Here the decision logic itself is verified: no-confirm + app-scoped → no dialog;
/// no-confirm + global → execution refused.
#[cfg(test)]
mod exec_confirm_logic {
    use super::*;
    use crate::config::Config;

        /// Take the empty-trash rule from the rule table and build an ExecRequest
    fn trash_request(confirm: bool) -> crate::exec::ExecRequest<'static> {
        let cfg = Config::with_defaults();
                let rules = compile_table(&cfg.rules).expect("should compile");
        let r = rules
            .iter()
            .find(|r| {
                matches!(&r.act, CompiledAction::Exec { program, .. } if program == "gio")
                    && r.desc.as_deref().unwrap_or("").contains("Option")
            })
                        .expect("there should be an Option version of the empty-trash rule");

        let (program, args, c) = match &r.act {
            CompiledAction::Exec {
                program,
                args,
                confirm,
                ..
            } => (program.clone(), args.clone(), *confirm),
            _ => unreachable!(),
        };
        assert_eq!(
            c, confirm,
                        "the rule table's confirm should be {confirm} — test premise does not hold"
        );

        let args: &'static [String] = Box::leak(args.into_boxed_slice());
        crate::exec::ExecRequest {
            program: Box::leak(program.into_boxed_str()),
            args,
                        label: Some("Test"),
            confirm,
                        prompt: Some("Test"),
                        ok_label: Some("Test"),
            confirm_key: Some("Cmd+Shift+⌫"),
            scoped: !matches!(r.app, AppMatch::Any),
        }
    }

        /// `gio trash --empty` must be judged destructive
    #[test]
    fn gio_trash_empty_is_flagged_destructive() {
        let args = vec!["trash".to_string(), "--empty".to_string()];
        assert!(crate::exec::is_destructive("gio", &args));
    }

        /// no-confirm + app-scoped → allowed to run (no dialog, no refusal)
    #[test]
    fn scoped_no_confirm_is_allowed() {
        let req = trash_request(false);
        assert!(
            !crate::exec::should_confirm(&req),
                        "a no-confirm rule should not pop up a dialog — previously `confirm || risk==Destructive` made it always true"
        );
        assert!(
            !crate::exec::refuse(&req),
                        "an app-scoped no-confirm should not be refused"
        );
    }

        /// no-confirm + global → execution must be refused
    #[test]
    fn global_no_confirm_is_refused() {
        let mut req = trash_request(false);
        req.scoped = false;
        assert!(
            crate::exec::refuse(&req),
                        "a global destructive no-confirm command must be refused"
        );
    }
}

/// A bare Shift must still output press and release normally (fcitx5 depends on it)
#[test]
fn bare_shift_still_works() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let d = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        assert_eq!(codes(&d), vec![KeyCode::KEY_LEFTSHIFT.0]);
        let u = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0));
                assert_eq!(codes(&u), vec![KeyCode::KEY_LEFTSHIFT.0], "a bare Shift release must pass through");
    }
}

/// Regression tests for the Fn-key preprocessing.
///
/// Corresponds to a real bug that was fixed: the Fn branch once short-circuited with `return`, bypassing the whole Cmd
/// lookup flow. See the doc comment on `resolve_fn`.
#[cfg(test)]
mod fn_resolve {
    use super::*;
    use crate::config::FnMode;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    fn codes(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }

    fn mapper(mode: FnMode) -> Mapper {
        let mut cfg = Config::with_defaults();
        cfg.fn_mode = Some(mode);
        Mapper::new(&cfg, false).unwrap()
    }

        /// fkeysfirst: F1 alone → F1
    #[test]
    fn fkeysfirst_bare_f1_stays_f1() {
        let mut m = mapper(FnMode::Fkeysfirst);
        let d = m.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(codes(&d), vec![KeyCode::KEY_F1.0]);
    }

        /// fkeysfirst: Fn+F1 → brightness down
    #[test]
    fn fkeysfirst_fn_f1_becomes_media() {
        let mut m = mapper(FnMode::Fkeysfirst);
        m.translate(&k(KeyCode::KEY_FN.0, 1));
        let d = m.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(
            codes(&d),
            vec![KeyCode::KEY_BRIGHTNESSDOWN.0],
                        "Fn+F1 should be brightness down"
        );
    }

        /// fkeyslast is the opposite direction: F1 alone → brightness down, Fn+F1 → F1
    #[test]
    fn fkeyslast_inverts_both_directions() {
        let mut m = mapper(FnMode::Fkeyslast);
        let bare = m.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(codes(&bare), vec![KeyCode::KEY_BRIGHTNESSDOWN.0]);

        let mut m2 = mapper(FnMode::Fkeyslast);
        m2.translate(&k(KeyCode::KEY_FN.0, 1));
        let with_fn = m2.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(codes(&with_fn), vec![KeyCode::KEY_F1.0]);
    }

        /// Core regression: Fn+Meta+Shift+F1 must not leak Meta (invariant 3).
    ///
        /// Before the fix the output was `Meta+media key` — Meta was emitted as-is when it should have been
        /// suppressed, and Ctrl/Shift were lost. After the fix it should match a bare `Meta+Shift+F1` in shape, with only the main key differing.
    #[test]
    fn fn_with_other_mods_does_not_leak_meta() {
        let mut m = mapper(FnMode::Fkeysfirst);
        m.translate(&k(KeyCode::KEY_FN.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = m.translate(&k(KeyCode::KEY_F1.0, 1));
        let c = codes(&out);

                // If Meta leaks there would be an orphan event with a press and no matching release,
                // so its occurrence count should be even (press/release paired).
        let meta_count = c.iter().filter(|x| **x == KeyCode::KEY_LEFTMETA.0).count();
        assert_eq!(
            meta_count % 2,
            0,
                        "Meta occurrences should be even (press/release paired): {:?}",
            c
        );

                // And it must really reach the Cmd lookup: inject Ctrl, not just emit a bare media key
        assert!(
            c.contains(&KeyCode::KEY_LEFTCTRL.0),
                        "should continue through the Cmd lookup and inject Ctrl: {:?}",
            c
        );
    }

        /// Release order does not matter: releasing Fn first or F1 first should release the same media key.
    ///
        /// Relies on `pressed` being keyed by the **physical key** — if it were keyed by the resolved key,
        /// releasing Fn first would make the release path derive the wrong key.
    #[test]
    fn release_is_symmetric_regardless_of_fn_order() {
        for fn_first in [true, false] {
            let mut m = mapper(FnMode::Fkeysfirst);
            let mut out = vec![];
            out.extend(m.translate(&k(KeyCode::KEY_FN.0, 1)));
            out.extend(m.translate(&k(KeyCode::KEY_F1.0, 1)));
            if fn_first {
                out.extend(m.translate(&k(KeyCode::KEY_FN.0, 0)));
                out.extend(m.translate(&k(KeyCode::KEY_F1.0, 0)));
            } else {
                out.extend(m.translate(&k(KeyCode::KEY_F1.0, 0)));
                out.extend(m.translate(&k(KeyCode::KEY_FN.0, 0)));
            }
            let last = out.last().unwrap().code();
            assert_eq!(
                last,
                KeyCode::KEY_BRIGHTNESSDOWN.0,
                                "when fn_first={} the last event must be the media-key release",
                fn_first
            );
        }
    }

        /// `Disabled` mode does no conversion at all
    #[test]
    fn disabled_mode_passes_through() {
        let mut m = mapper(FnMode::Disabled);
        m.translate(&k(KeyCode::KEY_FN.0, 1));
        let d = m.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(codes(&d), vec![KeyCode::KEY_F1.0]);
    }
}
