//! 配置解析。
//!
//! # 设计要点
//!
//! **内置默认映射，开箱即用。** 用户装上 mackbind 后无需任何配置
//! 即可获得 macOS 快捷键习惯。用户配置用于**覆盖**或**追加**，而非
//! 启用功能。
//!
//! **桌面环境是可配置项，不可硬编码。** Linux 上有 GNOME、KDE Plasma、
//! Sway、Hyprland 等多种 DE，同一 macOS 功能在不同 DE 下的实现方式、
//! 可用性、默认快捷键都不同。DE 相关行为通过 `[desktop] type`
//! 指定；查不到时相关规则跳过，但**不可当作不存在**——应保留在
//! 文档中待支持对应 DE 时启用。
//!
//! # Cmd 的默认语义
//!
//! `Cmd+X` 默认映射为 `Ctrl+X`（而非保留 Super）。依据实测：GNOME 的
//! 28 个 Super 绑定中涉及字母的仅 `Super+h` 一项，`Super+字母` 基本
//! 空闲——而 macOS 的 `Cmd+字母` 绝大多数对应的正是 Ctrl。保持 Super
//! 会让绝大多数组合什么也不发生。
//!
//! Super 相关规则（`Cmd+Tab`/`Cmd+H` 等）暂缓实现，理由见
//! `docs/macos-shortcuts.md`。

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 单条修饰键重映射：把 `from` 键改成 `to` 键。
#[derive(Debug, Clone, Deserialize)]
pub struct Remap {
    /// evdev 键码名，如 "KEY_CAPSLOCK"
    pub from: String,
    /// evdev 键码名，如 "KEY_LEFTCTRL"
    pub to: String,
    /// 可选说明，仅供人阅读
    #[serde(default)]
    pub desc: Option<String>,
}

/// Cmd 组合的显式映射规则。
///
/// 默认为空——绝大多数 Cmd 组合由「默认语义」自动处理（`Cmd+X` →
/// `Ctrl+X`），无需逐条列举。此表用于**目标键与主键不同**的情况，
/// 如 `Cmd+[` → `Alt+←`。
#[derive(Debug, Clone, Deserialize)]
pub struct CmdMap {
    /// 非修饰键，evdev 键码名，如 ["c"]。可带修饰前缀，如 "S-z"。
    pub keys: Vec<String>,
    /// 输出组合，如 "C-c"（C=Ctrl, A=Alt, S=Shift, M=Cmd）
    pub emit: String,
    #[serde(default)]
    pub desc: Option<String>,
}

/// 针对特定应用的 Cmd 规则覆盖。
///
/// 用于**同一按键在不同应用里需要不同输出**的情形。最典型的例子：
/// 终端的复制键。Linux 终端系普遍用 `Ctrl+Shift+C`，但 ptyxis 与
/// konsole 在无选中文本时会把它穿透给进程，当作 SIGINT ——于是
/// 「复制」会杀掉正在跑的任务。这两个应用必须改用 `Ctrl+Insert`
///（X11 传统复制键，不会误触发信号）。
///
/// Toshy 把这条写死在配置里（`term_sigint_preventer_lst`），此处改为
/// 数据驱动：判定依据是行为差异，不是应用名单。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppCmdMap {
    /// 适用的应用 WM_CLASS / app_id，匹配方式为子串包含
    pub apps: Vec<String>,
    /// 仅对这些应用生效的规则。优先于全局 `cmd_map`
    pub rules: Vec<CmdMap>,
}

/// 桌面环境类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopType {
    /// GNOME / Mutter 系（含 ptyxis、GNOME 应用）
    #[default]
    Gnome,
    /// KDE Plasma
    Kde,
    /// Sway、Hyprland、i3 等 WM，或其他无法查表的环境
    None,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopConfig {
    /// 桌面环境类型，决定 DE 相关规则的查表来源
    pub r#type: DesktopType,
}

/// Fn 键优先级模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FnMode {
    /// F 键优先，按住 Fn 出亮度/媒体键（同内核 fnmode=2）
    #[default]
    Fkeysfirst,
    /// 媒体键优先，按住 Fn 出 F 键（同内核 fnmode=1）
    Fkeyslast,
    /// 不做转换
    Disabled,
}

/// 完整配置。字段全部可选——不写即用内置默认值。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// 修饰键互换/替换。默认为 CapsLock ↔ 左 Ctrl 互换。
    pub remap: Vec<Remap>,
    /// Cmd 组合的显式覆盖规则。默认为空。
    pub cmd_map: Vec<CmdMap>,
    /// Fn 键优先级。`None` 表示未指定，保留默认值。
    #[serde(default)]
    pub fn_mode: Option<FnMode>,
    /// 桌面环境相关配置
    pub desktop: DesktopConfig,
    /// 「Ctrl 已被占用」的额外应用，Cmd 在其中映射为 Ctrl+Shift。
    ///
    /// 默认为空。**终端无需列举**——由 `.desktop` 条目的
    /// `Categories=TerminalEmulator` 判定（见 `src/desktop.rs`），
    /// 按 freedesktop 规范自动覆盖新装的终端。
    ///
    /// 此项用于终端之外、Ctrl 有非标准含义的应用，如独立窗口的 gvim。
    pub ctrl_busy_apps: Vec<String>,
    /// 在「Ctrl 已被占用」的应用内，Cmd 需映射为 Ctrl+Shift 的键。
    ///
    /// 只列**这些应用自身已用 Ctrl+Shift 绑定**的键：终端的复制
    /// （Ctrl+Shift+C）、关标签页（Ctrl+Shift+W）、新标签页
    /// （Ctrl+Shift+T）等。加 Shift 正是为了命中它们。
    ///
    /// 之所以逐键列举而非整体换语义：若终端里把 Cmd 一律变成
    /// Ctrl+Shift，`Cmd+Q` 就会变成 `Ctrl+Shift+Q`——一个谁也不认的
    /// 组合。而 `Cmd+Q`（退出）对应的 Linux 键是 `Alt+F4`，属跨应用的
    /// 全局规则，不该被终端特化吞掉。
    pub terminal_shift_keys: Vec<String>,
    /// 按应用生效的 Cmd 规则覆盖，优先于全局 `cmd_map` 与终端特化。
    ///
    /// 默认为 ptyxis / konsole 的复制键改用 `Ctrl+Insert`——这两个
    /// 终端在无选中文本时会把 `Ctrl+Shift+C` 穿透给进程当作 SIGINT，
    /// 使「复制」杀掉正在运行的任务。
    pub app_cmd_map: Vec<AppCmdMap>,
}

/// macOS 特有组合的显式映射表（目标键与主键不同者）。
///
/// 依据 Apple 官方快捷键表，详见 `docs/macos-shortcuts.md`。
const DEFAULT_CMD_MAP: &[(&str, &str, &str)] = &[
    // ── 光标移动（macOS 用 Cmd/Option，Linux 用 Ctrl/Alt）──
    ("KEY_LEFT", "C-Left", "行首 (Cmd+←)"),
    ("KEY_RIGHT", "C-Right", "行尾 (Cmd+→)"),
    ("KEY_UP", "C-Up", "文稿开头 (Cmd+↑)"),
    ("KEY_DOWN", "C-Down", "文稿末尾 (Cmd+↓)"),
    ("S-KEY_LEFT", "C-S-Left", "选中到行首"),
    ("S-KEY_RIGHT", "C-S-Right", "选中到行尾"),
    ("S-KEY_UP", "C-S-Up", "选中到文稿开头"),
    ("S-KEY_DOWN", "C-S-Down", "选中到文稿末尾"),
    // Option 类：macOS 用 Option 跳词，Linux 用 Alt
    ("A-KEY_LEFT", "A-Left", "上一词 (Option+←)"),
    ("A-KEY_RIGHT", "A-Right", "下一词 (Option+→)"),
    // ── 访达导航（Cmd+[ / Cmd+]）──
    ("KEY_LEFTBRACE", "A-Left", "上一文件夹 (Cmd+[)"),
    ("KEY_RIGHTBRACE", "A-Right", "下一文件夹 (Cmd+])"),
    // ── 删除 ──
    ("KEY_DELETE", "C-KEY_DELETE", "删左字符 (Ctrl+D)"),
    // ── 窗口管理 ──
    // 跨应用的全局规则，故不受终端特化影响。
    // macOS 的 Cmd+Q 是「退出应用」，Linux 侧最接近的通用绑定是
    // Alt+F4（GNOME 的 wm.keybindings close）。Toshy 用的是同一条。
    ("KEY_Q", "A-KEY_F4", "退出应用 (Cmd+Q → Alt+F4)"),
];

/// 「Ctrl 已被占用」的应用：终端与编辑器。
///
/// 这些应用内 Ctrl 有既定含义（终端 Ctrl+C 中断、GVIM Ctrl+V 块选择），
/// Cmd 需映射为 Ctrl+Shift 以避免冲突。
const DEFAULT_CTRL_BUSY_APPS: &[&str] = &[
    // 终端不再列举——由 `src/desktop.rs` 按 freedesktop 规范的
    // `Categories=TerminalEmulator` 判定，装了新终端自动生效。
    //
    // 曾在此硬编码 14 个终端，结果 ptyxis 漏判：名单写的是小写
    // `ptyxis`，实际 wm_class 是 `org.gnome.Ptyxis`。
    //
    // GVIM：Ctrl+C 退出、Ctrl+V 块选择、Ctrl+D 半选。
    // 注意终端里跑的 vim 其窗口属于终端，已被 TerminalEmulator 覆盖；
    // 此处针对独立窗口的 gvim。
    "gvim",
];

/// 终端自己用 Ctrl+Shift 绑定的键。
///
/// 依据 Toshy 的终端段配置（`default-toshy-config/toshy_config.py`），
/// 以及 GNOME 终端系（ptyxis / gnome-terminal / Console）的常用绑定。
/// 这些键在 macOS 是 Cmd 组合，在 Linux 终端里对应 Ctrl+Shift。
const DEFAULT_TERMINAL_SHIFT_KEYS: &[&str] = &[
    "KEY_C",          // 复制
    "KEY_V",          // 粘贴
    "KEY_X",          // 剪切
    "KEY_T",          // 新标签页
    "KEY_W",          // 关标签页
    "KEY_D",          // 水平分屏（部分终端）
    "KEY_F",          // 查找（部分终端）
    "KEY_S",          // 保存（部分终端）
    "KEY_EQUAL",      // 放大字号
    "KEY_MINUS",      // 缩小字号
    "KEY_LEFTBRACE",  // 上一个标签页
    "KEY_RIGHTBRACE", // 下一个标签页
];

/// 复制键会漏发 SIGINT 的终端。
///
/// ptyxis 与 konsole 在**无选中文本**时会把 `Ctrl+Shift+C` 穿透给
/// 子进程，当作 SIGINT ——复制会杀掉正在跑的任务。wezterm、ghostty、
/// gnome-terminal（VTE 系）无此问题：它们无条件吞掉该组合。
///
/// 改用 `Ctrl+Insert`（X11 传统复制键）：终端无一例外支持，且不会被
/// 误当作控制字符发送给进程。
const SIGINT_LEAKY_TERMINALS: &[&str] = &["org.gnome.Ptyxis", "org.kde.konsole", "konsole"];

impl Config {
    /// 内置默认配置。用户配置在此基础上叠加。
    pub fn with_defaults() -> Self {
        Config {
            remap: vec![
                Remap {
                    from: "KEY_CAPSLOCK".into(),
                    to: "KEY_LEFTCTRL".into(),
                    desc: Some("CapsLock → 左 Ctrl".into()),
                },
                Remap {
                    from: "KEY_LEFTCTRL".into(),
                    to: "KEY_CAPSLOCK".into(),
                    desc: Some("左 Ctrl → CapsLock（与上条成对互换）".into()),
                },
            ],
            cmd_map: DEFAULT_CMD_MAP
                .iter()
                .map(|(keys, emit, desc)| CmdMap {
                    keys: vec![(*keys).to_string()],
                    emit: (*emit).to_string(),
                    desc: Some((*desc).to_string()),
                })
                .collect(),
            fn_mode: Some(FnMode::Fkeysfirst),
            desktop: DesktopConfig::default(),
            ctrl_busy_apps: DEFAULT_CTRL_BUSY_APPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            terminal_shift_keys: DEFAULT_TERMINAL_SHIFT_KEYS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            app_cmd_map: vec![AppCmdMap {
                apps: SIGINT_LEAKY_TERMINALS.iter().map(|s| (*s).to_string()).collect(),
                rules: vec![CmdMap {
                    keys: vec!["KEY_C".into()],
                    emit: "C-KEY_INSERT".into(),
                    desc: Some(
                        "改用 Ctrl+Insert 复制：ptyxis/konsole 在无选中文本时会把 \
                         Ctrl+Shift+C 当作 SIGINT 漏给进程"
                            .into(),
                    ),
                }],
            }],
        }
    }
}

/// 读入并合并系统级与用户级配置，用户级覆盖系统级。
///
/// 从内置默认值开始。没有任何配置文件时，返回完整的默认映射。
pub fn load() -> Result<Config, ConfigError> {
    let mut cfg = Config::with_defaults();

    for path in candidate_paths() {
        if !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::Read(path.clone(), e))?;
        let loaded: Config = toml::from_str(&text)
            .map_err(|e| ConfigError::Parse(path.clone(), e))?;
        // 后读到的覆盖先读到的：用户级在列表末尾
        cfg.merge(loaded);
    }
    Ok(cfg)
}

/// 按优先级顺序列出候选配置路径（后者覆盖前者）。
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("MACKBIND_CONFIG") {
        v.push(PathBuf::from(p));
    }
    v.push(PathBuf::from("/etc/mackbind/config.toml"));
    if let Ok(home) = std::env::var("HOME") {
        v.push(Path::new(&home).join(".config/mackbind/config.toml"));
    }
    v
}

/// 合并用户配置到默认配置之上。
///
/// `remap` / `cmd_map` / `ctrl_busy_apps` 采用**追加**（用户条目在默认
/// 之后，查找时用户优先命中）；`fn_mode` 采用**替换**。
impl Config {
    fn merge(&mut self, other: Config) {
        self.remap.extend(other.remap);
        self.cmd_map.extend(other.cmd_map);
        self.ctrl_busy_apps.extend(other.ctrl_busy_apps);
        self.terminal_shift_keys.extend(other.terminal_shift_keys);
        self.app_cmd_map.extend(other.app_cmd_map);
        if other.fn_mode.is_some() {
            self.fn_mode = other.fn_mode;
        }
    }

    /// 人类可读的摘要，用于启动日志。
    pub fn summary(&self) -> String {
        format!(
            "{} 条 remap, {} 条 cmd_map, DE={:?}, {} 个 Ctrl 占用应用",
            self.remap.len(),
            self.cmd_map.len(),
            self.desktop.r#type,
            self.ctrl_busy_apps.len()
        )
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read(PathBuf, std::io::Error),
    Parse(PathBuf, toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read(p, e) => write!(f, "读取配置 {} 失败: {e}", p.display()),
            ConfigError::Parse(p, e) => write!(f, "解析配置 {} 失败: {e}", p.display()),
        }
    }
}

impl std::error::Error for ConfigError {}
