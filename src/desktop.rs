//! 桌面条目解析：判断应用是否属于某一类别。
//!
//! # 为什么需要
//!
//! 「Ctrl 已被占用」的应用（终端、GVIM）最初靠硬编码 `WM_CLASS` 名单
//! 判定，实测立刻出问题：ptyxis 的 `wm_class` 实际是
//! `org.gnome.Ptyxis`，而名单里写的是小写 `ptyxis`，匹配不上。
//!
//! freedesktop 桌面条目规范对此有标准约定——终端模拟器的 `.desktop`
//! 条目都带 `Categories=...TerminalEmulator;`：
//!
//! ```text
//! /usr/share/applications/org.gnome.Ptyxis.desktop
//!   Categories=GNOME;GTK;System;TerminalEmulator;
//! ```
//!
//! 按规范判定而非枚举列表，可自动覆盖新装的终端，也不会因大小写、
//! 前缀差异而漏判。
//!
//! # 注意
//!
//! 判定依据是**窗口所属的应用**。终端里运行的 nvim、htop、python 等
//! 其窗口的 `WM_CLASS` 仍是终端模拟器，因此已被终端规则覆盖。独立
//! 运行、有自己窗口的编辑器（gvim、code 等）才需另行配置。

use std::path::{Path, PathBuf};

/// XDG 数据目录，与 `xdg-user-dir` / GLib 的默认值一致。
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

/// 在 XDG 数据目录中查找 `wm_class` 对应的桌面条目。
///
/// `wm_class` 形如 `org.gnome.Ptyxis`，对应
/// `applications/org.gnome.Ptyxis.desktop`。用户目录优先。
fn find_desktop_file(wm_class: &str) -> Option<PathBuf> {
    let filename = format!("{wm_class}.desktop");
    data_dirs()
        .into_iter()
        .map(|d| d.join("applications").join(&filename))
        .find(|p| p.exists())
}

/// 读取桌面条目的 `Categories` 值。
///
/// 只解析本行，不引入完整 INI 解析器——桌面条目的 `Categories`
/// 惯例上单行给出。
fn read_categories(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        // 跳过段头（[Desktop Entry]）之后才生效
        if line.starts_with('[') {
            // 进入新段，若不是主段则放弃
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

/// 判断应用是否属于指定类别（大小写不敏感）。
///
/// 用于终端判定：`is_terminal("org.gnome.Ptyxis")` → true
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

/// 终端模拟器在桌面条目中的标准类别。
pub const CATEGORY_TERMINAL_EMULATOR: &str = "TerminalEmulator";

/// 判断窗口所属应用是否为终端模拟器。
pub fn is_terminal(wm_class: &str) -> bool {
    has_category(wm_class, CATEGORY_TERMINAL_EMULATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_installed_terminals() {
        // 本机应至少装有 ptyxis 与 gnome-terminal
        assert!(
            is_terminal("org.gnome.Ptyxis"),
            "ptyxis 应被识别为终端"
        );
        assert!(
            is_terminal("org.gnome.Console"),
            "gnome-console 应被识别为终端"
        );
    }

    #[test]
    fn non_terminal_is_not_matched() {
        assert!(!is_terminal("org.gnome.Nautilus"), "文件管理器不是终端");
        assert!(!is_terminal("code"), "VSCode 不是终端");
    }

    #[test]
    fn unknown_app_is_graceful() {
        // 不存在的应用不应崩溃，也不应误判
        assert!(!is_terminal("com.example.nonexistent.app"));
        assert!(!is_terminal(""));
    }

    #[test]
    fn case_insensitive_filename_lookup() {
        // wm_class 大小写需与文件名精确对应（desktop 文件名区分大小写），
        // 但类别比较不区分大小写
        let f = find_desktop_file("org.gnome.Ptyxis").expect("应找到 ptyxis 桌面条目");
        assert!(f.to_string_lossy().contains("org.gnome.Ptyxis"));
    }
}
