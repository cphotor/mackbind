//! mackbind — 在 Linux 上复现 macOS 键位习惯，同时保留 Linux 原生修饰键语义。
//!
//! # 设计要点
//!
//! 与 Kinto/Toshy 一类工具的根本区别在于**默认透传、查表例外**：
//!
//! - `Cmd` 始终保持 `KEY_LEFTMETA`（即 Linux 的 Super），**不做全局改写**。
//!   只有命中映射表的组合（如 `Cmd+C`）才翻译为 Ctrl；表外组合原样透传，
//!   交由桌面环境或应用自行解释。
//!
//! 这保留了 macOS 上「Cmd 与 Ctrl 并列」的结构。若像 Toshy 那样把 Cmd
//! 全局映射为 Ctrl，两个修饰键会指向同一槽位，`Ctrl+Cmd+Space` 这类组合
//! 语义将全部丢失，GUI 与终端下的 Ctrl 行为也会被迫分叉。
//!
//! # 安全措施
//!
//! 抓取键盘设备是特权操作，异常退出会导致键盘失灵。因此：
//!
//! - 抓取失败立即报错退出，不静默挂起
//! - 任何退出路径（正常结束、Ctrl+C、SIGTERM、panic）都释放 grab
//! - 退出前释放所有按下的键，避免修饰键残留在按下状态

mod accel;
mod config;
mod desktop;
mod exec;
mod focus;
mod keys;
mod mapper;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, Device, EventType, InputEvent, KeyCode, RelativeAxisCode};
use mapper::Mapper;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 收到信号时置位，主循环据此优雅退出。
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// RAII guard：Drop 时释放 grab 并松开所有按下的键。
///
/// 这是防止键盘失灵的最后一道防线——即使发生 panic，栈展开也会执行
/// Drop，从而保证设备不会停留在被独占状态。
struct Cleanup {
    device: Option<Device>,
    mapper_keys: Vec<KeyCode>,
    virtual_device: Option<VirtualDevice>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        // 先松开所有按下的键，避免修饰键残留在输出侧
        if let Some(vdev) = self.virtual_device.as_mut() {
            let mut events = Vec::new();
            for k in &self.mapper_keys {
                events.push(InputEvent::new(
                    EventType::KEY.0,
                    k.0,
                    0,
                ));
            }
            if !events.is_empty() {
                let _ = vdev.emit(&events);
                eprintln!("[exit] 已松开 {} 个按下的键", events.len());
            }
        }
        if let Some(dev) = self.device.as_mut() {
            match dev.ungrab() {
                Ok(()) => eprintln!("[exit] grab 已释放，键盘恢复正常"),
                Err(e) => eprintln!("[exit] 警告：ungrab 失败 ({e})，请重新插拔键盘或重启"),
            }
        }
    }
}

/// 判断设备是否为键盘：需支持 A-Z 这类基础按键。
fn looks_like_keyboard(dev: &Device) -> bool {
    match dev.supported_keys() {
        Some(keys) => keys.contains(KeyCode::KEY_A) && keys.contains(KeyCode::KEY_Z),
        None => false,
    }
}

/// 列出所有键盘设备，便于 `--list-devices` 与排障。
fn list_devices() {
    println!("{:<52} {}", "设备名", "路径");
    println!("{}", "-".repeat(80));
    for (path, dev) in evdev::enumerate() {
        if looks_like_keyboard(&dev) {
            println!(
                "{:<52} {}",
                dev.name().unwrap_or("<no name>"),
                path.display()
            );
        }
    }
}

fn print_usage() {
    println!(
        "\
mackbind — macOS 键位习惯 for Linux

用法：
    mackbind [选项]

选项：
    -l, --list-devices    列出检测到的键盘设备后退出
    -v, --verbose         输出逐事件调试日志
    -c, --check           仅校验配置文件，不抓取设备
    -h, --help            显示此帮助

环境变量：
    MACKBIND_CONFIG       指定配置文件路径（最高优先级）
    MACKBIND_VERBOSE      等价于 --verbose
    MACKBIND_DEVICE       指定设备路径，跳过自动选择

配置查找顺序（后者覆盖前者）：
    /etc/mackbind/config.toml
    ~/.config/mackbind/config.toml

无需任何配置即可运行——内置默认映射已提供 CapsLock↔Ctrl 互换
与常用 macOS 快捷键。"
    );
}


fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().any(|a| a == name);

    if flag("-h") || flag("--help") {
        print_usage();
        return;
    }
    if flag("-l") || flag("--list-devices") {
        list_devices();
        return;
    }

    let verbose = flag("-v") || flag("--verbose") || std::env::var("MACKBIND_VERBOSE").is_ok();

    // 配置：解析失败必须大声退出，不能以默认配置静默运行。
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[config] {e}");
            std::process::exit(1);
        }
    };
    println!("[config] {}", cfg.summary());
    if verbose {
        // desc 字段本就是「仅供人阅读」，在 verbose 下打出来，
        // 否则写了说明却永远看不到，等于没写。
        for r in &cfg.remap {
            if let Some(d) = &r.desc {
                println!("[config]   remap {} → {}{}", r.from, r.to,
                    if r.desc.is_some() { format!("   # {d}") } else { String::new() });
            }
        }
        // 打印整张规则表——不只是带 desc 的那些。
        //
        // 这是「到底做了什么」的唯一权威视图：表里有几行、每行管什么，
        // 一屏可见。desc 仅供人阅读，缺失不影响列出。
        println!("[config] 规则表（{} 行，Cmd 隐含）:", cfg.rules.len());
        for r in &cfg.rules {
            let app = if r.apps.is_empty() {
                "*".to_string()
            } else {
                r.apps.join(",")
            };
            let act = match &r.act {
                crate::config::Action::Emit { mods, key } => {
                    let mut s = String::new();
                    for m in mods {
                        for seg in m.split('-') {
                            let name = match seg {
                                "C" => "Ctrl+",
                                "A" => "Alt+",
                                "S" => "Shift+",
                                "M" => "Super+",
                                o => {
                                    s.push_str(o);
                                    s.push('+');
                                    ""
                                }
                            };
                            s.push_str(name);
                        }
                    }
                    // 主键去掉 KEY_ 前缀并小写：`KEY_DELETE` → `delete`、
                    // `KEY_LEFTBRACE` → `leftbrace`
                    let k = key.strip_prefix("KEY_").unwrap_or(key).to_lowercase();
                    s.push_str(&k);
                    s
                }
                crate::config::Action::DeAccel { feature } => format!("读DE({feature})"),
                crate::config::Action::TerminalCtrlShift => {
                    "Ctrl+Shift（若应用如此绑定）".to_string()
                }
                crate::config::Action::Exec {
                    program,
                    args,
                    confirm,
                    ..
                } => format!(
                    "执行 {}{}{}",
                    program,
                    if args.is_empty() { "" } else { " " },
                    args.join(" ")
                ) + if *confirm { "（需确认）" } else { "" },
            };
            println!(
                "[config]   {:<28} {:<20} → {}{}",
                format!("{app}"),
                format!("Cmd+{}", r.keys.join("+")),
                act,
                r.desc.as_deref().map(|d| format!("   # {d}")).unwrap_or_default()
            );
        }
    }

    let mut mapper = match Mapper::new(&cfg, verbose) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[config] 规则解析失败: {e}");
            std::process::exit(1);
        }
    };

    if flag("-c") || flag("--check") {
        println!("[check] 配置与规则校验通过");
        return;
    }

    // 注册信号处理：Ctrl+C 与 SIGTERM 时优雅退出并释放 grab。
    //
    // 先转成 extern "C" 函数指针再转整数：直接把函数项转成整数会
    // 被 lint 拦下（函数项与整数之间的隐式转换没有对应的 ABI 保证）。
    let handler: extern "C" fn(libc::c_int) = handle_signal;
    let handler = handler as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }

    // --trace：只观察，不接管。
    //
    // **不 grab、不创建虚拟设备、不注入任何事件**——键盘完全保持原生
    // 行为，用户可以正常打字。同时把「输入事件 → 本应发出的事件」
    // 记录到 stderr，用于诊断映射逻辑。
    //
    // 存在的理由：映射一旦生效，键盘就由本程序接管，出现问题时
    // 用户无法再打字，也就没有办法观察现场。trace 模式绕开这一点。
    if std::env::args().any(|a| a == "--trace") {
        run_trace(&target_from_env());
    }

    // 选择目标设备
    let target = match std::env::var("MACKBIND_DEVICE") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => match pick_keyboard() {
            Some(p) => p,
            None => {
                eprintln!("[fatal] 未找到键盘设备。用 --list-devices 查看可用设备。");
                std::process::exit(1);
            }
        },
    };
    println!("[device] {}", target.display());

    // 建虚拟设备。必须声明完整按键集，否则系统可能不认它是键盘。
    let keys = full_keyboard_capabilities();
    let mut vdev = match VirtualDevice::builder() {
        Ok(b) => match b
            .name("mackbind (virtual)")
            .with_keys(&keys)
            .and_then(|b| b.with_relative_axes(&relative_axes()))
            .and_then(|b| b.build())
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[fatal] 创建虚拟键盘失败: {e}");
                eprintln!("        请确认 /dev/uinput 存在，且当前用户在 input 组中。");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("[fatal] 无法打开 uinput: {e}");
            eprintln!("        请确认 /dev/uinput 存在，且当前用户在 input 组中。");
            std::process::exit(1);
        }
    };
    println!("[uinput] 虚拟键盘已创建");

    // 独占抓取。失败立即退出——绝不能在此挂起。
    let mut dev = match Device::open(&target) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[fatal] 打开设备失败: {e}");
            eprintln!("        若提示 Device or resource busy，说明已有其他程序（如 Toshy）占用。");
            std::process::exit(1);
        }
    };
    if let Err(e) = dev.grab() {
        eprintln!("[fatal] 抓取设备失败: {e}");
        eprintln!("        若提示 busy，请先停止 Toshy 等占用键盘的程序。");
        eprintln!("        键盘未受影响，可正常使用。");
        std::process::exit(1);
    };
    println!("[grab] 已独占，映射已生效");

    // 主动发出所有修饰键的**释放**事件，清除可能残留的按下态。
    //
    // 为什么需要：若此前 mackbind（或任何程序）让某个修饰键卡在按下态，
    // GNOME 会把它记在**合成器侧**。重启 mackbind 只销毁了虚拟设备，
    // 合成器并不保证自动清除——于是空格变 Cmd+空格、W 变 Cmd+W，
    // 且按物理 Cmd 也无济于事（本程序抑制了 Meta 的按下与释放，
    // GNOME 收不到任何事件来纠正自己的状态）。
    //
    // 「无对应的按下」而发释放，对 libinput 是安全的空操作。
    for key in [
        evdev::KeyCode::KEY_LEFTMETA,
        evdev::KeyCode::KEY_RIGHTMETA,
        evdev::KeyCode::KEY_LEFTCTRL,
        evdev::KeyCode::KEY_RIGHTCTRL,
        evdev::KeyCode::KEY_LEFTSHIFT,
        evdev::KeyCode::KEY_RIGHTSHIFT,
        evdev::KeyCode::KEY_LEFTALT,
        evdev::KeyCode::KEY_RIGHTALT,
        evdev::KeyCode::KEY_CAPSLOCK,
    ] {
        let _ = vdev.emit(&[evdev::InputEvent::new(
            evdev::EventType::KEY.0,
            key.0,
            0,
        )]);
    }
    // 打印 PID 与退出方式。Ctrl+C 不可用——本程序占用了 Ctrl 与 Cmd，
    // 键盘已无法产生 SIGINT，必须提供明确的替代退出通道。
    println!("[ready] 映射已生效（PID {}）", std::process::id());
    println!("        停止：systemctl --user stop mackbind");
    println!();

    let mut cleanup = Cleanup {
        device: Some(dev),
        mapper_keys: Vec::new(),
        virtual_device: Some(vdev),
    };

    // 主循环
    let mut ev_count: u64 = 0;
    // 上次查询到的焦点窗口，用于避免在按键热路径上重复解析 .desktop
    let mut last_focus: Option<String> = None;
    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            break;
        }

        let events: Vec<InputEvent> = match cleanup.device.as_mut().unwrap().fetch_events() {
            Ok(iter) => iter.collect(),
            Err(e) => {
                eprintln!("[error] 读取事件失败: {e}");
                break;
            }
        };
        let n_events = events.len();

        for batch in events.split(|e| e.event_type() == EventType::SYNCHRONIZATION) {
            if batch.is_empty() {
                continue;
            }

            // 懒查询焦点窗口：仅在收到按键时才查。
            //
            // 「Cmd → Ctrl+Shift」需知道当前焦点窗口是否终端或 Ctrl
            // 占用应用。焦点变化不频繁，无需常驻轮询；有输入才需要判断。
            //
            // wm_class 与上次相同时跳过 set_window_id——它内部要解析
            // .desktop 条目，不宜在按键热路径上重复执行。
            if batch.iter().any(|e| e.event_type() == EventType::KEY) {
                if let Some(wm_class) = focus::focused_wm_class() {
                    if last_focus.as_deref() != Some(wm_class.as_str()) {
                        last_focus = Some(wm_class.clone());
                        mapper.set_window_id(&wm_class);
                    }
                }
            }

            // 整批映射后一次性注入。
            //
            // 关键：不能逐事件 emit。`emit()` 每次都会自动补一个
            // SYN_REPORT，若逐事件发送，一次物理按键序列会被拆成多个
            // 同步报告。输入法（fcitx5/ibus）以 SYN 为批次边界判定
            // 「一次输入结束」，收到多余 SYN 就会把 j、k 当作两次独立
            // 输入直接提交——表现为双拼音打不出来、只出英文字母。
            let mut out: Vec<InputEvent> = Vec::new();
            for ev in batch {
                ev_count += 1;
                if verbose {
                    let in_desc = match ev.event_type() {
                        EventType::KEY => format!("{:?} {}", KeyCode(ev.code()), ev.value()),
                        t => format!("{t:?}"),
                    };
                    eprint!("[in {ev_count:>6}] {in_desc:<26} ");
                }
                out.extend(mapper.translate(ev));
            }

            if verbose {
                let out_desc: Vec<String> = out
                    .iter()
                    .map(|e| format!("{:?} {}", KeyCode(e.code()), e.value()))
                    .collect();
                eprintln!("-> [{}]", out_desc.join(", "));
            }

            if !out.is_empty() {
                // 记录当前按下的键，供退出时松开
                for e in &out {
                    if e.event_type() == EventType::KEY && e.value() == 1 {
                        cleanup.mapper_keys.push(KeyCode(e.code()));
                    }
                }
                if let Some(v) = cleanup.virtual_device.as_mut() {
                    if let Err(err) = v.emit(&out) {
                        eprintln!("[error] 注入事件失败: {err}");
                    }
                }
            }
        }

        // 无事件时短暂休眠，避免忙等占满 CPU。
        if n_events == 0 {
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    let n = cleanup.mapper_keys.len();
    drop(cleanup); // 触发清理
    eprintln!("\n[exit] 共处理 {ev_count} 个事件，松开 {n} 个按下的键");
}

/// 自动选择键盘：优先内置 Apple 键盘，否则取第一个。
fn pick_keyboard() -> Option<std::path::PathBuf> {
    let mut fallback = None;
    for (path, dev) in evdev::enumerate() {
        if !looks_like_keyboard(&dev) {
            continue;
        }
        let name = dev.name().unwrap_or("");
        if name.contains("Apple Internal Keyboard") {
            return Some(path);
        }
        if fallback.is_none() {
            fallback = Some(path);
        }
    }
    fallback
}

/// 完整的键盘能力集。
///
/// 选定 trace 模式要观察的设备（不 grab，仅读取）。
fn target_from_env() -> std::path::PathBuf {
    match std::env::var("MACKBIND_DEVICE") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => pick_keyboard().unwrap_or_else(|| {
            eprintln!("[fatal] 未找到键盘设备");
            std::process::exit(1);
        }),
    }
}

/// trace 模式：读入事件，跑一遍映射，把结果打到 stderr，不注入。
fn run_trace(target: &std::path::Path) -> ! {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[config] {e}");
            std::process::exit(1);
        }
    };
    let mut mapper = match Mapper::new(&cfg, false) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[config] 规则解析失败: {e}");
            std::process::exit(1);
        }
    };
    let mut dev = match Device::open(target) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[fatal] 打开设备失败: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("[trace] 观察 {}（不 grab、不注入）", target.display());
    loop {
    let events: Vec<InputEvent> = match dev.fetch_events() {
        Ok(iter) => iter.collect(),
        Err(e) => {
            eprintln!("[fatal] 读取事件失败: {e}");
            std::process::exit(1);
        }
    };
    for ev in events {
        if ev.event_type() != EventType::KEY {
            continue;
        }
        let outs = mapper.translate(&ev);
        let desc = |c: u16| {
            format!("{:?}", evdev::KeyCode(c))
        };
        eprintln!(
            "[trace] IN {:<22} {} -> OUT [{}]",
            desc(ev.code()),
            if ev.value() == 1 { "按下" } else if ev.value() == 0 { "松开" } else { "重复" },
            outs.iter()
                .map(|o| format!("{}{}", desc(o.code()),
                    if o.value() == 1 { "+" } else if o.value() == 0 { "-" } else { "=" }))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    }

}

/// uinput 创建的虚拟设备若不声明某按键，系统会忽略该键的事件。
/// 因此这里列出 evdev 的全部键码。
/// 相对轴能力声明。
///
/// 即便只做键盘映射，也需声明 REL_X/Y/WHEEL/HWHEEL。缺了它，内核会把
/// 虚拟设备归类成 rfkill 设备（`Handlers=... rfkill`），libinput 随即
/// 走完全不同的解析路径，输入法可能收不到完整的按键序列。
///
/// 这点参考了 Toshy/Kinto 的既有实现（`output.py` 的 `real_uinput()`）。
fn relative_axes() -> AttributeSet<RelativeAxisCode> {
    [
        RelativeAxisCode::REL_X,
        RelativeAxisCode::REL_Y,
        RelativeAxisCode::REL_HWHEEL,
        RelativeAxisCode::REL_WHEEL,
        RelativeAxisCode::REL_Z,
    ]
    .into_iter()
    .collect()
}

fn full_keyboard_capabilities() -> AttributeSet<KeyCode> {
    // 遍历 KeyCode 的全部变体填入集合。uinput 虚拟设备若不声明某按键，
    // 系统会直接忽略该键的事件，因此必须声明完整集合。
    // evdev 的 KeyCode 是 ArrayedEvdevEnum，可用 FromIterator 批量构造。
    // evdev 按键码从 0 连续编号（KEY_CNT 为总数），遍历整个区间即可。
    (0..=0x2ff).map(KeyCode::new).collect()
}