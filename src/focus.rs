//! 焦点窗口查询（GNOME Wayland）。
//!
//! # 为什么需要扩展
//!
//! Wayland 下 compositor 不把窗口信息交给客户端，`xdotool`、`wmctrl`、
//! X11 属性查询全部失效。GNOME 自带的几条路径经实测也不可用：
//!
//! | 方案 | 结果 |
//! |---|---|
//! |`org.gnome.Shell.Eval`|返回 `(false, '')`，GNOME 41+ 限制|
//! |`org.gnome.Shell.WindowTracker`|对象路径不存在|
//! |`xdotool` / `wmctrl`|Wayland 下不可用|
//!
//! 因此需要 GNOME Shell 扩展导出焦点窗口信息。本模块使用
//! `focused-window-dbus@flexagoon.com`（本机实测 version 11 可用），
//! 其 D-Bus 接口为：
//!
//! ```text
//! dest:   org.gnome.Shell
//! object: /org/gnome/shell/extensions/FocusedWindow
//! method: org.gnome.shell.extensions.FocusedWindow.Get
//! ```
//!
//! 返回 JSON，其中 `wm_class` 即应用标识。
//!
//! 注意 `dest` 是 `org.gnome.Shell` 本身：扩展由 GNOME Shell 进程直接
//! 导出对象，不占用独立的 bus name。用 `org.gnome.Shell.Extensions.Dbus`
//! 查询会得到 `ServiceUnknown`，那是地址写错，不代表扩展未启用。
//!
//! # 降级行为
//!
//! 扩展缺失或不可用时，`focused_wm_class()` 返回 `None`，调用方按
//! 「未知应用」处理。**不视为错误**——其他映射规则仍可正常工作，
//! 仅「按应用区分」的部分退化为默认行为。

use std::process::Command;

const DBUS_DEST: &str = "org.gnome.Shell";
const DBUS_PATH: &str = "/org/gnome/shell/extensions/FocusedWindow";
const DBUS_METHOD: &str = "org.gnome.shell.extensions.FocusedWindow.Get";

/// 查询当前焦点窗口的 `wm_class`。
///
/// 扩展不可用时返回 `None`。
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
    // 输出形如 ('{"title":"…","wm_class":"org.gnome.Ptyxis",…}',)
    // 从中取出 wm_class 的值
    extract_json_string(&text, "wm_class")
}

/// 从形如 `…"key":"value"…` 的文本中取出 value。
///
/// 只需处理 `wm_class` 这类简单字符串字段，不引入 JSON 解析库。
fn extract_json_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];

    // 跳过可选的空格
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;

    // 找到结束的引号（该字段无转义需求，取首个未转义的 "）
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
        // 本机可能未启用该扩展；无论结果如何都不应 panic
        let _ = focused_wm_class();
    }
}
