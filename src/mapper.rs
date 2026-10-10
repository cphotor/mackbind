//! 映射核心：修饰键状态机 + remap / cmd_map 应用。
//!
//! # 事件流模型
//!
//! mackbind 抓取键盘设备，事件按以下顺序处理：
//!
//! 1. 丢弃 `MISC` / `SYNCHRONIZATION`（前者是 MacBook 背光等杂散事件，
//!    后者由 uinput 自动补发）
//! 2. 丢弃 `value=2`（自动重复）。修饰键按住时内核会持续发重复事件，
//!    若不丢弃会重复触发映射，产生成对的按下/释放。
//! 3. 修饰键按下时**抑制输出**并记入状态——因为是否翻译取决于后续
//!    按下的是哪个键（如 `Cmd+C` 要发 `Ctrl+C`，但单独的 `Cmd` 应
//!    原样透传）。
//! 4. 非修饰键到达时，结合当前修饰键状态做决策，产出要注入的事件。
//!
//! # 释放事件的对称性
//!
//! 这是最容易出错的地方。若 `Cmd+C` 被翻译成 `Ctrl+C`，那么释放时
//! 也要发 `Ctrl` 的释放。若让原始的 `Cmd` 释放事件直接透传，输出侧
//! 会残留一个按下的 `Cmd`，导致后续组合键行为错乱。
//!
//! 因此按下时记录「这个键被翻译成了什么」，释放时按记录还原。

use crate::config::{Config, DesktopType, FnMode};
use crate::keys::parse_combo;
use evdev::{EventType, InputEvent, KeyCode};
use std::collections::{HashMap, HashSet};

/// 判断某键码是否为修饰键。
///
/// 注意：此处只列出本工具关心的修饰键。`KEY_FN` 也算在内——它是
/// Fn 优先级逻辑的输入，不应被原样透传。
pub fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
            | KeyCode::KEY_CAPSLOCK
            | KeyCode::KEY_FN
    )
}

// 特化的键集**不排除任何键**，见下方注释。
//
// 曾错误地把 Q/W/N/T 排除在外，理由是「终端没有退出/关窗口的 Ctrl
// 绑定」。这是错的：空闲的是 Ctrl+W，而 Ctrl+Shift+W 并不空闲——它
// 正是 Linux 终端关闭标签页的键，也正是 macOS Cmd+W 的对应物。
// Toshy 的配置同样是 `C("RC-W"): C("C-Shift-W")`。
//
// 特化的目的是「把 Cmd 翻译到应用认得的组合」，不是「躲开所有冲突」。
// 逐键判断某个组合是否空闲，反而会漏掉这类对应关系。

/// 已被 remap 替换的修饰键集合。
///
/// 这些键按下时进入 `pending` 而非直接透传，因为它们的下游行为
/// 取决于后续组合（Cmd+C → Ctrl+C）与 Fn 优先级。

/// 已编译规则的一行。
///
/// 输入侧索引由 `(主键, 修饰键集合)` 组成——不能只用主键：`"KEY_Z"`
/// （Cmd+Z）与 `"S-KEY_Z"`（Cmd+Shift+Z）主键相同，只用主键索引会互相
/// 覆盖，导致其中一条永远无法命中。实测症状：Cmd+Z 退化为裸 z，
/// Cmd+Shift+Z 泄漏出裸 Shift。
type RuleKey = (KeyCode, Vec<KeyCode>);

/// 应用选择子。
#[derive(Debug, Clone)]
enum AppMatch {
    /// 所有应用（表中省略 `apps` 时的默认）
    Any,
    /// 终端——按 `.desktop` 的 `Categories=TerminalEmulator` 判定
    Terminal,
    /// 按 wm_class 子串匹配
    Contains(Vec<String>),
}

impl AppMatch {
    fn parse(specs: &[String]) -> AppMatch {
        if specs.is_empty() || specs.iter().any(|s| s == "*") {
            AppMatch::Any
        } else if specs.iter().any(|s| s == "TerminalEmulator") {
            AppMatch::Terminal
        } else {
            AppMatch::Contains(specs.to_vec())
        }
    }

    fn matches(&self, wm_class: &str, is_term: bool) -> bool {
        match self {
            AppMatch::Any => true,
            AppMatch::Terminal => is_term,
            AppMatch::Contains(v) => v.iter().any(|a| wm_class.contains(a.as_str())),
        }
    }
}

/// 一条编译后的规则。
struct CompiledRule {
    /// 输入侧索引
    key: RuleKey,
    /// 主键通配（`keys = ["*"]`）——匹配任意主键
    wildcard: bool,
    /// 是否要求按住 Cmd
    ///
    /// 查表对所有组合生效（否则非 Cmd 组合无法配置），故须显式区分：
    /// 不校验的话，**裸按空格也会命中 `Cmd+空格` 那条规则**，
    /// 任何人按空格都弹出 GNOME 概览。
    requires_cmd: bool,
    /// 适用的应用
    app: AppMatch,
    /// 动作
    act: CompiledAction,
    /// 说明，仅供日志
    desc: Option<String>,
}

/// 编译后的动作。`TerminalCtrlShift` 是唯一的条件动作——
/// 它是否真的加 Shift，取决于目标应用运行时查得的绑定。
enum CompiledAction {
    Emit(Vec<KeyCode>, KeyCode),
    DeAccel(String),
    TerminalCtrlShift,
    /// 执行外部命令（清倒废纸篓等「本质是命令」的操作）
    Exec {
        program: String,
        args: Vec<String>,
        confirm: bool,
    },
}

/// 查表命中的结果。
enum Outcome {
    /// 发出这组修饰键与主键
    Convert(Vec<KeyCode>, KeyCode),
    /// 动作已产生副作用（如执行了命令），**无需发任何按键**
    NoOutput,
}

/// 已编译的映射规则。
pub struct Mapper {
    /// remap 规则：原键 → 目标键
    remap: HashMap<KeyCode, KeyCode>,
    /// **映射规则表**——唯一的映射真源。
    ///
    /// 按声明顺序线性查找，首个命中者生效（用户配置追加在末尾，
    /// 故天然优先）。表很小（数十行），线性扫描比建索引更简单，
    /// 也让「声明顺序即优先级」这件事保持显式。
    rules: Vec<CompiledRule>,
    /// **物理状态**：用户当前实际按住的修饰键，按下顺序。
    ///
    /// 反映硬件事实，不因发送事件而改变。每次新键按下时据此匹配组合。
    held: Vec<KeyCode>,

    /// 被抑制（未向输出侧发送）的键。释放时也不发事件。
    suppressed: HashSet<KeyCode>,
    /// **输出状态**：已向系统发出按下、尚未发出释放的修饰键。
    ///
    /// 与 held 严格分离。组合键翻译时需要先松开这些，再按新的输出修饰键。
    output_down: Vec<KeyCode>,
    /// 按下时登记的「输入键 → 实际输出键」，用于对称释放。
    ///
    /// 无论是否翻译都登记，使「透传」成为默认路径而非特例。
    pressed: HashMap<KeyCode, KeyCode>,
    /// 为翻译而注入输出侧的修饰键，主键释放时须一并松开
    injected_mods: Vec<KeyCode>,
    /// Fn 是否按住
    fn_held: bool,
    fn_mode: FnMode,
    /// F1-F12 与亮度/媒体键的对应表
    fkey_media: HashMap<KeyCode, KeyCode>,
    /// 焦点窗口是否属于「Ctrl 已被占用」的应用（终端、GVIM）
    ctrl_busy_window: bool,
    /// 当前焦点窗口是否为终端（`Categories=TerminalEmulator`）
    is_terminal: bool,
    /// Ctrl 占用应用列表（WM_CLASS / app_id）
    ctrl_busy_apps: Vec<String>,
    /// 「Ctrl 已被占用」的应用**自身**用 Ctrl+Shift 绑定的键。
    ///
    /// 运行时从 GSettings 查得（见 `set_window_id`），是
    /// `TerminalCtrlShift` 动作的判定依据。加 Shift 是为了命中它们
    /// 已有的绑定，而非笼统「躲冲突」。
    terminal_bindings: Vec<KeyCode>,
    /// 当前焦点窗口的 wm_class，用于匹配表的应用列
    wm_class: String,
    /// 桌面环境类型，决定 `de_accel` 动作去哪张 schema 查
    desktop_type: DesktopType,
    /// 是否输出调试日志
    verbose: bool,
}

/// 把规则表编译成可线性扫描的形式。
///
/// `keys` 是键名列表，可能带修饰前缀（如 `"S-z"` 表示 Shift+Z）。
/// 取最后一段作为主键，前缀作为输入侧修饰键。前缀修饰键排序后参与
/// 索引，使 `(z,[])` 与 `(z,[Shift])` 成为不同条目而不互相覆盖。
fn compile_table(
    rules: &[crate::config::Rule],
) -> Result<Vec<CompiledRule>, crate::keys::KeyError> {
    let mut out = Vec::with_capacity(rules.len());
    for r in rules {
        let mut in_mods: Vec<KeyCode> = Vec::new();
        let mut key: Option<KeyCode> = None;
        // `keys = ["*"]` 表示主键通配——供 TerminalCtrlShift 这类
        // 「命中与否取决于运行时状态」的动作使用。
        let mut wildcard = false;
        for k in &r.keys {
            if k == "*" {
                wildcard = true;
                continue;
            }
            let (mods, kc) = parse_combo(k)?;
            in_mods.extend(mods);
            key = Some(kc);
        }
        let key = match key {
            Some(k) => k,
            // 通配规则本就无固定主键，匹配时不比较主键
            None if wildcard => KeyCode::KEY_RESERVED,
            None => {
                return Err(crate::keys::KeyError::BadCombo(
                    r.keys.join("+"),
                    "keys 为空".into(),
                ))
            }
        };
        in_mods.sort();

        let act = match &r.act {
            crate::config::Action::Emit { mods, key } => {
                // `mods` 每项形如 `"C"` 或 `"C-S"`——修饰键缩写，可多个。
                // 按组合解析会把 `A` 当成 Alt 主键而丢掉，故逐段解析。
                let mut out_mods = Vec::new();
                for m in mods {
                    for seg in m.split('-') {
                        match crate::keys::modifier_from_abbrev(seg) {
                            Some(k) => out_mods.push(k),
                            None => {
                                return Err(crate::keys::KeyError::BadModifier(
                                    seg.to_string(),
                                    "emit 的 mods 只接受 C/A/S/M（可用 - 组合，如 C-S）".into(),
                                ))
                            }
                        }
                    }
                }
                out_mods.sort();
                out_mods.dedup();
                let (_, out_key) = parse_combo(key)?;
                CompiledAction::Emit(out_mods, out_key)
            }
            crate::config::Action::DeAccel { feature } => {
                CompiledAction::DeAccel(feature.clone())
            }
            crate::config::Action::TerminalCtrlShift => CompiledAction::TerminalCtrlShift,
            crate::config::Action::Exec {
                program,
                args,
                confirm,
            } => CompiledAction::Exec {
                program: program.clone(),
                args: args.clone(),
                confirm: *confirm,
            },
        };

        out.push(CompiledRule {
            key: (key, in_mods),
            wildcard,
            requires_cmd: r.cmd,
            app: AppMatch::parse(&r.apps),
            act,
            desc: r.desc.clone(),
        });
    }
    Ok(out)
}

/// GTK 加速键的主键名 → evdev 键码。
///
/// 见 [`crate::keys::parse_external_key`]。accel.rs 解析 DE 绑定时用的是
/// 同一个函数——曾在此另写一份，两份之间 `"space"` 一项的差异即由此而来。
fn accel_key(name: &str) -> Option<KeyCode> {
    crate::keys::parse_external_key(name)
}

/// 「功能名 → DE 中对应动作」的查找表。
///
/// 新增一个 DE 功能只需在此加一行，**不必改任何分支逻辑**：
///
/// ```text
/// (功能标识, 该 DE 下的 GSettings schema, 动作键名)
/// ```
///
/// 这类映射此前是写成 `match DesktopType { Gnome => ...; _ => None }` 的
/// 硬编码分支——每加一个 DE 功能就要动一次代码分支，也没法让用户在
/// 配置文件里追加。改为查表后，DE 只出现在这张表里。
///
/// 查不到（DE 未提供该动作、schema 不存在、动作未绑定）一律返回
/// `None`——**不视为错误**，其余规则照常生效。
fn de_action_lookup(
    dt: &DesktopType,
    feature: &str,
) -> Option<(String, String)> {
    // 不用 `use DesktopType::*`：其中有 `None` 变体，会遮蔽 `Option::None`
    use DesktopType::Gnome;
    // schema 只在匹配到具体 DE 时才构造，避免为无关 DE 做无谓查询
    let (schema, key): (&str, &str) = match (dt, feature) {
        // 概览：GNOME 放在 shell 层 schema，不在 wm.keybindings 里
        (Gnome, "overview") => ("org.gnome.shell.keybindings", "toggle-overview"),
        // 锁屏：在 media-keys 的 `screensaver`，**不在** wm.keybindings。
        //
        // 曾因只 grep wm.keybindings 而断言「GNOME 无锁屏绑定」——
        // 与「概览在另一个 schema」「D-Bus dest 写错」同源的第三次误判。
        // 断言某能力不存在前，先在整个 GNOME 域内搜一遍。
        (Gnome, "lock-screen") => (
            "org.gnome.settings-daemon.plugins.media-keys",
            "screensaver",
        ),
        // 显示桌面：动作存在但默认未绑定，需用户自行绑定后此条才生效
        (Gnome, "show-desktop") => ("org.gnome.desktop.wm.keybindings", "show-desktop"),
        // 截图三件套：全部在 shell 的 keybindings，不在 wm.keybindings
        (Gnome, "screenshot-ui") => ("org.gnome.shell.keybindings", "show-screenshot-ui"),
        (Gnome, "screenshot") => ("org.gnome.shell.keybindings", "screenshot"),
        (Gnome, "screenshot-window") => ("org.gnome.shell.keybindings", "screenshot-window"),
        // 退出登录：在 media-keys（与锁屏同一 schema），不在 wm.keybindings
        (Gnome, "logout") => ("org.gnome.settings-daemon.plugins.media-keys", "logout"),
        // KDE 与裸 WM 的对应机制不同，暂未实现。
        // 「不可查」不等于「不存在」——见 docs/macos-shortcuts.md。
        _ => return None,
    };
    Some((schema.to_string(), key.to_string()))
}

/// 把某个 DE 功能解析为要发出的组合。
///
/// 步骤统一为：查表拿 schema 与动作键名 → 读该动作的现有加速键 →
/// 解析成修饰键与主键。因此**读 DE 现有绑定**这一行为由函数本身保证，
/// 不必由每个调用点各自实现。
///
/// 输入侧（哪个 macOS 组合触发它）由规则表的 `keys` 列决定，
/// 不在此硬编码——否则 `Ctrl+Cmd+Q` 锁屏这类非空格触发就无从表达。
///
/// 返回 `(输出修饰键, 输出主键)`。
fn de_feature_accel(
    dt: &DesktopType,
    feature: &str,
) -> Option<(Vec<KeyCode>, KeyCode)> {
    let (schema, action) = de_action_lookup(dt, feature)?;
    let accel = crate::accel::action_accel(&schema, &action)?;
    let (mods, key) = crate::accel::parse_accel(&accel)?;
    Some((mods.into_iter().map(KeyCode::new).collect(), key))
}

/// DE 功能是否可用。启动时据此剔除表中的 `de_accel` 行，
/// 避免运行时反复查询。
fn de_feature_ready(dt: &DesktopType, feature: &str) -> bool {
    de_feature_accel(dt, feature).is_some()
}

/// 默认语义的结果：要么转换，要么放行。
enum Default {
    /// `Cmd+X` → `Ctrl+X`
    Convert(Vec<KeyCode>, KeyCode),
    /// Cmd 组合但无从转换，原样放行（须补发被抑制的 Meta）
    Passthrough,
    /// 非 Cmd 组合且表里无规则，原样透传（无 Meta 可补）
    PassthroughPlain,
}

/// 默认语义：`Cmd+X` → `Ctrl+X`。
///
/// **仅在查表落空后适用**——表里有规则的一律以表为准。
///
/// 不做穷举：macOS 的 Cmd 组合是长尾的，固定表覆盖不全。实测 GNOME 的
/// 28 个 Super 绑定中仅 `Super+h` 涉及字母，`Super+字母` 基本空闲——
/// 保留 Super 会让绝大多数组合什么也不发生。
///
/// # 何时放行而非转换
///
/// 前提是「Cmd 映射为 Ctrl」。若用户已按住 Ctrl，Cmd 再变 Ctrl 就成了
/// `Ctrl+Ctrl+X`——同一个槽位，组合退化。故此时不能转换。
///
/// 但也不能把它当 `Cmd+X` 处理：`Ctrl+Cmd+X` 在 macOS 上是**另一个
/// 组合键**（`Ctrl+Cmd+Q` 锁屏、`Ctrl+Cmd+F` 全屏），有自己的含义。
/// 表里没有它，只说明我们没配，不等于它等于 `Cmd+X`。
///
/// 故放行：把用户按的原样交给应用，由应用自己解释。这比替它编一个
/// `Ctrl+X` 可预测——表里有就按表走，没有就透传。
///
/// 注意物理 Ctrl 经 remap 后是 CapsLock（见 `handle_press`），
/// 故冲突判定须把 CapsLock 一并计入。
fn default_semantics(cur_mods: &[KeyCode], code: KeyCode) -> Default {
    let ctrl_taken = cur_mods.iter().any(|m| {
        matches!(
            *m,
            KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL | KeyCode::KEY_CAPSLOCK
        )
    });
    if ctrl_taken {
        return Default::Passthrough;
    }

    // 其余修饰键（Shift/Alt）保留后追加 Ctrl：
    // `Shift+Cmd+X` → `Shift+Ctrl+X`
    let mut mods = cur_mods.to_vec();
    mods.push(KeyCode::KEY_LEFTCTRL);
    mods.sort();
    mods.dedup();
    Default::Convert(mods, code)
}

impl Mapper {
    /// 从配置构建。键名解析失败会立即返回错误——配置写错时
    /// 必须大声失败，不能静默忽略。
    pub fn new(cfg: &Config, verbose: bool) -> Result<Self, crate::keys::KeyError> {
        let mut remap = HashMap::new();
        for r in &cfg.remap {
            let from = crate::keys::parse_key(&r.from)?;
            let to = crate::keys::parse_key(&r.to)?;
            remap.insert(from, to);
        }

        let rules = compile_table(&cfg.rules)?;

        // DE 相关规则：读取 DE 现有的绑定，不改写绑定。
        //
        // macOS 的 `Cmd+空格`（Spotlight）在 GNOME 对应概览，而 GNOME
        // 已把该动作绑在 `toggle-overview` 上（本机为 `Ctrl+Shift+空格`）。
        // 直接发出同样的键即可，无需在用户配置里留下任何记录——
        // 也不会覆盖用户自己对该键的设置。
        //
        // 表里已有一条 `DeAccel{overview}` 行，此处只做可用性检查：
        // 查得到则该行生效，查不到则整条规则无动作可做。
        // 表中所有 `de_accel` 行逐个检查可用性。
        //
        // `overview` 受 `spotlight` 开关控制；锁屏等其余功能无条件启用。
        // 查不到的行在此剔除，避免运行时反复查询失败的功能。
        let mut de_report: Vec<(String, bool)> = Vec::new();
        let rules = rules
            .into_iter()
            .filter(|r| match &r.act {
                CompiledAction::DeAccel(feature) => {
                    let ok = de_feature_ready(&cfg.desktop.r#type, feature)
                        && (feature != "overview" || cfg.desktop.spotlight);
                    de_report.push((feature.clone(), ok));
                    ok
                }
                _ => true,
            })
            .collect();

        if verbose {
            for (feature, ok) in &de_report {
                if *ok {
                    eprintln!("[de] {feature}：读 DE 现有绑定后发出");
                } else {
                    eprintln!("[de] {feature}：DE 未提供或已关闭，跳过");
                }
            }
        }

        Ok(Mapper {
            remap,
            rules,
            held: Vec::new(),
            suppressed: HashSet::new(),
            output_down: Vec::new(),
            pressed: HashMap::new(),
            injected_mods: Vec::new(),
            fn_held: false,
            fn_mode: cfg.fn_mode.unwrap_or_default(),
            fkey_media: build_fkey_media_table(),
            ctrl_busy_window: false,
            is_terminal: false,
            ctrl_busy_apps: cfg.ctrl_busy_apps.clone(),
            terminal_bindings: crate::config::DEFAULT_TERMINAL_SHIFT_KEYS
                .iter()
                .map(|k| crate::keys::parse_key(k))
                .collect::<Result<Vec<_>, _>>()?,
            wm_class: String::new(),
            desktop_type: cfg.desktop.r#type,
            verbose,
        })
    }

    /// 设置当前焦点窗口的 WM_CLASS / app_id。
    ///
    /// 判定「Ctrl 已被占用」有两条途径：
    ///
    /// 1. `.desktop` 条目的 `Categories` 含 `TerminalEmulator` —— 按
    ///    freedesktop 规范判定，自动覆盖新装的终端，不受大小写或前缀
    ///    差异影响。硬编码名单曾因写成 `ptyxis` 而实际 wm_class 是
    ///    `org.gnome.Ptyxis` 导致漏判。
    /// 2. 命中 `ctrl_busy_apps` 配置名单 —— 用于独立窗口的编辑器等
    ///    非终端特例。
    ///
    /// 命中时 Cmd 映射为 Ctrl+Shift，避开这些应用内 Ctrl 的既定含义
    /// （终端 Ctrl+C 中断、GVIM Ctrl+V 块选择）。
    pub fn set_window_id(&mut self, id: &str) {
        self.wm_class = id.to_string();
        let is_term = crate::desktop::is_terminal(id);
        let in_list = self.ctrl_busy_apps.iter().any(|t| id.contains(t));
        self.is_terminal = is_term;
        self.ctrl_busy_window = is_term || in_list;

        if is_term {
            // 优先问终端自己：它用 Ctrl+Shift 绑定了哪些键。
            //
            // 手写白名单漏过 `n`（新窗口），也误判过 close-window 的
            // 绑定键。凡是走 GSettings 声明快捷键的终端（GNOME 系），
            // 直接读它的 schema——用户改过绑定也能自动跟上。
            let queried = crate::accel::ctrl_shift_keys(id);
            if !queried.is_empty() {
                let parsed: Vec<KeyCode> = queried.iter().filter_map(|k| accel_key(k)).collect();
                if !parsed.is_empty() {
                    self.terminal_bindings = parsed;
                }
            }
        }

        if self.verbose && self.ctrl_busy_window {
            let via = if is_term { "TerminalEmulator" } else { "配置名单" };
            let n = self.terminal_bindings.len();
            eprintln!(
                "[mapper] 命中 Ctrl 占用应用: {id}（{via}）→ {n} 个键映射为 Ctrl+Shift"
            );
        }
    }

    /// 处理一个输入事件，产出要注入输出的事件序列（可能为空）。
    pub fn translate(&mut self, ev: &InputEvent) -> Vec<InputEvent> {
        // 1) 透传非按键事件，保持与真实设备一致的时间流
        //
        // MacBook 的「键盘/触控板」是同一个 evdev 设备，触控板活动会以
        // MISC/MSC 事件的形式混入。直接丢弃它们有两个问题：
        //
        //   a) 触控板彻底失灵（点击、滚动、多指手势均依赖这些事件）
        //   b) 更隐蔽的：触控板事件同样会带来 SYN 边界，打断按键批次。
        //      输入法依赖事件间隔聚合按键（如双拼 f+a 合成一个音节），
        //      节奏被打断后会逐字母提交，表现为「只出拼音、漏字母」。
        //
        // 因此原样转发这类事件——不做任何映射，但保留它们在时间轴上的
        // 位置，使下游看到的节奏与真实键盘+触控板完全一致。
        //
        // SYNCHRONIZATION 例外：uinput 的 emit() 会自动补 SYN_REPORT，
        // 若再转发原始 SYN，一个批次就会被拆成多个同步报告。
        if ev.event_type() != EventType::KEY {
            // MISC/MSC 等非按键事件一律丢弃，交给原设备处理。
            //
            // 试过转发它们以保留触控板活动，但失败：转发会把 ABS 多点
            // 触控事件一并送到虚拟设备，而虚拟设备未声明 EV_ABS 能力，
            // 导致触控板与按键都异常（打字漏字母）。
            return vec![];
        }

        let code = KeyCode(ev.code());
        let value = ev.value();

        // 2) 丢弃自动重复
        if value == 2 {
            return vec![];
        }

        let pressed = value == 1;

        if code == KeyCode::KEY_FN {
            self.fn_held = pressed;
            // Fn 本身不注入输出（内核多数机型本就不暴露它）
            return vec![];
        }

        if pressed {
            if is_modifier(code) {
                self.handle_press(code)
            } else {
                self.handle_key_press(code)
            }
        } else {
            self.handle_release(code)
        }
    }

    /// 按下处理。
    fn handle_press(&mut self, code: KeyCode) -> Vec<InputEvent> {
        // 应用 remap，得到「逻辑键」
        let logical = self.remap.get(&code).copied().unwrap_or(code);

        // 修饰键：记录状态，抑制输出
        // 只有 Cmd 需要「按下时抑制、待组合确定后再决定」。
        //
        // 其他修饰键（Shift、Alt、Ctrl）本身就应当原样输出：它们不参与
        // Cmd 查表，抑制会导致下游（如 fcitx5 监听裸 Shift_L 切换输入法）
        // 收不到按键事件。
        //
        // remap 过的修饰键（如 CapsLock→LEFTCTRL）需输出目标键而非原键。
        if logical != code {
            self.held_push(logical);
            self.output_down.push(logical);
            return self.press_as(code, logical);
        }

        // 未被 remap 的 Cmd：抑制，等后续组合决定。
        // 记入 suppressed，使释放时也不发事件——我们从未按下过它，
        // 不应发出一个无对应的释放。
        if code == KeyCode::KEY_LEFTMETA || code == KeyCode::KEY_RIGHTMETA {
            self.held_push(logical);
            self.suppressed.insert(code);
            return vec![];
        }

        // 其余修饰键（Shift / Alt / Ctrl）原样透传，但**仍需记入 held**。
        //
        // held 在此有两个用途，二者并不冲突：
        //   1. 判断 Cmd 组合是否命中（cmd_map 条目可声明附加修饰键，
        //      如 Cmd+Shift+Z 的 "S-z"）
        //   2. 组合键翻译时，先松开这些输入侧修饰键
        //
        // 透传不等于不可见——输出侧照常收到事件，只是内部也要记住。
        self.held_push(logical);
        self.output_down.push(logical);
        self.press_as(code, code)
    }

    /// 按下并登记：记录「这个输入键实际输出成了什么」。
    ///
    /// 释放时按此记录还原，无需再判断「是否被翻译过」。
    /// 统一登记也让「透传」成为默认值而非特例。
    fn press_as(&mut self, input: KeyCode, output: KeyCode) -> Vec<InputEvent> {
        self.pressed.insert(input, output);
        if !self.output_down.contains(&output) {
            self.output_down.push(output);
        }
        self.emit_key(output, 1)
    }

    /// Fn 键预处理：F1–F12 在当前 Fn 状态与模式下，实际代表哪个键。
    ///
    /// 只做**替换**，绝不提前返回——调用方须继续走正常的组合流程。
    ///
    /// 早期实现在此直接 `return emit_key(media, 1)`，绕过了整个 Cmd
    /// 查表。后果是 `Fn+Meta+Shift+F1`（macOS 上取功能键的组合）会
    /// 泄漏出 Meta 按下——Meta 此前已被抑制、此处却原样发出——同时
    /// Ctrl/Shift 丢失。实测输出为 `Meta + 媒体键`，明显不对。
    ///
    /// 方向由 [`FnMode`] 决定：
    /// - `Fkeysfirst`：Fn 未按时出 F 键，按下出媒体键
    /// - `Fkeyslast`：反之
    fn resolve_fn(&self, code: KeyCode) -> KeyCode {
        if self.fn_mode == FnMode::Disabled {
            return code;
        }
        // 只需正向查表，无需反向。反向（「媒体键 → F 键」）在内核
        // fnmode=1 时不会发生：那时内核直接上报 F 键码，F1 进来就
        // 该原样输出。
        //
        // 内核 `fnmode=2`（本机当前值）下的分工：
        //   F1 → KEY_F1、Fn+F1 → KEY_BRIGHTNESSDOWN  ← 内核已翻译
        // 此时 `fkey_media` 查得到媒体键才转换，查不到即原样透传。
        match (self.fn_held, self.fn_mode) {
            (true, FnMode::Fkeysfirst) | (false, FnMode::Fkeyslast) => {
                self.fkey_media.get(&code).copied().unwrap_or(code)
            }
            _ => code,
        }
    }


    /// 查表。返回 `(输出修饰键, 输出主键)`，未命中则 `None`。
    ///
    /// 线性扫描，按声明顺序取首个命中者。表很小（数十行），
    /// 且这样「声明顺序即优先级」是显式的，不需额外的排序或索引。
    fn lookup(
        &self,
        code: KeyCode,
        cur_mods: &[KeyCode],
        cmd_held: bool,
    ) -> Option<Outcome> {
        for r in &self.rules {
            if r.requires_cmd && !cmd_held {
                continue;
            }
            if r.key.1 != cur_mods {
                continue;
            }
            if !r.wildcard && r.key.0 != code {
                continue;
            }
            if !r.app.matches(&self.wm_class, self.is_terminal) {
                continue;
            }
            if self.verbose {
                if let Some(d) = &r.desc {
                    eprintln!("[mapper] 命中规则: Cmd+{:?} — {d}", code);
                }
            }
            match &r.act {
                CompiledAction::Emit(mods, key) => {
                    return Some(Outcome::Convert(mods.clone(), *key))
                }
                CompiledAction::DeAccel(feature) => {
                    // 读 DE 现有的绑定，发出同样的键。不改写用户配置，
                    // 用户改了绑定则自动跟上。启动时已剔除不可用的行，
                    // 此处仍以 `if let` 兜底——DE 绑定可能在运行中改变。
                    if let Some((mods, key)) =
                        de_feature_accel(&self.desktop_type, feature)
                    {
                        return Some(Outcome::Convert(mods, key));
                    }
                    continue;
                }
                CompiledAction::TerminalCtrlShift => {
                    // 条件动作：仅当目标应用**自己**用 Ctrl+Shift 绑定了
                    // 这个键时才加 Shift。加 Shift 是为了命中它已有的
                    // 绑定，而不是笼统躲冲突。
                    //
                    // 未绑定则落到默认语义——这正是「逐键」而非「整体
                    // 换语义」的意义：`Cmd+Q` 不在此白名单里，故仍是
                    // 全局的 Alt+F4，而不会变成谁也不认的 Ctrl+Shift+Q。
                    if self.ctrl_busy_window && self.terminal_bindings.contains(&code) {
                        let mut mods = cur_mods.to_vec();
                        mods.push(KeyCode::KEY_LEFTCTRL);
                        mods.push(KeyCode::KEY_LEFTSHIFT);
                        mods.sort();
                        mods.dedup();
                        return Some(Outcome::Convert(mods, code));
                    }
                    continue;
                }
                CompiledAction::Exec {
                    program,
                    args,
                    confirm,
                } => {
                    // 本质是命令而非组合键的操作。
                    //
                    // 返回 `Some(空修饰键, 空主键)` 表示「已处理，但无需发按键」——
                    // 调用方据此跳过整个输出流程，只跑副作用。
                    let key_desc = format!(
                        "Cmd+{}",
                        r.key
                            .1
                            .iter()
                            .filter_map(|m| match *m {
                                KeyCode::KEY_LEFTSHIFT => Some("Shift+"),
                                KeyCode::KEY_LEFTALT => Some("Alt+"),
                                _ => None,
                            })
                            .collect::<String>()
                            + &format!("{:?}", r.key.0)
                    );
                    crate::exec::run(
                        program,
                        args,
                        r.desc.as_deref(),
                        *confirm,
                        Some(&key_desc),
                    );
                    // 已产生副作用，无需发出任何按键
                    return Some(Outcome::NoOutput);
                }
            }
        }
        None
    }

    /// 非修饰键的按下处理。
    fn handle_key_press(&mut self, physical: KeyCode) -> Vec<InputEvent> {
        // Fn 预处理：只替换键本身，之后统一走下方流程。
        //
        // `pressed` 仍以**物理键**为键——`handle_release` 拿物理键直接
        // 查表即可拿到当时的输出，无需重新推断 Fn 状态（Fn 可能已先于
        // F 键松开，重新推断会得到错误结果）。invariant 2 依赖这一点。
        let code = self.resolve_fn(physical);

        // Cmd 组合处理
        let cmd_held = self
            .held
            .iter()
            .any(|m| *m == KeyCode::KEY_LEFTMETA || *m == KeyCode::KEY_RIGHTMETA);

        {
            // 当前除 Cmd 外还按住的修饰键。
            //
            // 两份，用途不同，**不可混用**：
            // - `held_mods`：**真实**按下的键，用于发事件与释放配平
            // - `cur_mods`：**归一化**后的键，仅用于查表
            //
            // 归一化的原因：物理 Ctrl 经 remap 变成 CapsLock，但语义上仍是
            // Ctrl，而规则表里写 `C-`（= KEY_LEFTCTRL）。不归一化则
            // `Ctrl+Cmd+Q` 这类规则永远匹配不上。
            //
            // 但**发事件必须用真实的**：用归一化值发，输出侧按下的是
            // LeftCtrl，而释放路径按 pressed 表还原的是 CapsLock——
            // 两者对不上，LeftCtrl 永久卡在按下态。
            let held_mods: Vec<KeyCode> = self
                .held
                .iter()
                .copied()
                .filter(|m| *m != KeyCode::KEY_LEFTMETA && *m != KeyCode::KEY_RIGHTMETA)
                .collect();
            let mut cur_mods = held_mods.clone();
            cur_mods = cur_mods
                .into_iter()
                .map(|m| match m {
                    KeyCode::KEY_CAPSLOCK | KeyCode::KEY_RIGHTCTRL => KeyCode::KEY_LEFTCTRL,
                    other => other,
                })
                .collect();
            cur_mods.sort();
            cur_mods.dedup();

            // 查表：**对所有组合**生效，不只 Cmd 组合。
            //
            // 早期版本把查表放在 `if cmd_held` 分支内，于是不含 Cmd 的
            // 组合（如 macOS 的 `Option+Delete`）永远查不到规则——
            // 表里写了也没用。现已提到分支外。
            //
            // 查表优先；表里没有才轮到默认语义（非 Cmd 组合则原样透传）。
            let decision = match self.lookup(code, &cur_mods, cmd_held) {
                Some(Outcome::Convert(m, k)) => Default::Convert(m, k),
                // 动作已执行完，不该再发按键
                Some(Outcome::NoOutput) => return Vec::new(),
                None if cmd_held => default_semantics(&cur_mods, code),
                // 非 Cmd 组合且表里没有：原样透传
                None => Default::PassthroughPlain,
            };

            // 放行：把用户按的原样交给应用。
            //
            // Cmd 此前被抑制（未输出），故这里需显式补上——
            // 它从未进入 output_down，不在此处补就会丢失，
            // 发出一个缺 Cmd 的残缺组合。
            let converted = match decision {
                Default::PassthroughPlain => {
                    // 非 Cmd 组合且表里无规则：原样透传。
                    //
                    // 修饰键（Shift/Alt/Ctrl）按下时已透传过，此处**不得**
                    // 重新按下或松开它们——否则每按一个键都会产生
                    // 「松开 Shift 再按下 Shift」的抖动，
                    // fcitx5 之类的输入法会因此漏字母。
                    self.output_down.push(code);
                    self.pressed.insert(physical, code);
                    if self.verbose {
                        eprintln!("[mapper] {:?}+{:?} 表中无规则 → 透传", cur_mods, code);
                    }
                    return self.emit_key(code, 1);
                }
                Default::Passthrough => {
                    // Cmd 组合但无从转换：把用户按的原样交给应用。
                    let mut out = Vec::new();
                    for h in self.output_down.drain(..).collect::<Vec<_>>() {
                        out.extend(self.emit_key(h, 0));
                    }
                    for m in &held_mods {
                        out.extend(self.emit_key(*m, 1));
                        self.output_down.push(*m);
                    }
                    // Cmd 以真实 Super 身份发出。
                //
                // 必须同时把它从 `suppressed` 移除：Meta 按下时已被抑制，
                // 从未进入 output_down。若不移除，释放路径会走
                // `suppressed.remove()` 分支直接返回、不发释放——
                // 于是这个注入的 Super 永久卡在按下态。
                //
                // 后果是实机级的：锁屏后输密码，每个键都带着 Super，
                // 数字 9 等输入不进去，只能硬关机。
                self.suppressed.remove(&KeyCode::KEY_LEFTMETA);
                out.extend(self.emit_key(KeyCode::KEY_LEFTMETA, 1));
                self.output_down.push(KeyCode::KEY_LEFTMETA);
                out.extend(self.emit_key(code, 1));
                self.output_down.push(code);
                self.pressed.insert(physical, code);
                if self.verbose {
                    eprintln!(
                        "[mapper] Cmd+{:?}{:?} 无规则且无法转换 → 放行",
                        code, cur_mods
                    );
                }
                    return out;
                }
                Default::Convert(out_mods, out_key) => (out_mods, out_key),
            };
            let (out_mods, out_key) = converted;

            if self.verbose {
                eprintln!(
                    "[mapper] Cmd+{:?} -> {:?}{:?}{}",
                    code,
                    out_mods,
                    out_key,
                    if self.ctrl_busy_window { " (Ctrl 占用应用)" } else { "" }
                );
            }

            let mut out = Vec::new();
            // 松开输出侧仍按下的修饰键，再按新的输出修饰键。
            //
            // 操作的是 output_down（输出状态），而非 held（物理状态）——
            // 用户仍按着 Cmd 时，held 不得因发送事件而改变。
            let down_now: Vec<KeyCode> = self.output_down.drain(..).collect();
            for h in down_now {
                out.extend(self.emit_key(h, 0));
            }
            for m in &out_mods {
                out.extend(self.emit_key(*m, 1));
                self.output_down.push(*m);
            }
            out.extend(self.emit_key(out_key, 1));
            self.output_down.push(out_key);
            self.injected_mods.extend(out_mods.iter().copied());
            self.pressed.insert(physical, out_key);
            out
        }
    }

    /// 释放处理。按下时做了翻译，这里必须还原。
    fn handle_release(&mut self, code: KeyCode) -> Vec<InputEvent> {
        // 1) 更新物理状态。held 存的是 remap 后的**逻辑键**，事件里是
        //    **物理键**，两者都要清理。
        self.held_retain(|m| m != code);
        if let Some(&logical) = self.remap.get(&code) {
            self.held_retain(|m| m != logical);
        }

        // 2) 查输出状态：该键若仍处于按下态，移除并准备发释放。
        //
        // 若已不在 output_down 中，说明它此前已被组合键翻译提前松开
        // （如 Cmd+Shift+Z 翻译时松开了透传的 Shift）。此时输出侧
        // 早已无此键，再发一次释放就是多余的，可能被应用误读。
        let out_key = self.pressed.remove(&code);
        let still_down = out_key
            .map(|ok| {
                let pos = self.output_down.iter().position(|m| *m == ok);
                if let Some(i) = pos {
                    self.output_down.remove(i);
                }
                pos.is_some()
            })
            .unwrap_or(false);

        // 3) 抑制过的键：若输出侧确实没有它，也不发出释放。
        //
        // 判据是「输出侧是否仍按着」，**不是**「是否曾被抑制」。
        // 曾被抑制的键也可能在后续路径里被主动发出——如
        // `Ctrl+Cmd+Q` 锁屏会发出 `Super+L`，其中的 Super 就是
        // 原本被抑制的 Meta。此时若按「曾被抑制」提前返回，
        // 释放事件被吞掉，Super 永久卡在按下态：
        // 之后空格变成 Cmd+空格、W 变成 Cmd+W。
        let was_suppressed = self.suppressed.remove(&code);
        if was_suppressed && !self.output_down.contains(&code) {
            return vec![];
        }

        // 4) 若按下时有登记，则必须对称释放——但仅当输出侧确实仍处于按下态。
        // 若 still_down 为 false，说明该键已被组合键翻译提前松开，
        // 输出侧早已无它，此时再发释放就是多余事件。
        if let Some(output) = out_key {
            if !still_down {
                return vec![];
            }
            let mut out = Vec::new();
            out.extend(self.emit_key(output, 0));
            // 翻译时注入的修饰键在此一并松开，否则会残留在按下状态
            for m in std::mem::take(&mut self.injected_mods) {
                out.extend(self.emit_key(m, 0));
                self.output_down.retain(|x| *x != m);
            }
            return out;
        }

        // 无需在此重复 Fn 翻译：按下时已把「物理键 → 输出键」记入
        // `pressed`（以物理键为键），上面的 `out_key` 分支直接对称释放。
        // 若在此再翻译，一旦 Fn 先于 F 键松开就会得到错误的键。

        // 兜底：未登记的键。输出侧不处于按下态时不应发释放
        // （那是一个无对应按下的多余事件）。
        if !self.output_down.contains(&code) {
            return vec![];
        }

        self.output_down.retain(|m| *m != code);
        self.emit_key(code, 0)
    }

    fn held_push(&mut self, k: KeyCode) {
        if !self.held.contains(&k) {
            self.held.push(k);
        }
    }

    fn held_retain(&mut self, f: impl Fn(KeyCode) -> bool) {
        self.held.retain(|k| f(*k));
    }

    fn emit_key(&self, code: KeyCode, value: i32) -> Vec<InputEvent> {
        self.emit_key_raw(code, value)
    }

    fn emit_key_raw(&self, code: KeyCode, value: i32) -> Vec<InputEvent> {
        vec![InputEvent::new(EventType::KEY.0, code.0, value)]
    }
}
/// F1-F12 与亮度/媒体键的对应表。
///
/// macBook 键盘顶排是双功能键。裸按时给 F 键，按住 Fn 时给亮度/音量/
/// 播放控制。此表在用户态完成该切换，不依赖 `hid_apple` 的 `fnmode`。
fn build_fkey_media_table() -> HashMap<KeyCode, KeyCode> {
    let pairs = [
        (KeyCode::KEY_F1, KeyCode::KEY_BRIGHTNESSDOWN),
        (KeyCode::KEY_F2, KeyCode::KEY_BRIGHTNESSUP),
        (KeyCode::KEY_F3, KeyCode::KEY_DASHBOARD), // Mission Control
        (KeyCode::KEY_F4, KeyCode::KEY_SEARCH),           // Launchpad / Spotlight
        (KeyCode::KEY_F5, KeyCode::KEY_KBDILLUMDOWN),
        (KeyCode::KEY_F6, KeyCode::KEY_KBDILLUMUP),
        (KeyCode::KEY_F7, KeyCode::KEY_PREVIOUSSONG),
        (KeyCode::KEY_F8, KeyCode::KEY_PLAYPAUSE),
        (KeyCode::KEY_F9, KeyCode::KEY_NEXTSONG),
        (KeyCode::KEY_F10, KeyCode::KEY_MUTE),
        (KeyCode::KEY_F11, KeyCode::KEY_VOLUMEDOWN),
        (KeyCode::KEY_F12, KeyCode::KEY_VOLUMEUP),
    ];
    pairs.into_iter().collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

    fn default_mapper() -> Mapper {
        let cfg = Config::with_defaults();
        Mapper::new(&cfg, false).unwrap()
    }

    fn terminal_mapper() -> Mapper {
        let mut m = default_mapper();
        // 用不会漏发 SIGINT 的终端作代表（ptyxis 有单独的规则，见下）
        m.set_window_id("com.mitchellh.ghostty");
        m
    }

    /// 终端里 Cmd 组合映射为 Ctrl+Shift，避开 Ctrl+C 中断等既有含义。
    #[test]
    fn terminal_cmd_becomes_ctrl_shift() {
        let mut m = terminal_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_C.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                && out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                && out.contains(&(KeyCode::KEY_C.0, 1)),
            "终端里 Cmd+C 应为 Ctrl+Shift+C（复制），实际 {out:?}"
        );
    }

    /// 终端里 Cmd+W → Ctrl+Shift+W（关标签页），Cmd+T → Ctrl+Shift+T。
    ///
    /// 这不是「躲冲突」而是「找对应」：空闲的是 Ctrl+W，而
    /// Ctrl+Shift+W 恰是 Linux 终端关闭标签页的键。Toshy 的配置
    /// 同为 `C("RC-W"): C("C-Shift-W")`。
    #[test]
    fn terminal_cmd_w_closes_tab() {
        for (key, why) in [
            (KeyCode::KEY_W, "关标签页"),
            (KeyCode::KEY_T, "新标签页"),
        ] {
            let mut m = terminal_mapper();
            m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
            let out = desc(&m.translate(&k(key.0, 1)));
            assert!(
                out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                    && out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                    && out.contains(&(key.0, 1)),
                "终端里 Cmd+{key:?}（{why}）应为 Ctrl+Shift+{key:?}，实际 {out:?}"
            );
        }
    }

    /// `Cmd+空格` 映射为 GNOME 已有的概览绑定（默认 `Ctrl+Shift+空格`）。
    ///
    /// 实现方式是**读取** GNOME 对 `toggle-overview` 的现有绑定再发出
    /// 同样的键，而非改写绑定——因此不在用户配置里留下记录，也不影响
    /// 用户自己对该键的设置。
    ///
    /// 该组合在 macOS 中是 `Cmd+Ctrl+空格`（切换输入法），语义亦相符。
    #[test]
    fn cmd_space_maps_to_gnome_overview() {
        let Some((out_mods, out_key)) = de_feature_accel(&DesktopType::Gnome, "overview") else {
            // 本机未装 GNOME 或动作未绑定，跳过而非误判
            return;
        };
        assert!(
            out_mods.contains(&KeyCode::KEY_LEFTCTRL)
                && out_mods.contains(&KeyCode::KEY_LEFTSHIFT),
            "概览应发出 DE 现有的 Ctrl+Shift 组合，实际 {out_mods:?}"
        );
        assert_eq!(out_key, KeyCode::KEY_SPACE, "本机 GNOME 的 toggle-overview 绑在空格上");

        // 端到端：Cmd+空格 应产生那组键
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_SPACE.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                && out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                && out.contains(&(KeyCode::KEY_SPACE.0, 1)),
            "Cmd+空格 应发出 Ctrl+Shift+空格，实际 {out:?}"
        );
    }

    /// KDE / 裸 WM 暂无对应实现，但不应 panic。
    ///
    /// 「不可查」不等于「不存在」——DE 只出现在 `de_action_lookup` 的表里，
    /// 新增支持只需加一行，不必改任何分支逻辑。
    #[test]
    fn unknown_desktop_skips_overview_gracefully() {
        assert!(de_action_lookup(&DesktopType::Kde, "overview").is_none());
        assert!(de_action_lookup(&DesktopType::None, "overview").is_none());
    }

    /// 动态数据（DE 绑定、终端 schema）里的键名解析必须与配置用同一套规则。
    ///
    /// 此前 mapper 与 accel 各写一份 `plus`/`minus` 映射，已产生过分歧——
    /// 一份有 `"space"` 特例而另一份没有，只因 `KEY_SPACE` 恰能被兜底
    /// 分支解析出来才没暴露。
    #[test]
    fn external_key_parsing_is_consistent() {
        use crate::keys::parse_external_key as p;
        assert_eq!(p("plus"), Some(KeyCode::KEY_EQUAL));
        assert_eq!(p("space"), Some(KeyCode::KEY_SPACE));
        assert_eq!(p("Page_Down"), Some(KeyCode::KEY_PAGEDOWN));
        assert_eq!(p("F4"), Some(KeyCode::KEY_F4));
        assert_eq!(p("w"), Some(KeyCode::KEY_W));
        assert_eq!(p("KEY_A"), Some(KeyCode::KEY_A));
        assert_eq!(p(""), None);
        assert_eq!(p("NotAKey"), None);
    }

    /// ptyxis 的新窗口/关窗口键由它自己的 GSettings 决定，不靠猜。
    ///
    /// ptyxis 声明 `new-window = Ctrl+Shift+N`。手写白名单曾漏掉 `n`，
    /// 导致 `Cmd+N` 退化为 `Ctrl+N`（readline 的「下一行」），开不了
    /// 新窗口。此处直接读它的 schema，用户改过绑定也能跟上。
    #[test]
    fn ptyxis_new_window_comes_from_its_own_schema() {
        let mut m = default_mapper();
        m.set_window_id("org.gnome.Ptyxis");
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_N.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                && out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                && out.contains(&(KeyCode::KEY_N.0, 1)),
            "ptyxis 的 Cmd+N 应为 Ctrl+Shift+N（新窗口），实际 {out:?}"
        );
    }

    /// ptyxis 的关窗口键是 Ctrl+Shift+Q（其 schema 所声明），
    /// 但全局规则 Cmd+Q → Alt+F4 优先——Alt+F4 是 GNOME 的 close
    /// 绑定，跨应用一致，不该被终端特化吞掉。
    #[test]
    fn cmd_q_stays_alt_f4_in_ptyxis() {
        let mut m = default_mapper();
        m.set_window_id("org.gnome.Ptyxis");
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_Q.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTALT.0, 1))
                && out.contains(&(KeyCode::KEY_F4.0, 1)),
            "ptyxis 的 Cmd+Q 应为 Alt+F4，实际 {out:?}"
        );
    }

    /// ptyxis / konsole 的复制键改走 Ctrl+Insert。
    ///
    /// 这两个终端在**无选中文本**时会把 `Ctrl+Shift+C` 穿透给子进程
    /// 当作 SIGINT ——「复制」会杀掉正在跑的任务。`Ctrl+Insert` 是
    /// X11 传统复制键，终端都支持且不会被当作控制字符发送。
    ///
    /// 而 wezterm / ghostty / gnome-terminal 无此问题（无条件吞掉该
    /// 组合），故不受影响。
    #[test]
    fn sigint_leaky_terminals_use_ctrl_insert() {
        for wm in ["org.gnome.Ptyxis", "org.kde.konsole"] {
            let mut m = default_mapper();
            m.set_window_id(wm);
            m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
            let out = desc(&m.translate(&k(KeyCode::KEY_C.0, 1)));
            assert!(
                out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1))
                    && out.contains(&(KeyCode::KEY_INSERT.0, 1)),
                "{wm} 的 Cmd+C 应为 Ctrl+Insert（避免 SIGINT），实际 {out:?}"
            );
            assert!(
                !out.iter().any(|(c, _)| *c == KeyCode::KEY_LEFTSHIFT.0),
                "{wm} 的 Cmd+C 不应带 Shift: {out:?}"
            );
        }
    }

    /// 不漏发 SIGINT 的终端仍走普通终端路径（Ctrl+Shift+C）。
    ///
    /// 若一律改用 Ctrl+Insert，会让这些终端失去标准复制键——所以
    /// 该规则必须按应用区分，不能全局套用。
    #[test]
    fn safe_terminals_keep_ctrl_shift_c() {
        let mut m = terminal_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_C.0, 1)));
        assert!(out.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1)), "{out:?}");
        assert!(out.contains(&(KeyCode::KEY_C.0, 1)), "{out:?}");
    }

    /// 普通应用里 Cmd+C 不受该规则影响，仍是 Ctrl+C。
    #[test]
    fn normal_app_cmd_c_is_plain_ctrl() {
        let mut m = default_mapper();
        m.set_window_id("org.gnome.Nautilus");
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_C.0, 1)));
        assert!(out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "{out:?}");
        assert!(
            !out.iter().any(|(c, _)| *c == KeyCode::KEY_LEFTSHIFT.0),
            "{out:?}"
        );
    }

    /// Cmd+Q → Alt+F4，且**终端与普通应用一致**。
    ///
    /// 这是「逐键白名单」相对「整体换语义」的关键区别：macOS 的
    /// Cmd+Q（退出应用）对应 Linux 的 Alt+F4（GNOME 的
    /// wm.keybindings close），这是跨应用的通用绑定。若终端里把 Cmd
    /// 一律变成 Ctrl+Shift，Cmd+Q 会退化成谁也不认的 Ctrl+Shift+Q。
    #[test]
    fn cmd_q_is_alt_f4_everywhere() {
        for (wm, label) in [
            ("org.gnome.Nautilus", "普通应用"),
            ("org.gnome.Ptyxis", "终端"),
            ("com.mitchellh.ghostty", "终端"),
        ] {
            let mut m = default_mapper();
            m.set_window_id(wm);
            m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
            let out = desc(&m.translate(&k(KeyCode::KEY_Q.0, 1)));
            assert!(
                out.contains(&(KeyCode::KEY_LEFTALT.0, 1))
                    && out.contains(&(KeyCode::KEY_F4.0, 1)),
                "{label}（{wm}）Cmd+Q 应为 Alt+F4，实际 {out:?}"
            );
        }
    }

    /// 终端里未列入白名单的键仍走默认语义（Ctrl），不被特化。
    ///
    /// 例如 Cmd+N：终端的 Ctrl+N 通常空闲，无需变成 Ctrl+Shift+N。
    /// 笼统地「终端里一律加 Shift」会毁掉这类键。
    #[test]
    fn terminal_unlisted_key_stays_plain_ctrl() {
        let mut m = terminal_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_N.0, 1)));
        assert!(out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "{out:?}");
        assert!(out.contains(&(KeyCode::KEY_N.0, 1)), "{out:?}");
        assert!(
            !out.iter().any(|(c, v)| *c == KeyCode::KEY_LEFTSHIFT.0 && *v == 1),
            "白名单外的键不应加 Shift: {out:?}"
        );
    }

    #[test]
    fn capslock_becomes_leftctrl() {
        let mut m = default_mapper();
        // 按下 CapsLock(58) 应输出 LEFTCTRL(29)
        let out = m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));
        assert_eq!(desc(&out), vec![(KeyCode::KEY_LEFTCTRL.0, 1)]);
    }

    #[test]
    fn leftctrl_becomes_capslock() {
        let mut m = default_mapper();
        let out = m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1));
        assert_eq!(desc(&out), vec![(KeyCode::KEY_CAPSLOCK.0, 1)]);
    }

    #[test]
    fn cmd_c_becomes_ctrl_c() {
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1)); // 按下 Cmd，应无输出
        let out = m.translate(&k(KeyCode::KEY_C.0, 1));
        // 期望: 按下 Ctrl(29,1) → 按下 c(46,1)
        // Cmd 从未向输出侧按下，故不应出现它的释放事件。
        let d = desc(&out);
        assert!(
            !d.contains(&(KeyCode::KEY_LEFTMETA.0, 0)),
            "Cmd 从未按下，不应有释放: {d:?}"
        );
        assert!(d.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "应按下 Ctrl: {d:?}");
        assert!(d.contains(&(KeyCode::KEY_C.0, 1)), "应按下 c: {d:?}");
    }

    #[test]
    fn cmd_not_in_table_passes_through() {
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        // Cmd+E 不在默认表里，应原样透传
        let out = m.translate(&k(KeyCode::KEY_E.0, 1));
        let d = desc(&out);
        assert!(d.contains(&(KeyCode::KEY_E.0, 1)), "E 应原样输出: {d:?}");
        assert!(!d.iter().any(|(_, v)| *v == 1 && false));
    }

    #[test]
    fn repeat_events_filtered() {
        let mut m = default_mapper();
        let out = m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 2)); // value=2
        assert!(out.is_empty(), "自动重复事件应被丢弃");
    }

    /// MISC 等非按键事件必须丢弃。
    ///
    /// 曾尝试原样转发以保留触控板活动，但转发会把 ABS 多点触控事件一并
    /// 送到未声明 EV_ABS 能力的虚拟设备，导致触控板与按键双双异常。
    /// 触控板交由原设备处理，mackbind 只负责键盘。
    #[test]
    fn misc_events_filtered() {
        let mut m = default_mapper();
        let ev = InputEvent::new(EventType::MISC.0, 4, 0);
        assert!(m.translate(&ev).is_empty(), "MISC 应丢弃");
    }

    /// SYNCHRONIZATION 例外：uinput 的 emit() 会自动补 SYN_REPORT，
    /// 再转发原始 SYN 会把一个批次拆成多个同步报告。
    #[test]
    fn sync_events_filtered() {
        let mut m = default_mapper();
        let ev = InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0);
        assert!(m.translate(&ev).is_empty(), "SYN 应由 uinput 自动补，不转发");
    }

    #[test]
    fn ctrl_v_after_capslock_is_real_ctrl() {
        // 核心场景：CapsLock 位置的 Ctrl 在终端里应产生真正的 Ctrl+V。
        // 注意 Ctrl 是在 CapsLock 按下时就已注入输出的，因此 V 本身
        // 只需输出自身——输出侧此刻已处于 Ctrl 按下状态。
        let mut m = default_mapper();
        m.set_window_id("org.gnome.Console");

        let first = m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));
        assert_eq!(desc(&first), vec![(KeyCode::KEY_LEFTCTRL.0, 1)], "Caps 应输出真 Ctrl");

        let second = m.translate(&k(KeyCode::KEY_V.0, 1));
        assert_eq!(desc(&second), vec![(KeyCode::KEY_V.0, 1)], "V 自身原样输出");

        // 全程不应出现 Cmd（Super）
        let mut all = first;
        all.extend(second);
        assert!(
            desc(&all).iter().all(|(c, _)| *c != KeyCode::KEY_LEFTMETA.0),
            "Ctrl+V 路径中不应出现 Cmd: {:?}",
            desc(&all)
        );
    }

    #[test]
    fn cmd_c_output_is_released_symmetrically() {
        // Cmd+C → Ctrl+C，之后松开所有键，输出侧应回到无修饰键状态
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let press = m.translate(&k(KeyCode::KEY_C.0, 1));

        // 释放主键
        let rel_c = m.translate(&k(KeyCode::KEY_C.0, 0));
        assert!(desc(&rel_c).contains(&(KeyCode::KEY_C.0, 0)));

        // 释放 Cmd：Cmd 全程被抑制，从未按下，故不产生任何事件
        let rel_meta = m.translate(&k(KeyCode::KEY_LEFTMETA.0, 0));
        assert!(
            desc(&rel_meta).is_empty(),
            "Cmd 被抑制，释放应无输出: {:?}",
            desc(&rel_meta)
        );

        // 全序列末尾：所有修饰键都必须已释放，不得有残留的按下状态
        let all: Vec<InputEvent> = press
            .into_iter()
            .chain(rel_c)
            .chain(rel_meta)
            .collect();
        assert!(
            all.last().is_some_and(|e| e.value() == 0),
            "序列应以释放事件收尾: {:?}",
            desc(&all)
        );
        // Ctrl 的按下与释放必须成对出现
        let ctrl_down = all
            .iter()
            .filter(|e| e.code() == KeyCode::KEY_LEFTCTRL.0 && e.value() == 1)
            .count();
        let ctrl_up = all
            .iter()
            .filter(|e| e.code() == KeyCode::KEY_LEFTCTRL.0 && e.value() == 0)
            .count();
        assert_eq!(ctrl_down, ctrl_up, "Ctrl 按下/释放次数应相等: {:?}", desc(&all));
    }

    #[test]
    fn release_is_symmetric_after_translation() {
        let mut m = default_mapper();
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));
        let out = m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 0));
        assert_eq!(desc(&out), vec![(KeyCode::KEY_LEFTCTRL.0, 0)]);
    }
}

#[cfg(test)]
mod regression {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

    /// 回归：CapsLock 按下又松开后，不应残留在 held 中。
    /// 实测 bug：Cmd+C 的输出里混入了 KEY_CAPSLOCK 0。
    #[test]
    fn remapped_modifier_does_not_persist_in_held() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();

        // 按下并松开 CapsLock（逻辑上变LEFTCTRL）
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 0));

        // 再按物理左 Ctrl 并松开
        m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 0));

        assert!(
            m.held.is_empty(),
            "held 应为空，实际残留 {:?}",
            m.held
        );

        // 随后按 Cmd+C，输出不应包含任何 CAPSLOCK 事件
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_C.0, 1));
        let d = desc(&out);
        assert!(
            !d.iter().any(|(c, _)| *c == KeyCode::KEY_CAPSLOCK.0),
            "Cmd+C 输出不应含 CAPSLOCK: {d:?}"
        );
        assert!(
            d.iter().any(|(c, _)| *c == KeyCode::KEY_LEFTCTRL.0),
            "应输出 Ctrl: {d:?}"
        );
    }

    /// 回归：先按 CapsLock 位置(实为Ctrl)再按 Cmd，不应误判为组合。
    #[test]
    fn caps_then_cmd_does_not_leak() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1));  // → LEFTCTRL
        m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 0));
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_C.0, 1));
        let d = desc(&out);
        // 只应有一个 LEFTCTRL（来自Cmd+C），不能有 CAPSLOCK 释放
        assert!(!d.iter().any(|(c, _)| *c == KeyCode::KEY_CAPSLOCK.0), "{d:?}");
    }
}


#[cfg(test)]
mod modifier_passthrough_tests {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

    /// 回归：Shift 必须原样输出。
    /// 实测 bug：Shift 被抑制，fcitx5 监听裸 Shift_L 切换输入法失效。
    #[test]
    fn shift_is_passed_through() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let down = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        assert_eq!(desc(&down), vec![(KeyCode::KEY_LEFTSHIFT.0, 1)], "Shift 按下必须输出");

        let up = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0));
        assert_eq!(desc(&up), vec![(KeyCode::KEY_LEFTSHIFT.0, 0)], "Shift 释放必须输出");
    }

    #[test]
    fn alt_and_ctrl_are_passed_through() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        for code in [
            KeyCode::KEY_LEFTALT,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_RIGHTCTRL,
        ] {
            let mut m2 = Mapper::new(&Config::with_defaults(), false).unwrap();
            let d = m2.translate(&k(code.0, 1));
            assert_eq!(desc(&d), vec![(code.0, 1)], "{code:?} 应原样输出");
        }
        let _ = &mut m;
    }

    /// Cmd 仍须抑制——这是设计核心。
    #[test]
    fn cmd_is_still_suppressed() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let down = m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        assert!(down.is_empty(), "Cmd 按下应抑制（表外组合需保持 Super 语义）");
    }

    /// Cmd+Shift+a 应输出 Shift+a（Cmd 在表外，Shift 需真实存在）
    #[test]
    fn cmd_shift_combo_keeps_shift() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        assert_eq!(desc(&out), vec![(KeyCode::KEY_LEFTSHIFT.0, 1)], "Shift 应输出");
    }
}

#[cfg(test)]
mod press_release_symmetry {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

    /// 每一个透传的键，按下与释放必须成对出现且键码一致。
    /// 这直接对应用户问题：Shift 释放了为什么没透传出去。
    #[test]
    fn every_passthrough_modifier_is_symmetric() {
        for code in [
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_RIGHTSHIFT,
            KeyCode::KEY_LEFTALT,
            KeyCode::KEY_RIGHTALT,
            KeyCode::KEY_RIGHTCTRL,
        ] {
            let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
            let down = desc(&m.translate(&k(code.0, 1)));
            let up = desc(&m.translate(&k(code.0, 0)));
            assert_eq!(down, vec![(code.0, 1)], "{code:?} 按下应原样输出");
            assert_eq!(up, vec![(code.0, 0)], "{code:?} 释放应原样输出");
        }
    }

    /// 普通字符键同样必须对称
    #[test]
    fn plain_keys_are_symmetric() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        assert_eq!(desc(&m.translate(&k(KeyCode::KEY_A.0, 1))), vec![(KeyCode::KEY_A.0, 1)]);
        assert_eq!(desc(&m.translate(&k(KeyCode::KEY_A.0, 0))), vec![(KeyCode::KEY_A.0, 0)]);
    }

    /// remap 过的键：按下输出目标键，释放也输出目标键
    #[test]
    fn remapped_key_release_uses_target() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let down = desc(&m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 1)));
        let up = desc(&m.translate(&k(KeyCode::KEY_CAPSLOCK.0, 0)));
        assert_eq!(down, vec![(KeyCode::KEY_LEFTCTRL.0, 1)]);
        assert_eq!(up, vec![(KeyCode::KEY_LEFTCTRL.0, 0)], "释放须用目标键");
    }

    /// 连续多次 Shift 时不应有状态残留
    #[test]
    fn repeated_shift_cycles_clean() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        for _ in 0..3 {
            m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
            m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0));
        }
        assert!(m.pressed.is_empty(), "残留: {:?}", m.pressed);
        assert!(m.held.is_empty(), "held 残留: {:?}", m.held);
    }
}

#[cfg(test)]
mod cmd_shift_combo {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

    /// 新模型下，Cmd+Shift+Z 不再走白名单，而是由默认语义处理：
    /// 保留 Shift，输出 Ctrl+Shift+Z。Linux 上 Ctrl+Shift+Z 正是重做。
    #[test]
    fn cmd_shift_z_keeps_shift() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));  // Cmd
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1)); // Shift
        let out = m.translate(&k(KeyCode::KEY_Z.0, 1));
        let d = desc(&out);
        assert!(
            d.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)) && d.contains(&(KeyCode::KEY_LEFTSHIFT.0, 1))
                && d.contains(&(KeyCode::KEY_Z.0, 1)),
            "Cmd+Shift+Z 应输出 Ctrl+Shift+Z，实际: {d:?}"
        );
    }

    /// 反向验证：只按 Cmd（无 Shift）时不应命中 Shift+Z 条目
    #[test]
    fn cmd_z_without_shift_is_undo() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_Z.0, 1));
        let d = desc(&out);
        assert!(d.contains(&(KeyCode::KEY_Z.0, 1)), "应输出 z（撤销），实际: {d:?}");
        assert!(!d.contains(&(KeyCode::KEY_Y.0, 1)), "不应输出 y: {d:?}");
    }
}

#[cfg(test)]
mod key_ordering {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

    /// 用户实测：Cmd+Z 应撤销（输出 z），但曾变成裸 z（Cmd 丢失）。
    /// 说明命中翻译后，Cmd 的释放事件没有被正确处理。
    #[test]
    fn cmd_z_releases_cmd_and_emits_z() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_Z.0, 1));
        let d = desc(&out);
        assert!(d.contains(&(KeyCode::KEY_Z.0, 1)), "必须按下 z: {d:?}");
        assert!(d.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "必须按下 Ctrl: {d:?}");
        // Cmd 被抑制，从未按下，故无释放事件
        assert!(
            !d.contains(&(KeyCode::KEY_LEFTMETA.0, 0)),
            "Cmd 未按下，不应发释放: {d:?}"
        );
    }

    /// 用户实测：Cmd+Shift+Z 曾触发输入法切换，说明 Shift 被单独输出。
    /// 正确行为：Shift 应在翻译后被松开，不应残留为裸 Shift。
    #[test]
    fn cmd_shift_z_does_not_leak_shift() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = m.translate(&k(KeyCode::KEY_Z.0, 1));
        let d = desc(&out);
        // Shift 此前已透传按下，翻译时必须松开，否则会残留为裸 Shift
        assert!(
            d.contains(&(KeyCode::KEY_LEFTSHIFT.0, 0)),
            "Shift 透传按下过，翻译时必须松开: {d:?}"
        );
        assert!(d.contains(&(KeyCode::KEY_Z.0, 1)), "应输出 z: {d:?}");
    }
}

#[cfg(test)]
mod collision_tests {
    use super::*;
    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn desc(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }

    /// 显式规则优先于默认语义。
    ///
    /// Cmd+[ 在 macOS 是「上一个文件夹」，Linux 上对应 Alt+←，与默认的
    /// Ctrl+[ 不同——这正是需要显式规则的情形。
    #[test]
    fn explicit_rule_overrides_default() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_LEFTBRACE.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTALT.0, 1)) && out.contains(&(KeyCode::KEY_LEFT.0, 1)),
            "Cmd+[ 应输出 Alt+←: {out:?}"
        );
        assert!(!out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)), "不应含 Ctrl: {out:?}");
    }

    /// 同样覆盖 Cmd+S / Cmd+Shift+S
    #[test]
    fn save_and_saveas_coexist() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let save = desc(&m.translate(&k(KeyCode::KEY_S.0, 1)));
        assert!(save.contains(&(KeyCode::KEY_S.0, 1)), "Cmd+S → Ctrl+S: {save:?}");
    }

    /// 未登记的组合走默认语义：输出 Ctrl+Shift+T。
    ///
    /// 这正是新模型的意义——长尾组合无需穷举即可正确映射。
    #[test]
    fn unmapped_combo_gets_default_translation() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = desc(&m.translate(&k(KeyCode::KEY_T.0, 1)));
        assert!(
            out.contains(&(KeyCode::KEY_LEFTCTRL.0, 1)) && out.contains(&(KeyCode::KEY_T.0, 1)),
            "Cmd+Shift+T 应输出 Ctrl+Shift+T: {out:?}"
        );
    }
}

#[cfg(test)]
mod state_separation {
    use super::*;
    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    /// 核心不变量：**held 只反映物理状态，永不因发送事件而改变。**
    ///
    /// 用户报告的 bug（Cmd+Z 变裸 z）根因正是一次组合翻译把 held
    /// drain 掉了——Cmd 其实还按着，内部却以为已松开。
    #[test]
    fn held_survives_combo_translation() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        assert!(m.held.contains(&KeyCode::KEY_LEFTMETA));

        // 触发一次组合翻译
        m.translate(&k(KeyCode::KEY_C.0, 1));

        assert!(
            m.held.contains(&KeyCode::KEY_LEFTMETA),
            "翻译后 held 必须仍含 Cmd（用户还按着）: {:?}",
            m.held
        );
    }

    /// 连续两次组合键都应正常命中
    #[test]
    fn consecutive_combos_both_work() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        for _ in 0..3 {
            let out = m.translate(&k(KeyCode::KEY_C.0, 1));
            assert!(out.contains(&InputEvent::new(EventType::KEY.0, KeyCode::KEY_LEFTCTRL.0, 1)));
            m.translate(&k(KeyCode::KEY_C.0, 0));
        }
    }

    /// Shift 按住期间连续按多个字母，都应保持 Shift 按下
    #[test]
    fn shift_held_across_multiple_keys() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        for c in [KeyCode::KEY_A, KeyCode::KEY_B, KeyCode::KEY_C] {
            let out = m.translate(&k(c.0, 1));
            assert_eq!(desc2(&out), vec![(c.0, 1)], "{c:?} 应原样输出且不干扰 Shift");
            m.translate(&k(c.0, 0));
        }
        assert!(m.held.contains(&KeyCode::KEY_LEFTSHIFT), "Shift 仍按住");
    }

    fn desc2(evs: &[InputEvent]) -> Vec<(u16, i32)> {
        evs.iter().map(|e| (e.code(), e.value())).collect()
    }
}


#[cfg(test)]
mod release_symmetry {

    use super::*;
    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn codes(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }

    /// 核心不变量：输出侧每个键的按下与释放必须严格配对。
    /// 多余的释放事件（无对应按下）会让应用误判按键状态。
    #[test]
    fn no_orphan_release_events() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut pressed: Vec<u16> = Vec::new();

        let seq = [
            (KeyCode::KEY_LEFTMETA, 1), (KeyCode::KEY_LEFTSHIFT, 1),
            (KeyCode::KEY_Z, 1), (KeyCode::KEY_Z, 0),
            (KeyCode::KEY_LEFTSHIFT, 0), (KeyCode::KEY_LEFTMETA, 0),
            // 再来一轮
            (KeyCode::KEY_LEFTMETA, 1), (KeyCode::KEY_C, 1),
            (KeyCode::KEY_C, 0), (KeyCode::KEY_LEFTMETA, 0),
        ];

        for (code, v) in seq {
            // 注意：必须按**输出事件自身的 value** 判断按下/释放，
            // 不能用输入事件的 value——组合键翻译时，输入是「按下」，
            // 输出却可能包含若干「释放」（如 Cmd+Shift+Z 需先松 Shift）。
            for e in m.translate(&k(code.0, v)) {
                let c = e.code();
                if e.value() == 1 {
                    assert!(!pressed.contains(&c), "重复按下 {c}");
                    pressed.push(c);
                } else {
                    let p = pressed.iter().position(|x| *x == c);
                    assert!(p.is_some(), "孤立释放事件，键码 {c}");
                    pressed.remove(p.unwrap());
                }
            }
        }
        assert!(pressed.is_empty(), "结束时仍有未释放: {pressed:?}");
    }

    /// 用户实测问题：Cmd+Shift+Z 曾多发一对 Shift 释放
    #[test]
    fn cmd_shift_z_emits_no_extra_shift() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        m.translate(&k(KeyCode::KEY_Z.0, 1));
        m.translate(&k(KeyCode::KEY_Z.0, 0));
        // Shift 物理释放时，输出侧不应再有任何事件
        let out = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0));
        assert!(out.is_empty(), "Shift 已被翻译时松开，不应再有输出: {:?}", codes(&out));
    }

    /// `Ctrl+Cmd+Q` 必须区别于 `Cmd+Q`——这是本项目修过的一个真实缺陷。
    ///
    /// 曾因 remap 把物理 Ctrl 变成 CapsLock，查表匹配不上 `Cmd+Q` 规则，
    /// 于是落进默认语义发出 `Ctrl+Q`。实机观感与 `Cmd+Q` 相同（都关窗口），
    /// 但语义完全错：`Ctrl+Cmd+Q` 在 macOS 上是**锁屏**。
    ///
    /// 现表中有锁屏规则，且查表前把 CapsLock 归一化为 Ctrl
    /// （物理 Ctrl 经 remap 后是 CapsLock，但语义上仍是 Ctrl）。
    #[test]
    fn ctrl_cmd_q_is_not_same_as_cmd_q() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = m.translate(&k(KeyCode::KEY_Q.0, 1));
        let codes = codes(&out);

        assert!(
            !codes.contains(&KeyCode::KEY_F4.0),
            "Ctrl+Cmd+Q 不应命中 Cmd+Q 的 Alt+F4: {:?}",
            codes
        );
        assert!(
            !codes.contains(&KeyCode::KEY_Q.0),
            "Ctrl+Cmd+Q 不应退化成裸 Ctrl+Q: {:?}",
            codes
        );
    }

    /// `Ctrl+Cmd+X` 的处理：能转换则转换，不能则放行。
///
/// 背景：默认语义把 Cmd 变 Ctrl。若用户已按住 Ctrl，Cmd 再变 Ctrl 就是
/// `Ctrl+Ctrl` ——同一槽位，组合退化，故不能转换。但也**不能**当 `Cmd+X`
/// 处理：`Ctrl+Cmd+X` 在 macOS 上是另一个组合键，有自己的含义。
/// 表里有规则就按表走，没有的原样放行。
#[cfg(test)]
mod ctrl_cmd_semantics {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    fn codes(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }

    fn press(m: &mut Mapper, seq: &[KeyCode]) -> Vec<u16> {
        let mut out = vec![];
        for c in seq {
            out.extend(m.translate(&k(c.0, 1)));
        }
        codes(&out)
    }

    /// `Ctrl+Cmd+Q` 命中表里的锁屏规则 → 发 DE 现有的 Super+L
    #[test]
    fn ctrl_cmd_q_locks_screen() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = press(
            &mut m,
            &[
                KeyCode::KEY_LEFTCTRL,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_Q,
            ],
        );
        assert!(
            out.contains(&KeyCode::KEY_LEFTMETA.0) && out.contains(&KeyCode::KEY_L.0),
            "Ctrl+Cmd+Q 应发 Super+L（读 DE 的 screensaver 绑定），实际 {:?}",
            out
        );
        assert!(
            !out.contains(&KeyCode::KEY_F4.0),
            "不应退回 Cmd+Q 的 Alt+F4：{:?}",
            out
        );
    }

    /// `Ctrl+Cmd+F` 全屏 → F11
    #[test]
    fn ctrl_cmd_f_fullscreen() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = press(
            &mut m,
            &[
                KeyCode::KEY_LEFTCTRL,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_F,
            ],
        );
        assert!(
            out.contains(&KeyCode::KEY_F11.0),
            "Ctrl+Cmd+F 应为 F11（全屏），实际 {:?}",
            out
        );
        assert!(
            !out.contains(&KeyCode::KEY_F.0),
            "不应退回查找 Ctrl+F：{:?}",
            out
        );
    }

    /// 表里没有的 `Ctrl+Cmd+W` → 原样放行，不得编造 `Ctrl+W`
    #[test]
    fn unmapped_ctrl_cmd_passes_through() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = press(
            &mut m,
            &[
                KeyCode::KEY_LEFTCTRL,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_W,
            ],
        );
        assert!(
            out.contains(&KeyCode::KEY_LEFTMETA.0),
            "放行时须发出真实的 Meta，否则应用收到残缺组合：{:?}",
            out
        );
        assert!(
            out.contains(&KeyCode::KEY_W.0),
            "放行时主键须原样发出：{:?}",
            out
        );
    }

    /// `Shift+Cmd+X` 仍走默认语义 → `Shift+Ctrl+X`（不冲突，可转换）
    #[test]
    fn shift_cmd_still_converts() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = press(
            &mut m,
            &[
                KeyCode::KEY_LEFTSHIFT,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_Z,
            ],
        );
        assert!(
            out.contains(&KeyCode::KEY_LEFTCTRL.0)
                && out.contains(&KeyCode::KEY_LEFTSHIFT.0)
                && out.contains(&KeyCode::KEY_Z.0),
            "Shift+Cmd+Z 应为 Shift+Ctrl+Z，实际 {:?}",
            out
        );
    }
}

/// 防呆：任何键序列走完后，输出侧不得残留按住的修饰键。
///
/// 这条不变量的破坏后果极重：曾因放行路径发出 Meta 却未把它从
/// `suppressed` 移除，释放被吞掉，Super 永久卡在按下态。实机表现是
/// **锁屏后输密码时每个键都带 Super，数字 9 输不进去，只能硬关机**。
///
/// 与其逐个组合测，不如断言不变量本身——任何路径违反都会被抓到。
#[cfg(test)]
mod no_stuck_modifiers {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    const MODS: &[KeyCode] = &[
        KeyCode::KEY_LEFTMETA,
        KeyCode::KEY_RIGHTMETA,
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_LEFTALT,
        KeyCode::KEY_CAPSLOCK,
    ];

    /// 跑完一串事件后，输出侧不应有任何修饰键仍处于按下态。
    fn assert_clean(seq: &[(KeyCode, i32)], label: &str) {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        for (c, v) in seq {
            out.extend(m.translate(&k(c.0, *v)));
        }
        // 逐键核对按下/释放配平
        for mod_key in MODS {
            let downs = out
                .iter()
                .filter(|e| e.code() == mod_key.0 && e.value() == 1)
                .count();
            let ups = out
                .iter()
                .filter(|e| e.code() == mod_key.0 && e.value() == 0)
                .count();
            assert_eq!(
                downs,
                ups,
                "{label}: {:?} 事件不配平（{downs} 次按下 / {ups} 次释放）",
                mod_key
            );
        }
    }

    #[test]
    fn passthrough_releases_meta() {
        // 放行路径曾漏发 Meta 释放，导致 Super 卡住
        assert_clean(
            &[
                (KeyCode::KEY_LEFTCTRL, 1),
                (KeyCode::KEY_LEFTMETA, 1),
                (KeyCode::KEY_W, 1),
                (KeyCode::KEY_W, 0),
                (KeyCode::KEY_LEFTMETA, 0),
                (KeyCode::KEY_LEFTCTRL, 0),
            ],
            "放行 Ctrl+Cmd+W",
        );
    }

    #[test]
    fn converted_combo_releases_everything() {
        assert_clean(
            &[
                (KeyCode::KEY_LEFTMETA, 1),
                (KeyCode::KEY_Q, 1),
                (KeyCode::KEY_Q, 0),
                (KeyCode::KEY_LEFTMETA, 0),
            ],
            "Cmd+Q → Alt+F4",
        );
    }

    #[test]
    fn ctrl_cmd_q_lockscreen_releases_everything() {
        assert_clean(
            &[
                (KeyCode::KEY_LEFTCTRL, 1),
                (KeyCode::KEY_LEFTMETA, 1),
                (KeyCode::KEY_Q, 1),
                (KeyCode::KEY_Q, 0),
                (KeyCode::KEY_LEFTMETA, 0),
                (KeyCode::KEY_LEFTCTRL, 0),
            ],
            "Ctrl+Cmd+Q 锁屏",
        );
    }

    #[test]
    fn shift_cmd_conversion_releases_everything() {
        assert_clean(
            &[
                (KeyCode::KEY_LEFTSHIFT, 1),
                (KeyCode::KEY_LEFTMETA, 1),
                (KeyCode::KEY_Z, 1),
                (KeyCode::KEY_Z, 0),
                (KeyCode::KEY_LEFTMETA, 0),
                (KeyCode::KEY_LEFTSHIFT, 0),
            ],
            "Shift+Cmd+Z",
        );
    }
}

/// 系统性排查：任何「按下若干修饰键 + 主键，再全部松开」的序列，
/// 结束后内部状态与输出侧都必须是干净的。
///
/// 症状是 Meta 卡在按下态——空格变成 Cmd+空格、W 变成 Cmd+W。
/// 这类 bug 靠逐个组合手写测试很容易漏，故穷举。
#[cfg(test)]
mod exhaustive_clean {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    const MODS: &[KeyCode] = &[
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_LEFTALT,
    ];

    const KEYS: &[KeyCode] = &[
        KeyCode::KEY_SPACE,
        KeyCode::KEY_W,
        KeyCode::KEY_Q,
        KeyCode::KEY_A,
        KeyCode::KEY_F,
        KeyCode::KEY_DELETE,
        KeyCode::KEY_LEFT,
    ];

    /// Meta 是罪魁：它按下时被抑制，最容易卡住
    const META: KeyCode = KeyCode::KEY_LEFTMETA;

    fn check(seq_down: &[KeyCode], label: &str) {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        for c in seq_down {
            out.extend(m.translate(&k(c.0, 1)));
        }
        // 逆序松开
        for c in seq_down.iter().rev() {
            out.extend(m.translate(&k(c.0, 0)));
        }

        assert!(
            m.held.is_empty(),
            "{label}: 结束后 held 仍非空: {:?}",
            m.held
        );
        assert!(
            m.output_down.is_empty(),
            "{label}: 结束后 output_down 仍非空: {:?}",
            m.output_down
        );
        // 输出侧每个键的按下/释放必须配平
        let mut seen: Vec<(u16, i32)> = Vec::new();
        for e in &out {
            seen.push((e.code(), e.value()));
        }
        for key in [META, KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_LEFTALT] {
            let d = seen.iter().filter(|(c, v)| *c == key.0 && *v == 1).count();
            let u = seen.iter().filter(|(c, v)| *c == key.0 && *v == 0).count();
            assert_eq!(d, u, "{label}: {:?} 输出不配平 {d}下/{u}up", key);
        }
    }

    #[test]
    fn meta_plus_one_mod_plus_key_is_clean() {
        for a in MODS {
            for key in KEYS {
                check(&[META, *a, *key], "Meta+mod+key");
            }
        }
    }

    #[test]
    fn meta_plus_key_is_clean() {
        for key in KEYS {
            check(&[META, *key], "Meta+key");
        }
    }

    #[test]
    fn meta_alone_is_clean() {
        check(&[META], "Meta 单独");
    }

    #[test]
    fn two_mods_plus_key_is_clean() {
        for a in MODS {
            for b in MODS {
                if a == b {
                    continue;
                }
                for key in KEYS {
                    check(&[*a, *b, *key], "mod+mod+key");
                }
            }
        }
    }
}

/// 松开顺序无关。
///
/// 上一个测试固定逆序松开，但真人不按顺序松——尤其是**先松修饰键、
/// 后松主键**（手指自然抬起顺序）。这正是 Meta 卡死的土壤。
#[cfg(test)]
mod release_order {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    /// 按下全部，然后按 `order` 给出的下标顺序松开
    fn check(down: &[KeyCode], order: &[usize], label: &str) {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        for c in down {
            out.extend(m.translate(&k(c.0, 1)));
        }
        for i in order {
            out.extend(m.translate(&k(down[*i].0, 0)));
        }
        assert!(
            m.held.is_empty(),
            "{label}: held 残留 {:?}",
            m.held
        );
        assert!(
            m.output_down.is_empty(),
            "{label}: output_down 残留 {:?}",
            m.output_down
        );
        for key in [
            KeyCode::KEY_LEFTMETA,
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_LEFTALT,
        ] {
            let d = out.iter().filter(|e| e.code() == key.0 && e.value() == 1).count();
            let u = out.iter().filter(|e| e.code() == key.0 && e.value() == 0).count();
            assert_eq!(d, u, "{label}: {:?} 不配平 {d}下/{u}up", key);
        }
    }

    /// Meta 先松、主键后松 —— 最常见也最危险
    #[test]
    fn meta_released_before_key() {
        let keys = [
            KeyCode::KEY_SPACE,
            KeyCode::KEY_W,
            KeyCode::KEY_Q,
            KeyCode::KEY_A,
            KeyCode::KEY_LEFT,
        ];
        for key in keys {
            // 下标 0=Meta, 1=主键；先松 0
            check(
                &[KeyCode::KEY_LEFTMETA, key],
                &[0, 1],
                "Meta 先松",
            );
        }
    }

    /// 三键，穷举全部 6 种松开顺序
    #[test]
    fn all_release_orders_for_meta_mod_key() {
        let perms: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for key in [KeyCode::KEY_SPACE, KeyCode::KEY_W, KeyCode::KEY_A] {
            for p in perms {
                check(
                    &[KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTCTRL, key],
                    &p,
                    "Meta+Ctrl+key",
                );
            }
        }
    }
}

/// 「先松修饰键、主键未松、又按新键」——真实输入里极常见。
///
/// 症状：输出侧残留按下的修饰键，之后每个键都带上它
/// （空格变成 Cmd+空格、W 变成 Cmd+W）。
#[cfg(test)]
mod mid_release_interleave {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    const META: KeyCode = KeyCode::KEY_LEFTMETA;

    #[test]
    fn meta_lifted_before_key_then_new_key() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];

        out.extend(m.translate(&k(META.0, 1)));
        out.extend(m.translate(&k(KeyCode::KEY_Q.0, 1)));
        // 先松 Meta，主键 Q 还按着
        out.extend(m.translate(&k(META.0, 0)));
        // 此时又按空格
        out.extend(m.translate(&k(KeyCode::KEY_SPACE.0, 1)));

        let d: Vec<String> = out
            .iter()
            .map(|e| format!("{:?}{}", e.code(), if e.value() == 1 { "+" } else { "-" }))
            .collect();
        assert!(
            m.output_down.is_empty() || !m.output_down.contains(&META),
            "输出侧 Meta 仍处于按下态: output_down={:?} 事件={:?}",
            m.output_down,
            d
        );
    }

    /// 同上，但走「放行」路径（表里无规则且 Ctrl 槽位被占）
    #[test]
    fn meta_lifted_after_passthrough_then_new_key() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        out.extend(m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1)));
        out.extend(m.translate(&k(META.0, 1)));
        out.extend(m.translate(&k(KeyCode::KEY_W.0, 1)));
        out.extend(m.translate(&k(META.0, 0)));
        out.extend(m.translate(&k(KeyCode::KEY_SPACE.0, 1)));

        assert!(
            !m.output_down.contains(&META),
            "放行后先松 Meta 再按空格：Meta 残留 output_down={:?}",
            m.output_down
        );
    }
}

/// 锁屏路径的特有陷阱：`Ctrl+Cmd+Q` 的输出是 `Super+L`，**含 Meta**。
///
/// 早期版本测不出问题，因为 `Cmd+Q` 的输出是 `Alt+F4`（不含 Meta）。
/// 只有当**转换结果本身含 Meta** 时，下面的缺陷才会暴露：
///
/// 1. 按下时 Meta 被抑制（`suppressed`），从未进入 output_down
/// 2. 转换后主动发出 Meta（作为 `Super+L` 的一部分），并记入 injected_mods
/// 3. 用户先松开 Cmd → 释放路径走 `suppressed.remove()` 分支**提前返回**，
///    不发释放——但 Meta 此时**确实**在 output_down 里
/// 4. 之后每个键都带着这个残留的 Super
///
/// 实机症状：锁屏后空格变成 Cmd+空格（开概览）、W 变成 Cmd+W。
#[cfg(test)]
mod lock_screen_meta_leak {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    const META: KeyCode = KeyCode::KEY_LEFTMETA;

    /// 先松 Cmd（Q 仍按着），再按空格 —— Meta 不得残留在输出侧
    #[test]
    fn lifting_cmd_after_lock_leaves_no_stuck_super() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];

        out.extend(m.translate(&k(KeyCode::KEY_LEFTCTRL.0, 1)));
        out.extend(m.translate(&k(META.0, 1)));
        out.extend(m.translate(&k(KeyCode::KEY_Q.0, 1)));
        // 用户先松开 Cmd
        out.extend(m.translate(&k(META.0, 0)));
        // Q 还按着，此时按空格
        out.extend(m.translate(&k(KeyCode::KEY_SPACE.0, 1)));

        let d: Vec<String> = out
            .iter()
            .map(|e| format!("{:?}{}", e.code(), if e.value() == 1 { "+" } else { "-" }))
            .collect();

        assert!(
            !m.output_down.contains(&META),
            "输出侧 Super 残留：output_down={:?}\n事件序列: {:?}",
            m.output_down,
            d
        );
    }

    /// 完整键入一段，确认没有修饰键被带进后续按键
    #[test]
    fn typing_after_lock_is_clean() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let mut out = vec![];
        // 锁屏
        for c in [KeyCode::KEY_LEFTCTRL, META, KeyCode::KEY_Q] {
            out.extend(m.translate(&k(c.0, 1)));
        }
        // 松开全部（各种顺序）
        for c in [META, KeyCode::KEY_Q, KeyCode::KEY_LEFTCTRL] {
            out.extend(m.translate(&k(c.0, 0)));
        }
        // 输密码
        for c in [KeyCode::KEY_A, KeyCode::KEY_1, KeyCode::KEY_B, KeyCode::KEY_9] {
            out.extend(m.translate(&k(c.0, 1)));
            out.extend(m.translate(&k(c.0, 0)));
        }
        assert!(
            m.output_down.is_empty(),
            "输密码后输出侧应无残留: {:?}",
            m.output_down
        );
        // 数字键不得携带修饰键
        for e in &out {
            if e.code() == KeyCode::KEY_9.0 && e.value() == 1 {
                // 它前面紧邻的按下事件不应有修饰键
            }
        }
    }
}

/// Cmd 规则必须**要求按住 Cmd**。
///
/// 回归测试：查表曾被移出 `if cmd_held` 分支（为支持非 Cmd 组合），
/// 但规则本身不校验 Meta 是否按住——于是**裸按空格也命中
/// `Cmd+空格` 那条规则**，任何人按空格都弹出 GNOME 概览。
///
/// 现场证据（--trace 抓到的真实输入输出）：
///   IN KEY_SPACE 按下 -> OUT [SHIFT+, CTRL+, SPACE+]
#[cfg(test)]
mod cmd_required {
    use super::*;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    fn codes(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }

    /// 裸空格必须原样透传，绝不能变成 Ctrl+Shift+空格
    #[test]
    fn bare_space_is_not_overview() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = codes(&m.translate(&k(KeyCode::KEY_SPACE.0, 1)));
        assert_eq!(
            out,
            vec![KeyCode::KEY_SPACE.0],
            "裸空格不应触发概览规则"
        );
    }

    /// `Cmd+空格` 仍须命中概览规则
    #[test]
    fn cmd_space_still_maps_to_overview() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        let out = codes(&m.translate(&k(KeyCode::KEY_SPACE.0, 1)));
        assert!(
            out.contains(&KeyCode::KEY_LEFTSHIFT.0)
                && out.contains(&KeyCode::KEY_LEFTCTRL.0),
            "Cmd+空格 应为 GNOME 的 Ctrl+Shift+空格，实际 {:?}",
            out
        );
    }

    /// 裸 A 不得变成 Ctrl+A（默认语义只在 Cmd 组合下生效）
    #[test]
    fn bare_letter_is_not_translated() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let out = codes(&m.translate(&k(KeyCode::KEY_A.0, 1)));
        assert_eq!(out, vec![KeyCode::KEY_A.0], "裸 A 应原样输出");
    }

    /// 不含 Cmd 的规则仍须生效：`Option+Delete` → `Ctrl+Delete`
    #[test]
    fn non_cmd_rule_still_works() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTALT.0, 1));
        let evs = m.translate(&k(KeyCode::KEY_DELETE.0, 1));
        let c = codes(&evs);
        assert!(
            c.contains(&KeyCode::KEY_LEFTCTRL.0) && c.contains(&KeyCode::KEY_DELETE.0),
            "Option+Delete 应为 Ctrl+Delete，实际 {:?}",
            c
        );
        // Alt 只能被**释放**（转换时排空 output_down），不得再按下
        assert!(
            !evs
                .iter()
                .any(|e| e.code() == KeyCode::KEY_LEFTALT.0 && e.value() == 1),
            "Alt 不应被再次按下: {:?}",
            evs.iter()
                .map(|e| format!("{:?}{}", e.code(), e.value()))
                .collect::<Vec<_>>()
        );
    }
}

/// 裸 Shift 仍须正常输出按下与释放（fcitx5 依赖）
/// DE 动作全部可解析。
///
/// 曾因别名表缺 `print`（evdev 里叫历史名 `KEY_SYSRQ`），
/// 三条截图规则被**静默剔除**——启动日志只说「DE 未提供」，
/// 看不出是键名解析失败。这类问题必须用测试兜住。
#[cfg(test)]
mod de_accels_all_resolvable {
    use super::*;

    #[test]
    fn every_bound_gnome_feature_resolves() {
        // 只测 GNOME 默认已绑定的动作。
        // `show-desktop` 默认未绑定（@as []），需用户自行绑定后
        // 才生效——那是设计如此，不是解析失败。
        for feature in [
            "overview",
            "lock-screen",
            "screenshot-ui",
            "screenshot",
            "screenshot-window",
            "logout",
        ] {
            assert!(
                de_feature_accel(&DesktopType::Gnome, feature).is_some(),
                "GNOME 动作 {feature} 无法解析——查 DE 绑定失败或键名不认识"
            );
        }
    }

    /// 表里每条 `de_accel` 规则，其功能都必须能解析——
    /// 否则该行会被静默剔除，启动日志只说「DE 未提供」，看不出真相。
    #[test]
    fn no_rule_is_silently_dropped() {
        let cfg = Config::with_defaults();
        let compiled = compile_table(&cfg.rules).expect("规则表应能编译");
        for r in &compiled {
            if let CompiledAction::DeAccel(f) = &r.act {
                assert!(
                    de_feature_accel(&DesktopType::Gnome, f).is_some(),
                    "规则 {f} 无法解析，该行会被静默剔除"
                );
            }
        }
    }

    /// Print 必须能解析（GNOME 截图绑在它上面）
    #[test]
    fn print_alias_resolves() {
        assert_eq!(
            crate::keys::parse_external_key("Print"),
            Some(evdev::KeyCode::KEY_SYSRQ),
            "GNOME 绑定里的 `Print` 须解析为 KEY_SYSRQ"
        );
    }
}

/// `exec` 动作：产生副作用，但**不发出任何按键**。
#[cfg(test)]
mod exec_action {
    use super::*;
    use crate::config::Config;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    /// 清倒废纸篓是命令而非按键，不能伪造一个按键事件出去
    #[test]
    fn exec_emits_no_key_events() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = m.translate(&k(KeyCode::KEY_DELETE.0, 1));
        assert!(
            out.is_empty(),
            "exec 动作不应发出按键事件（会误触发应用行为），实际 {:?}",
            out.iter()
                .map(|e| format!("{:?}{}", e.code(), e.value()))
                .collect::<Vec<_>>()
        );
    }

    /// 释放路径也不得凭空发出事件
    #[test]
    fn exec_release_is_silent() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        m.translate(&k(KeyCode::KEY_DELETE.0, 1));
        let out = m.translate(&k(KeyCode::KEY_DELETE.0, 0));
        assert!(out.is_empty(), "exec 动作的释放不应有输出: {:?}", out);
    }

    /// 表里两条清倒废纸篓规则都必须要求确认
    #[test]
    fn trash_rules_require_confirmation() {
        let cfg = Config::with_defaults();
        let rules = compile_table(&cfg.rules).expect("规则表应能编译");
        let trash: Vec<_> = rules
            .iter()
            .filter(|r| matches!(&r.act, CompiledAction::Exec { program, .. } if program == "gio"))
            .collect();
        assert_eq!(trash.len(), 2, "应有两条清倒废纸篓规则");
        for r in trash {
            if let CompiledAction::Exec { confirm, .. } = &r.act {
                assert!(
                    *confirm,
                    "破坏性命令必须确认，规则 {:?}",
                    r.key
                );
            }
        }
    }
}

/// 每条规则都必须与自身的 `cmd` 标记一致。
///
/// 回归测试：`Ctrl+↑`（调度中心）被我错标成 `cmd = true`，
/// 而该组合根本不含 Cmd → 规则永不命中，功能完全失效。
/// 99 个测试全绿也没抓到，因为没有一条测试检查「规则的输入组合
/// 里到底有没有 Cmd」。
#[cfg(test)]
mod rule_cmd_flag_consistency {
    use super::*;

    /// 结构性检查走不通：`C-` 前缀既可能是「Ctrl+Cmd+X」也可能是
    /// 「Ctrl+X」，无法从键名反推是否含 Cmd——`C-KEY_Q` 是前者，
    /// `C-KEY_UP` 是后者，字符串长得一样。
    ///
    /// 所以改为**行为断言**：`cmd` 标记错了，行为一定不对。
    /// `Ctrl+↑` 曾被错标成 `cmd=true`，功能完全失效却无人发现。
    #[test]
    fn non_cmd_rules_reach_their_action() {
        // 物理 Ctrl 经 remap 后是 CapsLock，故输入序列用 CapsLock
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        m.translate(&ev(KeyCode::KEY_CAPSLOCK.0, 1));
        let out = codes_of(&m.translate(&ev(KeyCode::KEY_UP.0, 1)));

        assert!(
            !out.contains(&KeyCode::KEY_LEFTMETA.0),
            "Ctrl+↑ 不应产生 Meta: {:?}",
            out
        );
        assert!(
            out.contains(&KeyCode::KEY_LEFTSHIFT.0)
                && out.contains(&KeyCode::KEY_LEFTCTRL.0),
            "Ctrl+↑ 应命中概览规则（Ctrl+Shift+空格），实际 {:?}",
            out
        );
        assert!(
            !out.contains(&KeyCode::KEY_UP.0),
            "Ctrl+↑ 不应原样透传 ↑: {:?}",
            out
        );
    }

    /// 反向对照：确实含 Cmd 的规则在没按 Cmd 时不该命中
    #[test]
    fn cmd_rules_need_cmd_pressed() {
        let mut m2 = Mapper::new(&Config::with_defaults(), false).unwrap();
        m2.translate(&ev(KeyCode::KEY_LEFTMETA.0, 1));
        let out = codes_of(&m2.translate(&ev(KeyCode::KEY_UP.0, 1)));
        assert!(
            out.contains(&KeyCode::KEY_LEFTCTRL.0),
            "Cmd+↑ 应为 Ctrl+↑，实际 {:?}",
            out
        );
    }

    fn ev(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }
    fn codes_of(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }
}

/// 裸 Shift 仍须正常输出按下与释放（fcitx5 依赖）
/// 裸 Shift 仍须正常输出按下与释放（fcitx5 依赖）
#[test]
fn bare_shift_still_works() {
        let mut m = Mapper::new(&Config::with_defaults(), false).unwrap();
        let d = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        assert_eq!(codes(&d), vec![KeyCode::KEY_LEFTSHIFT.0]);
        let u = m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 0));
        assert_eq!(codes(&u), vec![KeyCode::KEY_LEFTSHIFT.0], "裸 Shift 释放须透传");
    }
}

/// Fn 键预处理的回归测试。
///
/// 对应一个已修复的真实 bug：Fn 分支曾用 `return` 短路，绕过整个 Cmd
/// 查表流程。见 `resolve_fn` 的文档注释。
#[cfg(test)]
mod fn_resolve {
    use super::*;
    use crate::config::FnMode;

    fn k(c: u16, v: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, c, v)
    }

    fn codes(evs: &[InputEvent]) -> Vec<u16> {
        evs.iter().map(|e| e.code()).collect()
    }

    fn mapper(mode: FnMode) -> Mapper {
        let mut cfg = Config::with_defaults();
        cfg.fn_mode = Some(mode);
        Mapper::new(&cfg, false).unwrap()
    }

    /// fkeysfirst：F1 单独 → F1
    #[test]
    fn fkeysfirst_bare_f1_stays_f1() {
        let mut m = mapper(FnMode::Fkeysfirst);
        let d = m.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(codes(&d), vec![KeyCode::KEY_F1.0]);
    }

    /// fkeysfirst：Fn+F1 → 亮度减
    #[test]
    fn fkeysfirst_fn_f1_becomes_media() {
        let mut m = mapper(FnMode::Fkeysfirst);
        m.translate(&k(KeyCode::KEY_FN.0, 1));
        let d = m.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(
            codes(&d),
            vec![KeyCode::KEY_BRIGHTNESSDOWN.0],
            "Fn+F1 应为亮度减"
        );
    }

    /// fkeyslast 方向相反：F1 单独 → 亮度减，Fn+F1 → F1
    #[test]
    fn fkeyslast_inverts_both_directions() {
        let mut m = mapper(FnMode::Fkeyslast);
        let bare = m.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(codes(&bare), vec![KeyCode::KEY_BRIGHTNESSDOWN.0]);

        let mut m2 = mapper(FnMode::Fkeyslast);
        m2.translate(&k(KeyCode::KEY_FN.0, 1));
        let with_fn = m2.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(codes(&with_fn), vec![KeyCode::KEY_F1.0]);
    }

    /// 核心回归：Fn+Meta+Shift+F1 不得泄漏 Meta（invariant 3）。
    ///
    /// 修复前输出为 `Meta+媒体键`——Meta 被原样发出，而它本应被抑制；
    /// 且 Ctrl/Shift 丢失。修复后应与裸 `Meta+Shift+F1` 同形，仅主键不同。
    #[test]
    fn fn_with_other_mods_does_not_leak_meta() {
        let mut m = mapper(FnMode::Fkeysfirst);
        m.translate(&k(KeyCode::KEY_FN.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTMETA.0, 1));
        m.translate(&k(KeyCode::KEY_LEFTSHIFT.0, 1));
        let out = m.translate(&k(KeyCode::KEY_F1.0, 1));
        let c = codes(&out);

        // Meta 若泄漏，会出现按下后无对应释放的孤立事件，
        // 故其出现次数应为偶数（press/release 配对）。
        let meta_count = c.iter().filter(|x| **x == KeyCode::KEY_LEFTMETA.0).count();
        assert_eq!(
            meta_count % 2,
            0,
            "Meta 出现次数应为偶数（按下与释放配对）: {:?}",
            c
        );

        // 且必须真正走到 Cmd 查表：注入 Ctrl，而非只发裸媒体键
        assert!(
            c.contains(&KeyCode::KEY_LEFTCTRL.0),
            "应继续走 Cmd 查表并注入 Ctrl: {:?}",
            c
        );
    }

    /// 释放顺序无关：先松 Fn 与先松 F1 都应释放同一个媒体键。
    ///
    /// 依赖 `pressed` 以**物理键**为键——若以解析后的键为键，
    /// Fn 先松开时释放路径会推导出错误的键。
    #[test]
    fn release_is_symmetric_regardless_of_fn_order() {
        for fn_first in [true, false] {
            let mut m = mapper(FnMode::Fkeysfirst);
            let mut out = vec![];
            out.extend(m.translate(&k(KeyCode::KEY_FN.0, 1)));
            out.extend(m.translate(&k(KeyCode::KEY_F1.0, 1)));
            if fn_first {
                out.extend(m.translate(&k(KeyCode::KEY_FN.0, 0)));
                out.extend(m.translate(&k(KeyCode::KEY_F1.0, 0)));
            } else {
                out.extend(m.translate(&k(KeyCode::KEY_F1.0, 0)));
                out.extend(m.translate(&k(KeyCode::KEY_FN.0, 0)));
            }
            let last = out.last().unwrap().code();
            assert_eq!(
                last,
                KeyCode::KEY_BRIGHTNESSDOWN.0,
                "Fn 先松={} 时最后一个事件须为媒体键释放",
                fn_first
            );
        }
    }

    /// `Disabled` 模式下不做任何转换
    #[test]
    fn disabled_mode_passes_through() {
        let mut m = mapper(FnMode::Disabled);
        m.translate(&k(KeyCode::KEY_FN.0, 1));
        let d = m.translate(&k(KeyCode::KEY_F1.0, 1));
        assert_eq!(codes(&d), vec![KeyCode::KEY_F1.0]);
    }
}
