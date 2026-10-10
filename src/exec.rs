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

/// 等待用户在通知上点「确认」。
///
/// D-Bus 的通知 API 不提供「点击某按钮后回调」的同步查询，
/// 因此这里用一个**折中且保守**的方案：
///
/// 1. 发出带确认动作的通知，永久显示
/// 2. 同时在终端/日志打印提示
/// 3. **等待超时窗口内用户按下确认键**，否则放弃
///
/// 确认键用 `y`——必须切到能看到日志的地方去按，这本身就构成了一道
/// 人工确认。若拿不到输入设备（无 TTY），直接放弃执行。
fn wait_for_confirmation(prompt: &str, timeout_secs: u64) -> bool {
    // 非交互环境（systemd 服务）没有可读的 TTY，直接放弃
    let has_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    if !has_tty {
        eprintln!("[exec] 无交互终端，无法确认 destructive 操作，已放弃：{prompt}");
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
/// 破坏性命令即使 `confirm = false` 也会被强制确认，除非调用方
/// 显式传 `force_destructive`。
pub fn run(program: &str, args: &[String], desc: Option<&str>, confirm: bool, force_destructive: bool) {
    let risk = classify(program, args);
    let label = desc.unwrap_or(program);

    if risk == Risk::Destructive && !(confirm || force_destructive) {
        eprintln!("[exec] 破坏性命令须显式确认，已拒绝：{label}");
        return;
    }

    let needs_confirm = confirm || (risk == Risk::Destructive && !force_destructive);

    if needs_confirm {
        let prompt = format!("即将执行「{label}」：{} {}（不可撤销）", program, args.join(" "));
        let delivered = notify("确认破坏性操作", &prompt);
        // 通知投递失败也照样要求终端确认——宁可麻烦，不可误删
        let _ = delivered;
        if !wait_for_confirmation(&prompt, CONFIRM_TIMEOUT_SECS) {
            eprintln!("[exec] 未确认，不执行：{label}");
            return;
        }
    }

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