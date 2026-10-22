# macOS Keyboard Shortcut Mapping Inventory

Source: [Apple's official Mac keyboard shortcuts](https://support.apple.com/en-us/102650)

**Core principle: macOS semantics win.** The goal is to make behavior consistent with macOS, not to make key names consistent. If a target function is already claimed by the desktop environment, change the desktop environment's existing binding to make room.

---

## Classification

| Class | Meaning | Implementation |
|---|---|---|
| **A** | General in-app operations | default `Cmd+X` → `Ctrl+X` |
| **C** | Terminal/editor specialization | `Cmd+X` → `Ctrl+Shift+X` (**per key**, not wholesale) |
| **D** | macOS-specific keys or features | explicit rule table |
| **E** | Equivalent feature exists but the desktop environment does not bind it | mackbind implements it proactively (adapted per DE) |
| **F** | Concept does not exist on the target platform | dropped |
| **B** | Platform Super bindings | **deferred**, see "Deferred items" |

---

## Core design principles

### 1. macOS semantics win

The goal is to make the operation's **behavior** consistent with macOS, not the key names. If a target function is already claimed by the desktop environment, resolve the conflict according to "macOS first".

### 2. The desktop environment is a configurable option, not hard-coded

On Linux there are many desktop environments, such as GNOME, KDE Plasma, Sway, and Hyprland; the same macOS feature has a different implementation, availability, and default shortcuts under each DE.

So DE-related behavior must be exposed as a config option:

```toml
[desktop]
type = "gnome"      # gnome | kde | none | custom
```

| Target | GNOME | KDE Plasma | Sway / i3 |
|---|---|---|---|
|Switch apps|`Super+Tab`|`Meta+Tab`|bind it yourself|
|Show desktop|`show-desktop` (unbound by default)|Plasma overview|bind it yourself|
|Screenshot|unbound, must be registered|built into Spectacle|bind it yourself|

`type = "none"` means "do not query any DE bindings; skip the related rules, and let the rest apply as usual."

**A feature you cannot query is not the same as a feature that does not exist** -- record it in the docs and enable it once the relevant DE is supported.

### 3. Specialized apps

Inside these apps `Ctrl` is already taken, so `Cmd` maps to `Ctrl+Shift` there. **But this is per key, not wholesale** -- only keys that the app itself has already bound to `Ctrl+Shift` get Shift added; all other keys still go through the default `Ctrl`. See "Implementation status → Granularity of terminal specialization" for the rationale.

The decision is made in two layers:

1. The `.desktop` entry's `Categories` contains `TerminalEmulator` -- decided by the freedesktop spec, so newly installed terminals are covered automatically. A hard-coded WM_CLASS list once missed because it was written in lowercase as `ptyxis` while the actual value is `org.gnome.Ptyxis`.
2. The `ctrl_busy_apps` config option -- for apps outside terminals where Ctrl has a non-standard meaning (e.g. gvim in its own window).

Focused-window information is obtained through the GNOME Shell extension `focused-window-dbus`, using lazy queries (only query on a keypress; skip parsing when `wm_class` has not changed).

---

## Deferred items

### Super-key rules (former class B)

macOS's `Cmd+Tab` (switch apps), `Cmd+H` (hide), and `Cmd+↑/↓` (window management) correspond to `Super+X` combos on Linux.

**These rules are deferred.** Reasons:

1. The goal is to emulate macOS; on Linux, Super carries the "Windows key" semantics and is not equivalent to macOS's Cmd.
2. How much each DE claims Super varies a lot (GNOME uses it for window management; Sway has its own scheme).
3. Get the rules that do not depend on the DE (A/C/D/E) solid first; leave the Super-related ones for later.

When they are implemented later, make them DE config options per §2 -- do not hard-code any DE's binding table.

---

## 1. Common operations

|macOS|Linux target|Class|
|---|---|---|
|`Cmd+X` Cut|`Ctrl+X`|A|
|`Cmd+C` Copy|`Ctrl+C`|A|
|`Cmd+V` Paste|`Ctrl+V`|A|
|`Cmd+Z` Undo|`Ctrl+Z`|A|
|`Shift+Cmd+Z` Redo|`Ctrl+Shift+Z`|A|
|`Cmd+A` Select all|`Ctrl+A`|A|
|`Cmd+F` Find|`Ctrl+F`|A|
|`Cmd+G` Find next|`Ctrl+G`|A|
|`Shift+Cmd+G` Find previous|`Ctrl+Shift+G`|A|
|`Cmd+O` Open|`Ctrl+O`|A|
|`Cmd+P` Print|`Ctrl+P`|A|
|`Cmd+Q` Quit app|D-Bus request for the focused app to quit|E|
|`Cmd+S` Save|`Ctrl+S`|A|
|`Cmd+T` New tab|`Ctrl+T`|A|
|`Cmd+W` Close window|`Ctrl+W`|A|
|`Option+Cmd+W` Close all app windows|`Ctrl+Alt+W`|App-handled|
|`Cmd+H` Hide window|`Super+h`|B (deferred)|
|`Option+Cmd+H` Hide other apps|none|F|
|`Cmd+M` Minimize|`Super+Down`|B (deferred)|
|`Option+Cmd+M` Minimize all windows|none|F|
|`Option+Cmd+Esc` Force quit|`Ctrl+Alt+Esc`|A|
|`Cmd+Space` Focus/Spotlight|activities overview; **GNOME's original `Super+space` must give way**|E|
|`Option+Cmd+Space` Focus within Finder|no global equivalent|F|
|`Ctrl+Cmd+Space` Character viewer|none|F|
|`Ctrl+Cmd+F` Full screen|`F11`|D (implemented: explicit rule)|
|`Cmd+,` Preferences|no global equivalent|F|
|`Cmd+Tab` Switch apps|`Super+Tab`|B (deferred)|
|`Shift+Cmd+Tab` Switch apps in reverse|`Shift+Super+Tab`|B (deferred)|
|`Cmd+`` ` `` Switch window|no global equivalent|F|
|`Cmd+Shift+3/4/5` Screenshot|`Shift+Print` (3) / `Print` (4, 5)|D (implemented, see below)|
|`Cmd+Shift+N` New folder in Finder|`Ctrl+Shift+N`|A|
|`Ctrl+Cmd+N` New folder with selection|none|F|
|`Shift+Cmd+6` Ask Siri (macOS 27+)|none|F|
|`Fn+Q` Quick Note|none|F|
|`Space` Quick Look|no global equivalent|F|

## 2. Sleep, log out, and shut down

|macOS|Linux target|Class|
|---|---|---|
|`Ctrl+Cmd+Q` Lock screen|`Super+l`|D|
|`Shift+Cmd+Q` Log out|`Ctrl+Alt+Delete`|D (implemented)|
|`Option+Shift+Cmd+Q` Log out immediately|none|F|

> Power-button shortcuts have no corresponding keyboard key and are ignored.

## 3. Finder and system

Finder is macOS's Finder, corresponding to Nautilus on Linux. Most of the following are operations inside the file manager and can largely be handled by the app itself.

|macOS|Linux target|Class|
|---|---|---|
|`Cmd+D` Duplicate file|`Ctrl+D`|A|
|`Cmd+E` Eject disk|none|F|
|`Cmd+I` Get info|`Ctrl+I`|A|
|`Cmd+[` Previous folder|`Alt+←`|D|
|`Cmd+]` Next folder|`Alt+→`|D|
|`Cmd+Delete` Move to trash|`Delete` (without Ctrl)|D (implemented, see below)|
|`Shift+Cmd+Delete` Empty trash|`gio trash --empty --force`|E (implemented, forced confirmation)|
|`Option+Shift+Cmd+Delete` Empty trash without confirmation|`gio trash --empty --force`|E (implemented, no confirmation)|
|`Cmd+1/2/3/4` Icon/list/column/gallery view|Nautilus internal|App-handled|
|`Shift+Cmd+H` Home folder|Nautilus internal|App-handled|
|`Option+Cmd+L` Downloads folder|Nautilus internal|App-handled|
|`Shift+Cmd+O` Documents folder|Nautilus internal|App-handled|
|`Shift+Cmd+U` Utilities folder|Nautilus internal|App-handled|
|`Cmd+K` Connect to server|Nautilus internal|App-handled|
|`Cmd+J` Show options|Nautilus internal|App-handled|
|`Cmd+N` New Finder window|`Ctrl+N`|A|
|`Cmd+↑/↓` Open enclosing folder/selected item|`Ctrl+↑/↓`|A|
|`Ctrl+Cmd+↑` Open enclosing folder in new window|none|F|
|`Shift+Cmd+D` Desktop folder|Nautilus internal|App-handled|
|`Shift+Cmd+F` Recents|Nautilus internal|App-handled|
|`Shift+Cmd+G` Go to folder|Nautilus internal|App-handled|
|`Shift+Cmd+I` iCloud Drive|none (no iCloud)|F|
|`Shift+Cmd+K` Network window|Nautilus internal|App-handled|
|`Shift+Cmd+R` AirDrop|none (no AirDrop)|F|
|`Shift+Cmd+P` Preview pane|Nautilus internal|App-handled|
|`Shift+Cmd+T` Tab bar|Nautilus internal|App-handled|
|`Option+Cmd+P/S/T/V` Path bar/sidebar/toolbar/move|App-handled|App-handled|
|`Cmd+/` Status bar|Nautilus internal|App-handled|
|`Option+Cmd+N` Smart folder|none|F|
|`Option+Cmd+Y` Slideshow Quick Look|no global equivalent|F|
|`Option+Cmd+D` Show/hide Dock|none (GNOME has no Dock)|F|
|`Ctrl+Cmd+T` Add to Dock|none (GNOME has no Dock)|F|
|`Ctrl+Cmd+A` Make alias|none (no alias concept)|F|
|`Shift+Cmd+C` Computer window|none|F|
|`Shift+Cmd+X` Add to sidebar|App-handled|App-handled|
|`Cmd+Brightness` Video mirroring|none|F|
|`Option+Brightness` Display settings|no global equivalent|F|
|`Ctrl+Brightness` External display brightness|none (hardware-dependent)|F|
|`Option+Dashboard` Dashboard settings|none|F|
|`Cmd+Dashboard` / `Fn+H` / `Fn+F11` Show desktop|implementation TBD|E|
|`Ctrl+↑` Dashboard|activities overview|E (implemented)|
|`Ctrl+↓` All windows of the front app|none|F|
|`Option+Volume/Brightness` Open the corresponding settings|no global equivalent|F|

## 4. Text editing

### Cursor movement

|macOS|Linux target|Class|
|---|---|---|
|`Cmd+←/→` Start/end of line|`Ctrl+←/→`|A|
|`Cmd+↑/↓` Start/end of document|`Ctrl+↑/↓`|A|
|`Option+←/→` Previous/next word boundary|`Alt+←/→`|D|
|`Shift+Cmd+←/→` Select to start/end of line|`Ctrl+Shift+←/→`|A|
|`Shift+Cmd+↑/↓` Select to start/end of document|`Ctrl+Shift+↑/↓`|A|
|`Shift+↑/↓` Extend selection to previous/next line|`Ctrl+Shift+↑/↓`|D|
|`Shift+←/→` Extend by one character|`Ctrl+Shift+←/→`|D|
|`Option+Shift+↑/↓` Extend to start/end of paragraph|`Alt+Shift+↑/↓`|A (already covered by default semantics)|
|`Option+Shift+←/→` Extend to start/end of word|`Alt+Shift+←/→`|A (already covered by default semantics)|
|`Fn+↑/↓/←/→` PageUp/PageDown/Home/End|`Ctrl+↑/↓` (X11 convention)|D|

### Deletion

|macOS|Linux target|Class|
|---|---|---|
|`Option+Delete` Delete word to the left|`Ctrl+Delete`|A (implemented)|
|`Ctrl+H` Delete character to the left|`Backspace`|A|
|`Ctrl+D` Delete character to the right|`Ctrl+Delete`|A|
|`Fn+Delete` Forward delete|`Ctrl+Delete`|D|

### Emacs-style bindings (native to macOS, and Linux already has the same-named keys)

These already work on Linux, so **no mapping is done**:

```
Ctrl+A start of line    Ctrl+E end of line      Ctrl+F forward     Ctrl+B backward
Ctrl+P up               Ctrl+N down             Ctrl+O insert newline  Ctrl+T transpose chars
Ctrl+L center           Ctrl+H delete char left Ctrl+D delete char right
```

### Formatting and editing

|macOS|Linux target|Class|
|---|---|---|
|`Cmd+B/I/U` Bold/italic/underline|`Ctrl+B/I/U`|A|
|`Cmd+K` Insert web link|`Ctrl+K`|A|
|`Cmd+T` Show/hide Fonts window|none (F) / already present (Linux)|F|
|`Cmd+;` Check spelling|`Ctrl+;`|A|
|`Shift+Cmd+;` Spelling and grammar window|none|F|
|`Ctrl+Cmd+D` Show word definition|none|F|
|`Cmd+{` / `Cmd+}` Align left/right|no global equivalent|F|
|`Shift+Cmd+\|` Align center|no global equivalent|F|
|`Option+Cmd+F` Go to search field|`Alt+←` or already present|F|
|`Option+Cmd+T` Show/hide toolbar|App-handled|App-handled|
|`Option+Cmd+C` Copy style|none|F|
|`Option+Cmd+V` Paste style|none|F|
|`Option+Shift+Cmd+V` Paste and match style|none|F|
|`Option+Cmd+I` Inspector window|none|F|
|`Shift+Cmd+P` Page setup|none|F|
|`Shift+Cmd+-` / `Shift+Cmd+=` Zoom out/in|no global equivalent|F|
|`Shift+Cmd+?` Help menu|none|F|
|`Shift+Cmd+Space` Ask Siri (macOS 27+)|none|F|

## 5. Accessibility

Disabled by default; not implemented for now.

|macOS|Description|
|---|---|
|`Ctrl+Option+Cmd+8` Invert colors|
|`Ctrl+Option+Cmd+,` / `.` Decrease/increase contrast|
|`Ctrl+F2~F6` / `Fn+Ctrl+F2~F6` Move focus|
|`Ctrl+Shift+F6/F7/F8` Focus related|
|`Option+Cmd+F5` Accessibility shortcuts panel|
|`Ctrl+Tab` / `Ctrl+Shift+Tab` Cycle focus|

## 6. App-specific

Safari, Calendar, Music, Mail, etc. each have their own shortcuts and are **not included in the global mapping**. Most are already consistent on Linux (e.g. `Cmd+C` copy).

### Deciding the "App-handled" class: `Option+Cmd+W`

Apple's official docs do list this key -- "Close the frontmost window. To close all windows of an app, press Option-Command-W". But it is **not a system-level shortcut; each app implements it itself**:

|App|`Option+Cmd+W`|
|---|---|
|Finder (system app)|yes (closes all windows of that app)|
|Chrome (third-party)|**no** (measured: pressing it does nothing)|

Shortcuts listed in the system menu usually **only guarantee that they work for built-in apps**. Whether a third-party app implements them is up to its developer -- which explains why pressing it on macOS does nothing either: it is not that the key does not exist, but that Chrome does not implement it.

Hence it is classified as "App-handled"; marking it F or E would both be wrong:

- Marking it **F** (concept does not exist) → wrong; it does exist on macOS, Finder implements it.
- Marking it **E** (implemented by this tool) → wrong; GNOME has no corresponding action, and mackbind should not take it on.

On the Linux side the default semantics produce `Ctrl+Alt+W`. That combo **does nothing for any app on Linux** -- GNOME does not claim it (it only claims `Ctrl+Alt+arrows/Tab/Esc`), and no app claims it either.

**This is exactly the ideal outcome.** The tool's default semantics do not invent an action for it, so:

|Possible mapping|Outcome|
|---|---|
|`Ctrl+W`|❌ dangerous -- would close windows one by one|
|`Alt+F4`|❌ has side effects -- closes the whole app|
|`Ctrl+Alt+W`|✅ harmless -- unclaimed, a no-op|

In other words, "effective only for built-in apps on macOS" → "safely inert on Linux".
Not special-casing it is better than guessing an action for it.

> **Lesson**: "it does nothing when I try it in some app" ≠ "the shortcut does not exist".
> The three kinds of "not found" must be kept apart:
>
> |Symptom|Real meaning|
> |---|---|
> |command errors / returns empty|wrong place, or wrong schema|
> |pressing it in the app does nothing|**maybe the app just has not implemented it**|
> |absent from the official docs too|only then does it truly not exist|
>
> Before concluding "does not exist", retest with a different app or check the official docs.
> This is the same class of mistake as "looking in the wrong place and misjudging".

---

## App classification list

### Specialized class (`Cmd` → `Ctrl+Shift`)

Inside these apps `Ctrl` is already taken, so map to `Ctrl+Shift` to avoid conflicts.

**Terminals** (`Ctrl+C` interrupts a process, `Ctrl+Z` suspends):

```
gnome-terminal-server  org.gnome.Console  kitty  Alacritty
foot  wezterm  org.kde.konsole  konsole  xfce4-terminal
io.elementary.terminal  Tilix  org.tigervault
```

**GVIM** (`Ctrl+V` block select, `Ctrl+C` quit):

```
nvim  vim
```

**VSCode** (`Ctrl+C` copy line, `Ctrl+V` paste):

```
code  code-url-handler  codium
```

> VSCode's own shortcuts are already highly consistent with macOS; putting it in the specialized class would conflict with its existing bindings.
> Consider mapping only the combos that actually differ, or excluding it entirely.

---

## Bindings GNOME needs to adjust

Following the "macOS first" principle, the following need to give way or be added:

|macOS|Target|GNOME status|Action|
|---|---|---|---|
|`Cmd+Space` Spotlight|activities overview|`Super+space` = switch input method|give way|
|`Ctrl+Cmd+Q` Lock screen|`Super+l`|media-keys `screensaver` (bound)|implemented|
|`Cmd+Tab` Switch apps|`Super+Tab`|bound|no conflict|
|`Cmd+↑/↓`|`Super+↑/↓`|bound|no conflict|
|`Shift+Cmd+3/4/5` Screenshot|`Print` / `Shift+Print`|bound|no conflict|
|`Shift+Cmd+Q` Log out|`Ctrl+Alt+Delete`|media-keys `logout` (bound on this machine)|no conflict|

### GNOME claims the `Ctrl+Alt` family (important)

Default semantics turn `Cmd` into `Ctrl`, so every `Option+Cmd+X` lands on `Ctrl+Alt+X`.
And GNOME binds the `Ctrl+Alt` family to **workspace switching** -- these are intercepted at the desktop-environment layer, so **apps never receive them**:

```
switch-to-workspace-up      ['<Control><Alt>Up']
switch-to-workspace-down    ['<Control><Alt>Down']
switch-to-workspace-left    ['<Control><Alt>Left']
switch-to-workspace-right   ['<Control><Alt>Right']
switch-panels               ['<Control><Alt>Tab']
cycle-panels                ['<Control><Alt>Escape']
```

Impact: any `Option+Cmd+arrow/Tab/Esc` combo cannot pass through to the app.
This is not a mackbind defect; it is an existing binding of the target platform. With such combos,
**you must check whether the DE has already claimed it** -- not just whether the app recognizes it.

(`Ctrl+Alt+W` is not in this list -- `W` is not claimed and can reach the app. But Chrome does not recognize it either,
and the combo has no behavior on macOS in the first place, so it is marked F.)

---

## Implementation status

The result of checking this document item by item. **Combos not listed here are automatically covered by the default semantics** (`Cmd+X → Ctrl+X`) and need no separate implementation.

### Implemented

|Item|Implementation|
|---|---|
|A General in-app operations|default semantics, nothing to list|
|C Terminal/GVIM specialization|per-key granularity, see below|
|`Cmd+Q` → `Alt+F4`|global explicit rule (GNOME `wm.keybindings close`)|
|`Cmd+Space` → overview|read GNOME's existing `toggle-overview` binding and emit it; when that action is unbound (the default since GNOME 50), fall back to reading mutter's `overlay-key` and emit one tap|
|D Cursor movement / `Cmd+[` `Cmd+]`|14 entries in `DEFAULT_CMD_MAP`|
|Delete → `Ctrl+Delete`|`DEFAULT_CMD_MAP`|
|`Super+l` Lock screen|the user configured it themselves; mackbind stays out|
|Super-related (class B)|**deferred**, see "Deferred items"|

### Not implemented

|Item|Blocker|
|---|---|
|"Show desktop"|the action exists (`show-desktop`) but is unbound by default (`@as []`); the user must bind it before this tool can emit the corresponding key|
|Finder → Nautilus internal operations|most are handled by the app itself and need no global rule|

### Empty trash: the deliberate difference from macOS

On macOS, `Shift+Cmd+Delete` empties the trash (with confirmation), and
`Option+Shift+Cmd+Delete` skips confirmation (Option means "skip confirmation").
This tool **reproduces that difference**: the former shows a confirmation dialog, the latter executes directly.

Skipping confirmation is only acceptable because the rule is **scoped to Nautilus**.
Global skip-confirmation would be unacceptable: `gio trash --empty --force` bypasses gio's own
confirmation (`--force`), and a global binding is not scoped to an app or a focused window, so
one accidental press would delete permanently, with no trash to recover from.
The exec layer therefore has a guard: destructive commands that skip confirmation must be scoped to an app.

**Scoped to Nautilus** (on macOS these two are Finder menu items, and Finder also manages the desktop, so they are not system-level shortcuts). A global binding has two practical risks: the terminal is precisely the place where deletion most easily goes wrong, and an accidental press while typing a password would immediately pop up the confirmation dialog. Once scoped, an accidental press in those places does nothing at all.

Confirmation method: a **zenity modal dialog**, with the body "Are you sure you want to permanently erase the items in the Trash?", "Cancel" on the left and "Empty Trash" on the right.

Early versions used desktop notifications; when notifications are muted or missed, the feature is as good as absent --
the user presses the key with no feedback at all (learned the hard way). When the dialog cannot be shown, it falls back to
"press the same combo again"; if that fails too, execution is abandoned.

Writing `confirm = false` in the rule table cannot bypass this either -- confirmation for destructive commands
is enforced by `src/exec.rs`.

### Granularity of terminal specialization (important)

It is **not** "turn every `Cmd` into `Ctrl+Shift` inside a terminal". That would degrade `Cmd+Q` into `Ctrl+Shift+Q` -- a combo nobody recognizes.

The actual approach is **per key**: only keys that **the terminal itself has already bound to `Ctrl+Shift`** get Shift added.

Going further: GNOME-family terminals declare all their shortcuts via GSettings, and mackbind reads their schema directly:

```bash
gsettings list-recursively org.gnome.Ptyxis.Shortcuts
# new-window      '<ctrl><shift>n'
# close-window    '<ctrl><shift>q'
# copy-clipboard  '<ctrl><shift>c'
```

So if the user changes the terminal's shortcuts, mackbind follows automatically. A hand-written allowlist once missed `n` (new window); that is the direct reason for switching to the schema.

Terminals that do not use GSettings (kitty, wezterm, etc.) have no schema to query, so they fall back to the built-in allowlist.

> Note: ptyxis's `close-window` is actually `Ctrl+Shift+Q`, but the global rule `Cmd+Q → Alt+F4` takes priority over terminal specialization, so what is actually emitted is `Alt+F4`. This is intentional -- `Alt+F4` is GNOME's generic close binding and is consistent across apps.

### Terminal copy key: the ptyxis / konsole special case

These two terminals, **when no text is selected**, pass `Ctrl+Shift+C` through to the child process as SIGINT, so "copy" kills the running task. wezterm, ghostty, and gnome-terminal do not have this problem (they swallow the combo unconditionally).

So these two instead use `Ctrl+Insert` (the traditional X11 copy key, supported by all terminals and never sent as a control character). It is a row in the rule table, and the criterion is a **behavioral difference**, not an app list.

### Shift+Enter in a terminal that cannot report it

`Shift+Enter` ("insert a soft newline" in agent TUIs) requires the terminal to report the modifier, which only the kitty keyboard protocol can do. VTE — hence gnome-terminal, ptyxis, Tilix, Terminator, Guake — implements neither it nor xterm's `modifyOtherKeys` in any released version. VTE's keymap gives plain Return no modifier encoding at all: `_vte_keymap_GDK_Return` holds only an Alt variant, and `_vte_keymap_key_get_modifier_encoding_method` lists the arrows, Insert/Delete/Home/End/Page Up/Down, F1–F35 and the keypad operators (including the keypad's own `KP_Enter`) but **not Return**. So `Shift+Enter` and `Ctrl+Enter` arrive as the same `\r` (0x0D) as bare `Enter`, and the application cannot tell them apart.

mackbind therefore emits `Ctrl+J` instead inside those terminals: 0x0A is a **distinct** control character that every terminal transmits unchanged, and readline, vim and the agent TUIs all read it as a newline. The row is deliberately app-scoped — terminals that do implement the protocol (kitty, foot, wezterm, ghostty, alacritty) must keep the real `Shift+Enter`, and a global remap would also cost GUI apps their own `Shift+Enter` (VS Code inserts a line below, chat apps insert a newline). Same criterion as the copy key above: a **behavioral difference**, not an app list. The list covers the mainstream VTE terminals: `org.gnome.Ptyxis` (Ptyxis), `org.gnome.Terminal` (GNOME Terminal), `org.gnome.Console` (GNOME Console), `com.gexperts.Tilix` (Tilix), `terminator` (Terminator), `guake` (Guake), `xfce4-terminal` (Xfce Terminal), `mate-terminal` (MATE Terminal), `lxterminal` (LXTerminal), `sakura` (Sakura), `tilda` (Tilda) and `com.raggesilver.BlackBox` (BlackBox). Terminals that implement the protocol must never be added.

---

## Power-key combos (in Apple's docs, but not done here)

Apple's shortcut table lists four combos with the power key:

|Combo|Effect|
|---|---|
|`Ctrl+Shift+Power`|Display sleep|
|`Ctrl+Power`|Restart/sleep/shut-down dialog|
|`Ctrl+Cmd+Power`|**Force restart**|
|`Ctrl+Option+Cmd+Power`|Quit all apps, then shut down|

**This tool does none of them (F)**, for these reasons:

1. **GNOME has no corresponding bindable action** -- the first two do not exist on the GNOME side; the last two
   could only be implemented with commands like `systemctl reboot` / `loginctl terminate-user`.
2. **The last two are global one-key destruction of all work** -- a forced restart discards all unsaved
   content. An unguarded global hotkey is a time bomb.
3. **You cannot actually press them** -- they require holding the power key plus up to four modifiers at once.

The bare power key **needs no handling**: GNOME's `power-button-action` defaults to `suspend`,
so pressing the power key sleeps, same as macOS.

> Note: Apple's docs list the condition for these four as "on a built-in keyboard without Touch ID".
> Different production batches of the same model differ, so go by your own keyboard's actual behavior.

## Appendix: Implementing the overview (Spotlight)

macOS's `Cmd+Space` corresponds to GNOME's activities overview. GNOME has provided bindable actions for this:

```bash
gsettings get org.gnome.shell.keybindings toggle-overview
# older GNOME / EndeavourOS measured: ['<Shift><Control>space']
# GNOME 50 and later (incl. Ubuntu 26.04): @as [] (unbound)
```

**Since GNOME 50 this action is empty by default**, and the overview is instead triggered by mutter's `overlay-key`
(default `Super`) -- i.e. **pressing the bare Super key**. This is fixed behavior of that version,
not "the user did not bind it".

mackbind therefore handles it in two layers:

1. Prefer to **read the existing `toggle-overview` binding and emit the same key** (the older path).
2. When that action is unbound, fall back to reading `org.gnome.mutter overlay-key` and emit **a tap**
   of that modifier key (the GNOME 50 path).

**Only append, never overwrite**: as long as `toggle-overview` is bound, path 1 is always used, and
older environments behave unchanged. Neither path rewrites the user's config, nor affects the user's own settings.

Why a tap must be "press, then release immediately": GNOME's overlay-key opens the
overview on **release**; if Super is held until the physical key (Space) is released, mutter will treat it as
`Super+Space` (switch input method) in the meantime.

Note: `Super+space` (GNOME's input-method switch) is not claimed under mackbind's model
-- the default semantics never produce Super. The two no longer conflict, so no "giving way" is needed.

### Once misjudged as infeasible

During the initial investigation only `org.gnome.desktop.wm.keybindings` was checked, and it indeed has no overview action, so it was concluded that implementing it would require "writing to the user account settings" -- an invasive change -- and it was deferred.

That conclusion was wrong -- the overview action lives under **another schema** (`org.gnome.shell.keybindings`). The lesson is the same as the earlier "default direction reversed" one: **before asserting that some capability does not exist, first make sure you are looking in the right place.**

What is genuinely unusable is the following (measured record, to avoid repeated attempts):

|Approach|Result|
|---|---|
|`org.gnome.Shell.Eval`|returns `(false, '')`; restricted since GNOME 41|
|`org.gnome.Shell.WindowTracker`|object path `/org/gnome/Shell/WindowTracker` does not exist|
|`xdotool` / `wmctrl`|unusable under Wayland|

### Keyboard-type differences

macOS's `Cmd+Dashboard` corresponds to `Fn+F11` on a MacBook, and to no key on an external keyboard with dedicated function keys. GNOME likewise has no dedicated "Dashboard" action (`Ctrl+↑` is semantically close, but GNOME does not provide that binding).

---

## Appendix: Implementing "Show desktop"

GNOME has a built-in `show-desktop` action (hides all normal windows), unbound by default:

```bash
gsettings get org.gnome.desktop.wm.keybindings show-desktop   # @as [] (unbound)
```

It corresponds to macOS's `Cmd+Dashboard` / `Fn+H` / `Fn+F11` (show desktop), and to `Ctrl+↑` (Dashboard).

Once the action is bound, mackbind could emit the corresponding key, but this too requires writing to account settings, so it is deferred.

> Note: entry names in the settings UI vary by GNOME version. On GNOME 51 (this machine) the
> schema key for this action is `show-desktop`; "hide all normal windows" may be the name in
> other versions or other schemas. The display name in the settings UI and the underlying key name
> are not necessarily the same.

### Mapping the three screenshot keys (measured, counterintuitive)

GNOME's three screenshot actions -- **which one captures directly and which one opens the UI is the opposite of intuition**:

|GNOME action|Binding on this machine|Actual behavior|
|---|---|---|
|`screenshot`|`Shift+Print`|**captures the full screen directly**, no UI|
|`screenshot-window`|`Alt+Print`|captures directly (also full-screen on this machine)|
|`show-screenshot-ui`|`Print`|**opens the screenshot UI**|

macOS, by contrast, captures the full screen directly with `Cmd+Shift+3` and enters selection mode with `Cmd+Shift+4/5`.
So we align by function:

|Key|Maps to|Behavior|
|---|---|---|
|`Cmd+Shift+3`|`screenshot` → `Shift+Print`|capture the full screen directly (same as macOS)|
|`Cmd+Shift+4`|`show-screenshot-ui` → `Print`|screenshot UI; choose region/window there|
|`Cmd+Shift+5`|`show-screenshot-ui` → `Print`|same as above|

**That 4 and 5 behave the same is a compromise.** GNOME has only three screenshot actions, and
`screenshot` and `screenshot-window` both measured as direct full-screen captures,
so region and window cannot be told apart from them. Both go through the screenshot UI uniformly,
where a region/window can still be chosen.

### The "Delete" key's keycode trap (learned on this machine)

macOS calls the backspace key (⌫) **Delete**; only the forward delete key (absent on MacBooks) is called
Forward Delete. The everyday "Cmd+Delete moves to trash" presses ⌫.

But in evdev these are two different keycodes:

|macOS name|Physical key|evdev keycode|Produced on this machine|
|---|---|---|---|
|Delete|⌫ backspace|`KEY_BACKSPACE`|✓ the MacBook keyboard only has this|
|Forward Delete|⌦ forward delete|`KEY_DELETE`|✗ this machine has no such key|

Earlier rules were all written against `KEY_DELETE`, which made **every deletion rule a no-op on this machine**
-- `Cmd+Delete`, `Option+Delete`, and `Shift+Cmd+Delete` all fell through, and because they fell through to
the default semantics, it looked like they merely "became Ctrl+backspace", which is hard to notice.

Now rules are written for both keycodes: this machine goes through `KEY_BACKSPACE`, external keyboards through `KEY_DELETE`.

|Combo|Mapping|Physical keys|
|---|---|---|
|`Cmd+Delete`|move to trash → `Ctrl+Delete`|Cmd+⌫|
|`Option+Delete`|delete word to the left → `Ctrl+Backspace`|Option+⌫|
|`Shift+Cmd+Delete`|empty trash → `gio trash`|Shift+Cmd+⌫|
|`Option+Shift+Cmd+Delete`|same as above|Option+Shift+Cmd+⌫|

> Note: `Option+Delete` maps to **`Ctrl+Backspace`**, not `Ctrl+Delete`.
> `Ctrl+Delete` in GTK text widgets means "delete the next word", the opposite direction
> from "delete word to the left".
