#!/usr/bin/env bash
# Starts an extracted AppImage with GStreamer shadowed, then checks that the
# window opens and screen recording reports it unavailable.
#
# The AppImage must not bundle GStreamer. Empty files named for the helper's
# GStreamer sonames sit on the helper's own library path, so it cannot load a
# host copy either; nothing outside the extracted AppDir is touched.
#
# Usage: no-gstreamer-smoke.sh <AppDir>
set -euo pipefail

if [ "$#" -ne 1 ]; then
  printf 'usage: %s <AppDir>\n' "$0" >&2
  exit 2
fi
appdir="$(realpath "$1")"
for command in xvfb-run dbus-run-session gdbus; do
  if ! command -v "$command" >/dev/null; then
    printf 'the no-GStreamer smoke needs %s\n' "$command" >&2
    exit 1
  fi
done

work="$(mktemp -d)"
chmod 700 "$work"
placeholders=()
cleanup() {
  for placeholder in "${placeholders[@]}"; do
    rm -f "$placeholder"
  done
  rm -rf "$work"
}
trap cleanup EXIT

if [ -n "$(find "$appdir/usr/lib" "$appdir/lib" -name 'libgst*.so*' -print -quit 2>/dev/null)" ]; then
  printf 'The AppImage bundles GStreamer.\n' >&2
  exit 1
fi

# Shadow the runtime the helper links: an invalid file is found before any
# host library, whichever search path the AppImage runs with.
mkdir -p "$appdir/usr/lib"
for soname in libgstreamer-1.0.so.0 libgstapp-1.0.so.0 libgstbase-1.0.so.0; do
  placeholder="$appdir/usr/lib/$soname"
  : >"$placeholder"
  placeholders+=("$placeholder")
done
if env LD_LIBRARY_PATH="$appdir/usr/lib" timeout 10 \
  "$appdir/usr/bin/kestrel-record" <<<'{"command":"probe"}' >/dev/null 2>&1; then
  printf 'The helper ran with GStreamer shadowed.\n' >&2
  exit 1
fi

mkdir -p "$work/config/kestrel" "$work/data" "$work/cache" "$work/state"
printf '%s\n' 'schema_version = 3' \
  '[features."capture.recording"]' 'enabled = true' \
  >"$work/config/kestrel/config.toml"

export KESTREL_SMOKE_WORK="$work"
export KESTREL_SMOKE_APPDIR="$appdir"
dbus-run-session -- xvfb-run -a bash -c '
set -euo pipefail
work="$KESTREL_SMOKE_WORK"
appdir="$KESTREL_SMOKE_APPDIR"
export XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data"
export XDG_CACHE_HOME="$work/cache" XDG_STATE_HOME="$work/state"
export GTK_A11Y=none NO_AT_BRIDGE=1
log="$work/kestrel.log"
"$appdir/AppRun" >"$log" 2>&1 &
pid=$!
cleanup() {
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}
trap cleanup EXIT
owned() {
  grep -q true <<<"$(gdbus call --session --dest org.freedesktop.DBus \
    --object-path /org/freedesktop/DBus \
    --method org.freedesktop.DBus.NameHasOwner \
    io.github.kamleshkc2002.Kestrel 2>/dev/null)"
}
for _ in $(seq 1 120); do
  owned && break
  kill -0 "$pid" 2>/dev/null || break
  sleep 0.5
done
if ! kill -0 "$pid" 2>/dev/null; then
  printf "Kestrel exited without GStreamer:\n" >&2
  cat "$log" >&2
  exit 1
fi
if ! owned; then
  printf "Kestrel did not open without GStreamer:\n" >&2
  cat "$log" >&2
  exit 1
fi
logs="$work/state/kestrel/logs"
for _ in $(seq 1 40); do
  grep -q "capture.recording capability: unsupported: .*GStreamer" "$logs"/*.log 2>/dev/null && break
  sleep 0.25
done
if ! grep -q "capture.recording capability: unsupported: .*GStreamer" "$logs"/*.log 2>/dev/null; then
  printf "Screen recording did not report its missing GStreamer:\n" >&2
  cat "$logs"/*.log 2>/dev/null | tail -n 30 >&2
  cat "$log" >&2
  exit 1
fi
set +e
"$appdir/AppRun" --command recording.start >"$work/command.log" 2>&1
status=$?
set -e
if [ "$status" -ne 4 ]; then
  printf "recording.start exited %s instead of unavailable (4):\n" "$status" >&2
  cat "$work/command.log" >&2
  exit 1
fi
printf "Window opened and recording reported unavailable without GStreamer\n"
'
