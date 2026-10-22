# Table-Driven Refactor Plan

**Status: done (behavior-preserving).** The text below keeps the "why we did it this way",
including a few less-than-obvious design trade-offs.

## Why refactor

Before the refactor, "what should a given Cmd combo emit" was answered jointly by **six parallel mechanisms**:

|Mechanism|Location|Answers|
|---|---|---|
|`cmd_map`|`config.rs`|global explicit rules|
|`app_cmd_map`|`config.rs`|per-app overrides|
|`terminal_shift_keys` + `ctrl_busy_apps`|`config.rs`|add Shift to some keys inside terminals|
|default semantics|`mapper.rs` branch|`Cmd+X → Ctrl+X`|
|DE feature rules|`mapper.rs` + `accel.rs`|read an existing DE binding and emit it|
|runtime GSettings|`desktop.rs` / `accel.rs`|terminal detection and shortcut discovery|

They interlocked, and their priority was maintained by an if-else chain in `mapper.rs`:

```
app_cmd_map  >  cmd_map  >  terminal specialization  >  default semantics
```

The problem is not the number of lines but that **"what it does" cannot be seen at a glance**. This directly caused
`scripts/audit.py`'s first run to find 11 doc entries claiming an implementation with no code behind them, and 2 misclassified entries.

## The structure now

One table answers everything, 44 rows:

|App|Key combo|Action|
|---|---|---|
|`*`|`Cmd+Q`|`emit` `Alt+F4`|
|`*`|`Cmd+[`|`emit` `Alt+Left`|
|`*`|`Cmd+Space`|`de_accel` `overview`|
|`org.gnome.Ptyxis` etc.|`Cmd+C`|`emit` `Ctrl+Insert`|
|VTE terminals (Ptyxis, GNOME Terminal/Console, Tilix, Terminator, Guake, Xfce, MATE, LXTerminal, Sakura, Tilda, BlackBox)|`Shift+Enter` (no Cmd)|`emit` `Ctrl+J`|
|`gvim`|`Cmd+any key`|`terminal_ctrl_shift`|
|`TerminalEmulator`|`Cmd+any key`|`terminal_ctrl_shift`|

Cmd is implicit for almost every row. The handful of non-Cmd combos (macOS's `Option+Delete`,
`Ctrl+↑`, and the terminal `Shift+Enter` above) write `cmd = false` and are listed here without a Cmd prefix.

**Order is priority** -- the priority relationships formerly scattered across three places are now decided solely by
a rule's position in the table. Lookup is a linear scan taking the first hit; user rules are prepended before the
defaults, so they take priority over a built-in row with the same `(app, keys)`.

`mackbind --verbose` prints the whole table at startup.

## Actions are functions

The key point: **an action is not an enum variant, it is a function**. So the table is not limited to static mappings.

|Action|Responsibility|
|---|---|
|`emit`|produce key events directly|
|`de_accel`|look up an existing DE binding and emit the same key|
|`terminal_ctrl_shift`|look up the target app's own bindings and decide whether to add Shift|

The latter two do runtime lookups internally. This is not a compromise but the right layering: **the table names,
the function evaluates**. Cramming in a static snapshot is what goes stale -- and that is exactly the
disease that made the docs drift.

### `terminal_ctrl_shift` and the wildcard main key

This one best shows the necessity of "static table + runtime evaluation".

A terminal's `Ctrl+Shift` bindings are looked up **at runtime** from its GSettings, and they differ per terminal:
ptyxis declares `new-window = Ctrl+Shift+N`, and `N` is not on any static list.
Listing rules per key in the table would leave no place for keys discovered at runtime -- that was exactly
the defect before commit `18c03a2` (then solved by wholesale replacement of `terminal_bindings`, at the cost
that the logic could not be expressed as data).

So the table uses `keys = ["*"]` as a main-key wildcard, and the action decides at runtime which keys apply.
The fallback list `DEFAULT_TERMINAL_SHIFT_KEYS` is used only when no schema can be found
(terminals that do not go through GSettings, such as kitty and wezterm).

### `resolve_fn` is not in the table

The meaning of F1–F12 depends on the Fn state and the **full modifier context** (on macOS,
`Cmd+Shift+F1` requires pressing `Cmd+Shift+Fn+F1`); it is a **key-level** transformation rather than a combo rule,
so it stays outside the table -- but it too has been unified into a single function, `resolve_fn`, which **only substitutes and never short-circuits**.

It previously used a `return` to short-circuit, bypassing the whole table-lookup flow; measured,
`Fn+Meta+Shift+F1` emitted `Meta+media key`: Meta leaked (it should have been suppressed) and
Ctrl/Shift were lost. Fixed, with 6 tests added.

## Parts outside the table (must be declared explicitly)

Not every behavior is a row. The following two stay outside the table:

1. **Modifier remapping** -- CapsLock ↔ LeftCtrl swap as a pair. It happens **before** combo matching
   and applies to the key itself rather than a combo. The table's `keys` column cannot express
   "one physical key is replaced by another".
2. **Key-release symmetry** -- a release event must match the decision made on press (otherwise the
   output side is left with a stuck-down key). This is a state-machine responsibility and should not be expressed as a row.

## Values of the app column

|Value|Meaning|
|---|---|
|`*`|all apps|
|`TerminalEmulator`|decided by the `.desktop` entry's `Categories` (freedesktop spec)|
|`gvim` and other wm_class values|match the window class by substring|

The `TerminalEmulator` value is the key one -- installing a new terminal works automatically, with no list to
maintain. This capability is retained from `238c703`; do not regress to a hard-coded list (it once missed
the real `org.gnome.Ptyxis` because it was written in lowercase as `ptyxis`).

## Two pitfalls hit during the refactor

Both surfaced because of the new property "order is priority", and are worth recording:

1. **More specific rules must come before more general ones.** ptyxis's `Ctrl+Insert`
   was originally placed after the general terminal rule and could never match -- because lookup takes the **first** hit.
2. **The same block of rules got duplicated.** While fixing the above, a new
   `TerminalEmulator` loop was added but the original was not deleted, so the general rule still came first.

Lesson: after changing the rule table, print the actual order with `mackbind --verbose` to check it,
don't trust your memory.

## Premise: every row needs measured evidence

A table is only valuable when "every row knows why it exists". Of the 11 currently missing doc entries,
several turned out to be impossible to implement as described:

|Doc entry|Reality|
|---|---|
|`Ctrl+Cmd+Q` lock screen → `Super+l`|GNOME 51 has **no `lock-screen` binding key**; lock screen is not in the wm.keybindings schema. Should be marked **F**|
|`Cmd+Shift+3/4/5` screenshot → `Print`|GNOME's existing bindings are `Print` (screenshot UI) / `Alt+Print` (window) / `Shift+Print` (region), which do not match how the docs write it|
|`Ctrl+Cmd+F` full screen → `F11`|implementable. `toggle-fullscreen` exists but is unbound (`@as []`); emitting `F11` is enough|

These are not handled yet -- **the next step is to verify them one by one on the real machine and decide from the results which rows to add or remove from the table**.
