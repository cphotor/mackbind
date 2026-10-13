#!/usr/bin/env bash
#
# mackbind 卸载脚本
#
# 默认移除：systemd 用户服务、~/.local/bin/mackbind、udev 规则。
# 默认**不动** input 组——别的工具可能也在用，需要时用 --remove-group 显式移除。
#
# 用法：
#   ./uninstall.sh                 # 常规卸载（保留 input 组）
#   ./uninstall.sh --remove-group  # 顺便把当前用户移出 input 组
#   ./uninstall.sh -h | --help     # 显示帮助
#
# 需要 root 的步骤（删 udev 规则 / 改组）会用 sudo。

set -euo pipefail

BIN_DST="$HOME/.local/bin/mackbind"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/mackbind.service"
UDEV_DST="/etc/udev/rules.d/99-mackbind.rules"

DO_GROUP=0

usage() {
  cat <<'USAGE'
mackbind 卸载脚本

用法：./uninstall.sh [选项]

选项：
  --remove-group   顺便把当前用户移出 input 组（默认保留）
  -h, --help       显示本帮助

默认移除 systemd 用户服务、~/.local/bin/mackbind、udev 规则。
USAGE
}

for arg in "$@"; do
  case "$arg" in
    --remove-group) DO_GROUP=1 ;;
    -h|--help)      usage; exit 0 ;;
    *)              printf '未知参数：%s（用 --help 查看用法）\n' "$arg" >&2; exit 2 ;;
  esac
done

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_OK=$'\033[32m'; C_WARN=$'\033[33m'; C_ERR=$'\033[31m'; C_DIM=$'\033[2m'; C_RST=$'\033[0m'
else
  C_OK=; C_WARN=; C_ERR=; C_DIM=; C_RST=
fi
info() { printf '%s==>%s %s\n' "$C_OK" "$C_RST" "$*"; }
warn() { printf '%s[!]%s %s\n' "$C_WARN" "$C_RST" "$*" >&2; }
dim()  { printf '%s    %s%s\n' "$C_DIM" "$*" "$C_RST"; }

as_root() {
  if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}
ME="$(id -un)"

# ── 1. 停用并删除 systemd 服务 ──────────────────────────────────────────
if [ -f "$UNIT" ] || systemctl --user cat mackbind.service >/dev/null 2>&1; then
  info "停止并禁用服务"
  systemctl --user disable --now mackbind 2>/dev/null || true
fi

if [ -f "$UNIT" ]; then
  info "删除服务单元 → $UNIT"
  rm -f "$UNIT"
fi
systemctl --user daemon-reload 2>/dev/null || true
systemctl --user reset-failed mackbind 2>/dev/null || true

# ── 2. 删除二进制 ───────────────────────────────────────────────────────
if [ -e "$BIN_DST" ]; then
  info "删除二进制 → $BIN_DST"
  rm -f "$BIN_DST"
fi

# ── 3. 删除 udev 规则 ───────────────────────────────────────────────────
if [ -f "$UDEV_DST" ]; then
  info "删除 udev 规则 → $UDEV_DST"
  as_root rm -f "$UDEV_DST"
  as_root udevadm control --reload
  as_root udevadm trigger
else
  dim "未发现 udev 规则，跳过"
fi

# ── 4. （可选）移出 input 组 ────────────────────────────────────────────
if [ "$DO_GROUP" -eq 1 ]; then
  if id -nG "$ME" | tr ' ' '\n' | grep -qx input; then
    info "把 $ME 移出 input 组"
    as_root gpasswd -d "$ME" input
    warn "组的变更要注销重登才生效。"
  else
    dim "$ME 不在 input 组，跳过"
  fi
fi

echo
info "卸载完成。"
[ "$DO_GROUP" -eq 0 ] && dim "input 组未改动（如需移除：./uninstall.sh --remove-group）"
