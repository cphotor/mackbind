//! Config parsing.
//!
//! # Design points
//!
//! **Built-in default mappings, ready out of the box.** After installing
//! mackbind, the user gets the macOS shortcut habits with no configuration at
//! all. The user config is used to **override** or **append**, not to enable
//! features.
//!
//! **The desktop environment is a configurable item and must not be
//! hard-coded.** On Linux there are many DEs—GNOME, KDE Plasma, Sway,
//! Hyprland, and others—and the same macOS feature differs in how it is
//! implemented, whether it is available, and its default shortcut under each
//! DE. DE-related behavior is specified via `[desktop] type`; when it cannot be
//! looked up the related rule is skipped, but **must not be treated as
//! nonexistent**—it should be kept in the docs to be enabled when the
//! corresponding DE is supported.
//!
//! # Default semantics of Cmd
//!
//! `Cmd+X` maps to `Ctrl+X` by default (rather than keeping Super). Based on
//! measurement: of GNOME's 28 Super bindings, only `Super+h` involves a letter,
//! so `Super+letter` is essentially free—whereas the vast majority of macOS's
//! `Cmd+letter` correspond exactly to Ctrl. Keeping Super would make most
//! combos do nothing at all.
//!
//! Super-related rules (`Cmd+Tab`/`Cmd+H`, etc.) are deferred; see
//! `docs/macos-shortcuts.md` for the reasoning.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// A single modifier remap: change key `from` into key `to`.
#[derive(Debug, Clone, Deserialize)]
pub struct Remap {
    /// evdev keycode name, e.g. "KEY_CAPSLOCK"
    pub from: String,
    /// evdev keycode name, e.g. "KEY_LEFTCTRL"
    pub to: String,
    /// Optional description, for human reading only
    #[serde(default)]
    pub desc: Option<String>,
}

/// The action of a mapping rule.
///
/// **An action is a function**: the first two emit a fixed combo, and the last
/// two are **evaluated at runtime**—querying the DE's existing binding, or
/// querying the terminal's own declared shortcut. This way the table is
/// responsible for naming and evaluation is left to functions, so dynamically
/// discovered results need not be frozen into a snapshot that can go stale.
///
/// See `docs/table-driven-design.md`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Emit a fixed combo, e.g. `Cmd+Q` → `Alt+F4`
    Emit {
        /// Output-side modifiers
        #[serde(default)]
        mods: Vec<String>,
        /// Output main key
        key: String,
    },
    /// Read the desktop environment's existing binding for this feature and
    /// emit the same keys.
    ///
    /// The benefit is that no record is left in the user config and the user's
    /// own settings are not overridden; if the user changes the binding, this
    /// follows automatically. When it cannot be looked up, **skip** (not
    /// treated as an error).
    DeAccel {
        /// Feature identifier, e.g. `overview`. For the lookup table see
        /// `de_action_lookup`.
        feature: String,
    },
    /// Add Shift only when the target app **itself** binds this key with
    /// `Ctrl+Shift`.
    ///
    /// The purpose of adding Shift is to hit a binding the app already has, not
    /// to dodge conflicts in general. This key set is obtained at runtime from
    /// GSettings (see `set_window_id`), so it cannot be written here as a fixed
    /// output—this is the table's only "conditional" action.
    TerminalCtrlShift,
    /// Run an external command.
    ///
    /// Used for operations that are "essentially a command rather than a
    /// combo", such as empty trash. Destructive commands go through the
    /// confirmation flow in [`crate::exec`].
    Exec {
        /// Executable name, e.g. `gio`
        program: String,
        /// Arguments
        #[serde(default)]
        args: Vec<String>,
        /// Whether to ask the user for confirmation first; defaults to `true`.
        ///
        /// Destructive commands **force** confirmation; writing `false` does
        /// not work—a confirmation-free global hotkey means an accidental press
        /// is a permanent delete, with no trash to recover from.
        #[serde(default = "default_true")]
        confirm: bool,
        /// Confirmation body text. If omitted, an auto-generated description is
        /// used.
        #[serde(default)]
        prompt: Option<String>,
        /// Confirmation button text. If omitted, "Run" is used.
        #[serde(default)]
        ok_label: Option<String>,
    },
}

/// A row of the table: `(app, key combo) → action`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Applies to which apps. Omitted or `["*"]` means all apps.
    ///
    /// `"TerminalEmulator"` is a special value—decided by the `.desktop`
    /// `Categories` (freedesktop spec), so installing a new terminal takes
    /// effect automatically. The rest match by substring on wm_class.
    #[serde(default)]
    pub apps: Vec<String>,
    /// Input-side combo, **excluding Cmd** (see the `cmd` field).
    /// e.g. `["KEY_Q"]`, `["S-KEY_LEFT"]`.
    pub keys: Vec<String>,
    /// Whether this rule **requires holding Cmd**. Defaults to `true`.
    ///
    /// The vast majority of rows in the table are "Cmd+some key", so Cmd is
    /// required by default. The few combos without Cmd (such as macOS's
    /// `Option+Delete`) must explicitly write `cmd = false`.
    ///
    /// **This distinction is essential**: the table lookup applies to all
    /// combos (otherwise non-Cmd combos could not be configured), and if a rule
    /// no longer checks whether Cmd is held, then **a bare space press would
    /// also match the `Cmd+space` rule**, directly triggering the GNOME
    /// overview.
    #[serde(default = "default_true")]
    pub cmd: bool,
    /// Action
    #[serde(rename = "action")]
    pub act: Action,
    /// Optional description, for human reading only
    #[serde(default)]
    pub desc: Option<String>,
}

/// Desktop environment type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopType {
    /// GNOME / Mutter family (including ptyxis and GNOME apps)
    #[default]
    Gnome,
    /// KDE Plasma
    Kde,
    /// WMs such as Sway, Hyprland, i3, or other environments that cannot be
    /// looked up
    None,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopConfig {
    /// Desktop environment type; determines the lookup source for DE-related
    /// rules.
    ///
    /// `None` means "not specified in this config file"; the effective value
    /// defaults to [`DesktopType::Gnome`]. Keeping the distinction matters when
    /// merging several files: a higher-priority file that only sets `spotlight`
    /// must not reset the `type` chosen by a lower-priority file.
    pub r#type: Option<DesktopType>,
    /// Whether to map macOS's `Cmd+space` (Spotlight) to the desktop
    /// environment's overview.
    ///
    /// Enabled by default. The implementation **reads** the DE's existing
    /// binding and emits the same keys rather than rewriting the binding—so it
    /// leaves no record in the user config and does not affect the user's own
    /// setting for that key.
    ///
    /// When the corresponding action cannot be found (e.g. the DE provides no
    /// overview), it is skipped automatically and not treated as an error.
    pub spotlight: Option<bool>,
}

impl DesktopConfig {
    /// Effective desktop type (`gnome` unless overridden).
    pub fn de_type(&self) -> DesktopType {
        self.r#type.unwrap_or_default()
    }

    /// Effective overview switch (`true` unless overridden).
    pub fn spotlight(&self) -> bool {
        self.spotlight.unwrap_or(true)
    }
}

/// How `Shift+Enter` is treated inside VTE-style terminals.
///
/// Those terminals cannot distinguish `Shift+Enter` from `Enter`; the default
/// `On` emits `Ctrl+J` (0x0A), which every terminal transmits as a newline.
/// `Off` leaves the terminal's native behavior untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShiftEnterMode {
    /// Emit `Ctrl+J` in terminals that cannot report `Shift+Enter`.
    ///
    /// `ctrl_j` is accepted as a backwards-compatible alias.
    #[default]
    #[serde(rename = "on", alias = "ctrl_j")]
    On,
    /// Do not add the rule; keep the terminal's native `Shift+Enter`.
    Off,
}

/// Terminal-specific behavior.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    /// Whether `Shift+Enter` becomes `Ctrl+J` in terminals that cannot report
    /// the modifier. Defaults to [`ShiftEnterMode::On`].
    ///
    /// The set of terminals is fixed by the built-in VTE behavior list; users
    /// only choose on or off.
    pub shift_enter: Option<ShiftEnterMode>,
}

impl TerminalConfig {
    /// Effective `Shift+Enter` mode.
    pub fn shift_enter_mode(&self) -> ShiftEnterMode {
        self.shift_enter.unwrap_or_default()
    }
}

/// The true-value constant used by `#[serde(default)]`—most boolean config
/// items are enabled by default.
///
/// A `true` literal cannot be used: `Deserialize`'s `default` requires a
/// callable.
fn default_true() -> bool {
    true
}

/// Fn key priority mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FnMode {
    /// F keys take priority; holding Fn produces brightness/media keys (same as
    /// kernel fnmode=2)
    #[default]
    Fkeysfirst,
    /// Media keys take priority; holding Fn produces F keys (same as kernel
    /// fnmode=1)
    Fkeyslast,
    /// No conversion
    Disabled,
}

/// Complete configuration. All fields are optional—omitting one uses the
/// built-in default.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Whether the built-in CapsLock ↔ Left Ctrl swap is installed.
    ///
    /// `None` means unspecified. Defaults to `true`; set `false` to leave both
    /// keys untouched. `remap` is still applied afterwards, and can override
    /// either direction.
    pub swap_capslock_ctrl: Option<bool>,
    /// Modifier swap/replace. Entries are appended after the built-in swap, so
    /// a user entry with the same `from` key overrides that direction.
    ///
    /// **Not part of the rule table**: it acts on the key itself rather than a
    /// combo, and happens before combo matching.
    pub remap: Vec<Remap>,
    /// Mapping rule table. **The single source of truth for mappings**—a lookup
    /// just walks this list.
    ///
    /// Lookup order is declaration order, and the first match wins. User rules
    /// are **prepended** before the built-in defaults, so they take priority.
    pub rules: Vec<Rule>,
    /// Fn key priority. `None` means unspecified, keeping the default value.
    #[serde(default)]
    pub fn_mode: Option<FnMode>,
    /// Desktop-environment-related config
    pub desktop: DesktopConfig,
    /// Extra apps where "Ctrl is already taken", and Cmd maps to Ctrl+Shift
    /// within them.
    ///
    /// Empty by default. **Terminals need not be listed**—they are decided by
    /// the `.desktop` entry's `Categories=TerminalEmulator` (see
    /// `src/desktop.rs`), automatically covering newly installed terminals per
    /// the freedesktop spec.
    ///
    /// This item is for apps outside terminals where Ctrl has a non-standard
    /// meaning, such as gvim in a standalone window.
    ///
    /// It works together with the `TerminalCtrlShift` action in the rule table:
    /// the action can only take effect for apps that match this list.
    pub ctrl_busy_apps: Vec<String>,
    /// Terminal-specific behavior (`Shift+Enter` handling and its app list).
    pub terminal: TerminalConfig,
}

/// The **fallback** key set for terminal specialization.
///
/// At runtime the actual bindings are preferably looked up from the terminal's
/// own GSettings (`set_window_id`), and this list is used only when that
/// fails—it applies to terminals that do not go through GSettings (kitty,
/// wezterm).
///
/// This list is **necessarily incomplete**: terminals that go through GSettings
/// each declare different bindings—for example ptyxis has
/// `new-window = Ctrl+Shift+N`, and `N` is not in this list. So the table has a
/// separate `keys = ["*"]` wildcard row to catch keys discovered at runtime.
pub const DEFAULT_TERMINAL_SHIFT_KEYS: &[&str] = &[
    "KEY_C",          // copy
    "KEY_V",          // paste
    "KEY_X",          // cut
    "KEY_T",          // new tab
    "KEY_W",          // close tab
    "KEY_D",          // horizontal split (some terminals)
    "KEY_F",          // find (some terminals)
    "KEY_S",          // save (some terminals)
    "KEY_EQUAL",      // increase font size
    "KEY_MINUS",      // decrease font size
    "KEY_LEFTBRACE",  // previous tab
    "KEY_RIGHTBRACE", // next tab
];

/// Terminals that **cannot tell `Shift+Enter` from `Enter`**.
///
/// These are the VTE family. VTE implements neither the kitty keyboard protocol
/// nor xterm's `modifyOtherKeys`, so a modified Return has nowhere to be
/// encoded: `_vte_keymap_GDK_Return` holds only an Alt variant, and
/// `_vte_keymap_key_get_modifier_encoding_method` lists the arrows, Insert,
/// Delete, Home, End, Page Up/Down, F1-F35 and the keypad operators (including
/// the keypad's own `KP_Enter`) but **not Return**. `Shift+Enter` and
/// `Ctrl+Enter` therefore reach the application as the very same `\r` (0x0D) as
/// bare `Enter` -- which is exactly how a terminal coding agent loses
/// "Shift+Enter inserts a newline" and submits the prompt instead.
///
/// `Ctrl+J` is the escape hatch: 0x0A is a **different** control character that
/// every terminal transmits unchanged, and readline, vim and the agent TUIs all
/// read it as a newline.
///
/// The criterion is that **behavior**, not an app list -- the same rule as
/// `SIGINT_LEAKY_TERMINALS` below: only VTE (or another terminal family that
/// really drops the modifier) belongs here, and an entry is removed once
/// upstream implements the protocol. Terminals that *do* implement it (kitty,
/// foot, wezterm, ghostty, alacritty) must never be listed: there `Shift+Enter`
/// already works, and this rule would take that away.
///
/// The entries are the Wayland `wm_class` (app_id) values reported by the
/// focused-window extension. They cover the mainstream VTE terminals:
/// GNOME Terminal, GNOME Console, Ptyxis, Tilix, Terminator, Guake, Xfce
/// Terminal, MATE Terminal, LXTerminal, Sakura, Tilda and BlackBox. VTE is the
/// shared behavior that makes them blind, so a new VTE terminal only needs its
/// app_id added here (never kitty/foot/wezterm/ghostty/alacritty).
pub(crate) const SHIFT_ENTER_BLIND_TERMINALS: &[&str] = &[
    "org.gnome.Ptyxis",
    "org.gnome.Terminal",
    "org.gnome.Console",
    "com.gexperts.Tilix",
    "terminator",
    "guake",
    "xfce4-terminal",
    "mate-terminal",
    "lxterminal",
    "sakura",
    "tilda",
    "com.raggesilver.BlackBox",
];

/// Terminals that leak `Ctrl+Shift+C` as SIGINT.
///
/// ptyxis and konsole, when there is **no selected text**, pass this combo
/// through to the child process as SIGINT—so "copy" kills the running task.
/// wezterm, ghostty, and gnome-terminal (the VTE family) swallow the combo
/// unconditionally and do not have this problem.
///
/// Use `Ctrl+Insert` instead (the traditional X11 copy key): all terminals
/// support it, and it is not sent to the process as a control character. The
/// basis for the decision is the **behavioral difference**, not an app list—so
/// the `apps` here are just "which terminals have this behavior"; when swapping
/// terminals, add or remove by behavior.
const SIGINT_LEAKY_TERMINALS: &[&str] = &["org.gnome.Ptyxis", "org.gnome.konsole", "konsole"];

/// The built-in default rule table.
///
/// **This is the single source of truth for mappings**—user rules are
/// prepended before this table (first match wins).
/// For the full design see `docs/table-driven-design.md`.
fn default_rules(terminal: &TerminalConfig) -> Vec<Rule> {
    let mut v: Vec<Rule> = Vec::new();

    let mut push_emit = |apps: &[&str], keys: &str, mods: &str, key: &str, desc: &str| {
        v.push(Rule {
            apps: apps.iter().map(|s| s.to_string()).collect(),
            keys: vec![keys.to_string()],
            cmd: true,
            act: Action::Emit {
                mods: if mods.is_empty() {
                    vec![]
                } else {
                    vec![mods.to_string()]
                },
                key: key.to_string(),
            },
            desc: Some(desc.to_string()),
        });
    };

    // ── Cursor movement (macOS uses Cmd/Option, Linux uses Ctrl/Alt) ──
    push_emit(&[], "KEY_LEFT", "C", "Left", "start of line (Cmd+←)");
    push_emit(&[], "KEY_RIGHT", "C", "Right", "end of line (Cmd+→)");
    push_emit(&[], "KEY_UP", "C", "Up", "start of document (Cmd+↑)");
    push_emit(&[], "KEY_DOWN", "C", "Down", "end of document (Cmd+↓)");
    push_emit(&[], "S-KEY_LEFT", "C-S", "Left", "select to start of line");
    push_emit(&[], "S-KEY_RIGHT", "C-S", "Right", "select to end of line");
    push_emit(&[], "S-KEY_UP", "C-S", "Up", "select to start of document");
    push_emit(&[], "S-KEY_DOWN", "C-S", "Down", "select to end of document");
    push_emit(&[], "A-KEY_LEFT", "A", "Left", "previous word (Option+←)");
    push_emit(&[], "A-KEY_RIGHT", "A", "Right", "next word (Option+→)");
    // ── Finder navigation (Cmd+[ / Cmd+]) ──
    push_emit(&[], "KEY_LEFTBRACE", "A", "Left", "previous folder (Cmd+[)");
    push_emit(&[], "KEY_RIGHTBRACE", "A", "Right", "next folder (Cmd+])");
    // ── Delete ──
    // ── Window management ──
    // A cross-app global rule, so it is not affected by terminal
    // specialization.
    // macOS's Cmd+Q is "quit app"; the closest generic binding on Linux is
    // Alt+F4 (GNOME's wm.keybindings close). Toshy uses the same one.
    push_emit(&[], "KEY_Q", "A", "KEY_F4", "quit app (Cmd+Q → Alt+F4)");

    // ── Desktop environment features: read the DE's existing binding and emit
    // the same keys ──
    // Placed last: it matches by main key (space) and should not be stolen by
    // the rows above.
    v.push(Rule {
        apps: vec![],
        keys: vec!["KEY_SPACE".into()],
        cmd: true,
        act: Action::DeAccel {
            feature: "overview".into(),
        },
        desc: Some("Cmd+space → overview (read DE's existing binding)".into()),
    });

    // ── Ctrl+Cmd combos: they have a distinct meaning on macOS and cannot be
    // derived from the default semantics ──
    //
    // The premise is "Cmd maps to Ctrl". When the user is already holding Ctrl,
    // turning Cmd into Ctrl yields Ctrl+Ctrl—the same slot, a degenerate
    // combo—so such combos **cannot** be converted. Nor can they be treated as
    // Cmd+X (see the note on `default_semantics`). If the table has an entry,
    // follow the table; otherwise pass through unchanged.
    //
    // Placed after the global rules and before the `*` wildcard: priority above
    // terminal specialization.
    //
    // Ctrl+Cmd+Q lock screen: read GNOME's existing screensaver binding (on
    // this machine Super+l), the same approach as Cmd+space → overview—without
    // rewriting the user config.
    v.push(Rule {
        apps: vec![],
        keys: vec!["C-KEY_Q".into()],
        cmd: true,
        act: Action::DeAccel {
            feature: "lock-screen".into(),
        },
        desc: Some("lock screen (Ctrl+Cmd+Q, read DE's existing binding)".into()),
    });

    // Ctrl+Cmd+F fullscreen: the standard app-level fullscreen key.
    // GNOME's toggle-fullscreen action exists but is unbound by default
    // (@as []), so emit F11 directly, with no need to rewrite the user
    // settings.
    v.push(Rule {
        apps: vec![],
        keys: vec!["C-KEY_F".into()],
        cmd: true,
        act: Action::Emit {
            mods: vec![],
            key: "KEY_F11".into(),
        },
        desc: Some("fullscreen (Ctrl+Cmd+F → F11)".into()),
    });

    // ── Screenshot: corresponds to macOS's Cmd+Shift+3/4/5 ──
    //
    // Read GNOME's existing binding and emit the same keys, without rewriting
    // the user config.
    //
    // **One counter-intuitive point** (measured): GNOME's
    //   screenshot        (Shift+Print) = **captures full screen directly**, no UI
    //   show-screenshot-ui(Print)       = **pops up the screenshot UI**
    // whereas macOS is Cmd+Shift+3 for a direct full-screen capture and
    // Cmd+Shift+4/5 for selection mode. So align by function: 3 to "direct
    // capture", 4/5 to "UI":
    //   Cmd+Shift+3 → screenshot          direct full-screen capture
    //   Cmd+Shift+4 → show-screenshot-ui  screenshot UI (pick region/window in it)
    //   Cmd+Shift+5 → show-screenshot-ui  same as above
    //
    // 4 and 5 behaving the same is a compromise: GNOME has only three
    // screenshot actions, of which `screenshot` and `screenshot-window` were
    // both measured to capture the full screen directly, so region and window
    // cannot be distinguished from them. Region/window can still be selected in
    // the UI.
    for (keys, feature, desc) in [
        ("S-KEY_3", "screenshot", "full-screen capture (Cmd+Shift+3, capture directly without UI)"),
        (
            "S-KEY_4",
            "screenshot-ui",
            "screenshot UI (Cmd+Shift+4, pick region/window in it)",
        ),
        (
            "S-KEY_5",
            "screenshot-ui",
            "screenshot UI (Cmd+Shift+5, same as above)",
        ),
    ] {
        v.push(Rule {
            apps: vec![],
            keys: vec![keys.into()],
            cmd: true,
            act: Action::DeAccel {
                feature: feature.into(),
            },
            desc: Some(desc.into()),
        });
    }

    // ── Ctrl+↑ Mission Control → GNOME overview ──
    //
    // The same action as `Cmd+space`, just triggered by a different key.
    v.push(Rule {
        apps: vec![],
        keys: vec!["C-KEY_UP".into()],
        // Ctrl+↑ does not include Cmd—marking it true would make the rule never match
        cmd: false,
        act: Action::DeAccel {
            feature: "overview".into(),
        },
        desc: Some("Mission Control (Ctrl+↑ → overview)".into()),
    });

    // ── Shift+Cmd+Q log out ──
    //
    // GNOME's logout is bound to media-keys' `logout` (on this machine
    // Ctrl+Alt+Delete), the same schema as lock screen. Also read the existing
    // binding and emit it, without rewriting the user settings.
    v.push(Rule {
        apps: vec![],
        keys: vec!["S-KEY_Q".into()],
        cmd: true,
        act: Action::DeAccel {
            feature: "logout".into(),
        },
        desc: Some("log out (Shift+Cmd+Q, read DE's existing binding)".into()),
    });

    // ── Delete family: macOS's "Delete" key = other keyboards' Backspace(⌫) ──
    //
    // macOS calls the backspace key Delete and calls the forward-delete key
    // (which does not exist on MacBooks) Forward Delete. The everyday phrase
    // "Cmd+Delete moves to trash" refers to ⌫.
    //
    // In evdev ⌫ is `KEY_BACKSPACE` and forward delete is `KEY_DELETE`—they are
    // different keys. On this machine (MacBook keyboard) only the former is
    // produced; an external keyboard is needed for the latter. So every combo
    // is written as two rules, otherwise it would fail entirely on this machine
    // (previously only KEY_DELETE was written, making all delete-family rules
    // useless).

/// The two delete keys that need to be covered together.
const DELETE_KEYS: &[&str] = &["KEY_BACKSPACE", "KEY_DELETE"];

for key in DELETE_KEYS {
    // Cmd+Delete → move to trash
    //
    // Emit a **bare Delete** (without Ctrl): GNOME's file manager binds "move
    // to trash" to the Delete key; Ctrl+Delete is "delete next word" in GTK
    // text widgets, and is not trash in the file manager either. Adding Ctrl
    // before made it not work at all.
    //
    // The virtual keyboard can emit keycodes absent on this machine (MacBooks
    // have no ⌦ key), so this is feasible.
    v.push(Rule {
        apps: vec![],
        keys: vec![(*key).to_string()],
        cmd: true,
        act: Action::Emit {
            mods: vec![],
            key: "KEY_DELETE".into(),
        },
        desc: Some(format!(
            "move to trash (Cmd+Delete → Delete, without Ctrl; key={key})"
        )),
    });

    // Option+Delete → delete the word to the left
    //
    // Use Ctrl+Backspace rather than passing through Alt+Backspace: the former
    // is the X11/GNOME convention and works consistently in editors, terminals,
    // and address bars.
    v.push(Rule {
        apps: vec![],
        keys: vec![format!("A-{key}")],
        cmd: false,
        act: Action::Emit {
            mods: vec!["C".into()],
            key: "KEY_BACKSPACE".into(),
        },
        desc: Some(format!(
            "delete word to the left (Option+Delete → Ctrl+Backspace, key={key})"
        )),
    });

    // Shift+Cmd+Delete / Option+Shift+Cmd+Delete → empty trash
    //
    // **Scoped to Nautilus**: on macOS these two are Finder menu items (the
    // desktop is also Finder's domain) and not system-level shortcuts—pressing
    // them in a terminal, browser, or text box is meaningless.
    //
    // A global binding has two real risks: terminals are precisely where delete
    // operations go wrong most easily, and an accidental press while typing a
    // password would pop up the confirmation dialog directly. Once scoped, an
    // accidental press there does nothing at all.
    // The two keys have different meanings on macOS, and this tool copies that:
    //   Shift+Cmd+Delete        empty trash, **with confirmation**
    //   Option+Shift+Cmd+Delete empty trash, **no confirmation** (Option skips it)
    //
    // The reason no-confirmation is dared here is that the rule is already
    // scoped to Nautilus—a global binding must never skip confirmation (an
    // accidental press means permanent deletion), but once scoped the risk is
    // manageable.
    for (mods, desc, confirm) in [
        (vec!["S".to_string()], "empty trash (Shift+Cmd+Delete)", true),
        (
            vec!["A".to_string(), "S".to_string()],
            "empty trash (Option+Shift+Cmd+Delete, no confirmation)",
            false,
        ),
    ] {
        v.push(Rule {
            apps: vec!["org.gnome.Nautilus".into()],
            keys: vec![format!("{}-{}", mods.join("-"), key)],
            cmd: true,
            act: Action::Exec {
                program: "gio".into(),
                args: vec!["trash".into(), "--empty".into(), "--force".into()],
                confirm,
                prompt: Some("Are you sure you want to permanently delete the items in the trash?".into()),
                ok_label: Some("Empty Trash".into()),
            },
            desc: Some(format!("{desc}, key={key}")),
        });
    }
}

    // ── Terminals that leak SIGINT: copy key goes through Ctrl+Insert ──
    // Must be placed **before** terminal specialization—the lookup takes the
    // first match, and if it were after, the generic terminal rule would match
    // first and this one would never take effect.
    v.push(Rule {
        apps: SIGINT_LEAKY_TERMINALS.iter().map(|s| s.to_string()).collect(),
        keys: vec!["KEY_C".into()],
        cmd: true,
        act: Action::Emit {
            mods: vec!["C".into()],
            key: "KEY_INSERT".into(),
        },
        desc: Some(
            "use Ctrl+Insert for copy: with no selected text ptyxis/konsole \
             leak Ctrl+Shift+C to the process as SIGINT"
                .into(),
        ),
    });

    // ── Terminals that cannot report Shift+Enter: send Ctrl+J instead ──
    //
    // A table row is all this needs -- no mapper change. The combo-convert path
    // releases everything currently down on the output side (which includes the
    // Shift that was passed through on press -- invariant 4 requires that, for
    // fcitx5) and then presses exactly this rule's modifiers, so what reaches
    // the terminal is a clean `Ctrl+J` and never `Ctrl+Shift+J`. The
    // `Option+Delete` row above relies on the same path.
    //
    // A `cmd = false` row is **not** skipped while Cmd is held (the lookup only
    // gates on `requires_cmd && !cmd_held`), so `Cmd+Shift+Enter` lands here
    // too. That matches the existing `Option+Delete` row.
    // The row is installed only when the user has not opted out. `off` removes
    // it entirely. The terminal list is the built-in VTE behavior list; users
    // choose only on/off because naming terminal app ids is not a reasonable
    // configuration burden.
    if terminal.shift_enter_mode() != ShiftEnterMode::Off {
        v.push(Rule {
            apps: SHIFT_ENTER_BLIND_TERMINALS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            keys: vec!["S-KEY_ENTER".into()],
            cmd: false,
            act: Action::Emit {
                mods: vec!["C".into()],
                key: "KEY_J".into(),
            },
            desc: Some(
                "soft newline (Shift+Enter → Ctrl+J): this terminal cannot report \
                 Shift+Enter separately from Enter"
                    .into(),
            ),
        });
    }

    // ── GVIM: Ctrl is already taken (Ctrl+V block select, Ctrl+C quit) ──
    // vim running in a terminal has a window belonging to the terminal, already
    // covered by TerminalEmulator; this targets gvim in a standalone window.
    // Likewise it must come before the generic terminal rule.
    for key in DEFAULT_TERMINAL_SHIFT_KEYS {
        v.push(Rule {
            apps: vec!["gvim".into()],
            keys: vec![(*key).to_string()],
            cmd: true,
            act: Action::TerminalCtrlShift,
            desc: Some("GVIM specialization: Ctrl is already taken".into()),
        });
    }

    // ── Terminal specialization ──
    // The match condition is "the terminal itself bound this key", so it is a
    // conditional action.
    //
    // Placed last: the more specific app rules (above) must match before it.
    //
    // Note that `Cmd+Q` is not in this list: it is Alt+F4, already covered by
    // the global rule. If listed here, Cmd+Q would become Ctrl+Shift+Q—a combo
    // no one recognizes.
    //
    // Use the `keys = ["*"]` wildcard rather than listing keys one by one: the
    // terminal's bindings are obtained **at runtime** from GSettings and differ
    // per terminal (ptyxis has `new-window=Ctrl+Shift+N`, and N is not in any
    // static list). Listing keys one by one could only cover the fallback list,
    // leaving runtime-discovered keys nowhere to go—exactly the defect before
    // commit 18c03a2. The wildcard row catches everything, and which keys
    // actually take effect is decided by the action at runtime.
    v.push(Rule {
        apps: vec!["TerminalEmulator".into()],
        keys: vec!["*".into()],
        cmd: true,
        act: Action::TerminalCtrlShift,
        desc: Some("terminal specialization: add Shift only when the terminal itself bound this key".into()),
    });

    v
}

/// "Ctrl is already taken" apps: special cases outside terminals.
///
/// Terminals need not be listed—they are decided by `src/desktop.rs` via the
/// freedesktop spec's `Categories=TerminalEmulator`, automatically covering a
/// newly installed terminal.
///
/// 14 terminals were once hard-coded here, and ptyxis was missed: the list
/// wrote the lowercase `ptyxis`, but the actual wm_class is `org.gnome.Ptyxis`.
const DEFAULT_CTRL_BUSY_APPS: &[&str] = &[
    // GVIM: Ctrl+C quit, Ctrl+V block select, Ctrl+D half-select.
    // Note that vim running in a terminal has a window belonging to the
    // terminal, already covered by TerminalEmulator; this targets gvim in a
    // standalone window.
    "gvim",
];

impl Config {
    /// Built-in default config, with the default CapsLock swap and the default
    /// `Shift+Enter` behavior.
    #[cfg(test)]
    pub fn with_defaults() -> Self {
        Self::with_defaults_for(true, &TerminalConfig::default())
    }

    /// Built-in defaults with the two user-selectable behaviors resolved.
    ///
    /// `swap_capslock_ctrl` decides whether the paired CapsLock ↔ Left Ctrl
    /// remaps are installed. `terminal` decides whether the `Shift+Enter` row is
    /// installed and which apps it covers.
    pub(crate) fn with_defaults_for(swap_capslock_ctrl: bool, terminal: &TerminalConfig) -> Self {
        let mut remap = Vec::new();
        if swap_capslock_ctrl {
            remap.push(Remap {
                from: "KEY_CAPSLOCK".into(),
                to: "KEY_LEFTCTRL".into(),
                desc: Some("CapsLock → Left Ctrl".into()),
            });
            remap.push(Remap {
                from: "KEY_LEFTCTRL".into(),
                to: "KEY_CAPSLOCK".into(),
                desc: Some("Left Ctrl → CapsLock (paired swap with the entry above)".into()),
            });
        }

        Config {
            swap_capslock_ctrl: Some(swap_capslock_ctrl),
            remap,
            rules: default_rules(terminal),
            fn_mode: Some(FnMode::Fkeysfirst),
            desktop: DesktopConfig::default(),
            ctrl_busy_apps: DEFAULT_CTRL_BUSY_APPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            terminal: terminal.clone(),
        }
    }
}

/// Read and merge the system-level and user-level config; user-level overrides
/// system-level.
///
/// Reads the files first, then builds the defaults from the resolved
/// user-selectable options (`swap_capslock_ctrl`, `[terminal]`) so that a file
/// can remove a built-in behavior rather than only append to it.
pub fn load() -> Result<Config, ConfigError> {
    // Accumulate the file configs with the same precedence rules used for the
    // final merge. Starting from `Config::default()` keeps "not specified"
    // distinct from "explicitly false / empty".
    let mut user = Config::default();
    for path in candidate_paths() {
        if !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::Read(path.clone(), e))?;
        let loaded: Config = toml::from_str(&text)
            .map_err(|e| ConfigError::Parse(path.clone(), e))?;
        // Later reads override earlier ones: user-level is at the end of the list
        user.merge(loaded);
    }

    let swap = user.swap_capslock_ctrl.unwrap_or(true);
    let terminal = user.terminal.clone();
    let mut cfg = Config::with_defaults_for(swap, &terminal);
    cfg.merge(user);
    Ok(cfg)
}

/// List candidate config paths in priority order (later overrides earlier).
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    v.push(PathBuf::from("/etc/mackbind/config.toml"));
    if let Ok(home) = std::env::var("HOME") {
        v.push(Path::new(&home).join(".config/mackbind/config.toml"));
    }
    // The environment override is the highest priority, so it must be read last.
    if let Ok(p) = std::env::var("MACKBIND_CONFIG") {
        v.push(PathBuf::from(p));
    }
    v
}

/// Merge a higher-priority config on top of the current one.
///
/// Rules are **prepended** because lookup takes the first match. Remaps and
/// `ctrl_busy_apps` are appended (a later remap with the same `from` wins in
/// the mapper's map). Everything else uses field-level replacement, and `None`
/// leaves the lower-priority value untouched.
impl Config {
    fn merge(&mut self, other: Config) {
        let mut rules = other.rules;
        rules.extend(std::mem::take(&mut self.rules));
        self.rules = rules;

        self.remap.extend(other.remap);
        self.ctrl_busy_apps.extend(other.ctrl_busy_apps);

        if other.swap_capslock_ctrl.is_some() {
            self.swap_capslock_ctrl = other.swap_capslock_ctrl;
        }
        if other.fn_mode.is_some() {
            self.fn_mode = other.fn_mode;
        }
        if other.desktop.r#type.is_some() {
            self.desktop.r#type = other.desktop.r#type;
        }
        if other.desktop.spotlight.is_some() {
            self.desktop.spotlight = other.desktop.spotlight;
        }
        if other.terminal.shift_enter.is_some() {
            self.terminal.shift_enter = other.terminal.shift_enter;
        }
    }

    /// Human-readable summary, used for the startup log.
    pub fn summary(&self) -> String {
        format!(
            "{} remaps, {} rules, DE={:?}, {} Ctrl-busy apps",
            self.remap.len(),
            self.rules.len(),
            self.desktop.de_type(),
            self.ctrl_busy_apps.len()
        )
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read(PathBuf, std::io::Error),
    Parse(PathBuf, toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read(p, e) => write!(f, "failed to read config {}: {e}", p.display()),
            ConfigError::Parse(p, e) => write!(f, "failed to parse config {}: {e}", p.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_rule(desc: &str) -> Rule {
        Rule {
            apps: vec![],
            keys: vec!["KEY_Q".into()],
            cmd: true,
            act: Action::Emit {
                mods: vec!["C".into()],
                key: "KEY_Q".into(),
            },
            desc: Some(desc.into()),
        }
    }

    #[test]
    fn user_rules_are_prepended_so_they_override_defaults() {
        let mut cfg = Config::with_defaults();
        let default_len = cfg.rules.len();

        cfg.merge(Config {
            rules: vec![user_rule("user override")],
            ..Config::default()
        });

        assert_eq!(cfg.rules.len(), default_len + 1);
        assert_eq!(
            cfg.rules.first().and_then(|r| r.desc.as_deref()),
            Some("user override"),
            "lookup takes the first match, so user rules must come first"
        );
    }

    #[test]
    fn swap_capslock_ctrl_can_be_disabled() {
        let cfg = Config::with_defaults_for(false, &TerminalConfig::default());
        assert!(cfg.remap.is_empty());
        assert_eq!(cfg.swap_capslock_ctrl, Some(false));
    }

    #[test]
    fn user_remap_is_appended_and_overrides_the_same_direction() {
        let mut cfg = Config::with_defaults();
        cfg.merge(Config {
            remap: vec![Remap {
                from: "KEY_CAPSLOCK".into(),
                to: "KEY_ESCAPE".into(),
                desc: Some("CapsLock → Esc".into()),
            }],
            ..Config::default()
        });

        let caps = cfg
            .remap
            .iter()
            .rev()
            .find(|r| r.from == "KEY_CAPSLOCK")
            .expect("the user remap must be present");
        assert_eq!(caps.to, "KEY_ESCAPE");
        assert!(
            cfg.remap.iter().any(|r| r.from == "KEY_LEFTCTRL"),
            "the other direction of the default pair stays unless explicitly disabled"
        );
    }

    #[test]
    fn desktop_fields_merge_independently() {
        let mut cfg = Config::with_defaults();
        cfg.merge(Config {
            desktop: DesktopConfig {
                r#type: Some(DesktopType::Kde),
                spotlight: Some(false),
            },
            ..Config::default()
        });

        assert_eq!(cfg.desktop.de_type(), DesktopType::Kde);
        assert!(!cfg.desktop.spotlight());
    }

    #[test]
    fn shift_enter_can_be_disabled() {
        let cfg = Config::with_defaults_for(
            true,
            &TerminalConfig {
                shift_enter: Some(ShiftEnterMode::Off),
                ..TerminalConfig::default()
            },
        );
        assert!(
            !cfg.rules
                .iter()
                .any(|r| r.keys == vec!["S-KEY_ENTER".to_string()]),
            "off must remove the rule entirely"
        );
    }

    #[test]
    fn shift_enter_accepts_on_and_the_legacy_ctrl_j_value() {
        for value in ["on", "ctrl_j", "off"] {
            let toml = format!("[terminal]\nshift_enter = \"{value}\"\n");
            let cfg: Config = toml::from_str(&toml).expect("valid shift_enter value");
            assert!(cfg.terminal.shift_enter.is_some(), "{value} should parse");
        }
    }

    #[test]
    fn shift_enter_defaults_to_on() {
        let cfg: Config = toml::from_str("").expect("empty config");
        assert_eq!(cfg.terminal.shift_enter_mode(), ShiftEnterMode::On);
    }

    #[test]
    fn fn_mode_is_replaced_by_the_user() {
        let mut cfg = Config::with_defaults();
        cfg.merge(Config {
            fn_mode: Some(FnMode::Disabled),
            ..Config::default()
        });
        assert_eq!(cfg.fn_mode, Some(FnMode::Disabled));
    }
}
