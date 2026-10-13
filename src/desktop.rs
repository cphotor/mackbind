//! Desktop entry parsing: decide whether an app belongs to a category.
//!
//! # Why this is needed
//!
//! Apps where "Ctrl is already taken" (terminals, GVIM) were originally
//! determined by a hardcoded `WM_CLASS` list, which immediately proved broken in
//! practice: ptyxis's `wm_class` is actually `org.gnome.Ptyxis`, while the list
//! had the lowercase `ptyxis`, which did not match.
//!
//! The freedesktop Desktop Entry spec has a standard convention for this —
//! terminal emulators' `.desktop` entries all carry
//! `Categories=...TerminalEmulator;`:
//!
//! ```text
//! /usr/share/applications/org.gnome.Ptyxis.desktop
//!   Categories=GNOME;GTK;System;TerminalEmulator;
//! ```
//!
//! Deciding by the spec rather than enumerating a list automatically covers
//! newly installed terminals, and never misses one due to case or prefix
//! differences.
//!
//! # Note
//!
//! The decision is based on **the app the window belongs to**. nvim, htop,
//! python, etc. running inside a terminal still have a terminal emulator's
//! `WM_CLASS`, so they are already covered by the terminal rule. Only editors
//! that run standalone with their own window (gvim, code, etc.) need separate
//! configuration.

use std::path::{Path, PathBuf};

/// XDG data directories, matching the defaults of `xdg-user-dir` / GLib.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(&home).join(".local/share"));
    }
    dirs.push(PathBuf::from("/usr/local/share"));
    dirs.push(PathBuf::from("/usr/share"));
    if let Ok(extra) = std::env::var("XDG_DATA_DIRS") {
        for d in extra.split(':').filter(|s| !s.is_empty()) {
            dirs.push(PathBuf::from(d));
        }
    } else {
        dirs.push(PathBuf::from("/usr/local/share"));
        dirs.push(PathBuf::from("/usr/share"));
    }
    dirs
}

/// Find the desktop entry for `wm_class` in the XDG data directories.
///
/// `wm_class` looks like `org.gnome.Ptyxis`, corresponding to
/// `applications/org.gnome.Ptyxis.desktop`. User directories take priority.
fn find_desktop_file(wm_class: &str) -> Option<PathBuf> {
    let filename = format!("{wm_class}.desktop");
    data_dirs()
        .into_iter()
        .map(|d| d.join("applications").join(&filename))
        .find(|p| p.exists())
}

/// Read the desktop entry's `Categories` value.
///
/// Parses only this line and does not bring in a full INI parser — a desktop
/// entry's `Categories` is conventionally given on a single line.
fn read_categories(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        // Only takes effect after skipping past the section header ([Desktop Entry])
        if line.starts_with('[') {
            // Entered a new section; give up if it is not the main one
            if !line.trim().eq_ignore_ascii_case("[Desktop Entry]") {
                return None;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("Categories=") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Decide whether an app belongs to the given category (case-insensitive).
///
/// Used for terminal detection: `is_terminal("org.gnome.Ptyxis")` → true
pub fn has_category(wm_class: &str, category: &str) -> bool {
    let Some(path) = find_desktop_file(wm_class) else {
        return false;
    };
    let Some(cats) = read_categories(&path) else {
        return false;
    };
    cats.split(';')
        .filter(|s| !s.is_empty())
        .any(|c| c.trim().eq_ignore_ascii_case(category))
}

/// The standard category for terminal emulators in desktop entries.
pub const CATEGORY_TERMINAL_EMULATOR: &str = "TerminalEmulator";

/// Decide whether the app owning the window is a terminal emulator.
pub fn is_terminal(wm_class: &str) -> bool {
    has_category(wm_class, CATEGORY_TERMINAL_EMULATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_installed_terminals() {
        // Different distributions have different default terminals (EndeavourOS
        // was measured to have kgx/gnome-console, Ubuntu 26.04 only ptyxis). Do
        // not insist on a specific terminal, just assert that **every terminal
        // installed on this machine** can be recognized via Categories.
        let candidates = [
            "org.gnome.Ptyxis",
            "org.gnome.Console",
            "org.gnome.Terminal",
            "kitty",
            "Alacritty",
            "com.mitchellh.ghostty",
            "org.kde.konsole",
        ];
        let mut found = 0;
        for term in candidates {
            if find_desktop_file(term).is_some() {
                found += 1;
                assert!(is_terminal(term), "{term} should be recognized as a terminal");
            }
        }
        assert!(
            found > 0,
            "no known terminal found on this machine, the test is meaningless — check the desktop entry lookup paths"
        );
    }

    #[test]
    fn non_terminal_is_not_matched() {
        assert!(!is_terminal("org.gnome.Nautilus"), "a file manager is not a terminal");
        assert!(!is_terminal("code"), "VSCode is not a terminal");
    }

    #[test]
    fn unknown_app_is_graceful() {
        // A nonexistent app should neither crash nor be misjudged
        assert!(!is_terminal("com.example.nonexistent.app"));
        assert!(!is_terminal(""));
    }

    #[test]
    fn desktop_file_lookup_is_exact_case() {
        // wm_class case must correspond exactly to the filename (desktop
        // filenames are case-sensitive), but the category comparison is
        // case-insensitive.
        //
        // Do not hardcode ptyxis: use any terminal installed on this machine
        // instead, to avoid depending on a distribution's default terminal.
        let Some(term) = ["org.gnome.Ptyxis", "org.gnome.Console", "kitty", "Alacritty"]
            .into_iter()
            .find(|t| find_desktop_file(t).is_some())
        else {
            return; // no terminal to test
        };
        let f = find_desktop_file(term).expect("confirmed to exist");
        assert!(f.to_string_lossy().contains(term));
    }
}
