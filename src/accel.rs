//! 加速键（accel）表达式解析：GTK/GSettings 形式。
//!
//! # 为什么需要
//!
//! 终端用 Ctrl+Shift 绑定哪些键，是**终端自己**的事，不是 mackbind 能
//! 猜的。ptyxis 把全部快捷键列在 `org.gnome.Ptyxis.Shortcuts` 里：
//!
//! ```text
//! new-window      '<ctrl><shift>n'
//! close-window    '<ctrl><shift>q'
//! copy-clipboard  '<ctrl><shift>c'
//! ```
//!
//! 手写白名单漏掉过 `n`（新窗口），也漏掉了 `close-window` 实际绑定在
//! `Ctrl+Shift+Q` 而非 `Alt+F4`。与其维护猜测，不如直接问终端自己。
//!
//! # 格式
//!
//! GTK 加速键形如 `<ctrl><shift>n`、`<ctrl>plus`、`<Primary>F4`：
//!
//! - `<...>` 包裹修饰键，大小写不敏感
//! - 剩下的是键名，与 evdev 的 `KEY_*` 名大体一致，个别有别名
//!   （`plus`/`minus`/`equal`）

use crate::keys::modifier_from_abbrev;
use evdev::KeyCode;
use std::process::Command;

/// 判断加速键是否为 `<Ctrl><Shift>+主键` 形式。
///
/// 只关心「含 Ctrl 与 Shift」这一类——终端用这类绑定复制、关标签页、
/// 新窗口等。缺 Ctrl 或缺 Shift 的绑定（`Alt+1` 切标签、
/// `Ctrl+Page_Down` 移标签）不属于此列。
///
/// GTK 的 `<primary>` 是 Ctrl 的别名，此处一并视为 Ctrl。
pub fn is_ctrl_shift(accel: &str) -> bool {
    let (mods, _key) = split(accel);
    let has = |want: &[&str]| mods.iter().any(|m| want.iter().any(|w| m == *w));
    has(&["ctrl", "control", "primary"]) && has(&["shift"])
}

/// 取出加速键的主键名（如 `n`、`plus`、`F4`）。
pub fn main_key(accel: &str) -> Option<&str> {
    let k = split(accel).1;
    if k.is_empty() {
        None
    } else {
        Some(k)
    }
}

/// 拆分为 (修饰键列表, 主键名)。修饰键统一小写。
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

/// 查询某个 GSettings 动作当前绑定的加速键。
///
/// 用于把 macOS 快捷键接到**桌面环境已有的绑定**上，而不去改写绑定。
/// 例如 GNOME 的概览绑在 `toggle-overview`（默认 `Ctrl+Shift+空格`），
/// mackbind 只要发出这组键即可，无需在用户配置里留下任何记录。
///
/// 动作未绑定时返回 `None`。
pub fn action_accel(schema: &str, key: &str) -> Option<String> {
    let out = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // 未绑定时返回 @as [] 或 ''，都不是有效加速键
    if text.is_empty() || text.starts_with("@as") || text == "''" {
        return None;
    }
    // 形如 "['<ctrl><shift>space']"，取第一个；多绑定时取首个即可
    let inner = text.split('\'').nth(1)?;
    Some(inner.to_string())
}

/// 把加速键表达式转为「修饰键 + 主键」。
///
/// 用于 mackbind 的输出端——它只认自己的 `C-x` 简写，不认 GTK 的
/// `<ctrl><shift>space`。
pub fn parse_accel(accel: &str) -> Option<(Vec<u16>, KeyCode)> {
    let (mods, key) = split(accel);
    let mut out: Vec<u16> = Vec::new();
    for m in mods {
        // GTK 的修饰键名（ctrl/primary/shalt…）→ 本工具的缩写（C/A/S/M），
        // 复用键名解析器的表，避免两处映射不一致
        let abbrev = match m.as_str() {
            // GTK 的实际拼写是 `<Control>` / `<Shift>` / `<Alt>` / `<Super>`，
            // 全小写形式（`<ctrl>`）在部分 schema 里也会出现，一并接受。
            // 另有 `<Primary>` 表示 Ctrl、`<Mod4>` 表示 Super。
            "ctrl" | "control" | "primary" => "C",
            "shift" => "S",
            "alt" => "A",
            "super" | "meta" | "mod4" => "M",
            // hyper、lock 等本工具不产生，跳过
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

/// GTK 加速键的主键名 → evdev 键码。
fn key_to_keycode(name: &str) -> Option<KeyCode> {
    use std::str::FromStr;
    let evdev = match name {
        "plus" => "KEY_EQUAL",
        "minus" => "KEY_MINUS",
        "space" => "KEY_SPACE",
        other => {
            // GSettings 写 F1，evdev 是 KEY_F1
            if let Some(rest) = other.strip_prefix('F') {
                if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                    return KeyCode::from_str(&format!("KEY_F{rest}")).ok();
                }
            }
            // 含大写的多为 evdev 形式（Page_Down、Return）
            if other.chars().any(|c| c.is_ascii_uppercase()) {
                return KeyCode::from_str(other).ok();
            }
            return KeyCode::from_str(&format!("KEY_{}", other.to_ascii_uppercase())).ok();
        }
    };
    KeyCode::from_str(evdev).ok()
}

/// 终端的快捷键 schema 名候选。
///
/// GNOME 系终端用 `<app_id>.Shortcuts`；其他终端（kitty、wezterm 等）
/// 不走 GSettings，无从查询——那类终端继续用内置白名单。
fn schema_candidates(wm_class: &str) -> Vec<String> {
    vec![
        format!("{wm_class}.Shortcuts"),
        format!("{wm_class}.Keybindings"),
        format!("{}.shortcuts", wm_class.to_ascii_lowercase()),
    ]
}

/// 查询某个终端自己用 `<Ctrl><Shift>+键` 绑定了哪些主键名。
///
/// 查不到（终端不用 GSettings、或扩展未装）时返回空列表——**不视为
/// 错误**，调用方退回内置白名单。
pub fn ctrl_shift_keys(wm_class: &str) -> Vec<String> {
    for schema in schema_candidates(wm_class) {
        // 先确认 schema 存在，避免对每个候选都跑一次 list-recursively
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
            // 形如：schema key-name '<ctrl><shift>n'
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
                eprintln!("[accel] {schema} 提供 Ctrl+Shift 绑定: {keys:?}");
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
        assert!(is_ctrl_shift("<Shift><Ctrl>c"), "大小写不敏感");
        assert!(is_ctrl_shift("<primary><shift>t"), "primary 即 Ctrl");
        // GNOME 实际写出的拼法（本机 toggle-overview 即此形式）
        assert!(is_ctrl_shift("<Shift><Control>space"));
    }

    #[test]
    fn rejects_other_combos() {
        assert!(!is_ctrl_shift("<ctrl>c"), "缺 Shift");
        assert!(!is_ctrl_shift("<shift>n"), "缺 Ctrl");
        assert!(!is_ctrl_shift("<alt>1"), "两者都缺");
        assert!(!is_ctrl_shift(""), "空串");
    }

    #[test]
    fn extracts_main_key() {
        assert_eq!(main_key("<ctrl><shift>n"), Some("n"));
        assert_eq!(main_key("<ctrl>plus"), Some("plus"));
        assert_eq!(main_key("<Primary>F4"), Some("F4"));
        assert_eq!(main_key(""), None, "空串无主键");
    }

    #[test]
    fn parses_real_gnome_accel() {
        // 本机 GNOME 对 toggle-overview 的实际绑定
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
        // ptyxis 用 GSettings 声明快捷键；本机应装有
        let keys = ctrl_shift_keys("org.gnome.Ptyxis");
        if std::path::Path::new("/usr/share/glib-2.0/schemas").exists() {
            assert!(keys.contains(&"n".to_string()), "新窗口 Ctrl+Shift+N: {keys:?}");
            assert!(keys.contains(&"c".to_string()), "复制 Ctrl+Shift+C: {keys:?}");
            assert!(keys.contains(&"w".to_string()), "关标签页 Ctrl+Shift+W: {keys:?}");
            // 不应包含只有 Ctrl 的绑定（如 Ctrl+Page_Down 移标签）
            assert!(!keys.contains(&"1".to_string()), "Alt+1 不该入选: {keys:?}");
        }
    }

    #[test]
    fn unknown_app_yields_empty() {
        assert!(ctrl_shift_keys("com.example.nonexistent").is_empty());
    }
}