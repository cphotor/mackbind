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
    let has = |want: &[&str]| {
        mods.iter()
            .any(|m| want.iter().any(|w| m == *w))
    };
    has(&["ctrl", "primary"]) && has(&["shift"])
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