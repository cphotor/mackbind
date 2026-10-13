#!/usr/bin/env bash
#
# mackbind install script
#
# Does four things in order:
#   1. Build and install the binary to ~/.local/bin/mackbind
#   2. Install the udev rule that opens up /dev/uinput (to create a virtual keyboard)
#   3. Add the current user to the input group (to read the physical keyboard)
#   4. Install and enable the systemd user service
#
# Usage:
#   ./install.sh                 full run
#   ./install.sh --no-build      skip the build, use the existing target/release/mackbind
#   ./install.sh --no-udev       do not install the udev rule (e.g. /dev/uinput is already writable)
#   ./install.sh --no-group      do not modify the input group
#   ./install.sh --no-service    do not install the systemd user service
#   ./install.sh -h | --help     show help
#
# Uninstall: ./uninstall.sh
#
# Steps that need root (udev / input group) use sudo and may prompt for your password.

set -euo pipefail

# ── Paths ───────────────────────────────────────────────────────────────
REPO_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BIN_SRC="$REPO_DIR/target/release/mackbind"
BIN_DST="$HOME/.local/bin/mackbind"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/mackbind.service"
UDEV_SRC="$REPO_DIR/packaging/99-mackbind.rules"
UDEV_DST="/etc/udev/rules.d/99-mackbind.rules"

# ── Arguments ───────────────────────────────────────────────────────────
DO_BUILD=1
DO_UDEV=1
DO_GROUP=1
DO_SERVICE=1

usage() {
  cat <<'USAGE'
mackbind install script

Usage: ./install.sh [options]

Options:
  --no-build      skip the build, use the existing target/release/mackbind
  --no-udev       do not install the udev rule (e.g. /dev/uinput is already writable)
  --no-group      do not modify the input group
  --no-service    do not install the systemd user service
  -h, --help      show this help

Steps that need root (udev / input group) use sudo and may prompt for your password.
To uninstall, use ./uninstall.sh
USAGE
}

for arg in "$@"; do
  case "$arg" in
    --no-build)   DO_BUILD=0 ;;
    --no-udev)    DO_UDEV=0 ;;
    --no-group)   DO_GROUP=0 ;;
    --no-service) DO_SERVICE=0 ;;
    -h|--help)    usage; exit 0 ;;
    *)            printf 'Unknown argument: %s (use --help for usage)\n' "$arg" >&2; exit 2 ;;
  esac
done

# ── Output ──────────────────────────────────────────────────────────────
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

# root or sudo
as_root() {
  if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

ME="$(id -un)"

# ── Pre-flight checks ───────────────────────────────────────────────────
[ -f "$REPO_DIR/Cargo.toml" ] || die "Not at the repository root (cannot find $REPO_DIR/Cargo.toml)"
[ -f "$UDEV_SRC" ] || die "Missing udev rule file: $UDEV_SRC"

if [ "$DO_UDEV" -eq 1 ] || [ "$DO_GROUP" -eq 1 ]; then
  if [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 || die "Root permissions are required but sudo is not available"
    info "The next few steps need root permissions and may prompt for your password"
    sudo -v || die "Could not obtain sudo permissions"
  fi
fi

# ── 1. Build ────────────────────────────────────────────────────────────
if [ "$DO_BUILD" -eq 1 ]; then
  command -v cargo >/dev/null 2>&1 \
    || die "cargo not found. Install the Rust toolchain first, or use --no-build to point at an existing binary."
  info "Building the release version…"
  ( cd "$REPO_DIR" && cargo build --release )
elif [ ! -x "$BIN_SRC" ]; then
  die "--no-build was given but no executable was found: $BIN_SRC"
fi
[ -x "$BIN_SRC" ] || die "Build artifact does not exist or is not executable: $BIN_SRC"

# ── 2. Install the binary (stop the service first, or you get "text file busy") ──
SERVICE_WAS_RUNNING=0
if systemctl --user is-active --quiet mackbind 2>/dev/null; then
  SERVICE_WAS_RUNNING=1
  info "A mackbind service is already running; stopping it to replace the binary"
  systemctl --user stop mackbind || true
fi

info "Installing binary → $BIN_DST"
install -Dm755 "$BIN_SRC" "$BIN_DST"
# Verifying is mandatory: twice the user ended up testing an old binary
cmp -s "$BIN_SRC" "$BIN_DST" || die "Installed binary differs from the build artifact (MD5 check failed)"
dim "MD5 $(md5sum "$BIN_DST" | awk '{print $1}') (matches the build artifact)"

# Stopping the service was a means to replace the binary, not a requested
# outcome. With --no-service nothing later starts it again, and the keyboard is
# left silently un-mapped — which looks exactly like "the mapping is broken" —
# so put it back the way we found it.
if [ "$SERVICE_WAS_RUNNING" -eq 1 ] && [ "$DO_SERVICE" -eq 0 ]; then
  info "Restarting the service that was stopped to replace the binary"
  systemctl --user start mackbind \
    || warn "Could not restart the service; run: systemctl --user start mackbind"
fi

# ── 3. input group ──────────────────────────────────────────────────────
NEED_RELOGIN=0
if [ "$DO_GROUP" -eq 1 ]; then
  if id -nG "$ME" | tr ' ' '\n' | grep -qx input; then
    info "$ME is already in the input group, skipping"
  else
    info "Adding $ME to the input group (to read the physical keyboard /dev/input/event*)"
    as_root usermod -aG input "$ME"
    NEED_RELOGIN=1
  fi
fi

# ── 4. udev rule ────────────────────────────────────────────────────────
if [ "$DO_UDEV" -eq 1 ]; then
  if [ -f "$UDEV_DST" ] && cmp -s "$UDEV_SRC" "$UDEV_DST"; then
    info "The udev rule is already up to date, skipping"
  else
    info "Installing udev rule → $UDEV_DST (opening up /dev/uinput)"
    as_root install -m644 "$UDEV_SRC" "$UDEV_DST"
    as_root udevadm control --reload
    as_root udevadm trigger
  fi
fi

# ── 5. systemd user service ─────────────────────────────────────────────
if [ "$DO_SERVICE" -eq 1 ]; then
  info "Installing systemd user service → $UNIT"
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

# ── 6. Start right away if permissions are ready, otherwise suggest re-login ──
keyboard_readable() {
  for d in /dev/input/event*; do
    [ -r "$d" ] && return 0
  done
  return 1
}

STARTED=0
if [ "$DO_SERVICE" -eq 1 ]; then
  if keyboard_readable && [ -w /dev/uinput ]; then
    info "Permissions are ready, starting the service"
    systemctl --user restart mackbind
    sleep 1
    if systemctl --user is-active --quiet mackbind; then
      info "mackbind is running ✅"
      STARTED=1
    else
      warn "The service failed to start; check the log: journalctl --user -u mackbind -n 30 --no-pager"
    fi
  else
    NEED_RELOGIN=1
  fi
fi

# ── Wrap-up ─────────────────────────────────────────────────────────────
echo
if [ "$NEED_RELOGIN" -eq 1 ]; then
  warn "One step left: log out and back in (or reboot) so the new input group membership takes effect for your processes."
  [ "$DO_SERVICE" -eq 1 ] && dim "The service is enabled and will start automatically after you log back in."
  dim "Self-check after re-login: ls -l /dev/uinput    # should be root input 0660"
  dim "             systemctl --user status mackbind"
elif [ "$STARTED" -eq 0 ]; then
  info "Done (service not enabled; run mackbind yourself as needed)."
else
  info "Done."
fi
