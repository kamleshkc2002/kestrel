#!/usr/bin/env bash
# Measures Kestrel's idle cost in a private headless Sway session: average CPU,
# wakeups per second, resident memory, and thread count for each setup preset
# with the window open and closed. Nothing in the caller's session is touched.
#
# Usage: idle-measure.sh <kestrel-executable> [options]
#   --out DIR         write idle.json and the raw runs here (default: a temp dir)
#   --seconds N       sample window per run (default: 20)
#   --settle N        seconds to wait after opening or closing (default: 8)
#   --check FILE      compare against a recorded baseline and fail past it
#                     (wakeups +50%, RSS +25%; see the margins in the file)
#
# The measured process is started with fresh XDG directories seeded with one
# config.toml per preset, so setup never runs and the presets differ only in
# the features they enable. A private system bus serves the logind fake, so
# clipboard history (which requires a logind session) runs in the presets that
# enable it. Sway ignores xdg_toplevel.set_minimized, so the closed state sends
# the window's close request (Kestrel's minimize path) and then moves it to the
# scratchpad, which stops compositing it.
#
# Missing sway, dbus-run-session, or python3-gi skips the run;
# KESTREL_REQUIRE_IDLE_MEASURE=1 makes it fatal. SWAY_FLAGS adds Sway options
# (for example --unsupported-gpu on NVIDIA hosts).

set -euo pipefail

script_path="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/$(basename -- "${BASH_SOURCE[0]}")"
script_dir="$(dirname -- "$script_path")"

# Feature IDs the setup presets choose between; see FeaturePreset in
# apps/kestrel/src/runtime/mod.rs. The tray stays as the user left it.
all_features=(
  system.monitor
  audio.mixer
  audio.microphone
  network.speed_test
  capture.screenshot
  camera.preview
  capture.recording
  capture.text
  capture.qr
  capture.color
  system.processes
  network.listeners
  clipboard.history
  commands.bar
  snippets.text
  notes.scratchpad
  files.shelf
  overlay.status
  global.shortcuts
  power.keep-awake
  desktop.appearance
  display.brightness
  keyboard.backlight
  radio.bluetooth
  radio.wifi
  files.empty-trash
  storage.eject
  files.show-hidden
  desktop.icons
  session.lock
)
essentials_features=(
  system.monitor
  audio.mixer
  audio.microphone
  power.keep-awake
  session.lock
)
presets=(essentials balanced everything)
window_states=(open closed)

preset_enables() {
  local preset="$1" feature="$2" enabled
  case "$preset" in
    essentials)
      for enabled in "${essentials_features[@]}"; do
        [ "$enabled" = "$feature" ] && return 0
      done
      return 1
      ;;
    balanced)
      # Clipboard history stays on here so the idle guard measures its event
      # wait in both feature-rich presets.
      case "$feature" in
        global.shortcuts) return 1 ;;
        *) return 0 ;;
      esac
      ;;
    everything) return 0 ;;
    *) return 1 ;;
  esac
}

# Writes the same configuration under both profile names; release builds read
# kestrel/, debug builds kestrel-devel/.
write_configuration() {
  local root="$1" preset="$2" profile feature enabled
  for profile in kestrel kestrel-devel; do
    local path="$root/$profile/config.toml"
    mkdir -p "$root/$profile"
    {
      printf 'schema_version = 4\n'
      # A closed window stays running, which the closed measurement needs.
      printf '[ui]\nclose_action = "minimize"\n'
      printf '[startup]\nwhats_new = false\n'
      printf '[updates]\ncheck = "disabled"\n'
      for feature in "${all_features[@]}"; do
        enabled=false
        preset_enables "$preset" "$feature" && enabled=true
        printf '[features."%s"]\nenabled = %s\n' "$feature" "$enabled"
      done
    } >"$path"
  done
}

# Sums voluntary and nonvoluntary context switches over every live thread.
read_context_switches() {
  local pid="$1" total=0 file value
  for file in "/proc/$pid/task/"*/status; do
    [ -r "$file" ] || continue
    value="$(awk '/^voluntary_ctxt_switches/{switches+=$2} /^nonvoluntary_ctxt_switches/{switches+=$2} END{print switches+0}' "$file" 2>/dev/null)" || continue
    total=$((total + value))
  done
  printf '%s\n' "$total"
}

read_cpu_ticks() {
  awk '{print $14 + $15}' "/proc/$1/stat"
}

read_status_field() {
  awk -v field="$2" '$1 == field ":" {print $2; exit}' "/proc/$1/status"
}

# Prints one run as a compact JSON object: average CPU over the window, wakeups
# per second from context switches, peak RSS in KiB, and the final thread count.
sample_run() {
  local pid="$1" seconds="$2" preset="$3" window="$4"
  local hz cpu_ticks_before cpu_ticks_after switches_before switches_after
  local elapsed=0 peak_rss=0 threads=0 rss
  hz="$(getconf CLK_TCK)"
  cpu_ticks_before="$(read_cpu_ticks "$pid")"
  switches_before="$(read_context_switches "$pid")"
  while [ "$elapsed" -lt "$seconds" ]; do
    sleep 1
    elapsed=$((elapsed + 1))
    rss="$(read_status_field "$pid" VmRSS || true)"
    threads="$(read_status_field "$pid" Threads || true)"
    if [ -n "$rss" ] && [ "$rss" -gt "$peak_rss" ]; then
      peak_rss="$rss"
    fi
  done
  cpu_ticks_after="$(read_cpu_ticks "$pid")"
  switches_after="$(read_context_switches "$pid")"
  local switch_delta=$((switches_after - switches_before))
  [ "$switch_delta" -lt 0 ] && switch_delta=0
  printf '{"preset": "%s", "window": "%s", "seconds": %s, "cpu_percent": %.2f, "wakeups_per_second": %.2f, "rss_kib": %s, "threads": %s}\n' \
    "$preset" "$window" "$seconds" \
    "$(awk -v delta="$((cpu_ticks_after - cpu_ticks_before))" -v hz="$hz" -v seconds="$seconds" 'BEGIN {print delta / hz / seconds * 100}')" \
    "$(awk -v delta="$switch_delta" -v seconds="$seconds" 'BEGIN {print delta / seconds}')" \
    "$peak_rss" "$threads"
}

# Finds the app ID from Sway's tree once a Kestrel window exists.
detect_app_id() {
  swaymsg -t get_tree 2>/dev/null |
    grep -o 'io\.github\.kamleshkc2002\.Kestrel[A-Za-z.]*' |
    head -n 1 || true
}

wait_for_window() {
  local pid="$1" log="$2" app_id=""
  for _ in $(seq 1 120); do
    app_id="$(detect_app_id)"
    if [ -n "$app_id" ]; then
      printf '%s\n' "$app_id"
      return 0
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
      printf 'Kestrel exited before opening its window; last lines of %s:\n' "$log" >&2
      tail -n 20 -- "$log" >&2 || true
      return 1
    fi
    sleep 0.5
  done
  printf 'Kestrel did not open its window; last lines of %s:\n' "$log" >&2
  tail -n 20 -- "$log" >&2 || true
  return 1
}

close_window() {
  local pid="$1" app_id="$2"
  swaymsg "[app_id=\"$app_id\"] kill" >/dev/null 2>&1 || true
  sleep 2
  swaymsg "[app_id=\"$app_id\"] move scratchpad" >/dev/null 2>&1 || true
  if ! kill -0 "$pid" 2>/dev/null; then
    printf 'Kestrel exited when its window closed; [ui] close_action must be minimize\n' >&2
    return 1
  fi
}

measure_preset() {
  local preset="$1" window="$2" work="$3" seconds="$4" settle="$5" binary="$6"
  local run_dir="$work/runs/$preset-$window" log app_id pid
  mkdir -p "$run_dir/data" "$run_dir/state" "$run_dir/cache"
  write_configuration "$run_dir/config" "$preset"
  export XDG_CONFIG_HOME="$run_dir/config"
  export XDG_DATA_HOME="$run_dir/data"
  export XDG_STATE_HOME="$run_dir/state"
  export XDG_CACHE_HOME="$run_dir/cache"
  log="$run_dir/kestrel.log"
  "$binary" >"$log" 2>&1 &
  pid="$!"
  if ! app_id="$(wait_for_window "$pid" "$log")"; then
    kill "$pid" 2>/dev/null || true
    return 1
  fi
  sleep "$settle"
  if preset_enables "$preset" clipboard.history; then
    local state
    state="$(timeout 15 "$binary" --command diagnostics.dump-state 2>/dev/null || true)"
    if ! printf '%s\n' "$state" | grep -F $'\tclipboard.history\t' |
      grep -q 'lifecycle=running'; then
      printf 'clipboard history did not start in the %s/%s run:\n' \
        "$preset" "$window" >&2
      printf '%s\n' "$state" | grep -F clipboard.history >&2 || true
      tail -n 20 -- "$log" >&2 || true
      kill "$pid" 2>/dev/null || true
      return 1
    fi
    printf 'clipboard history in %s/%s: %s\n' "$preset" "$window" \
      "$(printf '%s\n' "$state" | grep -F $'\tclipboard.history\t')"
    sleep 1
  fi
  if [ "$window" = "closed" ] && ! close_window "$pid" "$app_id"; then
    kill "$pid" 2>/dev/null || true
    return 1
  fi
  [ "$window" = "closed" ] && sleep "$settle"
  sample_run "$pid" "$seconds" "$preset" "$window" >"$work/results/$preset-$window.json"
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

# Runs inside the private session bus. Every run gets fresh XDG directories.
measure_all() {
  set -u
  unset DISPLAY
  export GDK_BACKEND=wayland GTK_A11Y=none NO_AT_BRIDGE=1
  mkdir -p "$KESTREL_IDLE_WORK/results"
  local preset window
  for preset in "${presets[@]}"; do
    for window in "${window_states[@]}"; do
      measure_preset "$preset" "$window" "$KESTREL_IDLE_WORK" \
        "$KESTREL_IDLE_SECONDS" "$KESTREL_IDLE_SETTLE" "$KESTREL_IDLE_BINARY" ||
        exit 1
    done
  done
}

cpu_model() {
  awk -F': ' '/^model name/ {print $2; exit}' /proc/cpuinfo
}

json_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# The margins absorb runner differences: 50% more wakeups (plus 1/s for the
# one-second sampling grid and timer coalescing) and 25% more RSS (different
# Mesa, fontconfig, and glibc caches). Both sit well past run-to-run noise.
# Compares every measured run with its baseline; returns 1 when any exceeds a
# margin. Prints one line per run either way.
check_baseline() {
  local report="$1" baseline="$2"
  local wakeup_margin rss_margin failures=0
  wakeup_margin="$(jq -r '.margins.wakeups_percent // 50' "$baseline")"
  rss_margin="$(jq -r '.margins.rss_percent // 25' "$baseline")"
  printf '\nBaseline check: wakeups +%s%% (+1/s), RSS +%s%%\n' "$wakeup_margin" "$rss_margin"
  printf '%-11s %-7s %10s %10s %9s %9s\n' \
    "Preset" "Window" "Wakeups" "Baseline" "RSS KiB" "Baseline"
  local preset window wakeups rss baseline_wakeups baseline_rss wakeup_limit rss_limit verdict
  while read -r preset window wakeups rss; do
    baseline_wakeups="$(jq -r --arg preset "$preset" --arg window "$window" \
      '.runs[] | select(.preset == $preset and .window == $window) | .wakeups_per_second' "$baseline")"
    baseline_rss="$(jq -r --arg preset "$preset" --arg window "$window" \
      '.runs[] | select(.preset == $preset and .window == $window) | .rss_kib' "$baseline")"
    if [ -z "$baseline_wakeups" ] || [ "$baseline_wakeups" = "null" ] ||
      [ -z "$baseline_rss" ] || [ "$baseline_rss" = "null" ]; then
      printf '%-11s %-7s no baseline entry\n' "$preset" "$window"
      failures=$((failures + 1))
      continue
    fi
    wakeup_limit="$(awk -v base="$baseline_wakeups" -v margin="$wakeup_margin" 'BEGIN {print base * (1 + margin / 100) + 1}')"
    rss_limit="$(awk -v base="$baseline_rss" -v margin="$rss_margin" 'BEGIN {print base * (1 + margin / 100)}')"
    local over_wakeups=false over_rss=false
    verdict="ok"
    if awk -v value="$wakeups" -v limit="$wakeup_limit" 'BEGIN {exit !(value > limit)}'; then
      over_wakeups=true
      failures=$((failures + 1))
    fi
    if awk -v value="$rss" -v limit="$rss_limit" 'BEGIN {exit !(value > limit)}'; then
      over_rss=true
      failures=$((failures + 1))
    fi
    if [ "$over_wakeups" = true ] && [ "$over_rss" = true ]; then
      verdict="over wakeups and RSS"
    elif [ "$over_wakeups" = true ]; then
      verdict="over wakeups"
    elif [ "$over_rss" = true ]; then
      verdict="over RSS"
    fi
    printf '%-11s %-7s %10s %10s %9s %9s  %s\n' \
      "$preset" "$window" "$wakeups" "$(awk -v v="$wakeup_limit" 'BEGIN {printf "%.2f", v}')" \
      "$rss" "$(awk -v v="$rss_limit" 'BEGIN {printf "%.0f", v}')" "$verdict"
  done < <(jq -r '.runs[] | [.preset, .window, .wakeups_per_second, .rss_kib] | @tsv' "$report")
  if [ "$failures" -gt 0 ]; then
    printf 'Idle cost grew past the recorded baseline (%s run(s) over).\n' "$failures" >&2
    return 1
  fi
  printf 'Idle cost is within the recorded baseline.\n'
}

# Inner mode: the outer run already started Sway and the private session bus.
if [ "${KESTREL_IDLE_INNER:-0}" = "1" ]; then
  measure_all
  exit 0
fi

usage() {
  sed -n '2,/^$/p' "$script_path" | sed -e 's/^# \{0,1\}//' >&2
  exit 2
}

[ "$#" -ge 1 ] || usage
case "$1" in
  -h | --help) usage ;;
esac
kestrel="$(realpath "$1")"
shift
out_dir=""
seconds=20
settle=8
baseline=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out_dir="${2:?--out needs a directory}"; shift 2 ;;
    --seconds) seconds="${2:?--seconds needs a number}"; shift 2 ;;
    --settle) settle="${2:?--settle needs a number}"; shift 2 ;;
    --check) baseline="${2:?--check needs a baseline file}"; shift 2 ;;
    -h | --help) usage ;;
    *) printf 'idle-measure: unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
[ -x "$kestrel" ] || {
  printf 'idle-measure: not an executable: %s\n' "$kestrel" >&2
  exit 2
}
if [ -n "$baseline" ]; then
  command -v jq >/dev/null || {
    printf 'idle-measure: --check needs jq\n' >&2
    exit 2
  }
  [ -r "$baseline" ] || {
    printf 'idle-measure: baseline is unreadable: %s\n' "$baseline" >&2
    exit 2
  }
fi

require_measure="${KESTREL_REQUIRE_IDLE_MEASURE:-0}"
for command in sway swaymsg dbus-run-session dbus-daemon python3; do
  if ! command -v "$command" >/dev/null; then
    if [ "$require_measure" = "1" ]; then
      printf 'required idle-measure dependency is unavailable: %s\n' "$command" >&2
      exit 1
    fi
    printf 'idle measurement skipped: %s is unavailable\n' "$command"
    exit 0
  fi
done
if ! python3 -c 'import gi; gi.require_version("Gio", "2.0")' >/dev/null 2>&1; then
  if [ "$require_measure" = "1" ]; then
    printf 'required idle-measure dependency is unavailable: python3-gi\n' >&2
    exit 1
  fi
  printf 'idle measurement skipped: python3-gi is unavailable\n'
  exit 0
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/kestrel-idle.XXXXXX")"
chmod 700 "$work"
mkdir -p "$work/run"
chmod 700 "$work/run"
sway_pid=""
system_bus_pid=""
fake_logind_pid=""
cleanup() {
  if [ -n "$sway_pid" ]; then
    kill "$sway_pid" 2>/dev/null || true
    for _ in $(seq 1 20); do
      kill -0 "$sway_pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -9 "$sway_pid" 2>/dev/null || true
  fi
  if [ -n "$fake_logind_pid" ]; then
    kill "$fake_logind_pid" 2>/dev/null || true
    wait "$fake_logind_pid" 2>/dev/null || true
  fi
  if [ -n "$system_bus_pid" ]; then
    kill "$system_bus_pid" 2>/dev/null || true
    wait "$system_bus_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

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

# A private system bus serves the logind fake, so clipboard history starts
# instead of reporting that no logind session is available.
mkdir -p "$work/run/system-bus" "$work/logind"
chmod 700 "$work/run/system-bus"
cat >"$work/system-bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>system</type>
  <listen>unix:dir=$work/run/system-bus</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
system_bus_output="$(dbus-daemon --config-file="$work/system-bus.conf" --fork \
  --print-address=1 --print-pid=1)"
export DBUS_SYSTEM_BUS_ADDRESS="$(printf '%s\n' "$system_bus_output" | sed -n 1p)"
system_bus_pid="$(printf '%s\n' "$system_bus_output" | sed -n 2p)"
FAKE_LOGIND_BUS_ADDRESS="$DBUS_SYSTEM_BUS_ADDRESS" \
  FAKE_LOGIND_STATE_DIR="$work/logind" \
  python3 "$script_dir/fakes/logind.py" >"$work/logind.log" 2>&1 &
fake_logind_pid="$!"
for _ in $(seq 1 100); do
  if [ -f "$work/logind/ready" ]; then
    break
  fi
  if ! kill -0 "$fake_logind_pid" 2>/dev/null; then
    printf 'fake logind exited before becoming ready:\n' >&2
    cat "$work/logind.log" >&2
    exit 1
  fi
  sleep 0.1
done
if [ ! -f "$work/logind/ready" ]; then
  printf 'fake logind did not become ready:\n' >&2
  cat "$work/logind.log" >&2
  exit 1
fi

# shellcheck disable=SC2086 # SWAY_FLAGS is a list of options.
env -u DISPLAY -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
  XDG_RUNTIME_DIR="$work/run" \
  WLR_BACKENDS=headless \
  WLR_RENDERER=pixman \
  WLR_LIBINPUT_NO_DEVICES=1 \
  SWAYSOCK="$work/run/sway.sock" \
  sway ${SWAY_FLAGS:-} -c "$work/sway.conf" >"$work/sway.log" 2>&1 &
sway_pid="$!"

wayland_display=""
for _ in $(seq 1 100); do
  for socket in "$work/run"/wayland-*; do
    if [ -S "$socket" ] && [[ "$socket" != *.lock ]]; then
      wayland_display="$(basename -- "$socket")"
      break 2
    fi
  done
  if ! kill -0 "$sway_pid" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if [ -z "$wayland_display" ]; then
  printf 'Sway did not create a Wayland socket:\n' >&2
  cat "$work/sway.log" >&2
  exit 1
fi

export XDG_RUNTIME_DIR="$work/run"
export WAYLAND_DISPLAY="$wayland_display"
export SWAYSOCK="$work/run/sway.sock"

KESTREL_IDLE_INNER=1 \
  KESTREL_IDLE_WORK="$work" \
  KESTREL_IDLE_BINARY="$kestrel" \
  KESTREL_IDLE_SECONDS="$seconds" \
  KESTREL_IDLE_SETTLE="$settle" \
  dbus-run-session --config-file="$work/bus.conf" -- "$script_path"

report="$work/idle.json"
{
  printf '{\n'
  printf '  "cpu": "%s",\n' "$(json_escape "$(cpu_model)")"
  printf '  "session": "headless Sway (WLR_BACKENDS=headless, WLR_RENDERER=pixman)",\n'
  printf '  "seconds": %s,\n' "$seconds"
  printf '  "settle_seconds": %s,\n' "$settle"
  printf '  "runs": [\n'
  first=true
  for preset in "${presets[@]}"; do
    for window in "${window_states[@]}"; do
      [ "$first" = true ] || printf ',\n'
      printf '    %s' "$(cat "$work/results/$preset-$window.json")"
      first=false
    done
  done
  printf '\n  ]\n}\n'
} >"$report"

printf '\n%-11s %-7s %8s %11s %9s %8s\n' \
  "Preset" "Window" "CPU %" "Wakeups/s" "RSS MiB" "Threads"
jq -r '.runs[] | [.preset, .window, .cpu_percent, .wakeups_per_second, (.rss_kib / 1024 | floor), .threads] | @tsv' "$report" |
  while IFS=$'\t' read -r preset window cpu wakeups rss threads; do
    printf '%-11s %-7s %8s %11s %9s %8s\n' "$preset" "$window" "$cpu" "$wakeups" "$rss" "$threads"
  done

# Turning features off must stop their threads and timers: compare the open
# window across presets, which differ only in what the preset enables.
thread_line="$(jq -r '[.runs[] | select(.window == "open")] | map("\(.preset)=\(.threads)") | join(", ")' "$report")"
printf '\nThreads with the window open: %s\n' "$thread_line"
essentials_threads="$(jq -r '.runs[] | select(.preset == "essentials" and .window == "open") | .threads' "$report")"
balanced_threads="$(jq -r '.runs[] | select(.preset == "balanced" and .window == "open") | .threads' "$report")"
everything_threads="$(jq -r '.runs[] | select(.preset == "everything" and .window == "open") | .threads' "$report")"
if [ "$essentials_threads" -lt "$balanced_threads" ] && [ "$balanced_threads" -lt "$everything_threads" ]; then
  printf 'Threads rise with the features a preset turns on.\n'
else
  printf 'Threads do not rise strictly with the preset; see the numbers above.\n'
fi

if [ -n "$out_dir" ]; then
  mkdir -p "$out_dir"
  cp "$report" "$out_dir/idle.json"
  cp -R "$work/results" "$out_dir/runs"
  printf '\nWrote %s\n' "$out_dir/idle.json"
fi

if [ -n "$baseline" ]; then
  check_baseline "$report" "$baseline"
fi
