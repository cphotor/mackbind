//! 键名解析与组合键表达式。
//!
//! 配置里的键名采用 evdev 的 `KeyCode` 枚举名（`KEY_LEFTMETA` 等），
//! 可直接 `from_str` 解析，无需自建映射表。
//!
//! 组合键表达式用简写，覆盖常见修饰键：
//!   `C` = Ctrl, `A` = Alt, `S` = Shift, `M` = Cmd/Super
//! 例如 `C-s`、`S-C-s`、`M-A-Left`。

use evdev::KeyCode;
use std::str::FromStr;

/// 修饰键缩写 → 键码。
pub fn modifier_from_abbrev(s: &str) -> Option<KeyCode> {
    match s {
        "C" => Some(KeyCode::KEY_LEFTCTRL),
        "A" => Some(KeyCode::KEY_LEFTALT),
        "S" => Some(KeyCode::KEY_LEFTSHIFT),
        "M" => Some(KeyCode::KEY_LEFTMETA),
        _ => None,
    }
}

/// 解析修饰键缩写，失败时给出可读的错误。
fn parse_modifier(s: &str) -> Result<KeyCode, KeyError> {
    modifier_from_abbrev(s).ok_or_else(|| {
        KeyError::BadModifier(
            s.to_string(),
            "可用缩写：C(Ctrl) A(Alt) S(Shift) M(Cmd/Super)".into(),
        )
    })
}

/// 常用键的友好别名 → evdev 原名。
///
/// 配置里写 `Left`、`[` 这类可读形式更友好，但底层解析走
/// `KeyCode::from_str`，只认 evdev 的 `KEY_LEFT` 形式。
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
];

/// 解析单个键名。
///
/// 支持三种写法，优先级从高到低：
/// 1. evdev 原名，如 `KEY_LEFTMETA`
/// 2. 友好别名，如 `Left`、`[`
/// 3. 单字符简写，自动补 `KEY_`，如 `a` → `KEY_A`
pub fn parse_key(name: &str) -> Result<KeyCode, KeyError> {
    // 1) evdev 原名
    if let Ok(k) = KeyCode::from_str(name) {
        return Ok(k);
    }

    // 2) 友好别名（不区分大小写）
    let lower = name.to_lowercase();
    for (alias, real) in ALIASES {
        if lower == *alias {
            return KeyCode::from_str(real)
                .map_err(|_| KeyError::UnknownKey(name.to_string()));
        }
    }

    // 3) 单字符简写
    if name.chars().count() == 1 {
        let upper = name.to_uppercase();
        if let Ok(k) = KeyCode::from_str(&format!("KEY_{upper}")) {
            return Ok(k);
        }
    }

    Err(KeyError::UnknownKey(name.to_string()))
}

/// 解析组合表达式，按 `-` 分隔。返回 (修饰键列表, 主键)。
pub fn parse_combo(expr: &str) -> Result<(Vec<KeyCode>, KeyCode), KeyError> {
    let parts: Vec<&str> = expr.split('-').collect();
    if parts.is_empty() {
        return Err(KeyError::BadCombo(expr.to_string(), "表达式为空".into()));
    }

    let (last, mods) = parts.split_last().unwrap();

    let mut modifiers = Vec::new();
    for m in mods {
        modifiers.push(parse_modifier(m)?);
    }

    // 主键：可能是修饰键本身（"C-C" 这种），也可能普通键
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
            KeyError::UnknownKey(k) => write!(f, "未知键名 '{k}'（需 evdev 形式如 KEY_A）"),
            KeyError::BadModifier(m, h) => write!(f, "未知修饰键 '{m}'；{h}"),
            KeyError::BadCombo(c, r) => write!(f, "无效组合 '{c}'：{r}"),
        }
    }
}

impl std::error::Error for KeyError {}