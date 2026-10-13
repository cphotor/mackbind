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
    // GTK 加速键 / X11 键符号名。与上表合并而非另写一张——
    // 曾因两处各写一份 `plus`/`minus` 而产生分歧。
    ("plus", "KEY_EQUAL"),
    ("Return", "KEY_ENTER"),
    ("Page_Up", "KEY_PAGEUP"),
    ("Page_Down", "KEY_PAGEDOWN"),
    // Print：GNOME 截图绑在它上面，但 evdev 的键码名是历史遗留的
    // SYSRQ（SysRq 与 Print 是同一个物理键）。没有这条别名，
    // `de_accel` 读到的 `Print` 解析失败，截图规则会被静默剔除。
    ("print", "KEY_SYSRQ"),
    ("sysrq", "KEY_SYSRQ"),
    // GNOME 的 `overlay-key` 是**字符串型** schema（值为 `Super`），
    // 用于「裸按 Super 开概览」。解析它时主键就是这个词，故需此别名。
    ("super", "KEY_LEFTMETA"),
    ("meta", "KEY_LEFTMETA"),
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

/// 解析**来自动态数据**（GSettings、终端 schema）的键名。
///
/// 与 [`parse_key`] 的区别是这些名字不受本工具控制，大小写与拼写都只能
/// 按实际见到的处理：GTK 写 `<Shift><Control>space`、`F1`、`Page_Down`、
/// `plus`，形式比配置文件里可能出现的更杂。
///
/// 策略：
/// 1. 查别名表（大小写不敏感）
/// 2. 已是 evdev 形式（`KEY_A`、`Page_Down`）直接试
/// 3. 单字符或 `F<n>`，补 `KEY_` 前缀后试
///
/// 解析失败返回 `None`——调用方据此跳过该条规则，而非让整个程序失败。
/// 动态数据随时可能变，一个不认识的名字不该拖垮映射。
pub fn parse_external_key(name: &str) -> Option<KeyCode> {
    let t = name.trim();
    if t.is_empty() {
        return None;
    }

    // 1) 别名表（大小写不敏感——GTK 的 `Page_Down` 与 evdev 的
    //    `KEY_PAGEDOWN` 都要能命中，而表内两种大小写都出现过）
    let lower = t.to_lowercase();
    if let Some((_, real)) = ALIASES
        .iter()
        .find(|(a, _)| a.to_lowercase() == lower)
    {
        return KeyCode::from_str(real).ok();
    }

    // 2) 已是 evdev 或 X11 形式（含大写者，如 Page_Down、KEY_A）
    if let Ok(k) = KeyCode::from_str(t) {
        return Some(k);
    }

    // 3) 小写单字符 / F 数字，补 KEY_ 前缀
    let upper = t.to_ascii_uppercase();
    let prefixed = if upper.chars().count() == 1 || is_function_key(&upper) {
        format!("KEY_{upper}")
    } else {
        upper
    };
    KeyCode::from_str(&prefixed).ok()
}

/// 是否为 `F1`…`F24` 这类功能键写法。
fn is_function_key(upper: &str) -> bool {
    upper.starts_with('F')
        && upper.len() > 1
        && upper[1..].chars().all(|c| c.is_ascii_digit())
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