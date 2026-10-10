#!/usr/bin/env python3
"""核对 docs/macos-shortcuts.md 与 src/ 中实际实现的映射规则。

背景：本项目的文档与实现曾多次脱节——文档声称「已实现」而代码里
并无对应规则。两者的成因相同：靠人读文档核对，而不是机器求差集。
本脚本把两边都规范化成同一种组合表示，直接算差集。

用法：
    python3 scripts/audit.py            # 报告差异，退出码 0
    python3 scripts/audit.py --strict   # 有未实现项时退出码 1（供 CI 用）

两侧的规范化表示
----------------
文档侧 `Cmd+Shift+←`      → `S-KEY_LEFT`
代码侧 `"S-KEY_LEFT"`     → `S-KEY_LEFT`

Cmd 键在内部表示里是隐含的——按下 Meta 本身不输出任何事件，只有
「Meta + 某个主键 + 某些修饰键」的组合才会命中规则。因此文档里的
`Cmd` 不参与编码，其余修饰键照常编码（Shift→S、Option→A、Ctrl→C）。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "macos-shortcuts.md"
CONFIG_RS = ROOT / "src" / "config.rs"
MAPPER_RS = ROOT / "src" / "mapper.rs"

# ── 键名映射 ──────────────────────────────────────────────────────────
# 文档里的写法 → evdev 键名主干（不带 KEY_ 前缀）

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

# 修饰键 → 内部缩写。注意没有 Cmd/M：它被当作隐含项，见模块 docstring。
MOD_ABBREV = {
    "Shift": "S",
    "Option": "A",
    "Alt": "A",
    "Ctrl": "C",
    "Control": "C",
}


def encode_key(token: str) -> str | None:
    """单个键 token → 键名主干。未知返回 None。"""
    token = token.strip()
    if not token:
        return None
    if token in SPECIAL_KEYS:
        return SPECIAL_KEYS[token]
    # 亮度/音量/调度中心等键在 Linux 侧无独立键码，不参与比对
    if "亮度" in token or "音量" in token or "调度中心" in token:
        return None
    if re.fullmatch(r"[A-Za-z]", token):
        return token.upper()
    if re.fullmatch(r"\d", token):
        return token
    if re.fullmatch(r"F\d{1,2}", token):
        return token
    return None


def encode_combo(combo: str) -> list[str]:
    """`Cmd+Shift+←` → ['S-KEY_LEFT']。

    处理 `/` 交替（`Cmd+←/→` → 左右两键）与 `3/4/5` 这类数字并列。
    修饰键前缀在交替时共享。无法编码的返回空列表。
    """
    combo = combo.strip().strip("`")
    if not combo:
        return []

    parts = [p.strip() for p in combo.split("+")]
    if len(parts) < 2:
        return []

    main = parts[-1]
    mods = parts[:-1]

    # `Cmd+Shift+3/4/5` → 主键有多个候选
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
                continue  # 隐含，不编码
            if m == "Fn":
                unsupported = True  # Fn 走单独的键位切换层
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
    """该行是否含 Cmd——默认语义只对 Cmd 组合生效。"""
    return any(
        m in ("Cmd", "Command", "Super", "Meta")
        for combo in raw_combos
        for m in combo.strip("`").split("+")[:-1]
    )


def norm_target(target: str) -> str:
    """把文档里的目标写法归一化成 `Ctrl+LEFT` 形式。

    目标列的写法比组合列松散：可能是 `Ctrl+←/→`（含交替）、
    `Ctrl+B/I/U`（多主键并列）、也可能带反引号。这里统一成
    「修饰键+主键」的单键字符串，多主键时返回全部可能值的 joined
    形式由调用方比较——实践中带 `/` 的目标两边都会展开。
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


# ── 解析文档 ──────────────────────────────────────────────────────────

# 分类含义见 docs/macos-shortcuts.md「判定分类」
# A: 默认语义覆盖；C: 终端特化；D: 需显式规则；E: 主动实现
# F: 概念不存在；B: Super 相关（暂缓）；其余（应用处理等）不比对
NEEDS_RULE = {"D", "E"}


def parse_doc() -> list[dict]:
    """抽取文档中所有三列表格行。"""
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


# ── 解析代码 ──────────────────────────────────────────────────────────


def parse_rules() -> set[str]:
    """src/config.rs 的 default_rules()，取每条规则的输入侧组合。

    表驱动重构后，映射真源是 `default_rules()` 里的 `push_emit` 调用。
    形如 `push_emit(&[], "KEY_LEFT", "C", "Left", "…")`——
    第二个参数即输入侧组合（不含隐含的 Cmd）。

    注意：只取 `push_emit` 生成的固定输出规则。`TerminalCtrlShift`
    与 `DeAccel` 走的是条件/运行时动作，由 parse_conditional_rules
    单独统计。
    """
    text = CONFIG_RS.read_text(encoding="utf-8")
    out = set()
    for m in re.finditer(r'push_emit\(\s*&\[(.*?)\]\s*,\s*"([^"]+)"', text, re.S):
        apps, combo = m.group(1), m.group(2).strip()
        if combo:
            out.add(combo)
    # `keys: vec!["…"]` 形式的规则（de_accel、terminal_ctrl_shift 等）
    for m in re.finditer(r'keys:\s*vec!\[([^\]]*)\]', text, re.S):
        for k in re.findall(r'"([^"]+)"', m.group(1)):
            k = k.strip()
            if k and k != "*":
                out.add(k)
    return out


def parse_conditional_rules() -> set[str]:
    """条件动作规则覆盖的输入侧组合。

    `keys = vec!["*"]` 表示主键通配——运行时才决定哪些键生效，
    故不能逐键计入（那正是它存在的理由：终端绑定各终端不同）。
    """
    text = CONFIG_RS.read_text(encoding="utf-8")
    out = set()
    # push_emit 之外的、显式写了 keys 的规则
    for m in re.finditer(r'keys:\s*vec!\[([^\]]*)\]', text, re.S):
        for k in re.findall(r'"([^"]+)"', m.group(1)):
            k = k.strip()
            if k and k != "*":
                out.add(k)
    return out


def parse_terminal_shift_keys() -> set[str]:
    """终端特化的兜底键集（通配规则之外的静态部分）。"""
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
    """mapper.rs 里的 Fn 键位切换规则，形如 KEY_F11 → KEY_VOLUMEDOWN。"""
    text = MAPPER_RS.read_text(encoding="utf-8")
    return set(re.findall(r"\(KeyCode::(KEY_\w+),\s*KeyCode::(KEY_\w+)\)", text))


# ── 报告 ──────────────────────────────────────────────────────────────


def main() -> int:
    rows = parse_doc()
    implemented = (
        parse_rules()
        | parse_conditional_rules()
        | parse_terminal_shift_keys()
    )
    remap = parse_remap()
    fn_rules = parse_fn_rules()

    print(f"文档条目 {len(rows)} 条；显式规则 {len(implemented)} 条")
    print(f"remap {sorted(remap)}")
    print(f"Fn 规则 {sorted(fn_rules) or '（无）'}")
    print()

    missing: list[tuple[str, str, str]] = []
    misfiled: list[tuple[str, str, str]] = []

    for row in rows:
        combos = [c for combo in row["combos"] for c in encode_combo(combo)]
        if not combos:
            continue  # Fn / 亮度键 / 应用处理，不比对

        if row["cls"] in NEEDS_RULE:
            hit = [c for c in combos if c in implemented]
            if not hit:
                missing.append((row["macos"], row["target"], " / ".join(combos)))
        elif row["cls"] == "A":
            # A 类声称「默认 Cmd+X → Ctrl+X 覆盖」。默认语义的实际输出是
            # Ctrl + <同主键> + 原样保留的非 Cmd 修饰键——只有目标恰好
            # 等于它时才成立。否则默认映射给不出该目标。
            #
            # 不含 Cmd 的行（如 macOS 的 `Ctrl+H`）不参与比对：默认语义
            # 只作用于 Cmd 组合，这些键在 Linux 上本就同名。
            cmd_rows = [c for c in combos if len(c.split("-")) >= 1 and _has_cmd(row["combos"])]
            if not cmd_rows:
                continue
            target_norm = norm_target(row["target"])
            outs = []
            for combo in cmd_rows:
                parts = combo.split("-")
                key = parts[-1].replace("KEY_", "")
                outs.append("Ctrl+" + "+".join(parts[:-1] + [key]))
            # 交替键（`Cmd+←/→`）展开成多个候选：任一命中即算覆盖。
            if not any(o.upper() == target_norm.upper() for o in outs):
                misfiled.append((row["macos"], row["target"], " / ".join(outs)))

    if misfiled:
        print("── 分类存疑：标为 A（默认语义覆盖），但目标并非 Ctrl+同一主键 ──")
        print("   默认映射只会产出 Ctrl+<同键>，给不出这些目标。\n")
        for macos, target, combo in misfiled:
            print(f"  {macos:<24} 目标 {target:<14} 默认会发出 {combo}")
        print()

    if missing:
        print("── 声称需显式规则（D/E），代码中无对应条目 ──\n")
        for macos, target, combo in missing:
            print(f"  {macos:<28} 目标 {target:<22} 应为 {combo}")
        print()

    if not missing and not misfiled:
        print("文档与实现一致。")
        return 0

    print(f"共 {len(missing)} 条缺失、{len(misfiled)} 条分类存疑。")
    return 1 if "--strict" in sys.argv else 0


if __name__ == "__main__":
    sys.exit(main())