#!/usr/bin/env bash
# Runs the recovery checks in private headless Sway and D-Bus sessions: a
# logind suspend/resume, a SIGKILL and restart, a dependency appearing and
# disappearing, the compositor restarting, an audio device returning, and a
# denied logind call. Files, sockets, and buses all live in a private
# temporary directory and never touch the caller's session.
#
# Usage: recovery-checks.sh <kestrel-executable>
# KESTREL_REQUIRE_RECOVERY_CHECKS=1 makes missing dependencies fatal.
# SWAY_FLAGS adds Sway options (for example --unsupported-gpu on NVIDIA hosts).

set -euo pipefail

if [ "$#" -ne 1 ]; then
  printf 'usage: %s <kestrel-executable>\n' "$0" >&2
  exit 2
fi
kestrel="$(realpath "$1")"
require_checks="${KESTREL_REQUIRE_RECOVERY_CHECKS:-0}"

missing=0
for command in sway dbus-daemon gdbus python3 wl-copy wl-paste; do
  if ! command -v "$command" >/dev/null; then
    printf 'recovery checks need %s\n' "$command" >&2
    missing=1
  fi
done
if [ "$missing" = "1" ]; then
  if [ "$require_checks" = "1" ]; then
    exit 1
  fi
  printf 'recovery checks skipped: run them where the dependencies are installed\n'
  exit 0
fi
if ! python3 -c 'import gi; gi.require_version("Gio", "2.0")' >/dev/null 2>&1; then
  if [ "$require_checks" = "1" ]; then
    printf 'recovery checks need python3-gi for the fake logind\n' >&2
    exit 1
  fi
  printf 'recovery checks skipped: python3-gi is unavailable\n'
  exit 0
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/kestrel-recovery.XXXXXX")"
chmod 700 "$work"
mkdir -p "$work/run" "$work/config" "$work/data" "$work/cache" "$work/state" \
  "$work/home/bin" "$work/home/Videos" "$work/logind"
chmod 700 "$work/run"
export XDG_RUNTIME_DIR="$work/run"
export XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data"
export XDG_CACHE_HOME="$work/cache" XDG_STATE_HOME="$work/state"
export HOME="$work/home"
export PATH="$work/home/bin:$PATH"
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET PULSE_SERVER PULSE_RUNTIME_PATH

system_bus_pid=""
session_bus_pid=""
fake_logind_pid=""
sway_pid=""
kestrel_pid=""
pulse_pid=""

cleanup() {
  stop_kestrel
  if [ -n "$pulse_pid" ]; then
    kill "$pulse_pid" 2>/dev/null || true
    wait "$pulse_pid" 2>/dev/null || true
  fi
  if [ -n "$sway_pid" ]; then
    kill "$sway_pid" 2>/dev/null || true
    wait "$sway_pid" 2>/dev/null || true
  fi
  if [ -n "$fake_logind_pid" ]; then
    kill "$fake_logind_pid" 2>/dev/null || true
    wait "$fake_logind_pid" 2>/dev/null || true
  fi
  if [ -n "$session_bus_pid" ]; then
    kill "$session_bus_pid" 2>/dev/null || true
  fi
  if [ -n "$system_bus_pid" ]; then
    kill "$system_bus_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# A compositor crash must not leave a core file in the checks' evidence.
ulimit -c 0

bus_config() {
  local type="$1" directory="$2"
  cat <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>$type</type>
  <listen>unix:dir=$directory</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
}

start_bus() {
  local type="$1" directory="$2" address_variable="$3"
  mkdir -p "$directory"
  chmod 700 "$directory"
  bus_config "$type" "$directory" >"$work/$type-bus.conf"
  local output
  output="$(dbus-daemon --config-file="$work/$type-bus.conf" --fork \
    --print-address=1 --print-pid=1)"
  printf -v "$address_variable" '%s' "$(printf '%s\n' "$output" | sed -n 1p)"
  printf -v "${address_variable}_pid" '%s' "$(printf '%s\n' "$output" | sed -n 2p)"
}

start_bus system "$work/run/system-bus" system_bus_address
system_bus_pid="${system_bus_address_pid}"
start_bus session "$work/run/session-bus" session_bus_address
session_bus_pid="${session_bus_address_pid}"
export DBUS_SYSTEM_BUS_ADDRESS="$system_bus_address"
export DBUS_SESSION_BUS_ADDRESS="$session_bus_address"

FAKE_LOGIND_BUS_ADDRESS="$system_bus_address" \
  FAKE_LOGIND_STATE_DIR="$work/logind" \
  python3 "$script_dir/fakes/logind.py" >"$work/logind.log" 2>&1 &
fake_logind_pid="$!"
for _ in $(seq 1 100); do
  if [ -f "$work/logind/ready" ] &&
    gdbus call --address "$system_bus_address" --dest org.freedesktop.login1 \
      --object-path /org/freedesktop/login1 \
      --method org.freedesktop.DBus.Properties.Get \
      org.freedesktop.login1.Manager NCurrentInhibitors >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$fake_logind_pid" 2>/dev/null; then
    printf 'fake logind exited before becoming ready:\n' >&2
    cat "$work/logind.log" >&2
    exit 1
  fi
  sleep 0.1
done

control() {
  printf '%s\n' "$1" >"$work/logind/control"
  for _ in $(seq 1 100); do
    [ "$(cat "$work/logind/applied" 2>/dev/null)" = "$1" ] &&
      [ "$(read_property PreparingForSleep)" = "$2" ] && return 0
    sleep 0.05
  done
  return 1
}

read_property() {
  local property="$1"
  gdbus call --address "$system_bus_address" --dest org.freedesktop.login1 \
    --object-path /org/freedesktop/login1 \
    --method org.freedesktop.DBus.Properties.Get \
    org.freedesktop.login1.Manager "$property" 2>/dev/null |
    sed -n 's/^(<\(.*\)>,)$/\1/p'
}

inhibitor_count() {
  cat "$work/logind/inhibitors" 2>/dev/null || printf 'unknown\n'
}

start_sway() {
  rm -f "$XDG_RUNTIME_DIR"/wayland-*
  printf 'xwayland disable\n' >"$work/sway.conf"
  # shellcheck disable=SC2086 # SWAY_FLAGS is a list of options.
  env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" \
    WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    sway ${SWAY_FLAGS:-} -c "$work/sway.conf" >>"$work/sway.log" 2>&1 &
  sway_pid="$!"
  wayland_display=""
  for _ in $(seq 1 200); do
    local socket
    socket="$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s -name 'wayland-*' \
      -print -quit)"
    if [ -n "$socket" ]; then
      wayland_display="$(basename "$socket")"
      return 0
    fi
    if ! kill -0 "$sway_pid" 2>/dev/null; then
      printf 'Sway exited before creating a Wayland socket:\n' >&2
      tail -n 20 "$work/sway.log" >&2 || true
      return 1
    fi
    sleep 0.1
  done
  printf 'Sway did not create a Wayland socket:\n' >&2
  tail -n 20 "$work/sway.log" >&2 || true
  return 1
}

start_pulse() {
  if ! command -v pulseaudio >/dev/null || ! command -v pactl >/dev/null; then
    return 1
  fi
  env -u PULSE_SERVER -u PULSE_RUNTIME_PATH \
    XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
    HOME="$HOME" \
    pulseaudio -n --daemonize=no --exit-idle-time=-1 \
    --load="module-native-protocol-unix" \
    --load="module-null-sink sink_name=kestrel-recovery-initial" \
    >"$work/pulse.log" 2>&1 &
  pulse_pid="$!"
  for _ in $(seq 1 100); do
    if pactl info >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$pulse_pid" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
  printf 'PulseAudio did not start; the audio check is manual:\n' >&2
  tail -n 10 "$work/pulse.log" >&2 || true
  return 1
}

session_run() {
  env -u DISPLAY XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    WAYLAND_DISPLAY="$wayland_display" "$@"
}

kestrel_session() {
  env -u DISPLAY XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    WAYLAND_DISPLAY="$wayland_display" \
    DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" \
    DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SYSTEM_BUS_ADDRESS" \
    XDG_CONFIG_HOME="$XDG_CONFIG_HOME" XDG_DATA_HOME="$XDG_DATA_HOME" \
    XDG_CACHE_HOME="$XDG_CACHE_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" \
    HOME="$HOME" PATH="$PATH" GDK_BACKEND=wayland GTK_A11Y=none \
    NO_AT_BRIDGE=1 "$@"
}

kestrel_control() {
  session_run timeout 15 "$kestrel" --command "$1" >/dev/null 2>&1
}

wait_for_bus_name() {
  local name
  for _ in $(seq 1 300); do
    for name in io.github.kamleshkc2002.Kestrel \
      io.github.kamleshkc2002.Kestrel.Devel; do
      if gdbus call --address "$DBUS_SESSION_BUS_ADDRESS" \
        --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner "$name" 2>/dev/null |
        grep -q true; then
        return 0
      fi
    done
    if ! kill -0 "$kestrel_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

dump_state() {
  session_run timeout 15 "$kestrel" --command diagnostics.dump-state 2>/dev/null
}

start_kestrel() {
  # The environment is inlined so the recorded PID is Kestrel itself, not a
  # wrapper function; SIGKILL checks depend on that.
  env -u DISPLAY XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    WAYLAND_DISPLAY="$wayland_display" \
    DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" \
    DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SYSTEM_BUS_ADDRESS" \
    XDG_CONFIG_HOME="$XDG_CONFIG_HOME" XDG_DATA_HOME="$XDG_DATA_HOME" \
    XDG_CACHE_HOME="$XDG_CACHE_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" \
    HOME="$HOME" PATH="$PATH" GDK_BACKEND=wayland GTK_A11Y=none \
    NO_AT_BRIDGE=1 "$kestrel" >>"$work/kestrel.log" 2>&1 &
  kestrel_pid="$!"
  if ! wait_for_bus_name; then
    printf 'Kestrel did not start:\n' >&2
    tail -n 20 "$work/kestrel.log" >&2 || true
    return 1
  fi
  local state
  for _ in $(seq 1 100); do
    state="$(dump_state || true)"
    if printf '%s\n' "$state" | grep -q $'^feature\t'; then
      return 0
    fi
    if ! kill -0 "$kestrel_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  printf 'Kestrel did not answer the diagnostics query:\n' >&2
  tail -n 20 "$work/kestrel.log" >&2 || true
  return 1
}

stop_kestrel() {
  if [ -n "$kestrel_pid" ] && kill -0 "$kestrel_pid" 2>/dev/null; then
    kill "$kestrel_pid" 2>/dev/null || true
    wait "$kestrel_pid" 2>/dev/null || true
  fi
  kestrel_pid=""
}

feature_field() {
  local feature_id="$1" key="$2"
  dump_state | awk -F '\t' -v id="$feature_id" -v key="$key" '
    $1 == "feature" && $2 == id {
      for (i = 3; i <= NF; i++) {
        split($i, pair, "=")
        if (pair[1] == key) print pair[2]
      }
    }'
}

wait_for_field() {
  local feature_id="$1" key="$2" expected="$3"
  for _ in $(seq 1 100); do
    if [ "$(feature_field "$feature_id" "$key")" = "$expected" ]; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

wait_for_at_least() {
  local feature_id="$1" key="$2" minimum="$3" value
  for _ in $(seq 1 100); do
    value="$(feature_field "$feature_id" "$key")"
    if [[ "$value" =~ ^[0-9]+$ ]] && [ "$value" -ge "$minimum" ]; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

refresh_until() {
  local feature_id="$1" key="$2" expected="$3"
  for _ in $(seq 1 40); do
    kestrel_control capabilities.refresh || true
    sleep 0.25
    if [ "$(feature_field "$feature_id" "$key")" = "$expected" ]; then
      return 0
    fi
  done
  return 1
}

profile_directory_name() {
  if [ -d "$XDG_STATE_HOME/kestrel-devel" ]; then
    printf 'Kestrel (Development)\n'
  else
    printf 'Kestrel\n'
  fi
}

describe_feature() {
  local feature_id="$1"
  dump_state | grep -F $'\t'"$feature_id"$'\t' || true
}

failures=0
skip_scenarios=0
run_scenario() {
  local description="$1" function="$2"
  if "$function"; then
    printf 'PASS %s\n' "$description"
  else
    printf 'FAIL %s\n' "$description"
    failures="$((failures + 1))"
  fi
  stop_kestrel
}

# 1. PrepareForSleep(true) then (false): clipboard history is wiped and the
#    keep-awake inhibitor is still held after resume.
scenario_suspend() {
  control allow false || true
  start_kestrel || return 1
  wait_for_field clipboard.history lifecycle running || {
    printf '  clipboard history is not running: %s\n' \
      "$(describe_feature clipboard.history)"
    return 1
  }
  kestrel_control keep-awake.start-30m || true
  wait_for_field power.keep-awake active 1 || {
    printf '  keep awake did not start: %s\n' \
      "$(describe_feature power.keep-awake)"
    return 1
  }
  printf 'recovery-marker' | session_run wl-copy
  wait_for_at_least clipboard.history items 1 || {
    printf '  the clipboard was not captured: %s\n' \
      "$(describe_feature clipboard.history)"
    return 1
  }
  control sleep true || {
    printf '  the fake logind did not enter sleep\n'
    return 1
  }
  wait_for_at_least clipboard.history wipes 1 || {
    printf '  sleep did not wipe the clipboard: %s\n' \
      "$(describe_feature clipboard.history)"
    return 1
  }
  wait_for_field clipboard.history items 0 || {
    printf '  clipboard entries survived sleep: %s\n' \
      "$(describe_feature clipboard.history)"
    return 1
  }
  if [ -n "$(session_run wl-paste 2>/dev/null)" ]; then
    printf '  the selection survived the wipe\n'
    return 1
  fi
  control awake false || {
    printf '  the fake logind did not resume\n'
    return 1
  }
  if [ "$(feature_field power.keep-awake active)" != "1" ]; then
    printf '  keep awake did not survive the resume: %s\n' \
      "$(describe_feature power.keep-awake)"
    return 1
  fi
  if [ "$(feature_field clipboard.history items)" != "0" ]; then
    printf '  resume restored clipboard entries: %s\n' \
      "$(describe_feature clipboard.history)"
    return 1
  fi
}

# 2. SIGKILL and restart: no stale inhibitor, no temporary files, no owned
#    selection, and no completed-looking recording from a partial take.
scenario_kill_and_restart() {
  control allow false || true
  start_kestrel || return 1
  wait_for_field clipboard.history lifecycle running || return 1
  kestrel_control keep-awake.start-30m || true
  wait_for_field power.keep-awake active 1 || {
    printf '  keep awake did not start\n'
    return 1
  }
  printf 'recovery-marker' | session_run wl-copy
  wait_for_at_least clipboard.history items 1 || {
    printf '  the clipboard was not captured\n'
    return 1
  }
  if [ "$(session_run wl-paste 2>/dev/null)" != "recovery-marker" ]; then
    printf '  Kestrel did not take the selection\n'
    return 1
  fi
  local recordings="$HOME/Videos/$(profile_directory_name)"
  mkdir -p "$recordings"
  touch -d '5 minutes ago' \
    "$recordings/.Kestrel-recording-2026-01-01-00-00-00.webm.partial"

  if [ "$(inhibitor_count)" != "1" ]; then
    printf '  the fake logind did not see the inhibitor (count %s)\n' \
      "$(inhibitor_count)"
    return 1
  fi

  kill -9 "$kestrel_pid" 2>/dev/null || true
  wait "$kestrel_pid" 2>/dev/null || true
  kestrel_pid=""

  for _ in $(seq 1 50); do
    [ "$(inhibitor_count)" = "0" ] && break
    sleep 0.1
  done
  if [ "$(inhibitor_count)" != "0" ]; then
    printf '  a stale inhibitor is still held (count %s)\n' "$(inhibitor_count)"
    return 1
  fi
  if [ -n "$(session_run wl-paste 2>/dev/null)" ]; then
    printf '  the selection outlived the killed process\n'
    return 1
  fi
  local leftovers
  leftovers="$(find "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" \
    "$XDG_STATE_HOME" "$HOME/Videos" \( -name '*.tmp*' -o -name '.tmp*' \) \
    -print 2>/dev/null)"
  if [ -n "$leftovers" ]; then
    printf '  temporary files were left behind:\n%s\n' "$leftovers"
    return 1
  fi

  start_kestrel || return 1
  if [ -e "$recordings/.Kestrel-recording-2026-01-01-00-00-00.webm.partial" ]; then
    printf '  a stale partial recording survived the restart\n'
    return 1
  fi
  if [ -n "$(find "$recordings" -maxdepth 1 -name 'Kestrel-recording-*.webm' \
    -print -quit)" ]; then
    printf '  an incomplete recording was listed as a completed take\n'
    return 1
  fi
  if [ "$(feature_field clipboard.history items)" != "0" ]; then
    printf '  the restarted instance retained clipboard entries: %s\n' \
      "$(describe_feature clipboard.history)"
    return 1
  fi
  if [ "$(feature_field power.keep-awake active)" != "0" ]; then
    printf '  the restarted instance restarted keep awake: %s\n' \
      "$(describe_feature power.keep-awake)"
    return 1
  fi
}

# 3. A dependency appears and disappears while Kestrel runs; Refresh changes
#    the reported capability each time.
scenario_dependency() {
  control allow false || true
  rm -f "$work/home/bin/grim" "$work/home/bin/slurp"
  start_kestrel || return 1
  wait_for_field capture.screenshot capability unsupported || {
    printf '  the screenshot provider was not reported missing: %s\n' \
      "$(describe_feature capture.screenshot)"
    return 1
  }
  printf '#!/bin/sh\nexit 0\n' >"$work/home/bin/grim"
  chmod +x "$work/home/bin/grim"
  refresh_until capture.screenshot capability limited || {
    printf '  grim did not appear after Refresh: %s\n' \
      "$(describe_feature capture.screenshot)"
    return 1
  }
  printf '#!/bin/sh\nexit 0\n' >"$work/home/bin/slurp"
  chmod +x "$work/home/bin/slurp"
  refresh_until capture.screenshot capability supported || {
    printf '  slurp did not complete the provider after Refresh: %s\n' \
      "$(describe_feature capture.screenshot)"
    return 1
  }
  rm -f "$work/home/bin/grim"
  refresh_until capture.screenshot capability unsupported || {
    printf '  the removed dependency was still reported: %s\n' \
      "$(describe_feature capture.screenshot)"
    return 1
  }
}

# 4. The compositor goes away: Kestrel exits once with a clean status, then a
#    fresh instance works on a restarted compositor.
scenario_compositor() {
  control allow false || true
  start_kestrel || return 1
  wait_for_field app.command-surface lifecycle running || return 1
  kill "$sway_pid" 2>/dev/null || true
  wait "$sway_pid" 2>/dev/null || true
  sway_pid=""
  for _ in $(seq 1 100); do
    kill -0 "$kestrel_pid" 2>/dev/null || break
    sleep 0.1
  done
  if kill -0 "$kestrel_pid" 2>/dev/null; then
    printf '  Kestrel kept running after the display disappeared\n'
    return 1
  fi
  local status=0
  wait "$kestrel_pid" 2>/dev/null || status="$?"
  kestrel_pid=""
  if [ "$status" -gt 128 ]; then
    printf '  Kestrel crashed on compositor loss (signal %s)\n' \
      "$((status - 128))"
    return 1
  fi
  if [ "$status" -ne 0 ] && [ "$status" -ne 1 ]; then
    printf '  Kestrel exited with unexpected status %s\n' "$status"
    return 1
  fi
  printf '  compositor loss ends Kestrel with status %s, without a signal\n' \
    "$status"
  start_sway || return 1
  start_kestrel || return 1
  wait_for_field app.command-surface lifecycle running || {
    printf '  the restarted instance did not run: %s\n' \
      "$(describe_feature app.command-surface)"
    return 1
  }
}

# 5. An audio output disappears and returns; Refresh sees both transitions.
scenario_audio_device() {
  if [ -z "$pulse_pid" ]; then
    return 1
  fi
  control allow false || true
  start_kestrel || return 1
  wait_for_at_least audio.mixer outputs 1 || {
    printf '  the null sink was not listed: %s\n' \
      "$(describe_feature audio.mixer)"
    return 1
  }
  local module
  module="$(pactl load-module module-null-sink sink_name=kestrel-recovery-extra)"
  pactl unload-module "$module"
  refresh_until audio.mixer outputs 1 || {
    printf '  the removed device was still listed: %s\n' \
      "$(describe_feature audio.mixer)"
    return 1
  }
  pactl load-module module-null-sink sink_name=kestrel-recovery-returned \
    >/dev/null
  if ! wait_for_at_least audio.mixer outputs 2; then
    refresh_until audio.mixer outputs 2 || {
      printf '  the returned device was not listed: %s\n' \
        "$(describe_feature audio.mixer)"
      return 1
    }
  fi
}

# 6. A logind call is denied mid-session: the feature reports the denial, the
#    process keeps running, and the action works again once access returns.
scenario_permission_denied() {
  control allow false || true
  start_kestrel || return 1
  wait_for_field power.keep-awake lifecycle running || {
    printf '  keep awake is not running: %s\n' \
      "$(describe_feature power.keep-awake)"
    return 1
  }
  kestrel_control keep-awake.start-30m || true
  wait_for_field power.keep-awake active 1 || return 1
  kestrel_control keep-awake.stop || true
  wait_for_field power.keep-awake active 0 || {
    printf '  keep awake did not stop\n'
    return 1
  }
  control deny false || {
    printf '  the fake logind did not start denying access\n'
    return 1
  }
  kestrel_control keep-awake.start-30m || true
  wait_for_field power.keep-awake error permission-denied || {
    printf '  the denied call was not reported: %s\n' \
      "$(describe_feature power.keep-awake)"
    return 1
  }
  if [ "$(feature_field power.keep-awake active)" != "0" ]; then
    printf '  keep awake claims to be held without permission\n'
    return 1
  fi
  if ! kill -0 "$kestrel_pid" 2>/dev/null; then
    printf '  Kestrel exited after the denied call\n'
    return 1
  fi
  control allow false || true
  kestrel_control keep-awake.start-30m || true
  wait_for_field power.keep-awake active 1 || {
    printf '  keep awake did not recover after access returned: %s\n' \
      "$(describe_feature power.keep-awake)"
    return 1
  }
}

# Fixtures shared by every scenario.
for profile in kestrel kestrel-devel; do
  mkdir -p "$XDG_CONFIG_HOME/$profile"
  cat >"$XDG_CONFIG_HOME/$profile/config.toml" <<'EOF'
schema_version = 4

[features."clipboard.history"]
enabled = true

[features."audio.mixer"]
enabled = true

[features."audio.microphone"]
enabled = true

[features."capture.screenshot"]
enabled = true

[features."capture.text"]
enabled = true

[features."power.keep-awake"]
enabled = true
EOF
done

if ! start_sway; then
  exit 1
fi
audio_started=1
if ! start_pulse; then
  audio_started=0
fi

run_scenario "scenario 1 (suspend/resume): the clipboard is wiped and keep awake stays held" \
  scenario_suspend
run_scenario "scenario 2 (SIGKILL/restart): no stale inhibitor, temporary file, selection, or partial take" \
  scenario_kill_and_restart
run_scenario "scenario 3 (dependency): the screenshot provider appears and disappears after Refresh" \
  scenario_dependency
run_scenario "scenario 4 (compositor): Kestrel exits cleanly and a restarted instance runs" \
  scenario_compositor
if [ "$audio_started" = "1" ]; then
  run_scenario "scenario 5 (audio device): an output disappears and returns after Refresh" \
    scenario_audio_device
else
  printf 'SKIP scenario 5 (audio device): start PulseAudio or PipeWire with pactl to automate it\n'
  skip_scenarios="$((skip_scenarios + 1))"
fi
run_scenario "scenario 6 (permission): a denied logind call is reported without a crash" \
  scenario_permission_denied

if [ "$failures" -ne 0 ]; then
  printf '%s scenario(s) failed\n' "$failures" >&2
  exit 1
fi
printf 'Recovery checks passed (%s skipped)\n' "$skip_scenarios"
