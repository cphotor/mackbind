//! Accelerator (accel) expression parsing: the GTK/GSettings form.
//!
//! # Why this is needed
//!
//! Which keys the terminal binds with Ctrl+Shift is **the terminal's own**
//! business, not something mackbind can guess. ptyxis lists all of its
//! shortcuts in `org.gnome.Ptyxis.Shortcuts`:
//!
//! ```text
//! new-window      '<ctrl><shift>n'
//! close-window    '<ctrl><shift>q'
//! copy-clipboard  '<ctrl><shift>c'
//! ```
//!
//! A hand-written whitelist once missed `n` (new window), and also missed that
//! `close-window` is actually bound to `Ctrl+Shift+Q` rather than `Alt+F4`.
//! Rather than maintaining guesses, it is better to just ask the terminal
//! itself.
//!
//! # Format
//!
//! A GTK accelerator looks like `<ctrl><shift>n`, `<ctrl>plus`, `<Primary>F4`:
//!
//! - `<...>` wraps a modifier, case-insensitive
//! - what remains is the key name, broadly matching evdev's `KEY_*` names, with
//!   a few aliases (`plus`/`minus`/`equal`)

use crate::keys::modifier_from_abbrev;
use evdev::KeyCode;
use std::process::Command;

/// Decide whether an accelerator is of the form `<Ctrl><Shift>+main key`.
///
/// Only this class—"contains Ctrl and Shift"—is of interest; terminals use such
/// bindings for copy, close tab, new window, and so on. Bindings missing Ctrl or
/// Shift (`Alt+1` to switch tabs, `Ctrl+Page_Down` to move a tab) are not part
/// of this set.
///
/// GTK's `<primary>` is an alias for Ctrl and is treated as Ctrl here too.
pub fn is_ctrl_shift(accel: &str) -> bool {
    let (mods, _key) = split(accel);
    let has = |want: &[&str]| mods.iter().any(|m| want.iter().any(|w| m == *w));
    has(&["ctrl", "control", "primary"]) && has(&["shift"])
}

/// Take the main key name of an accelerator (e.g. `n`, `plus`, `F4`).
pub fn main_key(accel: &str) -> Option<&str> {
    let k = split(accel).1;
    if k.is_empty() {
        None
    } else {
        Some(k)
    }
}

/// Split into (modifier list, main key name). Modifiers are lowercased.
fn split(accel: &str) -> (Vec<String>, &str) {
    let mut mods = Vec::new();
    let mut rest = accel;
    while let Some(stripped) = rest.strip_prefix('<') {
        match stripped.find('>') {
            Some(end) => {
                mods.push(stripped[..end].to_ascii_lowercase());
                rest = &stripped[end + 1..];
            }
            None => break,
        }
    }
    (mods, rest)
}

/// Query the accelerator currently bound to a given GSettings action.
///
/// Used to hook macOS shortcuts onto **the desktop environment's existing
/// bindings** instead of rewriting the bindings. For example GNOME's overview
/// is bound to `toggle-overview` (default `Ctrl+Shift+space`); mackbind only
/// needs to emit that key set, leaving no trace in the user's config.
///
/// Returns `None` when the action is unbound.
pub fn action_accel(schema: &str, key: &str) -> Option<String> {
    let out = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // When unbound it returns @as [] or '', neither of which is a valid accelerator
    if text.is_empty() || text.starts_with("@as") || text == "''" {
        return None;
    }
    // Of the form "['<ctrl><shift>space']"; take the first; with multiple bindings the first suffices
    let inner = text.split('\'').nth(1)?;
    Some(inner.to_string())
}

/// Convert an accelerator expression into "modifiers + main key".
///
/// For mackbind's output side—it only understands its own `C-x` shorthand, not
/// GTK's `<ctrl><shift>space`.
pub fn parse_accel(accel: &str) -> Option<(Vec<u16>, KeyCode)> {
    let (mods, key) = split(accel);
    let mut out: Vec<u16> = Vec::new();
    for m in mods {
        // GTK modifier name (ctrl/primary/shalt…) → this tool's abbreviation
        // (C/A/S/M), reusing the key-name parser's table to avoid the two
        // mappings drifting apart
        let abbrev = match m.as_str() {
            // GTK's actual spellings are `<Control>` / `<Shift>` / `<Alt>` /
            // `<Super>`; the all-lowercase form (`<ctrl>`) also appears in some
            // schemas and is accepted as well. `<Primary>` also means Ctrl, and
            // `<Mod4>` means Super.
            "ctrl" | "control" | "primary" => "C",
            "shift" => "S",
            "alt" => "A",
            "super" | "meta" | "mod4" => "M",
            // hyper, lock, etc. are not produced by this tool; skip them
            _ => continue,
        };
        if let Some(kc) = modifier_from_abbrev(abbrev) {
            if !out.contains(&kc.0) {
                out.push(kc.0);
            }
        }
    }
    let kc = key_to_keycode(key)?;
    Some((out, kc))
}

/// GTK accelerator main key name → evdev keycode.
///
/// Reuses [`crate::keys::parse_external_key`]—the same set of parsing rules the
/// mapper side uses when reading the terminal schema. Previously the two were
/// written separately, which already caused a divergence.
fn key_to_keycode(name: &str) -> Option<KeyCode> {
    crate::keys::parse_external_key(name)
}

/// Candidate schema names for a terminal's shortcuts.
///
/// GNOME-family terminals use `<app_id>.Shortcuts`; other terminals (kitty,
/// wezterm, etc.) do not go through GSettings and cannot be queried—that class
/// of terminal keeps using the built-in whitelist.
fn schema_candidates(wm_class: &str) -> Vec<String> {
    vec![
        format!("{wm_class}.Shortcuts"),
        format!("{wm_class}.Keybindings"),
        format!("{}.shortcuts", wm_class.to_ascii_lowercase()),
    ]
}

/// Query which main key names a terminal itself binds with `<Ctrl><Shift>+key`.
///
/// Returns an empty list when the query fails (terminal does not use GSettings,
/// or the extension is not installed)—**not treated as an error**; the caller
/// falls back to the built-in whitelist.
pub fn ctrl_shift_keys(wm_class: &str) -> Vec<String> {
    for schema in schema_candidates(wm_class) {
        // Confirm the schema exists first, to avoid running
        // list-recursively for every candidate
        let exists = Command::new("gsettings")
            .args(["list-keys", &schema])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !exists {
            continue;
        }

        let out = match Command::new("gsettings")
            .args(["list-recursively", &schema])
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&out_of(o)).to_string(),
            _ => continue,
        };

        let mut keys = Vec::new();
        for line in out.lines() {
            // Of the form: schema key-name '<ctrl><shift>n'
            let Some(rest) = line.split_once(" '") else {
                continue;
            };
            let accel = rest.1.trim_end_matches('\'');
            if is_ctrl_shift(accel) {
                if let Some(k) = main_key(accel) {
                    keys.push(k.to_string());
                }
            }
        }
        if !keys.is_empty() {
            if std::env::var("MACKBIND_VERBOSE").is_ok() {
                eprintln!("[accel] {schema} provides Ctrl+Shift bindings: {keys:?}");
            }
            return keys;
        }
    }
    Vec::new()
}

fn out_of(o: std::process::Output) -> Vec<u8> {
    o.stdout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ctrl_shift() {
        assert!(is_ctrl_shift("<ctrl><shift>n"));
        assert!(is_ctrl_shift("<Shift><Ctrl>c"), "case-insensitive");
        assert!(is_ctrl_shift("<primary><shift>t"), "primary is Ctrl");
        // The spelling GNOME actually writes (on this machine toggle-overview uses this form)
        assert!(is_ctrl_shift("<Shift><Control>space"));
    }

    #[test]
    fn rejects_other_combos() {
        assert!(!is_ctrl_shift("<ctrl>c"), "missing Shift");
        assert!(!is_ctrl_shift("<shift>n"), "missing Ctrl");
        assert!(!is_ctrl_shift("<alt>1"), "both missing");
        assert!(!is_ctrl_shift(""), "empty string");
    }

    #[test]
    fn extracts_main_key() {
        assert_eq!(main_key("<ctrl><shift>n"), Some("n"));
        assert_eq!(main_key("<ctrl>plus"), Some("plus"));
        assert_eq!(main_key("<Primary>F4"), Some("F4"));
        assert_eq!(main_key(""), None, "empty string has no main key");
    }

    #[test]
    fn parses_real_gnome_accel() {
        // The real GNOME binding for toggle-overview on this machine
        let (mods, key) = parse_accel("<Shift><Control>space").unwrap();
        assert_eq!(key, KeyCode::KEY_SPACE);
        assert!(mods.contains(&KeyCode::KEY_LEFTCTRL.0), "{mods:?}");
        assert!(mods.contains(&KeyCode::KEY_LEFTSHIFT.0), "{mods:?}");
    }

    #[test]
    fn parses_plus_and_function_keys() {
        assert_eq!(parse_accel("<Control>plus").unwrap().1, KeyCode::KEY_EQUAL);
        assert_eq!(parse_accel("<Control>F4").unwrap().1, KeyCode::KEY_F4);
    }

    #[test]
    fn reads_ptyxis_own_bindings() {
        // ptyxis declares its shortcuts via GSettings; this machine should have it installed
        let keys = ctrl_shift_keys("org.gnome.Ptyxis");
        if std::path::Path::new("/usr/share/glib-2.0/schemas").exists() {
            assert!(keys.contains(&"n".to_string()), "new window Ctrl+Shift+N: {keys:?}");
            assert!(keys.contains(&"c".to_string()), "copy Ctrl+Shift+C: {keys:?}");
            assert!(keys.contains(&"w".to_string()), "close tab Ctrl+Shift+W: {keys:?}");
            // Should not include Ctrl-only bindings (e.g. Ctrl+Page_Down to move a tab)
            assert!(!keys.contains(&"1".to_string()), "Alt+1 should not be included: {keys:?}");
        }
    }

    #[test]
    fn unknown_app_yields_empty() {
        assert!(ctrl_shift_keys("com.example.nonexistent").is_empty());
    }
}
