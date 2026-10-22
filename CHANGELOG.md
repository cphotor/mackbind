# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **The version number has a single source of truth: the `version` field in
> [`Cargo.toml`](Cargo.toml).** The startup log reads it at compile time, the
> badge in `README.md` reads it from the git tag, and this file records released
> versions. `scripts/audit.py` fails if the newest heading below disagrees with
> `Cargo.toml`, so the number cannot drift.

## [0.1.4] - 2026-10-22

### Changed

- `[terminal] shift_enter` is now a simple `on` / `off` switch, defaulting to
  `on`. `"ctrl_j"` is still accepted as a legacy alias.
- Removed `shift_enter_apps`. Users should not have to maintain a list of
  terminal app ids to enable a behavior; the built-in VTE list is maintained by
  the tool.

## [0.1.3] - 2026-10-22

### Added

- `swap_capslock_ctrl` lets users disable the paired CapsLock ↔ Left Ctrl swap
  without writing identity remaps. `remap` continues to add or override
  individual directions.
- `[terminal] shift_enter = "ctrl_j" | "off"` controls the VTE `Shift+Enter`
  behavior. `shift_enter_apps` replaces the built-in terminal list, and an
  explicit empty list disables the rule.
- The README and `config.example.toml` now document all three user-selectable
  behaviors: Fn priority (`fn_mode`), the CapsLock swap, and terminal
  `Shift+Enter`.

### Changed

- User `[[rules]]` are now prepended before the built-in table, so they actually
  override a default row with the same `(app, keys)`. Previously they were
  appended after the defaults and shadowed by the first match.
- `MACKBIND_CONFIG` is now read last, matching the documented "highest
  priority" behavior.

### Fixed

- `[desktop] type` and `[desktop] spotlight` were silently ignored when merging
  config files. They now merge field by field, so a user config can override
  either one without resetting the other.

## [0.1.2] - 2026-10-22

### Fixed

- `Shift+Enter` → `Ctrl+J` now works in the whole mainstream VTE family, not
  just Ptyxis. GNOME Terminal, GNOME Console, Tilix, Terminator, Guake, Xfce
  Terminal, MATE Terminal, LXTerminal, Sakura, Tilda and BlackBox are matched by
  their Wayland app ids, so terminal agent TUIs receive a newline instead of
  submitting the prompt.

## [0.1.1] - 2026-10-14

### Added

- `Shift+Enter` → `Ctrl+J` inside terminals that cannot report `Shift+Enter`.
  VTE (gnome-terminal, ptyxis, Tilix, Terminator, Guake) implements neither the
  kitty keyboard protocol nor `modifyOtherKeys`, so `Shift+Enter` and
  `Ctrl+Enter` arrive as the same `\r` (0x0D) as bare `Enter`, and an agent TUI
  submits instead of inserting a newline. `Ctrl+J` is a distinct control
  character that every terminal transmits unchanged, and readline, vim and the
  TUIs all read it as a newline. The row is scoped per terminal, so terminals
  that do report `Shift+Enter` keep it.
- `install.sh` installs the GNOME Shell focused-window extension the per-app
  rules need, resolving the bundle from `gnome-shell --version` instead of
  pinning one version. GNOME sessions only; skip it with `--no-extension`.

### Changed

- `--verbose` no longer writes `Cmd+` on the handful of rows that set
  `cmd = false`, which read as if Cmd were required.

### Fixed

- Per-app rules silently never matched on a fresh GNOME install: with no
  focused-window extension the window class is always unknown, so every
  app-scoped rule fell through to the generic `Cmd → Ctrl` behavior — inside a
  terminal that meant `Cmd+C` sent `Ctrl+C` (SIGINT) instead of copying.
- `install.sh --no-service` no longer leaves the service stopped after replacing
  the binary, which looked exactly like "the mapping is broken".
- `--trace` now queries the focused window. Without it the window class stayed
  empty, so trace reported pass-through for app-scoped rules that do work in
  production — evidence that sends a diagnosis down the wrong path.

## [0.1.0] - 2026-10-14

First public release.

### Added

- CapsLock ↔ Left Ctrl swap, with both keys emitting a real Ctrl in the GUI and
  the terminal.
- `Cmd+X` → `Ctrl+X` by default, with a rule table for the exceptions. Long-tail
  combos not listed anywhere still work.
- `Cmd+Space` → overview: reads the desktop environment's existing binding and
  emits the same key. On GNOME 50 that action is unbound by default, so it falls
  back to a single mutter `overlay-key` tap.
- Per-key terminal specialization: `Cmd+C` → `Ctrl+Shift+C` (copy) inside a
  terminal, while `Ctrl+C` still interrupts. Bindings are read from each
  terminal's own GSettings schema, so custom bindings are followed too.
- Fn-key priority (`fn_mode`) handled in user space, with no kernel parameters
  or root required.
- Delete family: `Option+Delete` → `Ctrl+Delete`, `Cmd+Delete` → move to trash,
  and the two empty-trash rules implemented through the new `exec` action.
- Screenshot keys, log out, and `Ctrl+↑` mission control rules.
- `--trace` diagnostic mode: prints "input event → what should have been
  emitted" to stderr without grabbing the device, so the keyboard stays native.
- `--check` config validation, `--list-devices`, and `-v/--verbose` (prints the
  whole rule table at startup).
- One-shot `install.sh` / `uninstall.sh`, and a udev rule that opens up
  `/dev/uinput`.
- `scripts/audit.py`, which machine-checks `docs/macos-shortcuts.md` against the
  code instead of relying on a human to compare them.

### Changed

- Refactored to a table-driven design: six separate mechanisms merged into a
  single rule table. `default_rules()` in `src/config.rs` is now the single
  source of truth for mappings.

### Fixed

- Bare keypresses no longer trigger Cmd rules (a bare Space used to match
  `Cmd+Space` and open the overview, swallowing the following keystrokes).
- Two "Super stuck" bugs — one in the pass-through path and one in the release
  path — that could leave Super held down, for example after locking the screen.
  Now backed by three exhaustive tests.
- A 90-second hang on shutdown: `libc::signal()` implies `SA_RESTART`, so the
  blocking `read()` was restarted and the loop never saw `SHUTDOWN`.
- All delete-class rules were dead because macOS's "Delete" key is actually
  `KEY_BACKSPACE`.
- The `Cmd+Delete` target, its confirmation mechanism (now a modal `zenity`
  dialog), and the configurability of that dialog's text and buttons.
- Empty-trash is now scoped to Nautilus, and confirmation was removed for the
  Option variant to match macOS.
- `Ctrl+Cmd+X` was being wrongly rewritten by the default semantics.
- Fn preprocessing no longer bypasses the Cmd table lookup.

### Removed

- Tests that physically opened the "empty trash" dialog during `cargo test`.
- A Python bytecode cache that had been committed by accident.

[0.1.4]: https://github.com/cphotor/mackbind/releases/tag/v0.1.4
[0.1.3]: https://github.com/cphotor/mackbind/releases/tag/v0.1.3
[0.1.2]: https://github.com/cphotor/mackbind/releases/tag/v0.1.2
[0.1.1]: https://github.com/cphotor/mackbind/releases/tag/v0.1.1
[0.1.0]: https://github.com/cphotor/mackbind/releases/tag/v0.1.0
