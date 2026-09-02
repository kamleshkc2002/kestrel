#!/usr/bin/env bash
# Runs the generated-marker lifecycle test against fresh, no-manager X11 and
# Wayland sessions. Reports remain JSON metadata only; no clipboard content is
# printed or saved.

set -euo pipefail

require_clean_session="${KESTREL_REQUIRE_CLEAN_SESSION:-0}"
for command in Xvfb sway jq; do
  if ! command -v "$command" >/dev/null; then
    if [ "$require_clean_session" = "1" ]; then
      printf 'required clean-session dependency is unavailable: %s\n' "$command" >&2
      exit 1
    fi
    printf 'clean-session lifecycle test skipped: %s is unavailable\n' "$command"
    exit 0
  fi
done

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
manifest="$script_dir/Cargo.toml"
artifacts_dir="$(mktemp -d "${TMPDIR:-/tmp}/kestrel-clipboard-lifecycle.XXXXXX")"
wayland_runtime_dir=""
xvfb_pid=""
sway_pid=""

cleanup() {
  if [ -n "$sway_pid" ]; then
    kill "$sway_pid" 2>/dev/null || true
    wait "$sway_pid" 2>/dev/null || true
  fi
  if [ -n "$xvfb_pid" ]; then
    kill "$xvfb_pid" 2>/dev/null || true
    wait "$xvfb_pid" 2>/dev/null || true
  fi
  rm -rf "$artifacts_dir"
  if [ -n "$wayland_runtime_dir" ]; then
    rm -rf "$wayland_runtime_dir"
  fi
}
trap cleanup EXIT

assert_lifecycle() {
  local backend="$1"
  local report="$2"

  jq -e --arg backend "$backend" '
    .probe == "clipboard"
    and .phase == "0"
    and .read_only == false
    and (.lifecycle | length == 1)
    and (
      .lifecycle[0]
      | .backend == $backend
      and .mode == "attempted"
      and .initial_selection_empty == true
      and .available_while_owner_alive == true
      and .writer_exited_cleanly == true
      and .available_after_owner_exit == false
      and .selection_empty_before_cleanup == true
      and .cleanup == "not_required_selection_already_empty"
      and .empty_after_cleanup == true
      and .preexisting_clipboard_content_read == false
      and .generated_marker_compared_in_memory == true
      and .restored == true
    )
  ' "$report" >/dev/null
}

wait_for_x11() {
  local socket="/tmp/.X11-unix/X99"
  for _ in $(seq 1 100); do
    if [ -S "$socket" ]; then
      return 0
    fi
    sleep 0.05
  done
  printf 'Xvfb did not create its display socket\n' >&2
  return 1
}

wait_for_wayland() {
  for _ in $(seq 1 100); do
    for socket in "$wayland_runtime_dir"/wayland-*; do
      if [ -S "$socket" ] && [[ "$socket" != *.lock ]]; then
        basename "$socket"
        return 0
      fi
    done
    sleep 0.05
  done
  printf 'Sway did not create a Wayland socket\n' >&2
  return 1
}

Xvfb :99 -screen 0 640x480x24 -nolisten tcp >"$artifacts_dir/xvfb.log" 2>&1 &
xvfb_pid="$!"
wait_for_x11
x11_report="$artifacts_dir/x11.json"
env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u XDG_SESSION_TYPE \
  DISPLAY=:99 XDG_SESSION_TYPE=x11 \
  cargo run --quiet --manifest-path "$manifest" -- --exercise-lifecycle x11 >"$x11_report"
assert_lifecycle x11 "$x11_report"
cat "$x11_report"

wayland_runtime_dir="$(mktemp -d "${TMPDIR:-/tmp}/kestrel-wayland-runtime.XXXXXX")"
chmod 700 "$wayland_runtime_dir"
printf 'xwayland disable\n' >"$artifacts_dir/sway.conf"
env -u DISPLAY -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
  XDG_RUNTIME_DIR="$wayland_runtime_dir" \
  WLR_BACKENDS=headless \
  WLR_RENDERER=pixman \
  WLR_LIBINPUT_NO_DEVICES=1 \
  sway -c "$artifacts_dir/sway.conf" >"$artifacts_dir/sway.log" 2>&1 &
sway_pid="$!"
wayland_display="$(wait_for_wayland)"
wayland_report="$artifacts_dir/wayland.json"
env -u DISPLAY -u WAYLAND_SOCKET \
  XDG_RUNTIME_DIR="$wayland_runtime_dir" \
  WAYLAND_DISPLAY="$wayland_display" \
  XDG_SESSION_TYPE=wayland \
  cargo run --quiet --manifest-path "$manifest" -- --exercise-lifecycle wayland >"$wayland_report"
assert_lifecycle wayland "$wayland_report"
cat "$wayland_report"
