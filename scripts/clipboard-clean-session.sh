#!/usr/bin/env bash
# Run the clipboard lifecycle integration test in fresh no-manager X11
# (private Xvfb) and headless Sway sessions. Clipboard content is never printed
# or saved; the test uses a generated marker.
#
# Missing Xvfb or Sway skips the run; KESTREL_REQUIRE_CLEAN_SESSION=1 makes it fatal.

set -euo pipefail

require_clean_session="${KESTREL_REQUIRE_CLEAN_SESSION:-0}"
for command in Xvfb sway; do
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
workspace_manifest="$script_dir/../Cargo.toml"
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

# Arguments are env(1) options and assignments for the session under test.
run_lifecycle_test() {
  env "$@" cargo test --quiet --manifest-path "$workspace_manifest" -p kestrel-services \
    --test clipboard_production_lifecycle -- --exact \
    production_backend_reowns_and_releases_a_clean_session_selection
}

# 600 polls of 0.05 s allow cold CI startup (keymap compilation, first-run caches).
readiness_polls=600

# Server logs contain no clipboard data; their tail explains startup failures.
fail_with_log() {
  local message="$1"
  local log="$2"
  printf '%s; last lines of %s:\n' "$message" "$(basename -- "$log")" >&2
  tail -n 40 -- "$log" >&2 || true
  return 1
}

# Xvfb chooses a free display and reports it once listening. read accepts an
# unterminated line, so a missing newline cannot stall the wait.
wait_for_x11() {
  local display_file="$1"
  local display_number
  for _ in $(seq 1 "$readiness_polls"); do
    display_number=""
    IFS= read -r display_number <"$display_file" || true
    if [[ "$display_number" =~ ^[0-9]+$ ]]; then
      printf ':%s\n' "$display_number"
      return 0
    fi
    if ! kill -0 "$xvfb_pid" 2>/dev/null; then
      fail_with_log 'Xvfb exited before reporting a display' "$artifacts_dir/xvfb.log"
      return 1
    fi
    sleep 0.05
  done
  fail_with_log 'Xvfb did not report a display' "$artifacts_dir/xvfb.log"
}

wait_for_wayland() {
  for _ in $(seq 1 "$readiness_polls"); do
    for socket in "$wayland_runtime_dir"/wayland-*; do
      if [ -S "$socket" ] && [[ "$socket" != *.lock ]]; then
        basename -- "$socket"
        return 0
      fi
    done
    if ! kill -0 "$sway_pid" 2>/dev/null; then
      fail_with_log 'Sway exited before creating a Wayland socket' "$artifacts_dir/sway.log"
      return 1
    fi
    sleep 0.05
  done
  fail_with_log 'Sway did not create a Wayland socket' "$artifacts_dir/sway.log"
}

x11_display_file="$artifacts_dir/x11-display"
: >"$x11_display_file"
# -noreset keeps the server between clients: reset races provider discovery, and
# selection release must come from the owner exiting.
Xvfb -displayfd 3 -noreset -screen 0 640x480x24 -nolisten tcp \
  3>"$x11_display_file" >"$artifacts_dir/xvfb.log" 2>&1 &
xvfb_pid="$!"
x11_display="$(wait_for_x11 "$x11_display_file")"
run_lifecycle_test -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
  DISPLAY="$x11_display" XDG_SESSION_TYPE=x11 KESTREL_EXPECT_CLIPBOARD_PROVIDER=x11

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
run_lifecycle_test -u DISPLAY -u WAYLAND_SOCKET \
  XDG_RUNTIME_DIR="$wayland_runtime_dir" \
  WAYLAND_DISPLAY="$wayland_display" \
  XDG_SESSION_TYPE=wayland \
  KESTREL_EXPECT_CLIPBOARD_PROVIDER=wayland
