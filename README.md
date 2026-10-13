# mackbind

[![version](https://img.shields.io/github/v/tag/cphotor/mackbind)](CHANGELOG.md)

Reproduce macOS keybindings on Linux while keeping native Linux modifier semantics fully intact.

## Why it exists

macOS and Linux modifiers are **identical at the hardware level**, and the kernel reports them faithfully:

|Physical position|macOS|Linux keycode|
|---|---|---|
|1st from left|Ctrl|`KEY_LEFTCTRL`|
|2nd from left|Option|`KEY_LEFTALT`|
|3rd from left|Command|`KEY_LEFTMETA`|
|Leftmost on the Tab row|CapsLock|`KEY_CAPSLOCK`|

Existing tools (Toshy, Kinto) implement `Cmd+C` → `Ctrl+C` with a single global rewrite: map `KEY_LEFTMETA` to `KEY_RIGHT_CTRL`, **unconditionally**. That has three knock-on effects:

1. **Cmd loses its Super meaning.** On macOS, Cmd and Ctrl are two parallel system modifiers, and combos like `Ctrl+Cmd+Space` and `Ctrl+Cmd+F` are meaningful. After the rewrite both point at the same slot, so all of those combos lose their meaning.
2. **The `LEFT_CTRL` slot is vacated** to house Super, so behavior diverges between the GUI and the terminal.
3. **Ctrl habits in the GUI break.** In VSCode with a vim plugin, physical Ctrl actually emits `Super+V`, so vim never sees `Ctrl+V`.

mackbind's model: **`Cmd` *is* `Ctrl`; the rule table only covers exceptions.** By default it maps `Cmd+X` to `Ctrl+X` (preserving native Linux Ctrl behavior) and only consults the table when the target key differs from the main key — `Cmd+Q` → `Alt+F4`, `Cmd+[` → `Alt+←`. This preserves both native Ctrl behavior and macOS semantics.

## Features

Works out of the box, no configuration needed:

- **CapsLock ↔ Left Ctrl swap** — both keys emit a real Ctrl in both GUI and terminal
- **`Cmd+X` defaults to `Ctrl+X`** — no enumeration; long-tail combos just work (`Cmd+E`, `Cmd+B`, and any combo absent from the table)
- **14 explicit rules** — for cases where the target key differs from the main key: `Cmd+Q` → `Alt+F4`, cursor movement, `Cmd+[` `Cmd+]` → `Alt+←` `Alt+→`, etc.
- **`Cmd+Space` → overview** — prefers the desktop environment's existing binding and emits the same key; since GNOME 50 that action is unbound by default, so it emits a single `overlay-key` tap (a bare Super). Never rewrites your settings
- **Per-key terminal specialization** — inside a terminal `Cmd+C` → `Ctrl+Shift+C` (copy), while `Ctrl+C` still interrupts. GNOME-family terminals are read from their own GSettings schema, so custom bindings are followed too
- **Fn key priority** — toggles F-keys vs. brightness/media keys in user space, with no kernel parameters or root required

## Known issues

### Unimplemented shortcuts

|macOS|Linux|Status|
|---|---|---|
|`Cmd+Space` Spotlight|GNOME overview|✅ reads GNOME's existing `toggle-overview` binding; since GNOME 50 that action is unbound by default, so it emits an `overlay-key` tap|
|`Cmd+Tab` / `Cmd+H`|Super combos|**Deferred.** Super on Linux means the Windows key and is not equivalent to macOS Cmd|
|Show desktop|`show-desktop`|The action exists but is unbound by default; implementing it requires writing to the user account settings|
|`Shift+Cmd+Delete` empty trash|`gio trash`|Not implemented|

Full per-case status is in the "Implementation status" section of `docs/macos-shortcuts.md`.

### Requires a GNOME Shell extension

"Per-app behavior" needs to know which window is focused. On Wayland GNOME does not hand window information to clients; measured on this machine:

|Approach|Result|
|---|---|
|`org.gnome.Shell.Eval`|returns `(false, '')`, restricted since GNOME 41|
|`org.gnome.Shell.WindowTracker`|the object path does not exist|
|`xdotool` / `wmctrl`|unavailable under Wayland|

So the [`focused-window-dbus`](https://extensions.gnome.org/extension/focused-window-dbus/) extension is required:

```bash
gnome-extensions install focused-window-dbus@flexagoon.com
```

When the extension is absent the related features degrade to default behavior (not treated as an error); all other mappings keep working.

To verify the extension is actually available:

```bash
gdbus call --session --dest org.gnome.Shell \
  --object-path /org/gnome/shell/extensions/FocusedWindow \
  --method org.gnome.shell.extensions.FocusedWindow.Get
```

It should return JSON containing `wm_class`. Note that `--dest` must be `org.gnome.Shell`: the extension exports its object directly from the Shell process and does not own a separate bus name; using `org.gnome.Shell.Extensions.Dbus` yields `ServiceUnknown`, which means the address is wrong — **not** that the extension is disabled.

### Missing letters with input methods (not a defect in this tool)

If you use a touchpad with "tap to click" enabled, a palm touch while typing is registered as a mouse click, dropping a keystroke at that position and preventing the input method from composing a syllable (you see plain Latin letters).

GNOME's "disable touchpad while typing" **does not take effect** while this tool is running — it relies on the desktop receiving keyboard events, but this tool has grabbed the keyboard device.

Workaround:

```bash
gsettings set org.gnome.desktop.peripherals.touchpad tap-to-click false
gsettings set org.gnome.desktop.peripherals.touchpad tap-and-drag false
```

### Touchpad unavailable while running

On a MacBook the keyboard and the touchpad are the same evdev device. Once this tool grabs that device, touchpad events are no longer reported, so clicking, scrolling and gestures are unavailable while it runs.

This is an inherent limitation of evdev grabbing. Disabling "tap to click" avoids accidental touches while typing (see above).

## How it works

```
physical keyboard → /dev/input/eventN → [evdev grab] → mapping logic → [uinput inject] → virtual keyboard → system
                                          ↑ exclusive, cut here                     ↓ the system continues
```

It operates at the evdev layer, not the XKB layer, so it works under GNOME Wayland too (XKB only covers XWayland apps).

## Requirements

- Linux (evdev + uinput)
- Rust 1.64+ (to build)
- A C linker (required for Rust to link the executable). On Ubuntu/Debian:
  `sudo apt install build-essential`; otherwise you'll see
  `error: linker \`cc\` not found`
- The current user can access `/dev/input/event*` and `/dev/uinput`

**Permissions vary by distribution, and this is the most common place to get stuck. Check your machine first:**

```bash
ls -l /dev/input/event* /dev/uinput
```

Measured on Ubuntu 26.04:

```text
/dev/input/eventN   root:input  0660   ← readable by joining the input group
/dev/uinput         root:root   0600   ← a normal user cannot open it
```

So on Debian-family systems **two** things are required: add your user to the `input` group (to read the physical keyboard) and use the [`packaging/99-mackbind.rules`](packaging/99-mackbind.rules) rule shipped here to open up `/dev/uinput` (to create the virtual keyboard). Doing only the first exits with "cannot open uinput" when creating the virtual keyboard.

> `/dev/uinput` permissions may already be open on other distributions (this
> repo was originally debugged on EndeavourOS without hitting this issue), but
> always go by the `ls -l` output above — don't assume. This rule is
> **additive** and does not tighten an existing configuration.

## Install

**One-shot script (recommended):**

```bash
git clone https://github.com/cphotor/mackbind
cd mackbind
./install.sh
```

The script does four things in order: build the release binary → install it to `~/.local/bin` → open up `/dev/uinput` (udev rule) → add the current user to the `input` group → install and enable the systemd user service. Steps that need root use `sudo` and may prompt for your password; the script is safe to re-run (idempotent).

Afterward, **log out and back in once** so the new `input` group membership takes effect. The service is enabled, so it starts automatically after you log back in.

|Option|Effect|
|---|---|
|`--no-build`|Skip building; use the existing `target/release/mackbind`|
|`--no-udev`|Don't install the udev rule (e.g. `/dev/uinput` is already writable)|
|`--no-group`|Don't modify the `input` group|
|`--no-service`|Don't install the systemd user service|

Self-check: `ls -l /dev/uinput` should show `root input`, and `mackbind --list-devices` should list your keyboard.

<details>
<summary>Manual install (without the script)</summary>

```bash
# 1. make sure the user is in the input group (usually already the case)
sudo gpasswd -a $USER input

# 2. build and install to ~/.local/bin
cargo build --release
install -Dm755 target/release/mackbind ~/.local/bin/mackbind
```

**Debian/Ubuntu users also need to open up `/dev/uinput`** (see "Requirements"):

```bash
sudo install -m644 packaging/99-mackbind.rules /etc/udev/rules.d/99-mackbind.rules
sudo udevadm control --reload && sudo udevadm trigger
# then log out and back in so the new input group membership takes effect
```

</details>

It installs to `~/.local/bin/` rather than `cargo install`'s default `~/.cargo/bin/` because **a systemd user service's PATH does not include `~/.cargo/bin`**, whereas `~/.local/bin` is the more standard user-level path.

If you prefer `cargo install`, point `--root` at the target:

```bash
cargo install --path . --root ~/.local    # installs to ~/.local/bin/mackbind
```

## Usage

### Running manually

```bash
mackbind                    # start
mackbind -v                 # print every key's translation
mackbind --check            # validate config only; don't grab the keyboard
mackbind --list-devices     # list detected keyboard devices
```

To stop a manual run: `systemctl --user stop mackbind` (if the service is enabled), or `kill <PID>` from another terminal.

### As a systemd service (recommended)

Create `~/.config/systemd/user/mackbind.service`:

```ini
[Unit]
Description=mackbind — macOS keybindings for Linux
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/mackbind
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

Enable it:

```bash
systemctl --user daemon-reload
systemctl --user enable --now mackbind
```

`Restart=on-failure` restarts on a crash, but a deliberate `stop` won't trigger a restart.

**`linger` is not needed.** linger keeps a user service running after logout, but once you log out GNOME is gone and injected uinput events have no receiver, so it brings no real benefit to mackbind.

Common commands:

```bash
systemctl --user status mackbind        # show status
systemctl --user restart mackbind       # restart (after config changes)
systemctl --user stop mackbind          # stop
journalctl --user -u mackbind -f        # live logs
journalctl --user -u mackbind --since today   # today's logs
```

## Configuration

Optional. **With no config file you get the full default behavior.** Config lookup order (later overrides earlier):

```
/etc/mackbind/config.toml          system-wide
~/.config/mackbind/config.toml     per-user
$MACKBIND_CONFIG                   set via environment (highest priority)
```

See [`config.example.toml`](config.example.toml) for an example. Main options:

|Option|Default|Description|
|---|---|---|
|`remap`|CapsLock ↔ Left Ctrl|modifier remap/swap|
|`rules`|29 rows|**the mapping source of truth**. Each row = `(app, key combo) → action`; order is priority|
|`fn_mode`|`fkeysfirst`|Fn priority: `fkeysfirst` / `fkeyslast` / `disabled`|
|`desktop.type`|`gnome`|desktop environment: `gnome` / `kde` / `none`|
|`desktop.spotlight`|`true`|whether `Cmd+Space` maps to overview (reads the DE's existing binding; never rewrites it)|
|`ctrl_busy_apps`|`["gvim"]`|apps outside terminals where Ctrl has a non-standard meaning. **Terminals need no listing** — detected via `.desktop` `Categories=TerminalEmulator`|

A mistyped field name **fails hard at startup** instead of being silently ignored.

**The rule table is the entire mapping** — there is no hidden behavior scattered elsewhere. `mackbind --verbose` prints the whole table at startup, so what each row does fits on one screen.

### The rule table

Three columns: `apps` (applications) · `keys` (the combo the user presses, excluding Cmd) · `action` (what to do).

```toml
# global: Cmd+Q quits the app
[[rules]]
keys   = ["KEY_Q"]
action = { emit = { mods = ["A"], key = "KEY_F4" } }

# rewrite Cmd+B only inside VSCode
[[rules]]
apps   = ["code"]
keys   = ["KEY_B"]
action = { emit = { mods = ["C"], key = "KEY_B" } }

# Cmd+Space → overview: read the DE's existing binding and emit the same key
[[rules]]
keys   = ["KEY_SPACE"]
action = { de_accel = "overview" }
```

Three kinds of action:

|Action|Meaning|
|---|---|
|`emit`|emit a fixed combo|
|`de_accel`|read the desktop's existing binding and emit the same key (without rewriting the user's config)|
|`terminal_ctrl_shift`|add Shift only when the app **itself** has bound that key with `Ctrl+Shift`|

**Order is priority**: the first match from top to bottom wins. Your entries are appended after the defaults, so they naturally override them.

Rules **require Cmd to be held by default** (`cmd = true`). The few combos without Cmd must set `cmd = false` explicitly, e.g.:

```toml
[[rules]]
keys   = ["A-KEY_DELETE"]
cmd    = false                                   # no Cmd needed
action = { emit = { mods = ["C"], key = "KEY_DELETE" } }
```

### Debugging mapping problems

Once mapping is active the program owns the keyboard, so **when it misbehaves you cannot type and therefore cannot observe the situation**. `--trace` mode does not grab the keyboard; it only prints "input → actual output" to stderr:

```bash
./target/release/mackbind --trace 2>trace.log
# type normally, then look in another terminal
grep '^\[trace\] IN' trace.log
```

```text
[trace] IN KEY_SPACE     down -> OUT [KEY_SPACE+]                bare space passes through unchanged
[trace] IN KEY_LEFTMETA  down -> OUT []                          Cmd suppressed
[trace] IN KEY_SPACE     down -> OUT [SHIFT+, CTRL+, SPACE+]     Cmd+Space → overview
```

### Modification examples

```toml
# don't swap CapsLock/Ctrl
remap = []

# turn only CapsLock into Left Ctrl, leaving Left Ctrl itself alone
remap = [{ from = "KEY_CAPSLOCK", to = "KEY_LEFTCTRL" }]

# add your own shortcut: Cmd+E → Ctrl+Shift+E
[[rules]]
keys   = ["KEY_E"]
action = { emit = { mods = ["C-S"], key = "KEY_E" } }
desc   = "my shortcut"

# turn off Fn priority
fn_mode = "disabled"

# declare the desktop environment
[desktop]
type = "gnome"

# specialize an additional app (terminals are auto-detected)
ctrl_busy_apps = ["gvim", "org.gnome.gedit"]
```

A fuller example is in `config.example.toml` at the repo root.

After editing, run `systemctl --user restart mackbind` (or restart the manual run) to apply.

## Disabling

Temporarily disable while keeping the config:

```bash
systemctl --user stop mackbind
```

Stop it from starting at login:

```bash
systemctl --user disable mackbind
```

Once stopped, the keyboard immediately reverts to native behavior.

## Removing

**One-shot script**:

```bash
cd mackbind
./uninstall.sh                  # remove service, binary, udev rule (keeps the input group)
./uninstall.sh --remove-group   # also remove the current user from the input group
```

It leaves the `input` group alone by default, since other tools may use it.

<details>
<summary>Manual removal</summary>

```bash
# 1. disable and remove the service
systemctl --user disable --now mackbind

# 2. delete the unit file
rm ~/.config/systemd/user/mackbind.service
systemctl --user daemon-reload

# 3. delete the binary
rm ~/.local/bin/mackbind

# 4. delete config (optional)
rm ~/.config/mackbind/config.toml /etc/mackbind/config.toml

# 5. remove the udev rule / input group (if you added them)
#    usually not necessary
```

</details>

Confirm a clean removal:

```bash
ls ~/.config/systemd/user/ | grep mackbind   # no output expected
pgrep -a mackbind                            # no output expected
```

## Troubleshooting

**`Device or resource busy`**

Another program is holding the keyboard (most often Toshy). Stop it first:

```bash
toshy-services-stop
# if Toshy is uninstalled, check for leftovers
pgrep -af xwaykeyz
```

**The keyboard stopped working**

A crash may leave the grab unreleased. From another terminal:

```bash
systemctl --user restart mackbind    # if the service is enabled
# or
pkill mackbind
```

The kernel releases the grab automatically when the file descriptor closes, so it usually recovers immediately. If it does not, replug the keyboard or reboot.

**Startup fails because of a bad config**

Use `--check` to pinpoint it:

```bash
mackbind --check
```

**A combo doesn't work**

Use `-v` to see the actual translation:

```bash
mackbind -v
```

## Relationship to Toshy

The two **cannot run at the same time** — both grab the keyboard device, and the second one fails to grab.

| | Toshy | mackbind |
|---|---|---|
|Model|default rewrite + per-combo specified output|`Cmd` *is* `Ctrl`; the table only covers exceptions|
|Cmd identity|`LEFT_META` → `RIGHT_CTRL` (global)|`Cmd` becomes `Ctrl`, Super released|
|Ctrl semantics|rewritten in the GUI|always Ctrl|
|Cmd combos not in the table|must be specified explicitly|auto-mapped to `Ctrl+X`|
|Terminal shortcut source|enumerated per terminal in config|read from each terminal's own GSettings schema|

## Development

```bash
cargo build --release     # build
cargo test                # run unit tests
cargo clippy              # static checks
```

See [`DESIGN.md`](DESIGN.md) for architecture and design decisions.

## Versioning

The current version is defined in **exactly one place**: the `version` field in
[`Cargo.toml`](Cargo.toml). Everything else derives from it rather than repeating
it — the startup log reads it at compile time, and the badge at the top of this
file reads it from the git tag. [`CHANGELOG.md`](CHANGELOG.md) records released
versions, and `scripts/audit.py` fails if its newest entry disagrees with
`Cargo.toml`, so the number cannot drift.

## License

[MIT](LICENSE) © 2026 cphotor
