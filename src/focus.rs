//! Focused-window query (GNOME Wayland).
//!
//! # Why an extension is needed
//!
//! On Wayland the compositor does not hand window information to clients, so
//! `xdotool`, `wmctrl`, and all X11 property queries fail. The few paths GNOME
//! ships for this were also measured to be unusable:
//!
//! | Approach | Result |
//! |---|---|
//! |`org.gnome.Shell.Eval`|returns `(false, '')`, restricted since GNOME 41|
//! |`org.gnome.Shell.WindowTracker`|object path does not exist|
//! |`xdotool` / `wmctrl`|unusable under Wayland|
//!
//! So a GNOME Shell extension is needed to export focused-window information.
//! This module uses `focused-window-dbus@flexagoon.com` (version 11 declares
//! `shell-version: ["49", "50"]`; `./install.sh` installs and enables it on
//! GNOME). Its D-Bus interface is:
//!
//! ```text
//! dest:   org.gnome.Shell
//! object: /org/gnome/shell/extensions/FocusedWindow
//! method: org.gnome.shell.extensions.FocusedWindow.Get
//! ```
//!
//! It returns JSON, in which `wm_class` is the app identifier.
//!
//! Note that `dest` is `org.gnome.Shell` itself: the extension exports its
//! object directly from the GNOME Shell process and does not take a separate bus
//! name. Querying with `org.gnome.Shell.Extensions.Dbus` yields
//! `ServiceUnknown`; that is a wrong address, not proof the extension is
//! disabled.
//!
//! # Fallback behavior
//!
//! When the extension is missing or unavailable, `focused_wm_class()` returns
//! `None` and the caller treats it as an "unknown app". That is **not treated
//! as an error**—the other mapping rules still work; only the "distinguish by
//! app" part degrades to the default behavior. The degradation is silent, and
//! the default behavior is `Cmd → Ctrl`: in a terminal the result is that
//! `Cmd+C` becomes `Ctrl+C` (SIGINT) rather than copy. So when per-app
//! behavior looks wrong, query the interface above *first*.

use std::process::Command;

const DBUS_DEST: &str = "org.gnome.Shell";
const DBUS_PATH: &str = "/org/gnome/shell/extensions/FocusedWindow";
const DBUS_METHOD: &str = "org.gnome.shell.extensions.FocusedWindow.Get";

/// Query the `wm_class` of the current focused window.
///
/// Returns `None` when the extension is unavailable.
pub fn focused_wm_class() -> Option<String> {
    let out = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            DBUS_DEST,
            "--object-path",
            DBUS_PATH,
            "--method",
            DBUS_METHOD,
        ])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    // Output looks like ('{"title":"…","wm_class":"org.gnome.Ptyxis",…}',)
    // Extract the value of wm_class from it
    extract_json_string(&text, "wm_class")
}

/// Extract the value from text of the form `…"key":"value"…`.
///
/// Only simple string fields like `wm_class` need handling, so no JSON parsing
/// library is pulled in.
fn extract_json_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];

    // Skip optional whitespace
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;

    // Find the closing quote (this field needs no escaping; take the first
    // unescaped ")
    let mut escaped = false;
    for (i, ch) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(rest[..i].to_string()),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_wm_class_from_gdbus_output() {
        let sample = r#"('{"title":"Terminal","wm_class":"org.gnome.Ptyxis","pid":4182,"focus":true}',)"#;
        assert_eq!(
            extract_json_string(sample, "wm_class").as_deref(),
            Some("org.gnome.Ptyxis")
        );
    }

    #[test]
    fn handles_missing_key() {
        let sample = r#"('{"title":"Terminal","pid":1}',)"#;
        assert_eq!(extract_json_string(sample, "wm_class"), None);
    }

    #[test]
    fn handles_malformed_input_without_panic() {
        assert_eq!(extract_json_string("", "wm_class"), None);
        assert_eq!(extract_json_string("garbage", "wm_class"), None);
        assert_eq!(extract_json_string(r#"("x":"y""#, "wm_class"), None);
    }

    #[test]
    fn graceful_when_extension_absent() {
        // The extension may not be enabled on this machine; it must not panic
        // either way
        let _ = focused_wm_class();
    }
}
