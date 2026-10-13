//! mackbind — reproduces macOS key habits on Linux while preserving native
//! Linux modifier semantics.
//!
//! # Design highlights
//!
//! The fundamental difference from tools like Kinto/Toshy is **pass-through by
//! default, look up the table only for exceptions**:
//!
//! - `Cmd` always stays `KEY_LEFTMETA` (Linux's Super), **with no global rewrite**.
//!   Only combos that hit the rule table (such as `Cmd+C`) are translated to Ctrl;
//!   combos outside the table pass through unchanged and are left for the desktop
//!   environment or the app to interpret.
//!
//! This preserves the macOS structure where "Cmd and Ctrl stand side by side". If
//! Cmd were globally mapped to Ctrl as Toshy does, the two modifiers would point
//! at the same slot, combos like `Ctrl+Cmd+Space` would lose all their meaning,
//! and Ctrl behavior in the GUI and in the terminal would be forced to diverge.
//!
//! # Safety measures
//!
//! Grabbing a keyboard device is a privileged operation, and an abnormal exit
//! leaves the keyboard unusable. Therefore:
//!
//! - A failed grab errors out and exits immediately rather than hanging silently
//! - Every exit path (normal end, Ctrl+C, SIGTERM, panic) releases the grab
//! - All pressed keys are released before exit so modifiers are not left down

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

/// Set when a signal arrives; the main loop uses it to exit gracefully.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Install signal handling: on Ctrl+C, SIGTERM, SIGHUP set `SHUTDOWN`, and the
/// main loop exits.
///
/// **Must use `sigaction` with `sa_flags = 0`, not `libc::signal()` directly.**
///
/// `signal()` has BSD semantics and implies `SA_RESTART`: after the handler
/// returns, the kernel automatically restarts the `read()` it interrupted. But
/// the main loop is blocked in `fetch_events()`, so `read()` would never return,
/// the `SHUTDOWN` check at the top of the loop would never run — and we could
/// only wait for systemd to SIGKILL after 90 seconds. `Cleanup::drop` would then
/// never run, leaving modifiers held down on the virtual device with nothing to
/// release them.
///
/// With `SA_RESTART` removed, `read()` returns `EINTR` and the loop can get back
/// to the top. The cost is that the main loop must distinguish `Interrupted`
/// (continue) from a genuine read error (exit).
fn install_signal_handlers() {
    // Convert to a function pointer first, then to an integer: casting a function
    // item directly to an integer has no guaranteed ABI.
    let handler: extern "C" fn(libc::c_int) = handle_signal;
    let handler = handler as libc::sighandler_t;

    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler;
            libc::sigemptyset(&mut action.sa_mask);
            // Key: no SA_RESTART, so the blocking read() returns EINTR.
            action.sa_flags = 0;
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

/// RAII guard: on Drop it releases the grab and releases all pressed keys.
///
/// This is the last line of defense against an unusable keyboard — even if a
/// panic happens, stack unwinding still runs Drop, guaranteeing the device is
/// never left in the grabbed state.
struct Cleanup {
    device: Option<Device>,
    mapper_keys: Vec<KeyCode>,
    virtual_device: Option<VirtualDevice>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        // First release all pressed keys so modifiers are not left on the output side
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
                eprintln!("[exit] released {} pressed keys", events.len());
            }
        }
        if let Some(dev) = self.device.as_mut() {
            match dev.ungrab() {
                Ok(()) => eprintln!("[exit] grab released, keyboard back to normal"),
                Err(e) => eprintln!("[exit] warning: ungrab failed ({e}), replug the keyboard or reboot"),
            }
        }
    }
}

/// Decide whether a device is a keyboard: it must support basic keys like A-Z.
fn looks_like_keyboard(dev: &Device) -> bool {
    match dev.supported_keys() {
        Some(keys) => keys.contains(KeyCode::KEY_A) && keys.contains(KeyCode::KEY_Z),
        None => false,
    }
}

/// List all keyboard devices, for `--list-devices` and troubleshooting.
fn list_devices() {
    println!("{:<52} {}", "device name", "path");
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
mackbind — macOS key habits for Linux

Usage:
    mackbind [options]

Options:
    -l, --list-devices    list detected keyboard devices and exit
    -v, --verbose         print per-event debug logs
    -c, --check           only validate the config file, do not grab a device
    -h, --help            show this help

Environment variables:
    MACKBIND_CONFIG       config file path (highest priority)
    MACKBIND_VERBOSE      equivalent to --verbose
    MACKBIND_DEVICE       device path, skipping automatic selection

Config lookup order (later overrides earlier):
    /etc/mackbind/config.toml
    ~/.config/mackbind/config.toml

Runs with no configuration at all — the built-in default mapping already
provides the CapsLock↔Ctrl swap and common macOS shortcuts."
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

    // The version is defined in exactly one place: Cargo.toml's `[package]
    // version`. Read it at compile time, so no hand-copied string can drift from it.
    println!("[version] mackbind v{}", env!("CARGO_PKG_VERSION"));

    // Config: a parse failure must exit loudly, never silently run with defaults.
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[config] {e}");
            std::process::exit(1);
        }
    };
    println!("[config] {}", cfg.summary());
    if verbose {
        // The desc field is by nature "for humans only"; print it under verbose,
        // otherwise a description that can never be seen may as well not exist.
        for r in &cfg.remap {
            if let Some(d) = &r.desc {
                println!("[config]   remap {} → {}{}", r.from, r.to,
                    if r.desc.is_some() { format!("   # {d}") } else { String::new() });
            }
        }
        // Print the whole rule table — not just the rows that have a desc.
        //
        // This is the only authoritative view of "what it actually does": how
        // many rows the table has and what each governs, all on one screen. desc
        // is for humans only; a missing desc does not affect the listing.
        println!("[config] rule table ({} rows, Cmd implicit):", cfg.rules.len());
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
                    // Strip the KEY_ prefix from the main key and lowercase it:
                    // `KEY_DELETE` → `delete`, `KEY_LEFTBRACE` → `leftbrace`
                    let k = key.strip_prefix("KEY_").unwrap_or(key).to_lowercase();
                    s.push_str(&k);
                    s
                }
                crate::config::Action::DeAccel { feature } => format!("ReadDE({feature})"),
                crate::config::Action::TerminalCtrlShift => {
                    "Ctrl+Shift (if the app binds it so)".to_string()
                }
                crate::config::Action::Exec {
                    program,
                    args,
                    confirm,
                    ..
                } => format!(
                    "exec {}{}{}",
                    program,
                    if args.is_empty() { "" } else { " " },
                    args.join(" ")
                ) + if *confirm { " (needs confirmation)" } else { "" },
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
            eprintln!("[config] rule parse failed: {e}");
            std::process::exit(1);
        }
    };

    if flag("-c") || flag("--check") {
        println!("[check] config and rules validated");
        return;
    }

    // Register signal handling: on Ctrl+C, SIGTERM, SIGHUP exit gracefully and
    // release the grab.
    install_signal_handlers();

    // --trace: observe only, do not take over.
    //
    // **Does not grab, does not create a virtual device, does not inject any
    // event** — the keyboard keeps its fully native behavior and the user can
    // still type normally. At the same time it records "input event → the event
    // that should have been emitted" to stderr, to diagnose the mapping logic.
    //
    // Why it exists: once the mapping is active the keyboard is owned by this
    // program, and when something goes wrong the user can no longer type, so
    // there is no way to observe the scene. Trace mode sidesteps exactly that.
    if std::env::args().any(|a| a == "--trace") {
        run_trace(&target_from_env());
    }

    // Pick the target device
    let target = match std::env::var("MACKBIND_DEVICE") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => match pick_keyboard() {
            Some(p) => p,
            None => {
                eprintln!("[fatal] no keyboard device found. Use --list-devices to see available devices.");
                std::process::exit(1);
            }
        },
    };
    println!("[device] {}", target.display());

    // Build the virtual device. It must declare the full key set, or the system
    // may not recognize it as a keyboard.
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
                eprintln!("[fatal] failed to create virtual keyboard: {e}");
                uinput_permission_hint();
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("[fatal] failed to open uinput: {e}");
            uinput_permission_hint();
            std::process::exit(1);
        }
    };
    println!("[uinput] virtual keyboard created");

    // Exclusive grab. Exit immediately on failure — must never hang here.
    let mut dev = match Device::open(&target) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[fatal] failed to open device: {e}");
            eprintln!("        If it says Device or resource busy, another program (such as Toshy) is using it.");
            std::process::exit(1);
        }
    };
    if let Err(e) = dev.grab() {
        eprintln!("[fatal] failed to grab device: {e}");
        eprintln!("        If it says busy, first stop programs like Toshy that use the keyboard.");
        eprintln!("        The keyboard is unaffected and works normally.");
        std::process::exit(1);
    };
    println!("[grab] grabbed, mapping active");

    // Actively emit **release** events for all modifiers, clearing any residual
    // pressed state.
    //
    // Why this is needed: if mackbind (or any program) previously left a modifier
    // stuck down, GNOME records it on the **compositor side**. Restarting mackbind
    // only destroys the virtual device; the compositor does not guarantee it
    // clears automatically — so space becomes Cmd+Space, W becomes Cmd+W, and
    // pressing physical Cmd does not help either (this program suppresses Meta's
    // press and release, so GNOME receives no event to correct its own state).
    //
    // Emitting a release with "no matching press" is a safe no-op for libinput.
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
    // Print the PID and how to exit. Ctrl+C is unavailable — this program takes
    // over Ctrl and Cmd, so the keyboard can no longer produce SIGINT; an
    // explicit alternative exit channel must be provided.
    println!("[ready] mapping active (PID {})", std::process::id());
    println!("        stop: systemctl --user stop mackbind");
    println!();

    let mut cleanup = Cleanup {
        device: Some(dev),
        mapper_keys: Vec::new(),
        virtual_device: Some(vdev),
    };

    // Main loop
    let mut ev_count: u64 = 0;
    // Last queried focused window, to avoid re-parsing the .desktop on the key hot path
    let mut last_focus: Option<String> = None;
    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            break;
        }

        let events: Vec<InputEvent> = match cleanup.device.as_mut().unwrap().fetch_events() {
            Ok(iter) => iter.collect(),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // Interrupted by a signal: loop back, check SHUTDOWN, then read.
                // Must not break as a fatal error — receiving SIGTERM also lands here.
                continue;
            }
            Err(e) => {
                eprintln!("[error] failed to read events: {e}");
                break;
            }
        };
        let n_events = events.len();

        for batch in events.split(|e| e.event_type() == EventType::SYNCHRONIZATION) {
            if batch.is_empty() {
                continue;
            }

            // Lazily query the focused window: only when a key event arrives.
            //
            // "Cmd → Ctrl+Shift" needs to know whether the current focused
            // window is a terminal or a Ctrl-using app. Focus does not change
            // often, so there is no need for constant polling; only input
            // requires the decision.
            //
            // Skip set_window_id when wm_class is the same as last time — it
            // parses .desktop entries internally, which should not run repeatedly
            // on the key hot path.
            if batch.iter().any(|e| e.event_type() == EventType::KEY) {
                if let Some(wm_class) = focus::focused_wm_class() {
                    if last_focus.as_deref() != Some(wm_class.as_str()) {
                        last_focus = Some(wm_class.clone());
                        mapper.set_window_id(&wm_class);
                    }
                }
            }

            // Map the whole batch, then inject it in one shot.
            //
            // Key point: do not emit event by event. Every `emit()` automatically
            // appends a SYN_REPORT, so sending event by event would split one
            // physical key sequence into multiple sync reports. Input methods
            // (fcitx5/ibus) treat SYN as the batch boundary that determines "one
            // input is finished"; extra SYNs make it submit j and k as two
            // separate inputs — which shows up as double pinyin not working and
            // only English letters coming out.
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
                // Record currently pressed keys, to release them on exit
                for e in &out {
                    if e.event_type() == EventType::KEY && e.value() == 1 {
                        cleanup.mapper_keys.push(KeyCode(e.code()));
                    }
                }
                if let Some(v) = cleanup.virtual_device.as_mut() {
                    if let Err(err) = v.emit(&out) {
                        eprintln!("[error] failed to inject events: {err}");
                    }
                }
            }
        }

        // Brief sleep when there are no events, to avoid busy-waiting on CPU.
        if n_events == 0 {
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    let n = cleanup.mapper_keys.len();
    drop(cleanup); // trigger cleanup
    eprintln!("\n[exit] processed {ev_count} events, released {n} pressed keys");
}

/// Automatically pick a keyboard: prefer a built-in Apple keyboard, otherwise the first one.
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

/// Full keyboard capability set.
///
/// Pick the device for trace mode to observe (no grab, read only).
fn target_from_env() -> std::path::PathBuf {
    match std::env::var("MACKBIND_DEVICE") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => pick_keyboard().unwrap_or_else(|| {
            eprintln!("[fatal] no keyboard device found");
            std::process::exit(1);
        }),
    }
}

/// trace mode: read events, run them through the mapping, print the result to
/// stderr, do not inject.
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
            eprintln!("[config] rule parse failed: {e}");
            std::process::exit(1);
        }
    };
    let mut dev = match Device::open(target) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[fatal] failed to open device: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("[trace] observing {} (no grab, no injection)", target.display());
    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            eprintln!("[trace] exit signal received, finishing");
            // This function's signature is `-> !`, so use exit rather than break
            // to keep the "never returns" guarantee.
            std::process::exit(0);
        }
    let events: Vec<InputEvent> = match dev.fetch_events() {
        Ok(iter) => iter.collect(),
        // Interrupted by a signal: loop back, check SHUTDOWN, then read (see install_signal_handlers)
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
        Err(e) => {
            eprintln!("[fatal] failed to read events: {e}");
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
            if ev.value() == 1 { "down" } else if ev.value() == 0 { "up" } else { "repeat" },
            outs.iter()
                .map(|o| format!("{}{}", desc(o.code()),
                    if o.value() == 1 { "+" } else if o.value() == 0 { "-" } else { "=" }))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    }

}

/// Troubleshooting hint when `/dev/uinput` fails to open.
///
/// This deliberately distinguishes distributions: Arch/EndeavourOS udev already
/// opens `/dev/uinput` by default and it is only blocked by "not being in the
/// input group", whereas Debian/Ubuntu default to `root:root 0600` — **joining
/// the input group is not enough**; this repo's udev rule is also required. Only
/// hinting "join the input group" would make Ubuntu users try repeatedly in vain.
fn uinput_permission_hint() {
    eprintln!("        /dev/uinput must be readable and writable. Check:");
    eprintln!("          · are you in the input group: id -nG");
    eprintln!("          · Debian/Ubuntu default to root-only write, requiring an extra udev rule:");
    eprintln!("            sudo install -m644 packaging/99-mackbind.rules \\");
    eprintln!("              /etc/udev/rules.d/99-mackbind.rules");
    eprintln!("            sudo udevadm control --reload && sudo udevadm trigger");
    eprintln!("            then log out and back in. See the README 'Requirements' section.");
}

/// If the virtual device created by uinput does not declare a key, the system
/// ignores that key's events. So all evdev key codes are listed here.
/// Relative axis capability declaration.
///
/// Even for keyboard mapping only, REL_X/Y/WHEEL/HWHEEL must be declared.
/// Without it, the kernel classifies the virtual device as an rfkill device
/// (`Handlers=... rfkill`), libinput then takes a completely different parsing
/// path, and the input method may not receive the full key sequence.
///
/// This follows the existing implementation in Toshy/Kinto (`real_uinput()` in
/// `output.py`).
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
    // Iterate over all variants of KeyCode and fill the set. If the uinput
    // virtual device does not declare a key, the system ignores that key's
    // events, so the full set must be declared.
    // evdev's KeyCode is an ArrayedEvdevEnum, buildable in bulk with FromIterator.
    // evdev key codes are numbered consecutively from 0 (KEY_CNT is the total),
    // so iterating the whole range suffices.
    (0..=0x2ff).map(KeyCode::new).collect()
}