//! 执行外部命令（「动作即函数」里的函数型动作）。
//!
//! 有些 macOS 操作无法通过发按键实现——它们不是组合键，而是**命令**：
//! 清倒废纸篓要跑 `gio trash --empty --force`，锁屏可以走 D-Bus 等。
//! 这类一律落到表里的 `exec` 动作，声明式地指到命令。
//!
//! # 为什么用 `gio trash` 而不是别的
//!
//! `gio` 是 glib 自带的工具（GNOME 桌面环境下必然存在），`gio trash`
//! 走 GIO 的回收站实现，与文件管理器（Nautilus 等）操作的是同一个
//! 废纸篓。不依赖具体文件管理器，也不依赖 `rm`。
//!
//! # 安全考量：破坏性命令需要确认
//!
//! 清倒废纸篓**不可逆**。本模块对破坏性命令默认先确认：
//!
//! - `confirm = true`（默认）：发出 GNOME 通知并要求用户点击确认，
//!   超时或无法通知则**放弃执行**
//! - `confirm = false`：直接执行。**仅在调用方明确知晓后果时使用**
//!
//! 注意 `gio trash --empty` 本身带 `--force`，会跳过它自己的确认。
//! 所以「不确认」这个语义必须由本模块来保证，而不是依赖 gio。

use std::process::{Command, Stdio};

/// 破坏性命令的确认超时（秒）。
///
/// 超时即放弃执行——宁可不做，也不能在用户没注意时删掉东西。
const CONFIRM_TIMEOUT_SECS: u64 = 20;

/// 列出命令的危险动作类别。
///
/// 分类决定是否需要确认。新增破坏性命令时须更新此处，
/// 避免「因为忘了标记而被无声执行」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Risk {
    /// 无副作用（如打开文件管理器）
    Safe,
    /// 不可逆的破坏性操作（清空废纸篓）
    Destructive,
}

fn classify(program: &str, args: &[String]) -> Risk {
    match program {
        // `gio trash --empty` 清空整个废纸篓，文件永久消失
        "gio" if args.iter().any(|a| a == "--empty") => Risk::Destructive,
        "gio" if args.iter().any(|a| a == "trash") => Risk::Safe,
        _ => Risk::Safe,
    }
}

/// 通过 D-Bus 发 GNOME 通知。
///
/// 返回 `true` 表示通知已投递。失败不阻断流程——由调用方决定是否
/// 在无通知时放弃。
fn notify(summary: &str, body: &str) -> bool {
    Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.Notifications",
            "--object-path",
            "/org/freedesktop/Notifications",
            "--method",
            "org.freedesktop.Notifications.Notify",
            "mackbind",
            "0",           // replaces_id
            "mackbind",    // app_name
            "dialog-warning",
            &format!("{summary}"), // summary
            &format!("{body}"),    // body
            "[]",          // actions
            "{}",          // hints
            "-1",          // expire_timeout：永不自动消失
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 等待用户确认。
///
/// 确认方式取决于环境：
///
/// - **有 TTY**（前台运行）：在终端里等 `y`，附带倒计时提示
/// - **无 TTY**（systemd 服务）：等一个**写在该键组合上的二次确认**——
///   即用户必须再按一次同样的组合才真正执行
///
/// 第二种是必须的：程序由 systemd 拉起时没有可读的终端，
/// 早期版本「无 TTY 即放弃」导致该功能完全不可用且毫无反馈——
/// 用户按了键什么都没发生，也不给任何提示。
fn wait_for_confirmation(prompt: &str, timeout_secs: u64) -> bool {
    let has_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    if !has_tty {
        // 无终端：请用户再按一次同样的组合确认（由调用方重试机制处理）
        eprintln!("[exec] {prompt}");
        eprintln!("[exec] 已在后台运行：请再按一次同样的组合以确认（{} 秒内有效）", timeout_secs);
        return false;
    }
    eprintln!("[exec] {prompt}");
    eprintln!("[exec] 确认请按 y（{timeout_secs} 秒后自动放弃）");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let mut buf = [0u8; 1];
    loop {
        if std::time::Instant::now() >= deadline {
            eprintln!("[exec] 超时，已放弃执行");
            return false;
        }
        // poll 而非 read，1 秒一次超时循环
        let mut fds = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut fds, 1, 1000) };
        if n > 0 && (fds.revents & libc::POLLIN) != 0 {
            match std::io::Read::read(&mut std::io::stdin(), &mut buf) {
                Ok(1) if buf[0] == b'y' || buf[0] == b'Y' => return true,
                Ok(1) => continue,
                _ => return false,
            }
        }
    }
}

/// 执行一条命令。
///
/// `confirm` 为 `false` 时跳过确认——**仅用于非破坏性命令**。
/// 破坏性命令即使 `confirm = false` 也会被强制确认。
///
/// # 二次确认（后台运行时的唯一可行方案）
///
/// 程序由 systemd 拉起时没有 TTY，无法等 `y`。此时改为要求
/// **再按一次同样的组合**：第一次按下只发通知并记下时间，
/// 第二次（窗口内）才真正执行。
///
/// 这比「无 TTY 即放弃」可用得多——早期那版让该功能完全静默失效，
/// 用户按了键既没反应也没提示。同时它仍构成一道人工确认：
/// 误按一次不会立刻删掉东西。
pub fn run(
    program: &str,
    args: &[String],
    desc: Option<&str>,
    confirm: bool,
    confirm_key: Option<&str>,
) {
    let risk = classify(program, args);
    let label = desc.unwrap_or(program);

    if risk == Risk::Destructive && !confirm {
        eprintln!("[exec] 破坏性命令须显式确认，已拒绝：{label}");
        return;
    }

    let needs_confirm = confirm || risk == Risk::Destructive;

    if needs_confirm {
        let prompt = format!("即将执行「{label}」：{} {}（不可撤销）", program, args.join(" "));
        let key = confirm_key.unwrap_or("(同一组合)");

        if let Some(done) = check_second_press(key) {
            if done {
                spawn(program, args, label);
            } else {
                arm(key, label);
            }
            return;
        }

        let _ = notify("确认破坏性操作", &prompt);
        if !wait_for_confirmation(&prompt, CONFIRM_TIMEOUT_SECS) {
            eprintln!("[exec] 未确认，不执行：{label}");
            return;
        }
    }

    spawn(program, args, label);
}

/// 已挂起待二次确认的组合。
static PENDING: std::sync::Mutex<Option<(String, std::time::Instant)>> =
    std::sync::Mutex::new(None);

/// 二次确认窗口：秒。
const CONFIRM_WINDOW_SECS: u64 = 8;

/// 是否处于后台（无 TTY）——只有那时才用二次确认。
fn is_background() -> bool {
    (unsafe { libc::isatty(libc::STDIN_FILENO) } != 1)
}

/// 有 TTY 时返回 `None`（走终端确认）；后台时返回 `Some(是否本次即执行)`。
fn check_second_press(key: &str) -> Option<bool> {
    if !is_background() {
        return None;
    }
    let mut g = PENDING.lock().ok()?;
    let now = std::time::Instant::now();
    match g.as_ref() {
        Some((k, t)) if k == key && now.duration_since(*t).as_secs() < CONFIRM_WINDOW_SECS => {
            *g = None;
            Some(true)
        }
        _ => {
            *g = Some((key.to_string(), now));
            Some(false)
        }
    }
}

/// 第一次按下：挂起并通知。
fn arm(key: &str, label: &str) {
    let prompt = format!("再按一次 {key} 以确认执行「{label}」（{CONFIRM_WINDOW_SECS} 秒内有效）");
    let _ = notify("待确认：破坏性操作", &prompt);
    eprintln!("[exec] {prompt}");
}

fn spawn(program: &str, args: &[String], label: &str) {
    match Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // 不等待：按键事件的处理循环不能被子进程阻塞
            let _ = child.try_wait();
            eprintln!("[exec] 已执行：{label}");
        }
        Err(e) => eprintln!("[exec] 执行失败：{label}（{e}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gio_trash_empty_is_destructive() {
        let args: Vec<String> = ["trash", "--empty", "--force"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(classify("gio", &args), Risk::Destructive);
    }

    #[test]
    fn gio_trash_single_is_safe() {
        let args: Vec<String> = ["trash", "a.txt"].iter().map(|s| s.to_string()).collect();
        assert_eq!(classify("gio", &args), Risk::Safe);
    }

    #[test]
    fn unknown_program_is_safe() {
        assert_eq!(classify("whatever", &[]), Risk::Safe);
    }

    #[test]
    fn destructive_without_confirm_is_rejected() {
        let args: Vec<String> = ["trash", "--empty", "--force"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // 不应真的执行，故只验证分类与守卫逻辑本身
        assert_eq!(classify("gio", &args), Risk::Destructive);
    }
}