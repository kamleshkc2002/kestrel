#!/usr/bin/env bash
# Opens the command bar in a private headless Sway session and checks that it
# became a wlr-layer-shell overlay. Never touches the caller's session.
#
# Usage: layer-shell-smoke.sh <kestrel-executable>
# SWAY_FLAGS adds Sway options (for example --unsupported-gpu on NVIDIA hosts).
set -euo pipefail

if [ "$#" -ne 1 ]; then
  printf 'usage: %s <kestrel-executable>\n' "$0" >&2
  exit 2
fi
kestrel="$(realpath "$1")"
for command in sway dbus-run-session; do
  if ! command -v "$command" >/dev/null; then
    printf 'layer-shell smoke needs %s\n' "$command" >&2
    exit 1
  fi
done

work="$(mktemp -d)"
chmod 700 "$work"
sway_pid=""
cleanup() {
  if [ -n "$sway_pid" ]; then
    kill "$sway_pid" 2>/dev/null || true
    wait "$sway_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$work/run" "$work/data" "$work/cache" "$work/state"
chmod 700 "$work/run"
# Release builds read kestrel/, debug builds kestrel-devel/.
for profile in kestrel kestrel-devel; do
  mkdir -p "$work/config/$profile"
  printf 'schema_version = 3\n[features."commands.bar"]\nenabled = true\n' \
    >"$work/config/$profile/config.toml"
done
printf 'xwayland disable\n' >"$work/sway.conf"
# A private bus without service directories, so no host service is activated.
cat >"$work/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir=$work/run</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF

# shellcheck disable=SC2086 # SWAY_FLAGS is a list of options.
env -u DISPLAY -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
  XDG_RUNTIME_DIR="$work/run" \
  WLR_BACKENDS=headless \
  WLR_RENDERER=pixman \
  WLR_LIBINPUT_NO_DEVICES=1 \
  sway ${SWAY_FLAGS:-} -c "$work/sway.conf" >"$work/sway.log" 2>&1 &
sway_pid="$!"

wayland_display=""
for _ in $(seq 1 50); do
  socket="$(find "$work/run" -maxdepth 1 -type s -name 'wayland-*' -print -quit)"
  if [ -n "$socket" ]; then
    wayland_display="$(basename "$socket")"
    break
  fi
  if ! kill -0 "$sway_pid" 2>/dev/null; then
    break
  fi
  sleep 0.2
done
if [ -z "$wayland_display" ]; then
  printf 'Sway did not create a Wayland socket:\n' >&2
  cat "$work/sway.log" >&2
  exit 1
fi

export KESTREL_SMOKE_WORK="$work" KESTREL_SMOKE_BINARY="$kestrel"
export KESTREL_SMOKE_DISPLAY="$wayland_display"
dbus-run-session --config-file="$work/bus.conf" -- bash -c '
  set -euo pipefail
  unset DISPLAY
  export XDG_RUNTIME_DIR="$KESTREL_SMOKE_WORK/run" WAYLAND_DISPLAY="$KESTREL_SMOKE_DISPLAY"
  export XDG_CONFIG_HOME="$KESTREL_SMOKE_WORK/config" XDG_DATA_HOME="$KESTREL_SMOKE_WORK/data"
  export XDG_CACHE_HOME="$KESTREL_SMOKE_WORK/cache" XDG_STATE_HOME="$KESTREL_SMOKE_WORK/state"
  export GDK_BACKEND=wayland GTK_A11Y=none NO_AT_BRIDGE=1
  WAYLAND_DEBUG=1 "$KESTREL_SMOKE_BINARY" >"$KESTREL_SMOKE_WORK/kestrel.log" 2>&1 &
  kestrel_pid=$!
  sleep 6
  "$KESTREL_SMOKE_BINARY" --command command-bar.open
  sleep 2
  kill "$kestrel_pid" 2>/dev/null || true
  wait "$kestrel_pid" 2>/dev/null || true
' 2>"$work/session.log"

if ! grep -Eq 'zwlr_layer_shell_v1#[0-9]+\.get_layer_surface\(.*"kestrel-command-bar"\)' \
  "$work/kestrel.log"; then
  printf 'The command bar did not become a layer-shell surface. Last log lines:\n' >&2
  grep -v 'wl_buffer\|wl_callback\|frame\|damage' "$work/kestrel.log" | tail -n 40 >&2
  exit 1
fi
printf 'Command bar opened as a wlr-layer-shell overlay on headless Sway\n'
