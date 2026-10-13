#!/usr/bin/env bash
#
# mackbind uninstall script
#
# By default removes: the systemd user service, ~/.local/bin/mackbind, and the udev rule.
# By default it does **not** touch the input group — other tools may use it too; remove it
# explicitly with --remove-group if needed.
#
# Usage:
#   ./uninstall.sh                 # normal uninstall (keeps the input group)
#   ./uninstall.sh --remove-group  # also remove the current user from the input group
#   ./uninstall.sh -h | --help     # show help
#
# Steps that need root (removing the udev rule / changing the group) use sudo.

set -euo pipefail

BIN_DST="$HOME/.local/bin/mackbind"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/mackbind.service"
UDEV_DST="/etc/udev/rules.d/99-mackbind.rules"

DO_GROUP=0

usage() {
  cat <<'USAGE'
mackbind uninstall script

Usage: ./uninstall.sh [options]

Options:
  --remove-group   also remove the current user from the input group (kept by default)
  -h, --help       show this help

By default removes the systemd user service, ~/.local/bin/mackbind, and the udev rule.
USAGE
}

for arg in "$@"; do
  case "$arg" in
    --remove-group) DO_GROUP=1 ;;
    -h|--help)      usage; exit 0 ;;
    *)              printf 'Unknown argument: %s (use --help for usage)\n' "$arg" >&2; exit 2 ;;
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

# ── 1. Stop and remove the systemd service ──────────────────────────────
if [ -f "$UNIT" ] || systemctl --user cat mackbind.service >/dev/null 2>&1; then
  info "Stopping and disabling the service"
  systemctl --user disable --now mackbind 2>/dev/null || true
fi

if [ -f "$UNIT" ]; then
  info "Removing service unit → $UNIT"
  rm -f "$UNIT"
fi
systemctl --user daemon-reload 2>/dev/null || true
systemctl --user reset-failed mackbind 2>/dev/null || true

# ── 2. Remove the binary ────────────────────────────────────────────────
if [ -e "$BIN_DST" ]; then
  info "Removing binary → $BIN_DST"
  rm -f "$BIN_DST"
fi

# ── 3. Remove the udev rule ─────────────────────────────────────────────
if [ -f "$UDEV_DST" ]; then
  info "Removing udev rule → $UDEV_DST"
  as_root rm -f "$UDEV_DST"
  as_root udevadm control --reload
  as_root udevadm trigger
else
  dim "No udev rule found, skipping"
fi

# ── 4. (optional) Remove from the input group ───────────────────────────
if [ "$DO_GROUP" -eq 1 ]; then
  if id -nG "$ME" | tr ' ' '\n' | grep -qx input; then
    info "Removing $ME from the input group"
    as_root gpasswd -d "$ME" input
    warn "Group changes take effect only after you log out and back in."
  else
    dim "$ME is not in the input group, skipping"
  fi
fi

echo
info "Uninstall complete."
[ "$DO_GROUP" -eq 0 ] && dim "The input group was not changed (to remove it: ./uninstall.sh --remove-group)"
