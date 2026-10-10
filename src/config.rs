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

/// 一条映射规则的动作。
///
/// **动作即函数**：前两种是「发出固定组合」，后两种在**运行时求值**——
/// 查 DE 现有的绑定、或查终端自己声明的快捷键。这样表负责命名，
/// 求值交给函数，不必把动态发现的结果冻结成会过期的快照。
///
/// 参见 `docs/table-driven-design.md`。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// 发出固定组合，如 `Cmd+Q` → `Alt+F4`
    Emit {
        /// 输出侧修饰键
        #[serde(default)]
        mods: Vec<String>,
        /// 输出主键
        key: String,
    },
    /// 读取桌面环境对该功能的现有绑定，发出同样的键。
    ///
    /// 好处是不在用户配置里留下记录，也不覆盖用户自己的设置；
    /// 用户改了绑定则自动跟上。查不到时**跳过**（不视为错误）。
    DeAccel {
        /// 功能标识，如 `overview`。查表见 `de_action_lookup`。
        feature: String,
    },
    /// 仅当目标应用**自己**用 `Ctrl+Shift` 绑定了这个键时才加 Shift。
    ///
    /// 加 Shift 的目的是命中应用已有的绑定，而不是笼统躲冲突。
    /// 该键集在运行时从 GSettings 查得（见 `set_window_id`），
    /// 故此处无法写成固定输出——这是本表唯一的「条件」动作。
    TerminalCtrlShift,
    /// 执行一条外部命令。
    ///
    /// 用于「本质是命令而非组合键」的操作，如清倒废纸篓。
    /// 破坏性命令走 [`crate::exec`] 的确认流程。
    Exec {
        /// 可执行文件名，如 `gio`
        program: String,
        /// 参数
        #[serde(default)]
        args: Vec<String>,
        /// 是否先要求用户确认，默认 `true`。
        ///
        /// 破坏性命令**强制**确认，写 `false` 也不行——
        /// 免确认的全局热键一旦误按就是永久删除，没有回收站可捞。
        #[serde(default = "default_true")]
        confirm: bool,
        /// 确认框正文。省略则用自动生成的说明。
        #[serde(default)]
        prompt: Option<String>,
        /// 确认按钮文字。省略则用「执行」。
        #[serde(default)]
        ok_label: Option<String>,
    },
}

/// 表的一行：`(应用, 按键组合) → 动作`。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// 适用的应用。省略或 `["*"]` 表示所有应用。
    ///
    /// `"TerminalEmulator"` 是特殊取值——按 `.desktop` 的 `Categories`
    /// 判定（freedesktop 规范），装新终端自动生效。其余按 wm_class
    /// 子串匹配。
    #[serde(default)]
    pub apps: Vec<String>,
    /// 输入侧组合，**不含 Cmd**（见 `cmd` 字段）。
    /// 如 `["KEY_Q"]`、`["S-KEY_LEFT"]`。
    pub keys: Vec<String>,
    /// 本规则是否**要求按住 Cmd**。默认 `true`。
    ///
    /// 表内绝大多数行都是「Cmd+某键」，故默认要求 Cmd。
    /// 少数不含 Cmd 的组合（如 macOS 的 `Option+Delete`）须显式写
    /// `cmd = false`。
    ///
    /// **必须有这个区分**：查表对所有组合生效（否则非 Cmd 组合无法
    /// 配置），若规则不再校验 Cmd 是否按住，则**裸按空格也会命中
    /// `Cmd+空格` 那条规则**，直接触发 GNOME 概览。
    #[serde(default = "default_true")]
    pub cmd: bool,
    /// 动作
    #[serde(rename = "action")]
    pub act: Action,
    /// 可选说明，仅供人阅读
    #[serde(default)]
    pub desc: Option<String>,
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
    /// 是否把 macOS 的 `Cmd+空格`（Spotlight）映射为桌面环境的概览。
    ///
    /// 默认开启。实现方式是**读取** DE 现有的绑定并发出同样的键，
    /// 而不是改写绑定——因此不会在用户配置里留下任何记录，
    /// 也不影响用户自己对该键的设置。
    ///
    /// 查不到对应动作时（如 DE 未提供概览）自动跳过，不视为错误。
    #[serde(default = "default_true")]
    pub spotlight: bool,
}

/// `#[serde(default)]` 用的真值常量——多数布尔配置项默认开启。
///
/// 不能用 `true` 字面量：`Deserialize` 的 `default` 要求可调用。
fn default_true() -> bool {
    true
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
    ///
    /// **不在规则表内**：它作用于按键本身而非组合，发生在组合判定之前。
    pub remap: Vec<Remap>,
    /// 映射规则表。**唯一的映射真源**——查表即遍历此列表。
    ///
    /// 查找顺序即声明顺序，首个命中者生效。用户配置在默认值之后追加，
    /// 故用户条目天然优先。
    pub rules: Vec<Rule>,
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
    ///
    /// 与规则表里的 `TerminalCtrlShift` 动作配合：命中此名单的应用，
    /// 该动作才可能生效。
    pub ctrl_busy_apps: Vec<String>,
}

/// 终端特化的**兜底**键集。
///
/// 运行时优先从终端自己的 GSettings 查得实际绑定（`set_window_id`），
/// 查不到才用这份名单——适用于不走 GSettings 的终端（kitty、wezterm）。
///
/// 这份名单**必然不完整**：走 GSettings 的终端各自声明不同绑定，
/// 例如 ptyxis 有 `new-window = Ctrl+Shift+N`，而 `N` 不在此列。
/// 故表中另有一条 `keys = ["*"]` 的通配行承接运行时发现的键。
pub const DEFAULT_TERMINAL_SHIFT_KEYS: &[&str] = &[
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

/// 会把 `Ctrl+Shift+C` 漏发成 SIGINT 的终端。
///
/// ptyxis 与 konsole 在**无选中文本**时把该组合穿透给子进程当作
/// SIGINT——「复制」会杀掉正在跑的任务。wezterm、ghostty、
/// gnome-terminal（VTE 系）无条件吞掉该组合，无此问题。
///
/// 改用 `Ctrl+Insert`（X11 传统复制键）：终端均支持，且不会被当作
/// 控制字符发给进程。判定依据是**行为差异**，不是应用名单——故此行
/// 的 `apps` 只是「哪些终端有此行为」，换终端时按行为增删即可。
const SIGINT_LEAKY_TERMINALS: &[&str] = &["org.gnome.Ptyxis", "org.gnome.konsole", "konsole"];

/// 内置默认规则表。
///
/// **这是唯一的映射真源**——用户配置在其之上追加（后者优先命中）。
/// 完整设计见 `docs/table-driven-design.md`。
fn default_rules() -> Vec<Rule> {
    let mut v: Vec<Rule> = Vec::new();

    let mut push_emit = |apps: &[&str], keys: &str, mods: &str, key: &str, desc: &str| {
        v.push(Rule {
            apps: apps.iter().map(|s| s.to_string()).collect(),
            keys: vec![keys.to_string()],
            cmd: true,
            act: Action::Emit {
                mods: if mods.is_empty() {
                    vec![]
                } else {
                    vec![mods.to_string()]
                },
                key: key.to_string(),
            },
            desc: Some(desc.to_string()),
        });
    };

    // ── 光标移动（macOS 用 Cmd/Option，Linux 用 Ctrl/Alt）──
    push_emit(&[], "KEY_LEFT", "C", "Left", "行首 (Cmd+←)");
    push_emit(&[], "KEY_RIGHT", "C", "Right", "行尾 (Cmd+→)");
    push_emit(&[], "KEY_UP", "C", "Up", "文稿开头 (Cmd+↑)");
    push_emit(&[], "KEY_DOWN", "C", "Down", "文稿末尾 (Cmd+↓)");
    push_emit(&[], "S-KEY_LEFT", "C-S", "Left", "选中到行首");
    push_emit(&[], "S-KEY_RIGHT", "C-S", "Right", "选中到行尾");
    push_emit(&[], "S-KEY_UP", "C-S", "Up", "选中到文稿开头");
    push_emit(&[], "S-KEY_DOWN", "C-S", "Down", "选中到文稿末尾");
    push_emit(&[], "A-KEY_LEFT", "A", "Left", "上一词 (Option+←)");
    push_emit(&[], "A-KEY_RIGHT", "A", "Right", "下一词 (Option+→)");
    // ── 访达导航（Cmd+[ / Cmd+]）──
    push_emit(&[], "KEY_LEFTBRACE", "A", "Left", "上一文件夹 (Cmd+[)");
    push_emit(&[], "KEY_RIGHTBRACE", "A", "Right", "下一文件夹 (Cmd+])");
    // ── 删除 ──
    // ── 窗口管理 ──
    // 跨应用的全局规则，故不受终端特化影响。
    // macOS 的 Cmd+Q 是「退出应用」，Linux 侧最接近的通用绑定是
    // Alt+F4（GNOME 的 wm.keybindings close）。Toshy 用的是同一条。
    push_emit(&[], "KEY_Q", "A", "KEY_F4", "退出应用 (Cmd+Q → Alt+F4)");

    // ── 桌面环境功能：读 DE 现有绑定后发出同样的键 ──
    // 放在末尾：它按主键（空格）匹配，不该被上面的行抢走。
    v.push(Rule {
        apps: vec![],
        keys: vec!["KEY_SPACE".into()],
        cmd: true,
        act: Action::DeAccel {
            feature: "overview".into(),
        },
        desc: Some("Cmd+空格 → 概览（读 DE 现有绑定）".into()),
    });

    // ── Ctrl+Cmd 组合：macOS 上有独立含义，不可由默认语义推导 ──
    //
    // 前提是「Cmd 映射为 Ctrl」。用户已按住 Ctrl 时，Cmd 再变 Ctrl 就是
    // Ctrl+Ctrl ——同一槽位，组合退化，故此类组合**不能**转换。
    // 但也不能当 Cmd+X 处理（见 `default_semantics` 的说明）。
    // 表里有的按表走，没有的原样放行。
    //
    // 放在全局规则之后、`*` 通配之前：优先级高于终端特化。
    //
    // Ctrl+Cmd+Q 锁屏：读 GNOME 现有的 screensaver 绑定（本机 Super+l），
    // 与 Cmd+空格 → 概览同一路子——不改写用户配置。
    v.push(Rule {
        apps: vec![],
        keys: vec!["C-KEY_Q".into()],
        cmd: true,
        act: Action::DeAccel {
            feature: "lock-screen".into(),
        },
        desc: Some("锁屏（Ctrl+Cmd+Q，读 DE 现有绑定）".into()),
    });

    // Ctrl+Cmd+F 全屏：应用级全屏的标准键。
    // GNOME 的 toggle-fullscreen 动作存在但默认未绑定（@as []），
    // 故直接发 F11，无需改写用户设置。
    v.push(Rule {
        apps: vec![],
        keys: vec!["C-KEY_F".into()],
        cmd: true,
        act: Action::Emit {
            mods: vec![],
            key: "KEY_F11".into(),
        },
        desc: Some("全屏 (Ctrl+Cmd+F → F11)".into()),
    });

    // ── 截图：对应 macOS 的 Cmd+Shift+3/4/5 ──
    //
    // 读 GNOME 现有绑定后发出同样的键，不改写用户配置。
    //
    // **与直觉相反的一点**（实测）：GNOME 的
    //   screenshot        (Shift+Print) = **直接截全屏**，不弹界面
    //   show-screenshot-ui(Print)       = **弹出截图界面**
    // 而 macOS 是 Cmd+Shift+3 直接截全屏、Cmd+Shift+4/5 进选择模式。
    // 故按功能对齐，3 对「直接截」、4/5 对「界面」：
    //   Cmd+Shift+3 → screenshot          直接截全屏
    //   Cmd+Shift+4 → show-screenshot-ui  截图界面（选区/窗口在其中选）
    //   Cmd+Shift+5 → show-screenshot-ui  同上
    //
    // 4 与 5 行为相同是妥协：GNOME 只有三个截图动作，其中
    // `screenshot` 与 `screenshot-window` 实测都直接截全屏，
    // 无法据此区分选区与窗口。选区/窗口仍可在界面里选到。
    for (keys, feature, desc) in [
        ("S-KEY_3", "screenshot", "截全屏 (Cmd+Shift+3，直接截不弹界面)"),
        (
            "S-KEY_4",
            "screenshot-ui",
            "截图界面 (Cmd+Shift+4，选区/窗口在其中选)",
        ),
        (
            "S-KEY_5",
            "screenshot-ui",
            "截图界面 (Cmd+Shift+5，同上)",
        ),
    ] {
        v.push(Rule {
            apps: vec![],
            keys: vec![keys.into()],
            cmd: true,
            act: Action::DeAccel {
                feature: feature.into(),
            },
            desc: Some(desc.into()),
        });
    }

    // ── Ctrl+↑ 调度中心 → GNOME 概览 ──
    //
    // 与 `Cmd+空格` 是同一个动作，只是触发键不同。
    v.push(Rule {
        apps: vec![],
        keys: vec!["C-KEY_UP".into()],
        // Ctrl+↑ 不含 Cmd——标成 true 会导致规则永不命中
        cmd: false,
        act: Action::DeAccel {
            feature: "overview".into(),
        },
        desc: Some("调度中心 (Ctrl+↑ → 概览)".into()),
    });

    // ── Shift+Cmd+Q 退出登录 ──
    //
    // GNOME 的 logout 绑在 media-keys 的 `logout`（本机 Ctrl+Alt+Delete），
    // 与锁屏同一 schema。同样读现有绑定发出，不改写用户设置。
    v.push(Rule {
        apps: vec![],
        keys: vec!["S-KEY_Q".into()],
        cmd: true,
        act: Action::DeAccel {
            feature: "logout".into(),
        },
        desc: Some("退出登录 (Shift+Cmd+Q，读 DE 现有绑定)".into()),
    });

    // ── 删除类：macOS 的 "Delete" 键 = 其他键盘的 Backspace(⌫) ──
    //
    // macOS 把退格键叫 Delete，把向前删除键（MacBook 上不存在）叫
    // Forward Delete。日常说的「Cmd+Delete 移到废纸篓」按的就是 ⌫。
    //
    // evdev 里 ⌫ 是 `KEY_BACKSPACE`、向前删除是 `KEY_DELETE`——两者
    // 不同键。本机（MacBook 键盘）只产生前者，外接键盘才有后者。
    // 故每个组合都写两条规则，否则在本机上完全失效
    //（曾因只写 KEY_DELETE 导致全部删除类规则形同虚设）。

/// 需要同时覆盖的两个删除键。
const DELETE_KEYS: &[&str] = &["KEY_BACKSPACE", "KEY_DELETE"];

for key in DELETE_KEYS {
    // Cmd+Delete → 移到废纸篓
    //
    // 发**裸 Delete**（不带 Ctrl）：GNOME 文件管理器把「移到回收站」
    // 绑在 Delete 键上；Ctrl+Delete 在 GTK 文本控件里是「删下一个词」，
    // 在文件管理器里也不是回收站。此前误加 Ctrl 导致完全不生效。
    //
    // 虚拟键盘能发出本机不存在的键码（MacBook 无 ⌦ 键），故可行。
    v.push(Rule {
        apps: vec![],
        keys: vec![(*key).to_string()],
        cmd: true,
        act: Action::Emit {
            mods: vec![],
            key: "KEY_DELETE".into(),
        },
        desc: Some(format!(
            "移到废纸篓 (Cmd+Delete → Delete，不带 Ctrl；key={key})"
        )),
    });

    // Option+Delete → 删除左边字词
    //
    // 用 Ctrl+Backspace 而非透传 Alt+Backspace：前者是 X11/GNOME 惯例，
    // 在编辑器、终端、地址栏里都一致生效。
    v.push(Rule {
        apps: vec![],
        keys: vec![format!("A-{key}")],
        cmd: false,
        act: Action::Emit {
            mods: vec!["C".into()],
            key: "KEY_BACKSPACE".into(),
        },
        desc: Some(format!(
            "删除左边字词 (Option+Delete → Ctrl+Backspace，key={key})"
        )),
    });

    // Shift+Cmd+Delete / Option+Shift+Cmd+Delete → 清倒废纸篓
    //
    // **限定在 Nautilus**：macOS 上这两个是 Finder 的菜单项
    // （桌面也归 Finder 管），并非系统级快捷键——在终端、浏览器、
    // 文本框里按没有意义。
    //
    // 全局绑定有两个实际风险：终端恰恰是删除操作最容易出事的场景，
    // 而写密码时误按会直接弹确认框。限定后误按在那些地方毫无反应。
    for (mods, desc) in [
        (vec!["S".to_string()], "清倒废纸篓 (Shift+Cmd+Delete)"),
        (
            vec!["A".to_string(), "S".to_string()],
            "清倒废纸篓 (Option+Shift+Cmd+Delete)",
        ),
    ] {
        v.push(Rule {
            apps: vec!["org.gnome.Nautilus".into()],
            keys: vec![format!("{}-{}", mods.join("-"), key)],
            cmd: true,
            act: Action::Exec {
                program: "gio".into(),
                args: vec!["trash".into(), "--empty".into(), "--force".into()],
                confirm: true,
                prompt: Some("确定要永久抹掉回收站中的项目吗？".into()),
                ok_label: Some("清倒回收站".into()),
            },
            desc: Some(format!("{desc}，key={key}")),
        });
    }
}

    // ── 会漏发 SIGINT 的终端：复制键改走 Ctrl+Insert ──
    // 必须放在终端特化**之前**——查找取首个命中者，若在其后，
    // 通用终端规则会先命中，本条永不生效。
    v.push(Rule {
        apps: SIGINT_LEAKY_TERMINALS.iter().map(|s| s.to_string()).collect(),
        keys: vec!["KEY_C".into()],
        cmd: true,
        act: Action::Emit {
            mods: vec!["C".into()],
            key: "KEY_INSERT".into(),
        },
        desc: Some(
            "改用 Ctrl+Insert 复制：ptyxis/konsole 在无选中文本时会把 \
             Ctrl+Shift+C 当作 SIGINT 漏给进程"
                .into(),
        ),
    });

    // ── GVIM：Ctrl 已被占用（Ctrl+V 块选择、Ctrl+C 退出）──
    // 终端里跑的 vim 其窗口属于终端，已被 TerminalEmulator 覆盖；
    // 此处针对独立窗口的 gvim。同样须在通用终端规则之前。
    for key in DEFAULT_TERMINAL_SHIFT_KEYS {
        v.push(Rule {
            apps: vec!["gvim".into()],
            keys: vec![(*key).to_string()],
            cmd: true,
            act: Action::TerminalCtrlShift,
            desc: Some("GVIM 特化：Ctrl 已被占用".into()),
        });
    }

    // ── 终端特化 ──
    // 命中条件是「终端自己绑定了该键」，故为条件动作。
    //
    // 放在最后：更具体的应用规则（上方）须先于它命中。
    //
    // 注意 `Cmd+Q` 不在此列：它是 Alt+F4，已被全局规则覆盖。若在此
    // 列出，Cmd+Q 会变成 Ctrl+Shift+Q——一个谁也不认的组合。
    //
    // 用 `keys = ["*"]` 通配而非逐键列举：终端的绑定是**运行时**从
    // GSettings 查得的，各终端不同（ptyxis 有 `new-window=Ctrl+Shift+N`，
    // 而 N 不在任何静态名单里）。逐键列举只能覆盖兜底名单，
    // 运行时发现的键将无处安放——这正是 commit 18c03a2 之前的缺陷。
    // 通配行承接全部，具体哪些键生效由动作在运行时判定。
    v.push(Rule {
        apps: vec!["TerminalEmulator".into()],
        keys: vec!["*".into()],
        cmd: true,
        act: Action::TerminalCtrlShift,
        desc: Some("终端特化：终端自己绑定了该键时才加 Shift".into()),
    });

    v
}

/// 「Ctrl 已被占用」的应用：终端之外的特例。
///
/// 终端无需列举——由 `src/desktop.rs` 按 freedesktop 规范的
/// `Categories=TerminalEmulator` 判定，装了新终端自动生效。
///
/// 曾在此硬编码 14 个终端，结果 ptyxis 漏判：名单写的是小写
/// `ptyxis`，实际 wm_class 是 `org.gnome.Ptyxis`。
const DEFAULT_CTRL_BUSY_APPS: &[&str] = &[
    // GVIM：Ctrl+C 退出、Ctrl+V 块选择、Ctrl+D 半选。
    // 注意终端里跑的 vim 其窗口属于终端，已被 TerminalEmulator 覆盖；
    // 此处针对独立窗口的 gvim。
    "gvim",
];

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
            rules: default_rules(),
            fn_mode: Some(FnMode::Fkeysfirst),
            desktop: DesktopConfig {
                spotlight: true,
                ..DesktopConfig::default()
            },
            ctrl_busy_apps: DEFAULT_CTRL_BUSY_APPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
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
/// `rules` / `remap` / `ctrl_busy_apps` 采用**追加**——用户条目在默认
/// 之后，而查找取**首个命中者**，故用户条目天然优先。
/// `fn_mode` 采用**替换**。
impl Config {
    fn merge(&mut self, other: Config) {
        self.remap.extend(other.remap);
        self.rules.extend(other.rules);
        self.ctrl_busy_apps.extend(other.ctrl_busy_apps);
        if other.fn_mode.is_some() {
            self.fn_mode = other.fn_mode;
        }
    }

    /// 人类可读的摘要，用于启动日志。
    pub fn summary(&self) -> String {
        format!(
            "{} 条 remap, {} 条规则, DE={:?}, {} 个 Ctrl 占用应用",
            self.remap.len(),
            self.rules.len(),
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
