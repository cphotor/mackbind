//! Key-name parsing and combo-key expressions.
//!
//! Key names in config use the evdev `KeyCode` enum names (`KEY_LEFTMETA`,
//! etc.), which can be parsed directly with `from_str`, with no need for a
//! hand-rolled mapping table.
//!
//! Combo-key expressions use shorthand covering the common modifiers:
//!   `C` = Ctrl, `A` = Alt, `S` = Shift, `M` = Cmd/Super
//! For example `C-s`, `S-C-s`, `M-A-Left`.

use evdev::KeyCode;
use std::str::FromStr;

/// Modifier abbreviation → keycode.
pub fn modifier_from_abbrev(s: &str) -> Option<KeyCode> {
    match s {
        "C" => Some(KeyCode::KEY_LEFTCTRL),
        "A" => Some(KeyCode::KEY_LEFTALT),
        "S" => Some(KeyCode::KEY_LEFTSHIFT),
        "M" => Some(KeyCode::KEY_LEFTMETA),
        _ => None,
    }
}

/// Parse a modifier abbreviation, giving a readable error on failure.
fn parse_modifier(s: &str) -> Result<KeyCode, KeyError> {
    modifier_from_abbrev(s).ok_or_else(|| {
        KeyError::BadModifier(
            s.to_string(),
            "available abbreviations: C(Ctrl) A(Alt) S(Shift) M(Cmd/Super)".into(),
        )
    })
}

/// Friendly aliases for common keys → the original evdev names.
///
/// Writing readable forms like `Left` or `[` in config is friendlier, but the
/// underlying parsing goes through `KeyCode::from_str`, which only understands
/// evdev's `KEY_LEFT` form.
const ALIASES: &[(&str, &str)] = &[
    ("left", "KEY_LEFT"),
    ("right", "KEY_RIGHT"),
    ("up", "KEY_UP"),
    ("down", "KEY_DOWN"),
    ("home", "KEY_HOME"),
    ("end", "KEY_END"),
    ("pageup", "KEY_PAGEUP"),
    ("pagedown", "KEY_PAGEDOWN"),
    ("del", "KEY_DELETE"),
    ("delete", "KEY_DELETE"),
    ("ins", "KEY_INSERT"),
    ("esc", "KEY_ESC"),
    ("escape", "KEY_ESC"),
    ("space", "KEY_SPACE"),
    ("tab", "KEY_TAB"),
    ("enter", "KEY_ENTER"),
    ("backspace", "KEY_BACKSPACE"),
    ("grave", "KEY_GRAVE"),
    ("minus", "KEY_MINUS"),
    ("equal", "KEY_EQUAL"),
    ("comma", "KEY_COMMA"),
    ("dot", "KEY_DOT"),
    ("slash", "KEY_SLASH"),
    ("backslash", "KEY_BACKSLASH"),
    ("semicolon", "KEY_SEMICOLON"),
    ("apostrophe", "KEY_APOSTROPHE"),
    ("[", "KEY_LEFTBRACE"),
    ("]", "KEY_RIGHTBRACE"),
    ("-", "KEY_MINUS"),
    ("=", "KEY_EQUAL"),
    (",", "KEY_COMMA"),
    (".", "KEY_DOT"),
    ("/", "KEY_SLASH"),
    ("\\", "KEY_BACKSLASH"),
    (";", "KEY_SEMICOLON"),
    ("'", "KEY_APOSTROPHE"),
    ("`", "KEY_GRAVE"),
    // GTK accelerator / X11 keysym names. Merged into the table above rather
    // than written as a separate one—having two copies of `plus`/`minus`
    // previously caused them to diverge.
    ("plus", "KEY_EQUAL"),
    ("Return", "KEY_ENTER"),
    ("Page_Up", "KEY_PAGEUP"),
    ("Page_Down", "KEY_PAGEDOWN"),
    // Print: GNOME binds screenshot to it, but the evdev keycode name is the
    // historical SYSRQ (SysRq and Print are the same physical key). Without
    // this alias, the `Print` read by `de_accel` fails to parse and the
    // screenshot rules get silently dropped.
    ("print", "KEY_SYSRQ"),
    ("sysrq", "KEY_SYSRQ"),
    // GNOME's `overlay-key` is a **string** schema (value `Super`), used for
    // "press bare Super to open the overview". When parsing it the main key is
    // that word, so this alias is needed.
    ("super", "KEY_LEFTMETA"),
    ("meta", "KEY_LEFTMETA"),
];

/// Parse a single key name.
///
/// Three forms are supported, in decreasing priority:
/// 1. the original evdev name, e.g. `KEY_LEFTMETA`
/// 2. a friendly alias, e.g. `Left`, `[`
/// 3. a single-character shorthand, auto-prefixed with `KEY_`, e.g. `a` → `KEY_A`
pub fn parse_key(name: &str) -> Result<KeyCode, KeyError> {
    // 1) original evdev name
    if let Ok(k) = KeyCode::from_str(name) {
        return Ok(k);
    }

    // 2) friendly alias (case-insensitive)
    let lower = name.to_lowercase();
    for (alias, real) in ALIASES {
        if lower == *alias {
            return KeyCode::from_str(real)
                .map_err(|_| KeyError::UnknownKey(name.to_string()));
        }
    }

    // 3) single-character shorthand
    if name.chars().count() == 1 {
        let upper = name.to_uppercase();
        if let Ok(k) = KeyCode::from_str(&format!("KEY_{upper}")) {
            return Ok(k);
        }
    }

    Err(KeyError::UnknownKey(name.to_string()))
}

/// Parse a key name **coming from dynamic data** (GSettings, terminal schema).
///
/// Unlike [`parse_key`], these names are not under this tool's control, so case
/// and spelling can only be handled as actually seen: GTK writes
/// `<Shift><Control>space`, `F1`, `Page_Down`, `plus`, in a messier variety
/// than what may appear in a config file.
///
/// Strategy:
/// 1. look up the alias table (case-insensitive)
/// 2. already in evdev form (`KEY_A`, `Page_Down`) → try directly
/// 3. single character or `F<n>` → try with a `KEY_` prefix
///
/// Returns `None` on failure—the caller skips that rule on this basis instead
/// of failing the whole program. Dynamic data can change at any time; one
/// unrecognized name should not bring down the mapping.
pub fn parse_external_key(name: &str) -> Option<KeyCode> {
    let t = name.trim();
    if t.is_empty() {
        return None;
    }

    // 1) alias table (case-insensitive—GTK's `Page_Down` and evdev's
    //    `KEY_PAGEDOWN` must both match, and both cases appear in the table)
    let lower = t.to_lowercase();
    if let Some((_, real)) = ALIASES
        .iter()
        .find(|(a, _)| a.to_lowercase() == lower)
    {
        return KeyCode::from_str(real).ok();
    }

    // 2) already evdev or X11 form (including uppercase ones, e.g. Page_Down, KEY_A)
    if let Ok(k) = KeyCode::from_str(t) {
        return Some(k);
    }

    // 3) lowercase single character / F-number, add the KEY_ prefix
    let upper = t.to_ascii_uppercase();
    let prefixed = if upper.chars().count() == 1 || is_function_key(&upper) {
        format!("KEY_{upper}")
    } else {
        upper
    };
    KeyCode::from_str(&prefixed).ok()
}

/// Whether this is a function-key spelling like `F1`…`F24`.
fn is_function_key(upper: &str) -> bool {
    upper.starts_with('F')
        && upper.len() > 1
        && upper[1..].chars().all(|c| c.is_ascii_digit())
}

/// Parse a combo expression, separated by `-`. Returns (modifier list, main key).
pub fn parse_combo(expr: &str) -> Result<(Vec<KeyCode>, KeyCode), KeyError> {
    let parts: Vec<&str> = expr.split('-').collect();
    if parts.is_empty() {
        return Err(KeyError::BadCombo(expr.to_string(), "expression is empty".into()));
    }

    let (last, mods) = parts.split_last().unwrap();

    let mut modifiers = Vec::new();
    for m in mods {
        modifiers.push(parse_modifier(m)?);
    }

    // Main key: may be a modifier itself ("C-C" style), or an ordinary key
    let key = match modifier_from_abbrev(last) {
        Some(k) => k,
        None => parse_key(last)?,
    };

    Ok((modifiers, key))
}

#[derive(Debug)]
pub enum KeyError {
    UnknownKey(String),
    BadModifier(String, String),
    BadCombo(String, String),
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::UnknownKey(k) => write!(f, "unknown key name '{k}' (expected evdev form such as KEY_A)"),
            KeyError::BadModifier(m, h) => write!(f, "unknown modifier '{m}'; {h}"),
            KeyError::BadCombo(c, r) => write!(f, "invalid combo '{c}': {r}"),
        }
    }
}

impl std::error::Error for KeyError {}
