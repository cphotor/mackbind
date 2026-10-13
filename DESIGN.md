# mackbind — Design

Emulate macOS keyboard habits on Linux while fully preserving Linux's native
modifier semantics.

## Problem

The modifier semantics of macOS and Linux/Windows are nearly identical; only the
names differ:

|Physical position|macOS|Linux kernel report|X11 keysym|
|---|---|---|---|
|1st from the left|Ctrl|`KEY_LEFTCTRL`|Control_L|
|2nd from the left|Option|`KEY_LEFTALT`|Alt_L|
|3rd from the left|Command|`KEY_LEFTMETA`|Super_L / Meta_L|
|Leftmost of the Tab row|CapsLock|`KEY_CAPSLOCK`|Caps_Lock|

At the hardware level **no conversion whatsoever** is needed. The real problem
lies in the software layer.

### Toshy's approach and its side effects

To implement "`Cmd+X` → `Ctrl+X`", Toshy (and the Kinto it inherited from) uses a
global modmap:

```
Key.LEFT_META → Key.RIGHT_CTRL     # Cmd unconditionally becomes Ctrl
```

This rewrite **takes effect unconditionally**, producing three chained
consequences:

1. **Cmd loses its Super semantics.** On macOS, Cmd and Ctrl are two
   side-by-side system modifiers; `Ctrl+Cmd+Space`, `Ctrl+Cmd+Q`, and
   `Ctrl+Cmd+F` are all valid and meaningful combinations. Once Cmd is mapped to
   Ctrl, the two modifiers point at the same slot and cannot be distinguished.

2. **The LEFT_CTRL slot is vacated, so only Super can be placed there.** Because
   Cmd has taken over Ctrl's semantics, the physical Ctrl key must be rerouted:

   ```python
   modmap("Cond modmap - GUI - Mac kbd", {
       Key.LEFT_CTRL:  Key.LEFT_META,   # under GUI → Super
   })
   ```

   Yet that same position is `LEFT_CTRL` under a terminal:

   ```python
   modmap("Caps mode - lctrl_role_swap - GUI",   {Key.CAPSLOCK: [Key.ESC, Key.LEFT_META]})
   modmap("Caps mode - lctrl_role_swap - Terms", {Key.CAPSLOCK: [Key.ESC, Key.LEFT_CTRL]})
   ```

   So the same physical key behaves differently under the GUI and under a
   terminal.

3. **Ctrl habits under the GUI are broken.** In a scenario like VSCode with a vim
   plugin, the physical Ctrl actually emits `Super+V`; the vim plugin never
   receives `Ctrl+V`, and block selection stops working.

Moreover, **none** of Toshy's 7 `capslock_mode` options can simultaneously
satisfy "Caps→real Ctrl" and "physical Ctrl→Caps", because its `role_swap`
family is designed on the premise that "left Ctrl's GUI role is Super".

### An incidental problem: implementation leakage

Toshy names Cmd's internal slot `RIGHT_CTRL`, and in its docs `Super`/`Cmd`/
`Command`/`Meta` are synonyms. As a result `C("Super-e")` *looks* like the
correct spelling but never works — by the time the physical `Cmd+E` reaches the
keymap layer it has already become `RIGHT_CTRL+E`. This kind of mismatch between
naming and implementation reliably misleads users.

## Goals

1. **Swap the CapsLock ↔ left Ctrl positions**, with both keys emitting a real
   Ctrl in every environment and never producing Super.
2. **Map `Cmd+X` to `Ctrl+X`**, with no need for exhaustive enumeration — long-tail
   combinations are correct by default.
3. **Ctrl always equals Ctrl**, consistent between GUI and terminal. Native
   terminal behaviors such as `Ctrl+C` interrupting a process, `Ctrl+L` clearing
   the screen, and `Ctrl+A` going to the start of the line are entirely
   unaffected.
4. **The rule table only handles exceptions**: the table is consulted only when
   the target key differs from the main key.
5. Super is left to the desktop environment (input-method switching, `Super+Tab`,
   etc.); mackbind does not produce Super by default.

## Core model

Opposite in direction to Toshy.

| | Toshy | mackbind |
|---|---|---|
|Model|Rewrite by default + per-combination output|`Cmd` is `Ctrl`; the rule table only handles exceptions|
|Cmd identity|`LEFT_META` → `RIGHT_CTRL` (global)|`Cmd` becomes `Ctrl`; Super is left free|
|Modifier slots|Five keys rearranged, crowding each other out|Untouched|
|Mapping target|Specified individually for each combination|Defaults + exception table|
|Beyond the table|Must be specified explicitly; anything missed is broken|Works by default|

mackbind's logic reduces to:

```
receive CAPSLOCK        → emit LEFT_CTRL
receive LEFT_CTRL       → emit CAPSLOCK
receive LEFT_META + key → exception table? emit exception : emit Ctrl+key
receive anything else   → pass through unchanged
```

There are no conflicts between rules and no ordering dependencies. This is an
order-of-magnitude difference from Toshy, which must handle "the full remapping
matrix of five modifiers, 6 keyboard types, thousands of combination rules, and
multipurpose-key timing decisions".

## Key fact: on the Linux desktop, `Super` is used only for system-level operations

GNOME's current 28 Super bindings (measured with
`gsettings list-recursively org.gnome.desktop.wm.keybindings`):

```
Window management  Super+↑ ↓ ← →          maximize/minimize/tile
                   Super+Shift+arrow      move to another monitor
Workspaces         Super+Page_Up/Down     switch workspace
                   Super+Shift+PgUp/Down  move window to workspace
                   Super+Home/End         jump to first/last workspace
App switching      Super+Tab               switch app
Input method       Super+space             switch input method
Other              Super+h / Super+F1 / Super+↑ etc.
```

**The only one involving a letter key is `<Super>h`** (lock screen `Super+l` and
`Super+o` are user-defined). GNOME deliberately avoids `Super+letter` — letter
keys are left to applications, and Super is left to the desktop environment for
managing windows.

This means that macOS's `Cmd+letter` (copy, save, find…) **never corresponds to
Super on Linux, but always to Ctrl**. The earlier design judgment to "let Cmd
keep its Super meaning" merely disabled the keys for the vast majority of
combinations.

### Once misjudged: keeping Super is better

The initial design was "pass through by default, table for exceptions" — `Cmd`
always remained `LEFT_META`, and only combinations hitting the mapping table were
translated. The flaw in this model showed up in testing: **it cannot be
enumerated exhaustively**.

At the time there was a built-in whitelist of 23 `cmd_map` entries. That number
itself is a signal of design failure — real Cmd shortcuts have a long tail.
Combinations not on the list (such as `Cmd+E`, `Cmd+Shift+N`) passed through as
`Super+…`, and on Linux **no application recognizes `Super+letter`**, so nothing
happened.

User feedback confirmed this: "There are too many cmd combo keys, and every app
is different! It's impossible to enumerate them all."

The corrected rules:

```
default    LEFT_META + key  →  Ctrl + key
exception 1  target key ≠ main key  →  consult table (Cmd+[ → Alt+←, Cmd+Q → Alt+F4…)
exception 2  Ctrl already taken in-app and the key has a Ctrl+Shift binding  →  Ctrl+Shift + key
```

The exception list is small and can be derived automatically, so long-tail
combinations are correct by default.

## Terminal specialization: per key, not wholesale

A problem that was once misjudged: the worry that "`Cmd+C` translated to `Ctrl+C`
would wrongly trigger SIGINT in a terminal".

**That is wrong.** In the macOS Terminal, `Cmd+C` is `Ctrl+Shift+C`, and Linux
terminals support the same:

| |Linux terminal|macOS terminal|
|---|---|---|
|`Ctrl+C`|SIGINT interrupt|SIGINT interrupt|
|`Ctrl+Shift+C`|copy|copy|

But concluding from this that "in a terminal, turn Cmd into Ctrl+Shift
wholesale" is also wrong — that would degrade `Cmd+Q` into `Ctrl+Shift+Q`, a
combination nobody recognizes.

The correct approach is **per key**: add Shift only to keys that the **terminal
itself already binds with `Ctrl+Shift`**.

Further, GNOME-family terminals declare all their shortcuts via GSettings, so
read their schema directly rather than guessing:

```bash
gsettings list-recursively org.gnome.Ptyxis.Shortcuts
# new-window      '<ctrl><shift>n'
# copy-clipboard  '<ctrl><shift>c'
```

A hand-written whitelist once missed `n` (new window), and the user discovered
it. After switching to the schema, changes the user makes to terminal shortcuts
are picked up automatically.

### Exception: the copy key of ptyxis / konsole

When there is **no selected text**, these two terminals pass `Ctrl+Shift+C`
through to the child process as SIGINT, so "copy" kills the running task
(wezterm, ghostty, and gnome-terminal do not have this problem). Hence
`Ctrl+Insert` (the traditional X11 copy key) is used instead. This is carried by
one app-scoped `emit` rule in the rule table, judged on the basis of **behavioral
difference** rather than an app list.

### Terminal detection: follow the freedesktop spec, do not enumerate

A `.desktop` entry whose `Categories` contains `TerminalEmulator` is treated as a
terminal. The original hard-coded WM_CLASS list wrote lowercase `ptyxis`, whereas
the actual value is `org.gnome.Ptyxis`, so it was missed outright — user
feedback: "cmd-c works in the terminal and console, but not in ptyxis or
ghostty".

The focused window is obtained via the GNOME Shell extension
`focused-window-dbus`, with a lazy query (query only when a key arrives; skip
`.desktop` parsing if `wm_class` is unchanged).

## On the cost of "Cmd and Ctrl being indistinguishable"

`Cmd+C` and physical `Ctrl+C` produce **the same output** in ordinary
applications (both `Ctrl+C`). Is this a misjudgment?

**No.** The interpretation belongs to the application, not the keymap: mackbind
only produces a stream of events; a GTK application interprets `Ctrl+C` as copy
according to its own menus, while a terminal interprets it as SIGINT according to
termios' `ISIG`. The same output with different readings is the normal state of
the Linux desktop.

The only affected scenario is one in which an application requires `Cmd+X` and
`Ctrl+X` to trigger different actions. Such combinations are very rare and can be
preserved as-is through the exception table.

## Incident record: the input method dropping letters

### Symptom

While mackbind was running, the input method (fcitx5 + Xiaohe double pinyin)
dropped letters when typing; a single pinyin syllable could not be composed and
the raw English letters were output directly. The symptom was that typing
`fazhan` (the pinyin for "develop") produced `fa vj`.

**Unrelated to mackbind.** The root cause was the touchpad's `tap-to-click`:

While typing, the palm accidentally touched the touchpad, which was recognized as
a mouse click, and the key at that position was swallowed. Although GNOME has
`disable-while-typing`, that feature depends on GNOME receiving keyboard events;
once mackbind grabbed the keyboard, GNOME no longer received them, judged that
the user "was not typing", and therefore did not automatically disable the
touchpad.

Fix:

```bash
gsettings set org.gnome.desktop.peripherals.touchpad tap-to-click false
gsettings set org.gnome.desktop.peripherals.touchpad tap-and-drag false
```

After turning these off, the input method returned to normal.

### Detours taken during troubleshooting

They are recorded because these directions **all looked reasonable but were
ineffective**, so that those who come later do not repeat them:

|Hypothesis | Measured result |
|---|---|
|Batch splitting causes the input method to commit early|No effect. A real keyboard likewise sends one SYN per key; this was not introduced by mackbind|
|`InputEvent::new`'s timestamp being 0 disables timeout aggregation|No effect. The evdev docs are explicit: the kernel overwrites the timestamp when reading the event; the injected 0 never reaches the input method|
|Copy Toshy by adding a fixed 9ms key interval|**Worse.** Toshy's `throttle_delays` exist to solve key stickiness and are unrelated to the input method|
|Restore the original key rhythm from event timestamps|No effect, and every key sleeps, slowing the program down|
|Forward MISC events to preserve touchpad activity|**Worse.** ABS multitouch events hang off MISC; sending them all to a virtual device that does not declare `EV_ABS` capability made both the touchpad and the keys misbehave|

### Methodology lesson

The common error in all five attempts above: **inferring "same cause" from
"similar symptoms"**, treating external precedents (Toshy's implementation,
ptyxis's protocol stack) as evidence rather than starting from actual
observation.

What actually located the problem was a single observation from the user —
"I accidentally touched the touchpad while typing" — not repeated analysis of
mackbind's logs. The event stream on mackbind's side was correct from beginning
to end.

When troubleshooting problems of this kind in the future, first confirm "are the
events this program outputs correct?", and only then suspect downstream; in this
case the logs showed the events were entirely correct yet the problem persisted,
which means the observation perspective itself had a blind spot.

## Coverage

Based on Macworld's complete shortcut table and Apple's official documentation
categories.

### L1 — pure combination translation (fully covered by the design)

With Cmd as a modifier, pressing it with a key outputs Ctrl. The application
interprets it itself, with no application context needed.

```
Cmd+C / V / X / Z / A      →  Ctrl+C / V / X / Z / A      copy paste cut undo select-all
Shift+Cmd+Z                →  Ctrl+Shift+Z                 redo
Cmd+F / G                  →  Ctrl+F / G                   find / find next
Shift+Cmd+G                →  Ctrl+G                       find previous
Cmd+S / O                  →  Ctrl+S / O                   save / open
Cmd+B / I / U / K          →  Ctrl+B / I / U / K           bold / italic / underline / link
Cmd+T                      →  Ctrl+T                      new tab
Cmd+W                      →  Ctrl+W                      close window
Cmd+P                      →  Ctrl+P                      print
Cmd+R                      →  Ctrl+R                      refresh
Cmd+[ / ]                  →  Alt+Left / Alt+Right        browser back / forward
Cmd+N                      →  Ctrl+N                      new window
Shift+Cmd+P                →  Ctrl+Shift+P                print-related
```

### L2 — platform-level features (handed to the DE or not implemented)

These are macOS-specific features; on Linux they either have an equivalent or do
not exist. For the status of each, see the "Implementation status" section of
`docs/macos-shortcuts.md`.

```
Shift+Cmd+3 / 4 / 5   screenshot       → read GNOME's existing screenshot binding (see macos-shortcuts.md)
Cmd+Space             Spotlight        → GNOME overview; read toggle-overview, fall back to the mutter overlay-key when unbound
Cmd+Del               move to trash    → depends on the application
Ctrl+Cmd+Q            lock screen      → read media-keys screensaver (this machine: Super+L)
Fn family             Show Desktop/Dictation → no equivalent
Cmd+Tab               switch app       → Super+Tab (classification B, deferred)
```

### L3 — Finder-specific (to be tested on the real machine, not in v1)

Nautilus's bindings differ from Finder's and must be checked one by one. The
priority is low, because a mouse path can reach them.

```
Cmd+D duplicate / Cmd+E eject / Cmd+I get info / Cmd+1~4 view switching
Shift+Cmd+N new folder / Shift+Cmd+H home directory / Cmd+, preferences
```

### Emacs bindings (left untouched)

On macOS, `Ctrl` has independent Emacs-style bindings. This design does not touch
Ctrl, so these **work naturally**:

```
Ctrl+A line start   Ctrl+E line end   Ctrl+F forward   Ctrl+B backward
Ctrl+P line up      Ctrl+N line down  Ctrl+L clear screen  Ctrl+D delete char
Ctrl+K delete to end of line          Ctrl+W delete word
```

## Implementation

### Why evdev + uinput

Under GNOME Wayland, the XKB layer is unavailable:

|Layer|Mechanism|Coverage|App discrimination|
|---|---|---|---|
|XKB (`setxkbmap`)|keymap inside the X server|**XWayland apps only**|No|
|evdev grab + uinput|a userspace process intercepts events|global, including native Wayland|Yes (requires a focus query)|

mackbind takes the second path, with the same tech stack as Toshy but far simpler
logic.

### Components

```
1. Device enumeration and grabbing
   - enumerate /dev/input/event*, identify keyboards
   - device.grab() for exclusive access (needs the input group)

2. Modifier state machine
   - record the set of currently pressed modifiers
   - handle left/right distinction (LEFT_CTRL / RIGHT_CTRL)

3. Mapping decision
   - single-key rules: CapsLock ↔ LEFT_CTRL (static)
   - combination rules: LEFT_META + key → consult table (static)
   - pass through anything not in the table

4. Virtual device injection
   - create a virtual keyboard with uinput
   - replay the converted events

5. Terminal detection
   - read the focused-window info from the GNOME Shell extension
   - disable all mappings in terminal-type windows
```

### Key technical points

**A modifier is not output immediately on press.** When a modifier is received it
is first recorded in the state and its output suppressed; when a non-modifier key
is received, a decision is made using the complete modifier set to determine what
to output.

**The release event must match the decision made on press.** A converted
modifier's release event must be emitted under its converted identity, otherwise
a pressed state is left behind.

**Excluding terminals requires only the window type.** This is the only place
application context is needed, and it only requires judging "is it a terminal?",
not "which keys are bound". The mapping target is determined by a static table;
the application only needs to receive the correct `Ctrl+X` and will interpret it
correctly itself.

Under GNOME Wayland, a Shell extension must provide the focused-window info. The
project therefore depends on `focused-window-dbus@flexagoon.com` (version 11 at
the time of writing; its `metadata.json` declares
`shell-version: ["49", "50"]`), which `./install.sh` installs and enables on
GNOME. Its interface is:

```text
dest:   org.gnome.Shell
object: /org/gnome/shell/extensions/FocusedWindow
method: org.gnome.shell.extensions.FocusedWindow.Get
```

This extension exports its object directly from the GNOME Shell process and is
**not activated via a separate bus name**, so `--dest` must be `org.gnome.Shell`
itself. Using `org.gnome.Shell.Extensions.Dbus` by mistake yields
`ServiceUnknown: The name is not activatable` — this does not mean the extension
is disabled, but that the address is wrong. The criterion: querying
`org.gnome.Shell` returns JSON normally, while `metadata.json` exists in the
extension directory and `enabled-extensions` contains that uuid.

**When the extension is missing the failure is silent.** `focused_wm_class()`
returns `None`, so *every* app-scoped rule stops matching and the mapping falls
back to the generic `Cmd → Ctrl` semantics. In a terminal that turns `Cmd+C`
into `Ctrl+C`, i.e. SIGINT instead of copy, and nothing logs an error — the
keys simply have the wrong meaning. So the first diagnostic step for any
"per-app behavior is wrong" report is the `gdbus call` above, not reading the
rule table: this exact silent degradation was once mistaken for a mapping bug.

## Implementation choice

| | Rust | Python |
|---|---|---|
|Distribution|single-file binary, no runtime dependency|needs an interpreter + dependencies|
|evdev bindings|the `evdev` crate (requires pulling in libc details)|`python-evdev` (mature, but on this machine only inside the Toshy venv)|
|Development speed|slower|fast|
|Long-term maintainability|excellent|acceptable|

Leaning toward **Rust**: dependency-free distribution is a substantial advantage
for a tool at the input-method level, avoiding users having to install the whole
Python ecosystem just for a hundred lines of mapping logic.

Dependency list: `evdev` (device access and uinput), `serde` + `toml` (config
parsing). All are mature crates.

Note: the `evdev` crate **already ships** `Device::grab()` / `ungrab()`; there is
no need to implement the `EVIOCGRAB` ioctl yourself, and the `libc` dependency
can be dropped.

## Measured conclusions about the event stream

The following are measured results on a MacBook Pro 2015 (`/dev/input/event6`,
`Apple Inc. Apple Internal Keyboard / Trackpad`), and are the factual basis for
the implementation.

### Modifier key codes

|Physical key|Key code|
|---|---|
|Cmd|`KEY_LEFTMETA` (125)|
|left Ctrl|`KEY_LEFTCTRL` (29)|
|right Ctrl|`KEY_RIGHTCTRL` (97)|
|Option|`KEY_LEFTALT` (56)|
|CapsLock|`KEY_CAPSLOCK` (58)|
|Fn|`KEY_FN` (464)|

**Cmd is `KEY_LEFTMETA`**, the same code as the Win key on a PC. This is the
foothold of the entire design: macOS's Cmd and Linux's Super need no conversion at
the hardware layer, and the mapping table can be built directly on `LEFT_META`.

### The Fn key is visible and can be used for userspace priority switching

`KEY_FN` (464) is declared in the device capability list and arrives as an
ordinary key event:

```
Fn alone       → 464 press(1), release(0)
holding Fn     → 464 press(1), repeat(2) × N, release(0)
Fn + F1        → 464 press(1) and 59 press(1) arrive independently
```

The kernel **does no synthesis** — with `Fn+F1`, `KEY_FN` and `KEY_F1` are
reported separately, rather than directly giving `KEY_BRIGHTNESSDOWN` (224).

Therefore **Fn key priority can be implemented in userspace**, and both modes
(`fkeysfirst` / `fkeyslast`) need neither root nor changes to `hid_apple` kernel
parameters:

```
Fn not held + F1 →  emit KEY_F1
Fn held     + F1 →  emit KEY_BRIGHTNESSDOWN (224)
```

This overturns the assumption that "fnmode can only be switched via
`/sys/module/hid_apple/parameters/fnmode` and requires root".

### Implementation cautions

**Auto-repeat must be handled.** Holding a modifier produces many repeat events
with `value=2` (such as the consecutive `464 2`s in the example above). These
must not trigger the mapping repeatedly, or they produce paired press/release
events.

**`MISC` events must be filtered.** Measurements show every keypress has a `MISC`
event before and after it (stray events from the MacBook keyboard backlight/
touchpad); they are not part of key semantics and should be passed through or
discarded.

**The release event must match the decision made on press.** A converted
modifier's release event must be emitted under the converted key code, otherwise
a pressed state is left behind on the output side.

## Phasing

### v1 — minimal viable

- CapsLock ↔ left Ctrl swapped, both directions emitting a real Ctrl
- About 18 combination mappings from the L1 table
- Combinations not in the table pass through unchanged
- Filter `MISC` events and `value=2` auto-repeat events
- No app discrimination (verify first whether Ctrl is back to normal)

**Verification goal**: in VSCode with a vim plugin, `Ctrl+V`, `Ctrl+A`, and
`Ctrl+W` work again; in a terminal, `Ctrl+C` interrupts the process normally.

This step solves the core pain point on its own, independent of later features.
Even if v2 is never done, the project already has real value.

### v1.5 — Fn key priority

- Implement F-key / media-key switching in userspace based on `KEY_FN` press
  state
- A config option selects `fkeysfirst` (F key takes priority; Fn combos produce
  media keys) or `fkeyslast` (media key takes priority; Fn combos produce F keys)
- Requires maintaining a mapping table between F1–F12 and the brightness/media
  keys

This feature does not depend on the kernel `fnmode` parameter and is implemented
purely in userspace.

### v2 — terminal exclusion

- GNOME focused-window query
- Disable mappings in terminal-type windows

**Verification goal**: `Cmd` combos inside a terminal produce no unexpected
behavior; the `Ctrl` series works entirely normally.

### v3 — refinement

- Check the L3 Finder mappings one by one
- Config hot reload

> Autostart is already in place: `~/.config/systemd/user/mackbind.service`,
> `WantedBy=default.target`. Listing it as a to-do earlier was an oversight — it
> had in fact long since been configured.

## Risks

|Risk|Mitigation|
|---|---|
|A failed grab leaves the keyboard dead|verify the grab result at startup; on failure exit with an error rather than hanging silently|
|Modifier state leakage|define the release path explicitly; release all pressed keys on exit|
|Conflict with Toshy|the two cannot grab the same device at once; Toshy must be disabled or explicitly detected|
|Wayland focus info unavailable|degrade to no app discrimination (v1 has no such dependency anyway)|

**Relationship with Toshy**: the two cannot coexist on the same device. Disable
Toshy once mackbind passes verification. If mackbind falls short of expectations,
the Toshy configuration has been backed up at
`~/.config/toshy/toshy_user_preferences.sqlite.bak-20261009-174016` and can be
rolled back.

## To be confirmed

- [ ] Whether `Cmd+N` in the L1 table has a terminal semantic conflict (new tab
  vs new process), and whether to discriminate by app
- [ ] Whether `Cmd+P` in VSCode is "go to file" rather than "print", and whether
  to discriminate by app
- [ ] Whether the L3 Finder mappings are included after v3
- [ ] Whether the project name `mackbind` is adopted in the end
