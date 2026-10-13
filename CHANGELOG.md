# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **The version number has a single source of truth: the `version` field in
> [`Cargo.toml`](Cargo.toml).** The startup log reads it at compile time, the
> badge in `README.md` reads it from the git tag, and this file records released
> versions. `scripts/audit.py` fails if the newest heading below disagrees with
> `Cargo.toml`, so the number cannot drift.

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

[0.1.0]: https://github.com/cphotor/mackbind/releases/tag/v0.1.0
