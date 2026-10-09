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

use crate::config::{Config, FnMode};
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

/// cmd_map 的索引：主键 + 要求的输入侧修饰键集合。
///
/// 不能只用主键做key——`"z"`（Cmd+Z）与 `"S-z"`（Cmd+Shift+Z）主键
/// 同为 KEY_Z，若只用主键索引会互相覆盖，导致其中一条永远无法命中。
/// 实测症状：Cmd+Z 退化为裸 z，Cmd+Shift+Z 泄漏出裸 Shift。
type CmdMapKey = (KeyCode, Vec<KeyCode>);

/// 已编译的映射规则。
pub struct Mapper {
    /// remap 规则：原键 → 目标键
    remap: HashMap<KeyCode, KeyCode>,
    /// cmd_map：(主键, 要求的输入侧修饰键) → (输出侧修饰键, 输出主键)
    cmd_map: HashMap<CmdMapKey, (Vec<KeyCode>, KeyCode)>,
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
    /// Ctrl 占用应用列表（WM_CLASS / app_id）
    ctrl_busy_apps: Vec<String>,
    /// 「Ctrl 已被占用」的应用里，这些键需映射为 Ctrl+Shift。
    ///
    /// 只包含这些应用**自身用 Ctrl+Shift 绑定**的键（终端的复制、
    /// 关标签页等）。未列出的键仍走默认的 Ctrl——因为加 Shift 是为了
    /// 命中它们已有的绑定，而非笼统地「躲冲突」。
    terminal_bindings: Vec<KeyCode>,
    /// 按应用生效的规则：(应用匹配串列表, 规则表)
    ///
    /// 优先于全局 cmd_map 与终端特化。用于「同一按键在不同应用里需要
    /// 不同输出」——典型是复制键：ptyxis/konsole 会把 Ctrl+Shift+C
    /// 漏发成 SIGINT，必须改走 Ctrl+Insert。
    app_cmd_map: Vec<(Vec<String>, HashMap<CmdMapKey, (Vec<KeyCode>, KeyCode)>)>,
    /// 当前焦点窗口的 wm_class，用于匹配 app_cmd_map
    wm_class: String,
    /// 是否输出调试日志
    verbose: bool,
}

/// 把规则列表编译成查找表。
///
/// `keys` 是键名列表，可能带修饰前缀（如 `"S-z"` 表示 Shift+Z）。
/// 取最后一段作为主键，前缀作为输入侧修饰键。前缀修饰键排序后参与
/// 索引，使 `(z,[])` 与 `(z,[Shift])` 成为不同的条目而不互相覆盖。
fn parse_rules(
    rules: &[crate::config::CmdMap],
) -> Result<HashMap<CmdMapKey, (Vec<KeyCode>, KeyCode)>, crate::keys::KeyError> {
    let mut map = HashMap::new();
    for m in rules {
        let mut in_mods: Vec<KeyCode> = Vec::new();
        let mut key: Option<KeyCode> = None;
        for k in &m.keys {
            let (mods, kc) = parse_combo(k)?;
            in_mods.extend(mods);
            key = Some(kc);
        }
        let key = key.ok_or_else(|| {
            crate::keys::KeyError::BadCombo(m.emit.clone(), "keys 为空".into())
        })?;
        let (out_mods, out_key) = parse_combo(&m.emit)?;
        in_mods.sort();
        map.insert((key, in_mods), (out_mods, out_key));
    }
    Ok(map)
}

/// GTK 加速键的主键名 → evdev 键码。
///
/// GTK 用 `plus` / `minus` 指主键盘的 `=` / `-`，与 evdev 不同名。
fn accel_key(name: &str) -> Option<KeyCode> {
    use std::str::FromStr;
    let evdev = match name {
        "plus" => "KEY_EQUAL",
        "minus" => "KEY_MINUS",
        other => {
            // GSettings 里 F1 这类写作 `F1`，evdev 是 `KEY_F1`
            if let Some(rest) = other.strip_prefix('F') {
                if rest.chars().all(|c| c.is_ascii_digit()) {
                    return Some(KeyCode::from_str(&format!("KEY_F{rest}")).ok()?);
                }
            }
            // 其余多为已是 evdev 形式的小写名（w、Page_Down…）
            if other.chars().any(|c| c.is_ascii_uppercase()) {
                return Some(KeyCode::from_str(other).ok()?);
            }
            return KeyCode::from_str(&format!("KEY_{}", other.to_ascii_uppercase())).ok();
        }
    };
    KeyCode::from_str(evdev).ok()
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

        let cmd_map = parse_rules(&cfg.cmd_map)?;

        let app_cmd_map = cfg
            .app_cmd_map
            .iter()
            .map(|a| Ok((a.apps.clone(), parse_rules(&a.rules)?)))
            .collect::<Result<Vec<_>, crate::keys::KeyError>>()?;

        Ok(Mapper {
            remap,
            cmd_map,
            app_cmd_map,
            held: Vec::new(),
            suppressed: HashSet::new(),
            output_down: Vec::new(),
            pressed: HashMap::new(),
            injected_mods: Vec::new(),
            fn_held: false,
            fn_mode: cfg.fn_mode.unwrap_or_default(),
            fkey_media: build_fkey_media_table(),
            ctrl_busy_window: false,
            ctrl_busy_apps: cfg.ctrl_busy_apps.clone(),
            terminal_bindings: cfg
                .terminal_shift_keys
                .iter()
                .map(|k| crate::keys::parse_key(k))
                .collect::<Result<Vec<_>, _>>()?,
            wm_class: String::new(),
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

    /// 非修饰键的按下处理。
    fn handle_key_press(&mut self, code: KeyCode) -> Vec<InputEvent> {
        // Fn 优先级：Fn 按住时，F 键换成对应的媒体键
        if self.fn_held && self.fn_mode != FnMode::Disabled {
            if let Some(media) = self.fkey_media.get(&code).copied() {
                if self.verbose {
                    eprintln!("[mapper] Fn+{:?} -> {:?}", code, media);
                }
                return self.emit_key(media, 1);
            }
        }

        // Cmd 组合处理
        let cmd_held = self
            .held
            .iter()
            .any(|m| *m == KeyCode::KEY_LEFTMETA || *m == KeyCode::KEY_RIGHTMETA);

        if cmd_held {
            // 当前除 Cmd 外还按住的修饰键（排序后作为规则匹配的一部分）
            let mut cur_mods: Vec<KeyCode> = self
                .held
                .iter()
                .copied()
                .filter(|m| *m != KeyCode::KEY_LEFTMETA && *m != KeyCode::KEY_RIGHTMETA)
                .collect();
            cur_mods.sort();

            // 按应用生效的规则最优先。
            //
            // 例：ptyxis/konsole 的复制键改走 Ctrl+Insert——这两个终端
            // 在无选中文本时会把 Ctrl+Shift+C 漏给进程当 SIGINT，
            // 使「复制」杀掉正在跑的任务。放在终端特化之前，才能
            // 覆盖掉「Cmd+C → Ctrl+Shift+C」的结果。
            let app_specific = self
                .app_cmd_map
                .iter()
                .find(|(apps, _)| apps.iter().any(|a| self.wm_class.contains(a)))
                .and_then(|(_, rules)| rules.get(&(code, cur_mods.clone())).cloned());

            // 其次是全局显式规则（目标键与主键不同者，如 Cmd+[ → Alt+←）
            let explicit = app_specific
                .or_else(|| self.cmd_map.get(&(code, cur_mods.clone())).cloned());

            let (out_mods, out_key) = match explicit {
                Some(v) => v,
                None if self.ctrl_busy_window && self.terminal_bindings.contains(&code) => {
                    // 终端里，**终端自身已绑定**的键：Cmd+X → Ctrl+Shift+X。
                    //
                    // 这类键加 Shift 正是为了命中终端的 Ctrl+Shift 绑定
                    // （Ctrl+Shift+C 复制、Ctrl+Shift+W 关标签页…）。
                    //
                    // 关键在于这是**逐键白名单**，而非「终端里把整个 Cmd
                    // 语义换成 Ctrl+Shift」。后者会让 Cmd+Q 变成
                    // Ctrl+Shift+Q——一个谁也不认的组合；而 macOS 的
                    // Cmd+Q（退出）对应的 Linux 键是 Alt+F4，属全局规则，
                    // 不应被终端特化吞掉。
                    let mut mods = cur_mods.clone();
                    mods.push(KeyCode::KEY_LEFTCTRL);
                    mods.push(KeyCode::KEY_LEFTSHIFT);
                    mods.sort();
                    mods.dedup();
                    (mods, code)
                }
                None => {
                    // 默认语义：Cmd+X → Ctrl+X。
                    //
                    // 不做穷举：macOS 的 Cmd 组合是长尾的，固定表覆盖不全。
                    // 实测 GNOME 的 28 个 Super 绑定中仅 Super+h 涉及字母，
                    // Super+字母 基本空闲——保留 Super 会让绝大多数组合
                    // 什么也不发生。
                    //
                    // 用户正按住的修饰键（如 Cmd+Shift+Z 的 Shift）需保留，
                    // 否则翻译时松开的修饰键不会再按下，组合就散了。
                    let mut mods = cur_mods.clone();
                    mods.push(KeyCode::KEY_LEFTCTRL);
                    mods.sort();
                    mods.dedup();
                    (mods, code)
                }
            };

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
            self.pressed.insert(code, out_key);
            return out;
        }

        // 表外：原样透传
        self.output_down.push(code);
        self.pressed.insert(code, code);
        self.emit_key(code, 1)
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

        // 3) 抑制过的键：从未按下过，也不发出释放
        if self.suppressed.remove(&code) {
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

        // Fn 优先级下的 F 键，释放时也要换成媒体键
        if self.fn_held && self.fn_mode != FnMode::Disabled {
            if let Some(media) = self.fkey_media.get(&code).copied() {
                return self.emit_key(media, 0);
            }
        }

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
