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
# Release builds read kestrel/, debug builds kestrel-devel/. A memory alert at
# 1% fires on the first sample, so the status overlay has something to show.
for profile in kestrel kestrel-devel; do
  mkdir -p "$work/config/$profile"
  printf '%s\n' 'schema_version = 3' \
    '[features."commands.bar"]' 'enabled = true' \
    '[features."system.monitor"]' 'enabled = true' \
    '[features."overlay.status"]' 'enabled = true' \
    '[monitoring.alerts.memory]' 'enabled = true' 'threshold = 1.0' 'sustain_samples = 1' \
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
  log="$KESTREL_SMOKE_WORK/kestrel.log"
  WAYLAND_DEBUG=1 "$KESTREL_SMOKE_BINARY" >"$log" 2>&1 &
  kestrel_pid=$!
  # The instance owns its bus name before it maps the main window, so a
  # forwarded command reaches it once a toplevel title appears. libwayland
  # prints object IDs as name@id before 1.23 and name#id since.
  for _ in $(seq 1 120); do
    grep -Eq "xdg_toplevel[@#][0-9]+\.set_title" "$log" && break
    kill -0 "$kestrel_pid" 2>/dev/null || break
    sleep 0.5
  done
  timeout 20 "$KESTREL_SMOKE_BINARY" --command command-bar.open || true
  for _ in $(seq 1 30); do
    grep -Eq "zwlr_layer_shell_v1[@#][0-9]+\.get_layer_surface\(.*\"kestrel-command-bar\"" "$log" &&
      grep -Eq "zwlr_layer_shell_v1[@#][0-9]+\.get_layer_surface\(.*\"kestrel-status\"" "$log" &&
      break
    sleep 0.5
  done
  kill "$kestrel_pid" 2>/dev/null || true
  wait "$kestrel_pid" 2>/dev/null || true
' 2>"$work/session.log"

if ! grep -Eq 'zwlr_layer_shell_v1[@#][0-9]+\.get_layer_surface\(.*"kestrel-command-bar"\)' \
  "$work/kestrel.log"; then
  printf 'The command bar did not become a layer-shell surface.\n' >&2
  printf 'Layer-shell protocol traffic:\n' >&2
  grep -E 'zwlr_layer_shell|zwlr_layer_surface' "$work/kestrel.log" | head -n 10 >&2 || true
  printf 'Toplevel titles:\n' >&2
  grep -E 'xdg_toplevel[@#][0-9]+\.set_title' "$work/kestrel.log" | head -n 10 >&2 || true
  printf 'Kestrel output:\n' >&2
  grep -v '^\[' "$work/kestrel.log" | tail -n 30 >&2 || true
  printf 'Session output:\n' >&2
  tail -n 20 "$work/session.log" >&2 || true
  exit 1
fi
printf 'Command bar opened as a wlr-layer-shell overlay on headless Sway\n'

# The status overlay is a top-layer surface (2) that never takes the keyboard.
status_surface="$(grep -Eo 'get_layer_surface\(new id zwlr_layer_surface_v1[@#][0-9]+, wl_surface[@#][0-9]+, [a-z0-9_@#]+, 2, "kestrel-status"\)' \
  "$work/kestrel.log" | head -n 1 | grep -Eo 'zwlr_layer_surface_v1[@#][0-9]+' || true)"
if [ -z "$status_surface" ]; then
  printf 'The status overlay did not become a top-layer layer-shell surface.\n' >&2
  grep -E 'zwlr_layer_shell|zwlr_layer_surface' "$work/kestrel.log" | head -n 10 >&2 || true
  exit 1
fi
if grep -Eq "${status_surface}\.set_keyboard_interactivity\([1-9]" "$work/kestrel.log"; then
  printf 'The status overlay asked for keyboard input.\n' >&2
  exit 1
fi
printf 'Status overlay opened as a top-layer overlay without keyboard input\n'
