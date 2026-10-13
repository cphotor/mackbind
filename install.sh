#!/usr/bin/env bash
#
# mackbind 安装脚本
#
# 依次做四件事：
#   1. 编译并把二进制安装到 ~/.local/bin/mackbind
#   2. 安装 udev 规则放开 /dev/uinput（建虚拟键盘）
#   3. 把当前用户加入 input 组（读物理键盘）
#   4. 安装并启用 systemd 用户服务
#
# 用法：
#   ./install.sh                 全套
#   ./install.sh --no-build      跳过编译，使用已有的 target/release/mackbind
#   ./install.sh --no-udev       不装 udev 规则（例如 /dev/uinput 本就可写）
#   ./install.sh --no-group      不修改 input 组
#   ./install.sh --no-service    不装 systemd 用户服务
#   ./install.sh -h | --help     显示帮助
#
# 卸载：./uninstall.sh
#
# 需要 root 的步骤（udev / input 组）会用 sudo，届时可能提示输入密码。

set -euo pipefail

# ── 路径 ────────────────────────────────────────────────────────────────
REPO_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BIN_SRC="$REPO_DIR/target/release/mackbind"
BIN_DST="$HOME/.local/bin/mackbind"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/mackbind.service"
UDEV_SRC="$REPO_DIR/packaging/99-mackbind.rules"
UDEV_DST="/etc/udev/rules.d/99-mackbind.rules"

# ── 参数 ────────────────────────────────────────────────────────────────
DO_BUILD=1
DO_UDEV=1
DO_GROUP=1
DO_SERVICE=1

usage() {
  cat <<'USAGE'
mackbind 安装脚本

用法：./install.sh [选项]

选项：
  --no-build      跳过编译，使用已有的 target/release/mackbind
  --no-udev       不装 udev 规则（例如 /dev/uinput 本就可写）
  --no-group      不修改 input 组
  --no-service    不装 systemd 用户服务
  -h, --help      显示本帮助

需要 root 的步骤（udev / input 组）会用 sudo，届时可能提示输入密码。
卸载请用 ./uninstall.sh
USAGE
}

for arg in "$@"; do
  case "$arg" in
    --no-build)   DO_BUILD=0 ;;
    --no-udev)    DO_UDEV=0 ;;
    --no-group)   DO_GROUP=0 ;;
    --no-service) DO_SERVICE=0 ;;
    -h|--help)    usage; exit 0 ;;
    *)            printf '未知参数：%s（用 --help 查看用法）\n' "$arg" >&2; exit 2 ;;
  esac
done

# ── 输出 ────────────────────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_OK=$'\033[32m'; C_WARN=$'\033[33m'; C_ERR=$'\033[31m'; C_DIM=$'\033[2m'; C_RST=$'\033[0m'
else
  C_OK=; C_WARN=; C_ERR=; C_DIM=; C_RST=
fi
info() { printf '%s==>%s %s\n' "$C_OK" "$C_RST" "$*"; }
warn() { printf '%s[!]%s %s\n' "$C_WARN" "$C_RST" "$*" >&2; }
err()  { printf '%s[x]%s %s\n' "$C_ERR" "$C_RST" "$*" >&2; }
dim()  { printf '%s    %s%s\n' "$C_DIM" "$*" "$C_RST"; }
die()  { err "$*"; exit 1; }

# root 或 sudo
as_root() {
  if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

ME="$(id -un)"

# ── 前置检查 ────────────────────────────────────────────────────────────
[ -f "$REPO_DIR/Cargo.toml" ] || die "不在仓库根目录（找不到 $REPO_DIR/Cargo.toml）"
[ -f "$UDEV_SRC" ] || die "缺少 udev 规则文件：$UDEV_SRC"

if [ "$DO_UDEV" -eq 1 ] || [ "$DO_GROUP" -eq 1 ]; then
  if [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 || die "需要 root 权限但系统里没有 sudo"
    info "接下来几步需要 root 权限，可能提示你输入密码"
    sudo -v || die "无法获得 sudo 权限"
  fi
fi

# ── 1. 编译 ─────────────────────────────────────────────────────────────
if [ "$DO_BUILD" -eq 1 ]; then
  command -v cargo >/dev/null 2>&1 \
    || die "找不到 cargo。请先装 Rust 工具链，或用 --no-build 指定已有二进制。"
  info "编译 release 版本…"
  ( cd "$REPO_DIR" && cargo build --release )
elif [ ! -x "$BIN_SRC" ]; then
  die "--no-build 但找不到可执行文件：$BIN_SRC"
fi
[ -x "$BIN_SRC" ] || die "编译产物不存在或不可执行：$BIN_SRC"

# ── 2. 安装二进制（先停服务，否则会「文本文件忙」）────────────────────────
if systemctl --user is-active --quiet mackbind 2>/dev/null; then
  info "已有 mackbind 服务在运行，先停下以便替换二进制"
  systemctl --user stop mackbind || true
fi

info "安装二进制 → $BIN_DST"
install -Dm755 "$BIN_SRC" "$BIN_DST"
# 核对必须是硬性步骤：曾两次让用户测了旧二进制
cmp -s "$BIN_SRC" "$BIN_DST" || die "安装后的二进制与编译产物不一致（MD5 校验失败）"
dim "MD5 $(md5sum "$BIN_DST" | awk '{print $1}')（与编译产物一致）"

# ── 3. input 组 ─────────────────────────────────────────────────────────
NEED_RELOGIN=0
if [ "$DO_GROUP" -eq 1 ]; then
  if id -nG "$ME" | tr ' ' '\n' | grep -qx input; then
    info "$ME 已在 input 组，跳过"
  else
    info "把 $ME 加入 input 组（读物理键盘 /dev/input/event*）"
    as_root usermod -aG input "$ME"
    NEED_RELOGIN=1
  fi
fi

# ── 4. udev 规则 ────────────────────────────────────────────────────────
if [ "$DO_UDEV" -eq 1 ]; then
  if [ -f "$UDEV_DST" ] && cmp -s "$UDEV_SRC" "$UDEV_DST"; then
    info "udev 规则已是最新，跳过"
  else
    info "安装 udev 规则 → $UDEV_DST（放开 /dev/uinput）"
    as_root install -m644 "$UDEV_SRC" "$UDEV_DST"
    as_root udevadm control --reload
    as_root udevadm trigger
  fi
fi

# ── 5. systemd 用户服务 ─────────────────────────────────────────────────
if [ "$DO_SERVICE" -eq 1 ]; then
  info "安装 systemd 用户服务 → $UNIT"
  mkdir -p "$UNIT_DIR"
  cat > "$UNIT" <<'EOF'
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
EOF
  systemctl --user daemon-reload
  systemctl --user enable mackbind >/dev/null
  systemctl --user reset-failed mackbind 2>/dev/null || true
fi

# ── 6. 权限就绪就立刻启动，否则提示重登 ─────────────────────────────────
keyboard_readable() {
  for d in /dev/input/event*; do
    [ -r "$d" ] && return 0
  done
  return 1
}

STARTED=0
if [ "$DO_SERVICE" -eq 1 ]; then
  if keyboard_readable && [ -w /dev/uinput ]; then
    info "权限已就绪，启动服务"
    systemctl --user restart mackbind
    sleep 1
    if systemctl --user is-active --quiet mackbind; then
      info "mackbind 正在运行 ✅"
      STARTED=1
    else
      warn "服务未能启动，查看日志：journalctl --user -u mackbind -n 30 --no-pager"
    fi
  else
    NEED_RELOGIN=1
  fi
fi

# ── 收尾 ────────────────────────────────────────────────────────────────
echo
if [ "$NEED_RELOGIN" -eq 1 ]; then
  warn "还差一步：注销并重新登录（或重启），让 input 组的新成员关系对进程生效。"
  [ "$DO_SERVICE" -eq 1 ] && dim "服务已启用，重登后会自动启动。"
  dim "重登后自检：ls -l /dev/uinput    # 应为 root input 0660"
  dim "             systemctl --user status mackbind"
elif [ "$STARTED" -eq 0 ]; then
  info "完成（未启用服务，按需自行运行 mackbind）。"
else
  info "完成。"
fi
