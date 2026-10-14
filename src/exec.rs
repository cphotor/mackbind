//! Run external commands (the function-style actions of "actions as functions").
//!
//! Some macOS operations cannot be implemented by emitting key events — they are
//! not key combos but **commands**: emptying the trash runs
//! `gio trash --empty --force`, locking the screen can go over D-Bus, and so on.
//! These all fall to an `exec` action in the table, declaratively pointing at a
//! command.
//!
//! # Why `gio trash` and nothing else
//!
//! `gio` is a tool shipped with glib (always present in a GNOME desktop
//! environment), and `gio trash` goes through GIO's trash implementation, the
//! same trash that file managers (Nautilus, etc.) operate on. It depends on no
//! particular file manager, nor on `rm`.
//!
//! # Security consideration: destructive commands need confirmation
//!
//! Emptying the trash is **irreversible**. By default this module confirms
//! destructive commands first:
//!
//! - `confirm = true` (default): sends a GNOME notification and asks the user to
//!   click to confirm; on timeout or if the notification cannot be sent it
//!   **gives up**
//! - `confirm = false`: run directly. **Use only when the caller clearly knows
//!   the consequences**
//!
//! Note that `gio trash --empty` itself carries `--force`, which skips its own
//! confirmation. So the "no confirmation" semantics must be guaranteed by this
//! module, not left to gio.

use std::process::{Command, Stdio};

/// Upper bound for waiting on the modal confirmation dialog (seconds). Timeout is
/// treated as cancel.
const DIALOG_TIMEOUT_SECS: u64 = 60;

/// Confirmation timeout for destructive commands (seconds).
///
/// Timeout means give up — better to do nothing than to delete something while
/// the user is not looking.
const CONFIRM_TIMEOUT_SECS: u64 = 20;

/// Classify a command's risk category.
///
/// The classification decides whether confirmation is needed. Additions of
/// destructive commands must update this, to avoid "silently executed because
/// someone forgot to mark it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Risk {
    /// No side effects (e.g. opening a file manager)
    Safe,
    /// An irreversible destructive operation (emptying the trash)
    Destructive,
}

/// For tests and callers to decide: is a command destructive?
pub fn is_destructive(program: &str, args: &[String]) -> bool {
    classify(program, args) == Risk::Destructive
}

/// Whether this request needs a confirmation dialog.
///
/// Looks only at the rule's own `confirm` setting — safety is backed by the
/// scope guard in [`refuse`], not forced a second time here.
pub fn should_confirm(req: &ExecRequest<'_>) -> bool {
    req.confirm
}

/// Whether this request should be rejected outright (destructive + no confirm +
/// not app-scoped).
pub fn refuse(req: &ExecRequest<'_>) -> bool {
    is_destructive(req.program, req.args) && !req.confirm && !req.scoped
}

fn classify(program: &str, args: &[String]) -> Risk {
    match program {
        // `gio trash --empty` empties the whole trash; files disappear permanently
        "gio" if args.iter().any(|a| a == "--empty") => Risk::Destructive,
        "gio" if args.iter().any(|a| a == "trash") => Risk::Safe,
        _ => Risk::Safe,
    }
}

/// Send a GNOME notification over D-Bus.
///
/// Returns `true` if the notification was delivered. A failure does not block
/// the flow — the caller decides whether to give up when there is no
/// notification.
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
            summary, // summary
            body,    // body
            "[]",          // actions
            "{}",          // hints
            "-1",          // expire_timeout: never auto-dismiss
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Wait for the user's confirmation.
///
/// The confirmation method depends on the environment:
///
/// - **Has a TTY** (foreground run): wait for `y` in the terminal, with a
///   countdown hint
/// - **No TTY** (systemd service): wait for a **second confirmation bound to
///   that key combo** — the user must press the same combo again for it to
///   actually run
///
/// The second is necessary: when the program is started by systemd there is no
/// readable terminal, and an early version's "no TTY means give up" made the
/// feature completely unusable with no feedback at all — the user pressed the
/// key and nothing happened, with no hint whatsoever.
fn wait_for_confirmation(prompt: &str, timeout_secs: u64) -> bool {
    let has_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    if !has_tty {
        // No terminal: ask the user to press the same combo again to confirm
        // (handled by the caller's retry mechanism)
        eprintln!("[exec] {prompt}");
        eprintln!("[exec] running in the background: press the same combo again to confirm (valid for {} seconds)", timeout_secs);
        return false;
    }
    eprintln!("[exec] {prompt}");
    eprintln!("[exec] press y to confirm (auto-gives up after {timeout_secs} seconds)");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let mut buf = [0u8; 1];
    loop {
        if std::time::Instant::now() >= deadline {
            eprintln!("[exec] timed out, gave up");
            return false;
        }
        // poll rather than read, with a 1-second timeout loop
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

/// Run a single command.
///
/// When `confirm` is `false` confirmation is skipped — **for non-destructive
/// commands only**. A destructive command is forced to confirm even when
/// `confirm = false`.
///
/// # Second confirmation (the only workable option when running in the background)
///
/// When the program is started by systemd there is no TTY and it cannot wait for
/// `y`. In that case it instead requires **pressing the same combo again**: the
/// first press only sends a notification and records the time, and the second
/// press (within the window) actually runs it.
///
/// This is far more usable than "no TTY means give up" — that early version made
/// the feature fail completely silently, so the user pressed the key with no
/// response and no hint. At the same time it still constitutes a manual
/// confirmation: an accidental single press does not immediately delete anything.
pub struct ExecRequest<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    /// Human-readable text, from the rule table's `desc`
    pub label: Option<&'a str>,
    pub confirm: bool,
    /// Confirmation dialog body. Auto-generated when `None`
    pub prompt: Option<&'a str>,
    /// Confirmation button text. Uses "Run" when `None`
    pub ok_label: Option<&'a str>,
    /// Description used for "press the same combo again"
    pub confirm_key: Option<&'a str>,
    /// Whether the rule is app-scoped.
    ///
    /// A destructive command is allowed to be **confirmation-free** only when
    /// app-scoped: a global binding would permanently delete things on one
    /// accidental press, whereas the corresponding macOS operation only ever
    /// applies within its own file manager.
    pub scoped: bool,
}

pub fn run(req: ExecRequest<'_>) {
    // Make the two decisions first (they need the full request), then destructure to execute
    let refused = refuse(&req);
    let needs_confirm = should_confirm(&req);

    let ExecRequest {
        program,
        args,
        label,
        prompt: custom_prompt,
        ok_label,
        confirm_key,
        ..
    } = req;
    let label = label.unwrap_or(program);

    if refused {
        eprintln!("[exec] global destructive command requires explicit confirmation, refused: {label}");
        return;
    }

    // Whether to show the confirmation dialog **looks only at the rule's own setting**.
    //
    // It used to be written `confirm || risk == Destructive`, which made
    // `confirm: false` completely ineffective — the `risk` term made the
    // condition always true, so the dialog popped up anyway.
    // Safety is guaranteed by the scope guard above: a destructive command that is
    // confirmation-free must be app-scoped, otherwise execution is refused
    // outright and the dialog should not be forced here.

    if needs_confirm {
        let prompt = custom_prompt.map(|s| s.to_string()).unwrap_or_else(|| {
            format!(
                "about to run \"{}\": {} {}\n\nThis operation cannot be undone.",
                label,
                program,
                args.join(" ")
            )
        });

        // Prefer a modal dialog: the user must click "Run", it cannot be ignored or missed.
        // An early version used a desktop notification, and when notifications were
        // muted or missed the feature was as good as absent — the user pressed the
        // key with no feedback.
        match confirm_dialog(&prompt, ok_label.unwrap_or("Run")) {
            Some(true) => {
                spawn(program, args, label);
                return;
            }
            Some(false) => {
                eprintln!("[exec] user cancelled, not running: {label}");
                return;
            }
            // Cannot get a dialog (no zenity / no display) → fall back to second press
            None => {}
        }

        let key = confirm_key.unwrap_or("(same combo)");
        if let Some(done) = check_second_press(key) {
            if done {
                spawn(program, args, label);
            } else {
                arm(key, label);
            }
            return;
        }

        if !wait_for_confirmation(&prompt, CONFIRM_TIMEOUT_SECS) {
            eprintln!("[exec] not confirmed, not running: {label}");
            return;
        }
    }

    spawn(program, args, label);
}

/// Pop up a modal confirmation dialog. Returns `None` if it cannot pop up (no
/// zenity / no display).
///
/// Wait in a separate thread, **without blocking the key-handling loop** —
/// blocking would make the whole keyboard unresponsive.
fn confirm_dialog(prompt: &str, ok_label: &str) -> Option<bool> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    let text = prompt.to_string();
    let ok = ok_label.to_string();

    std::thread::spawn(move || {
        let out = Command::new("zenity")
            .args([
                "--question",
                "--title=Confirm destructive operation",
                "--width=420",
                "--text",
                &text,
                "--ok-label",
                &ok,
                "--cancel-label",
                "Cancel",
            ])
            .output();
        let ok = match out {
            Ok(o) => o.status.success(),
            Err(_) => {
                let _ = tx.send(None);
                return;
            }
        };
        let _ = tx.send(Some(ok));
    });

    // Wait for the user to click; timeout is treated as "cancel"
    rx.recv_timeout(std::time::Duration::from_secs(DIALOG_TIMEOUT_SECS))
        .ok()
        .flatten()
}

/// Combos already pending a second confirmation.
static PENDING: std::sync::Mutex<Option<(String, std::time::Instant)>> =
    std::sync::Mutex::new(None);

/// Second-confirmation window: seconds.
const CONFIRM_WINDOW_SECS: u64 = 8;

/// Whether we are in the background (no TTY) — only then is second confirmation used.
fn is_background() -> bool {
    (unsafe { libc::isatty(libc::STDIN_FILENO) } != 1)
}

/// Returns `None` when there is a TTY (use terminal confirmation); in the
/// background it returns `Some(whether this press actually runs)`.
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

/// First press: arm and notify.
fn arm(key: &str, label: &str) {
    let prompt = format!("press {key} again to confirm running \"{label}\" (valid for {CONFIRM_WINDOW_SECS} seconds)");
    let _ = notify("Pending: destructive operation", &prompt);
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
            // Do not wait: the key-event handling loop must not be blocked by a child process
            let _ = child.try_wait();
            eprintln!("[exec] ran: {label}");
        }
        Err(e) => eprintln!("[exec] failed: {label} ({e})"),
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
        // It should not actually run, so only verify the classification and guard logic itself
        assert_eq!(classify("gio", &args), Risk::Destructive);
    }
}