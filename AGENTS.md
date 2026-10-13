# AGENTS.md

Hard reminders for agents (including future me) working in this repo.
**These are not style preferences. They are lessons this project paid for.**

## 0. Language: everything pushed must be English

**All content that is committed and pushed must be written in English. No Chinese
characters may appear anywhere in the repository** — including code comments,
docstrings, `eprintln!`/log messages, test assertion messages, `README.md`,
`DESIGN.md`, `docs/**`, shell-script output, `config.example.toml` comments, and
**commit messages**.

Rationale: the repository is public (https://github.com/cphotor/mackbind) and its
audience is not assumed to read Chinese.

Consequences to keep in mind:

- When you change user-visible behavior, also update the English wording in the
  matching docs. Do not leave a Chinese description behind "because it's clearer".
- `scripts/audit.py` machine-parses `docs/macos-shortcuts.md`; a few of its
  matchers key off the **literal wording** in that doc (e.g. the tokens
  `Brightness` / `Volume` / `Dashboard` in the macOS-combo column). If you reword
  the doc, update `audit.py` in the same commit — otherwise `audit.py --strict`
  fails.
- Before committing, grep the tree for CJK to be sure:
  `grep -rlP '[\x{4e00}-\x{9fff}]' $(git ls-files)` should print nothing.

## 1. Before claiming a capability does not exist, check that you are looking in the right place

This project drew the wrong conclusion three times by "looking in the wrong
place"; the symptom was always `ServiceUnknown` / "does not exist" / empty result:

|Claim|Truth|
|---|---|
|Overview action must be written to user settings — too invasive|It lives in `org.gnome.shell.keybindings`, not `wm.keybindings`|
|The `focused-window-dbus` extension is unavailable|The D-Bus address was wrong. The extension exports its object directly from the Shell process, so `--dest` must be `org.gnome.Shell`, not `org.gnome.Shell.Extensions.Dbus`|
|`Ctrl+Cmd+Q` lock-screen cannot be implemented (basis: GNOME has no such binding)|The conclusion was right, but only *by luck* — the real reason is that this action is not in the `wm.keybindings` schema and does not constitute "the user pressed it and nothing happened"|

When `gsettings` reports "there is no such key xxx", it has two meanings: the
capability truly does not exist, or it lives in **another schema**. To tell them
apart, first `list-keys` the whole relevant schema instead of jumping to a
conclusion.

**Search the whole domain first**, not just the single most likely place.

### A fourth form of the same mistake: "measured no effect" ≠ "does not exist"

`Option+Cmd+W` (close all app windows) did nothing in Chrome, so it was once
tagged **F (capability does not exist)**. In fact Apple's official docs clearly
list this shortcut — it is just **implemented by each app**: Finder has it,
Chrome does not.

Three kinds of "can't find it" must be kept apart:

|Symptom|Real meaning|
|---|---|
|The command errors / returns empty|Wrong place, or wrong schema|
|Pressing it in the app does nothing|**Maybe that app just hasn't implemented it**|
|It is absent from the official docs too|Only then does it truly not exist|

Before concluding "does not exist", ask: **is it only this one target that has not
implemented it?** Retest with a different app, or check the official docs.

## 2. Do not infer behavior by reading code — write a probe and measure

**Preferred tool: `mackbind --trace`.** It does not grab, create a virtual device,
or inject any event — the keyboard keeps its native behavior, **you can still
type normally**, and it prints "input event → what should have been emitted" to
stderr:

```bash
setsid ./target/release/mackbind --trace > trace.log 2>&1 < /dev/null &
# after the user presses keys
grep '^\[trace\] IN' trace.log
```

**Why this tool is irreplaceable**: once mapping is active the keyboard is owned
by this program, and **when it misbehaves the user cannot type, so they cannot
observe the situation**. Trace mode sidesteps exactly that. For any mapping
problem, get evidence with it first; do not reason it out by reading code.

A throwaway `mod probe` inside a unit test is the second-best option.

### One misdiagnosis, then two wrong turns

The user reported "A became Cmd+A". I chased two directions in turn:

1. The pass-through path failed to emit the Meta release → fixed it, did not help.
2. Residual modifier state on the GNOME side → added startup cleanup, still did not help.

The user said "**it's still like this after a reboot**" — that sentence should have
made me drop the "residual state" hypothesis immediately, because a reboot clears
all in-memory state. I didn't react, and burned another round.

The real root cause: **a bare space press matched the `Cmd+Space` rule**, popped
up the GNOME overview, which swallowed the following keystrokes — so it looked
like the key had been remapped. It was a deterministic bug, not residual state.
**"A reboot doesn't fix it" is the key signal that separates the two classes.**

Lesson: for any "state-like" symptom (stuck, sticky, stops working), first ask
**"does a reboot / restarting the service recover it?"** If yes → a state problem;
if no → a logic problem. Getting this order backwards wastes a lot of time.

**Process red line: after changing a core path (lookup scope, match conditions,
default semantics), you must verify with `--trace` on the real machine before
deploying.** 89 unit tests all green (the count at the time) did not stop that bug
— the tests covered the cases I had written down, not common sense like "a bare
keypress must not trigger a Cmd rule".

### Deploy procedure (fixed after two mishaps)

```bash
cargo build --release
systemctl --user stop mackbind        # must stop first, or cp fails with "text file busy"
cp target/release/mackbind ~/.local/bin/mackbind
systemctl --user start mackbind
# verify:
md5sum ~/.local/bin/mackbind target/release/mackbind
```

**Checking the MD5 is mandatory.** Twice the user tested an old version because it
was skipped: once I ran `cargo build` but forgot to `cp` and just `start`ed; once
the service was already running and `cp` failed with "text file busy" but the
failure was swallowed by `&&`. The user tested the old binary, two rounds of
verification were wasted, and the reported symptom ("Ctrl+↑ does nothing") sent me
down the wrong path.

**Whether the service is running and which version it runs are two separate
questions and must be confirmed separately.** I once assumed deployment succeeded
just because the service was active.

### Before stopping the service to diagnose, plan how to restore it

To capture evidence with `--trace` you must stop the service (trace does not grab,
but while the service holds the device, trace cannot read events). **This once led
me to have the user test three rounds against a program that was not running** —
the keyboard was native, Cmd was just an ignored modifier, so `Cmd+Shift+4` acted
as `Shift+4` and typed `$`. I then concluded "the mapping is wrong" and hunted in
the wrong direction.

Rules:

1. **Before starting a diagnosis, tell the user "the service is stopped, the
   keyboard is native again"** and say what to test.
2. **Restore the service as soon as the diagnosis ends** — don't defer it to the
   next round.
3. Before reporting a symptom, run `systemctl --user is-active mackbind` once —
   "the mapping didn't take effect" and "the program isn't running at all" look
   nearly identical.

**When a symptom appears, confirm the premises before analyzing the cause.** With
the service not running, every mapping-related conclusion is invalid.

## 3. Docs and implementation must be checked by machine, not by hand

`scripts/audit.py` exists for this. Hand-checking has already cost twice
(see DESIGN.md and `docs/table-driven-design.md`).

**After changing mapping rules, run it once:**

```bash
python3 scripts/audit.py            # report differences
python3 scripts/audit.py --strict   # exit code 1 on any difference, for CI
```

When adding a mapping rule, update the matching row in
`docs/macos-shortcuts.md` at the same time, or the script reports it as
"the docs claim it must be implemented but the code does not have it".

## 4. When touching original macOS behavior, state your confidence

Some entries in `docs/macos-shortcuts.md` were written from memory, not from
Apple's official documentation. Known-suspect ones include
`Option+Shift+←/→` extending to word boundaries.

**Do not implement rules on top of unverified entries.** Marking something F
(give up) is more honest than guessing an implementation.

## 5. Run the tests before changing things, and leave no probe code behind

```bash
cargo test
```

Temporary probes (the `mod probe` used to print actual output) must be deleted
when done. `git status` should be clean before committing.

## 6. Core invariants of this project (do not break them while editing)

1. **CapsLock ↔ LeftCtrl is a paired swap** — the goal is for both keys to emit a
   **real Ctrl**, not for them to become each other. A one-way swap leaves the
   physical Ctrl key dead.
2. **A release event must match the decision made on press** — otherwise the
   output side is left with a stuck-down key. Any new key translation must
   consider the release path.
3. **A suppressed key must not emit an event** — after Meta is suppressed, its
   release is not emitted either. **The test is "is the output side still holding
   it", not "was it ever suppressed"**: a key that was suppressed may later be
   emitted deliberately on another path (e.g. `Ctrl+Cmd+Q` lock-screen emits
   `Super+L`), and returning early on "was suppressed" would swallow the release.
   See the incident record below.
4. **Suppress output while a modifier is held, and decide after the combo is
   known** — a lone modifier is not passed through directly (Meta), but
   Shift/Alt/Ctrl must be passed through (fcitx5 and friends watch a bare Shift).
5. **Emit using the physically pressed key; look up using the normalized key.**
   Mixing the two makes the release path mismatch and leaves modifiers stuck.
6. **Auto-repeat events (`value=2`) and `MISC` events must be filtered** —
   otherwise they produce paired press/release.
7. **Exit must actually exit** — otherwise `Cleanup` does not run, modifiers are
   left behind, with the same consequences as the two Super-stuck incidents
   below. See the note below.

### Exit path: don't use `libc::signal()`

Symptom: shutdown hung for 90 seconds with "A stop job is running for User
Manager for UID 1000", then SIGKILLed, and `Cleanup`'s Drop never ran.

Cause: `libc::signal()` has BSD semantics and implies `SA_RESTART` — after the
handler returns, the kernel **automatically restarts** the `read()` it
interrupted. The main loop was blocked in `fetch_events()`, so it could never get
back to the top of the loop to check `SHUTDOWN`, and the program never exited.

Key points:

1. Use `sigaction` with `sa_flags = 0`, so the blocking read returns `EINTR`.
2. The main loop must treat `ErrorKind::Interrupted` as "continue", not as a read
   error that exits — otherwise any unrelated signal's `EINTR` would make the
   program exit.
3. **Every** blocking loop must respond to `SHUTDOWN`. `run_trace` once missed the
   check, so trace mode hung the same way.
4. In a function whose signature is `-> !`, use `exit(0)`, not `break`.

When diagnosing such a hang, first check whether the duration is **exactly equal to
some timeout value** (here 90 seconds = `DefaultTimeoutStopSec=1min 30s`); if it is,
it means you waited for the timeout and got KILLed, not that some unit was doing
slow work. `journalctl -b -1` and search for `timed out. Killing` points straight
at the culprit.

### The two Super-stuck incidents (same class, twice)

**First**: the pass-through path emitted a real Meta without removing it from
`suppressed`. Real-world result: after locking the screen, every character typed
at the password prompt carried Super, `9` could not be entered, and the only way
out was a hard power-off.

**Second**: the release path returned early on "was suppressed" and did not emit
the release. `Ctrl+Cmd+Q` lock-screen emits `Super+L` — that Super *is* the
suppressed Meta. When the user released Cmd the release was swallowed and Super
stuck. Real-world result: Space became Cmd+Space (opening the overview), W became
Cmd+W, and typing only worked while holding Shift.

Both stem from the same wrong assumption: **"was suppressed ⇒ was never emitted"**.
It does not hold — suppression only means "not emitted at press time"; a later path
may still emit it deliberately.

Lesson one: when fixing this class of bug, the test must use **observable state**
("is it in `output_down`"), not a **historical marker** ("is it in `suppressed`").
Historical markers are falsified by later paths.

Lesson two: this class of bug is very hard to exhaust with hand-written tests per
combination. Three **exhaustive** tests now back it up:

- `exhaustive_clean` — exhaustively press "modifier + main key" combinations, then release everything
- `release_order` — exhaustively try release orders (including releasing the modifier first)
- `lock_screen_meta_leak` — specifically covers paths whose output contains Meta

Any new output path will be caught by these three tests for the same class of
problem. **Note**: they only expose it when **the conversion result itself contains
Meta**; a path like `Cmd+Q`→`Alt+F4` that contains no Meta will not be caught — so
validate with the lock-screen path.

## 7. Decision priority: real machine > docs > code inference

When the three conflict, the real machine wins, and **go back and fix the docs and
code comments**. Several comments in this project used to state speculation as
fact (see lesson one); that kind of comment is more harmful than no comment — it
stops the next person (or future me) from doubting.

## 8. Conventions for the mapping rule table

After the refactor, `default_rules()` in `src/config.rs` is the **single source of
truth** for mappings: 29 rows, each `(app, key combo) → action`. See
`docs/table-driven-design.md`.

Two easy-to-miss points when editing the rule table:

1. **Order is priority**; lookup takes the first match. More specific rules
   (app-scoped) must come **before** more general ones (e.g.
   `TerminalEmulator`), or they never take effect.
2. **`keys = ["*"]` is a main-key wildcard**, used by actions like
   `terminal_ctrl_shift` that decide at runtime which keys apply. Do not read it
   as "all keys".
3. **Cmd is implicit, but "implicit" does not mean "unconditional".** The vast
   majority of rules require Cmd to be held (`cmd = true`, the default); only a
   handful of non-Cmd combos write `cmd = false`. Missing this distinction is what
   makes **a bare space also match `Cmd+Space`** — anyone pressing space pops up
   the overview.

After editing, print the actual table with `mackbind --verbose` to check the order
— don't trust memory; then verify the actual input/output with `--trace`.

## 9. Work in progress

The table-driven refactor is done. **The next step is to verify, one by one, on the
real machine** the 5 entries that "the docs claim must be implemented but the code
does not have", and add or remove rows in the rule table based on the results:

|Combo|Target|Status|
|---|---|---|
|`Cmd+Shift+3/4/5`|screenshot → `Print` / `Shift+Print`|GNOME has a ready-made binding; to implement|
|`Ctrl+↑`|mission control → overview|same action as `Cmd+Space`; to implement|
|`Shift+Cmd+Q`|log out → `Super+Shift+Q`|to check on the GNOME side|
|`Shift+Cmd+Delete`|empty trash|needs a new `exec` action|
|`Option+Shift+Cmd+Delete`|same (no confirmation)|needs a new `exec` action|

The last two happen to validate the "call a function when it cannot be mapped"
design — just add an `exec` action to the table.

Today's date: Tue Oct 13 2026
