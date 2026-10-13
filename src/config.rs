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
    /// rules
    pub r#type: DesktopType,
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
    #[serde(default = "default_true")]
    pub spotlight: bool,
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
    /// Modifier swap/replace. Defaults to the CapsLock ↔ Left Ctrl swap.
    ///
    /// **Not part of the rule table**: it acts on the key itself rather than a
    /// combo, and happens before combo matching.
    pub remap: Vec<Remap>,
    /// Mapping rule table. **The single source of truth for mappings**—a lookup
    /// just walks this list.
    ///
    /// Lookup order is declaration order, and the first match wins. The user
    /// config is appended after the defaults, so user entries naturally take
    /// priority.
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
/// **This is the single source of truth for mappings**—the user config is
/// appended on top of it (later entries win).
/// For the full design see `docs/table-driven-design.md`.
fn default_rules() -> Vec<Rule> {
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
    /// Built-in default config. The user config is layered on top of it.
    pub fn with_defaults() -> Self {
        Config {
            remap: vec![
                Remap {
                    from: "KEY_CAPSLOCK".into(),
                    to: "KEY_LEFTCTRL".into(),
                    desc: Some("CapsLock → Left Ctrl".into()),
                },
                Remap {
                    from: "KEY_LEFTCTRL".into(),
                    to: "KEY_CAPSLOCK".into(),
                    desc: Some("Left Ctrl → CapsLock (paired swap with the entry above)".into()),
                },
            ],
            rules: default_rules(),
            fn_mode: Some(FnMode::Fkeysfirst),
            desktop: DesktopConfig {
                spotlight: true,
                ..DesktopConfig::default()
            },
            ctrl_busy_apps: DEFAULT_CTRL_BUSY_APPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

/// Read and merge the system-level and user-level config; user-level overrides
/// system-level.
///
/// Starts from the built-in defaults. When there is no config file at all, the
/// full default mapping is returned.
pub fn load() -> Result<Config, ConfigError> {
    let mut cfg = Config::with_defaults();

    for path in candidate_paths() {
        if !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::Read(path.clone(), e))?;
        let loaded: Config = toml::from_str(&text)
            .map_err(|e| ConfigError::Parse(path.clone(), e))?;
        // Later reads override earlier ones: user-level is at the end of the list
        cfg.merge(loaded);
    }
    Ok(cfg)
}

/// List candidate config paths in priority order (later overrides earlier).
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("MACKBIND_CONFIG") {
        v.push(PathBuf::from(p));
    }
    v.push(PathBuf::from("/etc/mackbind/config.toml"));
    if let Ok(home) = std::env::var("HOME") {
        v.push(Path::new(&home).join(".config/mackbind/config.toml"));
    }
    v
}

/// Merge the user config on top of the default config.
///
/// `rules` / `remap` / `ctrl_busy_apps` use **append**—user entries come after
/// the defaults, and lookup takes the **first match**, so user entries
/// naturally take priority. `fn_mode` uses **replace**.
impl Config {
    fn merge(&mut self, other: Config) {
        self.remap.extend(other.remap);
        self.rules.extend(other.rules);
        self.ctrl_busy_apps.extend(other.ctrl_busy_apps);
        if other.fn_mode.is_some() {
            self.fn_mode = other.fn_mode;
        }
    }

    /// Human-readable summary, used for the startup log.
    pub fn summary(&self) -> String {
        format!(
            "{} remaps, {} rules, DE={:?}, {} Ctrl-busy apps",
            self.remap.len(),
            self.rules.len(),
            self.desktop.r#type,
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
