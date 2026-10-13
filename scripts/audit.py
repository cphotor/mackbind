#!/usr/bin/env python3
"""Cross-check docs/macos-shortcuts.md against the mapping rules actually
implemented in src/.

Background: this project's docs and implementation have drifted apart several
times -- the docs claimed a rule was "implemented" while the code had no such
rule. Both cases share the same cause: checking by reading the docs by hand
instead of computing the difference by machine. This script normalizes both
sides into the same combo representation and computes the difference directly.

Usage:
    python3 scripts/audit.py            # report differences, exit code 0
    python3 scripts/audit.py --strict   # exit code 1 on any unimplemented item (for CI)

Normalized representation on both sides
---------------------------------------
Doc side `Cmd+Shift+←`      → `S-KEY_LEFT`
Code side `"S-KEY_LEFT"`    → `S-KEY_LEFT`

The Cmd key is implicit in the internal representation -- pressing Meta itself
emits no event; only a combo of "Meta + some main key + some modifiers" matches
a rule. So `Cmd` in the docs is not encoded, while the other modifiers are
encoded as usual (Shift→S, Option→A, Ctrl→C).

It also guards the version number: `Cargo.toml` is the single source of truth,
and this script fails if the newest `CHANGELOG.md` heading disagrees with it.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "macos-shortcuts.md"
CONFIG_RS = ROOT / "src" / "config.rs"
MAPPER_RS = ROOT / "src" / "mapper.rs"
CARGO_TOML = ROOT / "Cargo.toml"
CHANGELOG = ROOT / "CHANGELOG.md"

# ── Key-name mapping ──────────────────────────────────────────────────
# Doc spelling → evdev key-name stem (without the KEY_ prefix)

SPECIAL_KEYS = {
    "←": "LEFT",
    "→": "RIGHT",
    "↑": "UP",
    "↓": "DOWN",
    "Delete": "DELETE",
    "Backspace": "BACKSPACE",
    "Space": "SPACE",
    "Enter": "ENTER",
    "Return": "ENTER",
    "Tab": "TAB",
    "Esc": "ESC",
    "Escape": "ESC",
    "[": "LEFTBRACE",
    "]": "RIGHTBRACE",
    "{": "LEFTBRACE",
    "}": "RIGHTBRACE",
    ",": "COMMA",
    ";": "SEMICOLON",
    "'": "APOSTROPHE",
    ".": "DOT",
    "/": "SLASH",
    "\\": "BACKSLASH",
    "`": "GRAVE",
    "-": "MINUS",
    "_": "MINUS",
    "=": "EQUAL",
    "+": "EQUAL",
    "Home": "HOME",
    "End": "END",
    "PageUp": "PAGEUP",
    "PageDown": "PAGEDOWN",
    "Print": "SYSRQ",
}

# Modifier → internal abbreviation. Note there is no Cmd/M: it is treated as
# implicit, see the module docstring.
MOD_ABBREV = {
    "Shift": "S",
    "Option": "A",
    "Alt": "A",
    "Ctrl": "C",
    "Control": "C",
}


def encode_key(token: str) -> str | None:
    """A single key token → key-name stem. Returns None for unknown tokens."""
    token = token.strip()
    if not token:
        return None
    if token in SPECIAL_KEYS:
        return SPECIAL_KEYS[token]
    # Keys like brightness/volume/dashboard have no dedicated keycode on Linux,
    # so they are not compared.
    if "Brightness" in token or "Volume" in token or "Dashboard" in token:
        return None
    if re.fullmatch(r"[A-Za-z]", token):
        return token.upper()
    if re.fullmatch(r"\d", token):
        return token
    if re.fullmatch(r"F\d{1,2}", token):
        return token
    return None


def encode_combo(combo: str) -> list[str]:
    """`Cmd+Shift+←` → ['S-KEY_LEFT'].

    Handles `/` alternation (`Cmd+←/→` → both left and right) and numeric lists
    such as `3/4/5`. Modifier prefixes are shared across the alternation. Returns
    an empty list when it cannot encode.
    """
    combo = combo.strip().strip("`")
    if not combo:
        return []

    parts = [p.strip() for p in combo.split("+")]
    if len(parts) < 2:
        return []

    main = parts[-1]
    mods = parts[:-1]

    # `Cmd+Shift+3/4/5` → several candidate main keys
    candidates = [main]
    if "/" in main:
        head, *rest = main.split("/")
        candidates = [head] + rest

    out = []
    for cand in candidates:
        key = encode_key(cand)
        if key is None:
            continue

        modset = []
        unsupported = False
        for m in mods:
            if m in ("Cmd", "Command", "Super", "Meta"):
                continue  # implicit, not encoded
            if m == "Fn":
                unsupported = True  # Fn goes through a separate key-stage layer
                break
            if m in MOD_ABBREV:
                modset.append(MOD_ABBREV[m])
            else:
                unsupported = True
                break
        if unsupported:
            continue

        modset.sort()
        out.append("-".join(modset + [f"KEY_{key}"]))
    return out


def _has_cmd(raw_combos: list[str]) -> bool:
    """Whether the row contains Cmd -- the default semantics only apply to Cmd combos."""
    return any(
        m in ("Cmd", "Command", "Super", "Meta")
        for combo in raw_combos
        for m in combo.strip("`").split("+")[:-1]
    )


def norm_target(target: str) -> str:
    """Normalize a doc target spelling into the `Ctrl+LEFT` form.

    The target column is looser than the combo column: it may be `Ctrl+←/→`
    (with alternation), `Ctrl+B/I/U` (several main keys in parallel), or carry
    backticks. Here we unify it into a single "modifiers+main key" string; when
    several main keys are present, the caller compares the joined form of all
    candidates -- in practice a target containing `/` is expanded on both sides.
    """
    t = target.strip().strip("`").strip()
    if "/" in t:
        t = t.split("/")[0].strip()
    parts = [p.strip() for p in t.split("+")]
    mods, main = parts[:-1], parts[-1]
    key = encode_key(main)
    if key is None:
        return t
    out = []
    for m in mods:
        if m in ("Ctrl", "Control"):
            out.append("Ctrl")
        elif m in ("Shift",):
            out.append("S")
        elif m in ("Alt", "Option"):
            out.append("A")
        else:
            out.append(m)
    return "+".join(out + [key])


# ── Parse the docs ────────────────────────────────────────────────────

# For the meaning of the classes see docs/macos-shortcuts.md "Classification".
# A: covered by default semantics; C: terminal specialization; D: needs an
# explicit rule; E: proactively implemented
# F: concept does not exist; B: Super-related (deferred); the rest (app-handled,
# etc.) are not compared.
NEEDS_RULE = {"D", "E"}


def parse_doc() -> list[dict]:
    """Extract every three-column table row from the docs."""
    rows = []
    for line in DOC.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) != 3:
            continue
        macos, target, cls = cells
        if cls.startswith("---") or set(cls) <= {"-", ":"}:
            continue
        if not cls or cls.startswith("**"):
            cls = cls.strip("*")
        if not re.fullmatch(r"[A-F]", cls):
            continue
        combos = [c for c in re.findall(r"`([^`]+)`", macos)]
        if not combos:
            continue
        rows.append(
            {"macos": macos, "target": target, "cls": cls, "combos": combos}
        )
    return rows


# ── Parse the code ────────────────────────────────────────────────────


def parse_rules() -> set[str]:
    """The input-side combos of every rule in src/config.rs's default_rules().

    After the table-driven refactor, the single source of truth for mappings is
    the `push_emit` calls inside `default_rules()`. They look like
    `push_emit(&[], "KEY_LEFT", "C", "Left", "...")` -- the second argument is
    the input-side combo (without the implicit Cmd).

    Note: this only takes the fixed-output rules produced by `push_emit`.
    `TerminalCtrlShift` and `DeAccel` are conditional/runtime actions and are
    counted separately by parse_conditional_rules.
    """
    text = CONFIG_RS.read_text(encoding="utf-8")
    out = set()
    for m in re.finditer(r'push_emit\(\s*&\[(.*?)\]\s*,\s*"([^"]+)"', text, re.S):
        apps, combo = m.group(1), m.group(2).strip()
        if combo:
            out.add(combo)
    # All combo-shaped strings in the rule table.
    #
    # We do not locate them via `keys: vec![...]`: some rules are generated in a
    # loop (`keys: vec![keys.into()]`), so the regex cannot grab a literal.
    # Instead we match the "modifier prefixes + KEY_xxx" shape inside the body of
    # default_rules(); the desc strings are plain prose and will not match by
    # accident.
    body = _body_of(text, "fn default_rules")
    for k in re.findall(r'"([ASCM-]*KEY_[A-Z0-9]+)"', body):
        out.add(k)
    return out


def _body_of(text: str, marker: str) -> str:
    """Return the body from after `marker` up to the next top-level `fn`/`impl`.

    `default_rules()` is the last big function in the file, followed directly by
    other top-level definitions, so when no further `\\nfn ` is found we fall back
    to the end of the file.
    """
    i = text.index(marker)
    m = re.search(r"\n(fn |impl |const |#\[cfg)", text[i + len(marker):])
    j = i + len(marker) + m.start() if m else len(text)
    return text[i:j]


def parse_conditional_rules() -> set[str]:
    """Input-side combos covered by conditional-action rules.

    `keys = vec!["*"]` means a main-key wildcard -- which keys apply is decided
    at runtime, so they cannot be counted key by key (that is precisely why it
    exists: terminal bindings differ per terminal).
    """
    text = CONFIG_RS.read_text(encoding="utf-8")
    out = set()
    # Rules outside push_emit that spell out `keys` explicitly
    for m in re.finditer(r'keys:\s*vec!\[([^\]]*)\]', text, re.S):
        for k in re.findall(r'"([^"]+)"', m.group(1)):
            k = k.strip()
            if k and k != "*":
                out.add(k)
    return out


def parse_terminal_shift_keys() -> set[str]:
    """The fallback key set for terminal specialization (the static part outside the wildcard rule)."""
    text = CONFIG_RS.read_text(encoding="utf-8")
    m = re.search(r"DEFAULT_TERMINAL_SHIFT_KEYS[^=]*=\s*&\[(.*?)\n\];", text, re.S)
    if not m:
        return set()
    return {k.strip() for k in re.findall(r'"([^"]+)"', m.group(1))}


def parse_remap() -> set[str]:
    text = CONFIG_RS.read_text(encoding="utf-8")
    m = re.search(r"remap:\s*vec!\[(.*?)\n\s*\],", text, re.S)
    if not m:
        return set()
    return set(re.findall(r'from:\s*"([^"]+)"', m.group(1)))


def parse_fn_rules() -> set[str]:
    """Fn key-stage switching rules in mapper.rs, of the form KEY_F11 → KEY_VOLUMEDOWN."""
    text = MAPPER_RS.read_text(encoding="utf-8")
    return set(re.findall(r"\(KeyCode::(KEY_\w+),\s*KeyCode::(KEY_\w+)\)", text))


# ── Report ────────────────────────────────────────────────────────────


def parse_cargo_version() -> str | None:
    """The one authoritative version: `[package] version` in Cargo.toml."""
    text = CARGO_TOML.read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    return m.group(1) if m else None


def parse_changelog_version() -> str | None:
    """The newest released heading in CHANGELOG.md, e.g. `## [0.1.0] - ...`."""
    text = CHANGELOG.read_text(encoding="utf-8")
    m = re.search(r"^##\s*\[([^\]]+)\]", text, re.M)
    return m.group(1) if m else None


def check_version() -> list[str]:
    """The version number has a single source of truth: Cargo.toml.

    Everything else either derives from it (the startup log reads it at compile
    time; the README badge reads the git tag) or records it (CHANGELOG.md). This
    check keeps that record from ever drifting away from the source.
    """
    errors: list[str] = []
    cargo_v = parse_cargo_version()
    changelog_v = parse_changelog_version()
    if cargo_v is None:
        errors.append(f"{CARGO_TOML.name}: no [package] version found")
    if changelog_v is None:
        errors.append(f"{CHANGELOG.name}: no '## [x.y.z]' heading found")
    if cargo_v and changelog_v and cargo_v != changelog_v:
        errors.append(
            f"version mismatch: {CARGO_TOML.name} says {cargo_v} but "
            f"{CHANGELOG.name}'s newest entry says {changelog_v}"
        )
    return errors


def main() -> int:
    rows = parse_doc()
    implemented = (
        parse_rules()
        | parse_conditional_rules()
        | parse_terminal_shift_keys()
    )
    remap = parse_remap()
    fn_rules = parse_fn_rules()

    print(f"{len(rows)} doc entries; {len(implemented)} explicit rules")
    print(f"remap {sorted(remap)}")
    print(f"Fn rules {sorted(fn_rules) or '(none)'}")
    print()

    missing: list[tuple[str, str, str]] = []
    misfiled: list[tuple[str, str, str]] = []

    for row in rows:
        combos = [c for combo in row["combos"] for c in encode_combo(combo)]
        if not combos:
            continue  # Fn / brightness keys / app-handled: not compared

        if row["cls"] in NEEDS_RULE:
            hit = [c for c in combos if c in implemented]
            if not hit:
                missing.append((row["macos"], row["target"], " / ".join(combos)))
        elif row["cls"] == "A":
            # Class A claims "covered by the default Cmd+X → Ctrl+X". What the
            # default semantics actually produce is Ctrl + <same main key> + the
            # non-Cmd modifiers kept as-is -- the claim holds only when the target
            # is exactly that. Otherwise the default mapping cannot yield it.
            #
            # Rows without Cmd (e.g. macOS's `Ctrl+H`) are not compared: the
            # default semantics apply only to Cmd combos, and these keys already
            # have the same name on Linux.
            cmd_rows = [c for c in combos if len(c.split("-")) >= 1 and _has_cmd(row["combos"])]
            if not cmd_rows:
                continue
            target_norm = norm_target(row["target"])
            outs = []
            for combo in cmd_rows:
                parts = combo.split("-")
                key = parts[-1].replace("KEY_", "")
                outs.append("Ctrl+" + "+".join(parts[:-1] + [key]))
            # Alternating keys (`Cmd+←/→`) expand into several candidates: a hit
            # on any one counts as covered.
            if not any(o.upper() == target_norm.upper() for o in outs):
                misfiled.append((row["macos"], row["target"], " / ".join(outs)))

    if misfiled:
        print("── Questionable classification: marked A (covered by default semantics), but the target is not Ctrl+the same main key ──")
        print("   The default mapping only produces Ctrl+<same key>; it cannot yield these targets.\n")
        for macos, target, combo in misfiled:
            print(f"  {macos:<24} target {target:<14} default would emit {combo}")
        print()

    if missing:
        print("── Claimed to need an explicit rule (D/E), but the code has no such entry ──\n")
        for macos, target, combo in missing:
            print(f"  {macos:<28} target {target:<22} expected {combo}")
        print()

    version_errors = check_version()
    if version_errors:
        print("── Version drift: the single source of truth is Cargo.toml ──\n")
        for e in version_errors:
            print(f"  {e}")
        print()

    if not missing and not misfiled and not version_errors:
        print("Docs and implementation agree.")
        return 0

    print(
        f"{len(missing)} missing, {len(misfiled)} questionable classification, "
        f"{len(version_errors)} version error(s)."
    )
    return 1 if "--strict" in sys.argv else 0


if __name__ == "__main__":
    sys.exit(main())
