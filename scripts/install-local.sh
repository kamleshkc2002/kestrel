#!/usr/bin/env bash
# Build and install Kestrel for the current user.
#
#   bash scripts/install-local.sh              install or update
#   bash scripts/install-local.sh --restart    also (re)start Kestrel; it logs to
#                                              ~/.local/state/kestrel/logs
#   bash scripts/install-local.sh --uninstall  remove installed files and the
#                                              autostart entry
#
# Settings and data in ~/.config/kestrel and ~/.local/share/kestrel stay.
# Debug builds use their own ID and kestrel-devel folders alongside this copy.
# A gtk4-layer-shell installed under the prefix (for the command bar and status
# overlay) is linked with an rpath, so the installed binary finds it.

set -euo pipefail

app_id="io.github.kamleshkc2002.Kestrel"
repository="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="${KESTREL_PREFIX:-$HOME/.local}"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
binary="$prefix/bin/kestrel"
desktop_file="$prefix/share/applications/$app_id.desktop"
case "$config_home" in
  /*) autostart_file="$config_home/autostart/$app_id.desktop" ;;
  *) autostart_file="" ;;
esac
icon_root="$prefix/share/icons/hicolor"
icon_file="$icon_root/scalable/apps/$app_id.svg"

refresh_caches() {
  if command -v update-desktop-database >/dev/null; then
    update-desktop-database -q "$prefix/share/applications" || true
  fi
  if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -q -t "$icon_root" || true
  fi
}

running() {
  command -v gdbus >/dev/null &&
    gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
      --method org.freedesktop.DBus.NameHasOwner "$app_id" 2>/dev/null | grep -q true
}

case "${1:-}" in
  --uninstall)
    if running; then
      "$binary" --command app.quit || true
    fi
    rm -f "$binary" "$desktop_file" "$icon_file"
    if [ -n "$autostart_file" ]; then
      rm -f "$autostart_file"
    fi
    refresh_caches
    printf 'Removed Kestrel from %s; settings and data were kept.\n' "$prefix"
    exit 0
    ;;
  "" | --restart) ;;
  *)
    printf 'usage: %s [--restart | --uninstall]\n' "$0" >&2
    exit 2
    ;;
esac

layer_shell_lib="$prefix/lib/x86_64-linux-gnu"
if [ -f "$layer_shell_lib/pkgconfig/gtk4-layer-shell-0.pc" ]; then
  export PKG_CONFIG_PATH="$layer_shell_lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-Wl,-rpath,$layer_shell_lib"
fi
cargo build --release --locked -p kestrel --manifest-path "$repository/Cargo.toml"

# Replacing the file lets a running copy keep its binary.
install -Dm755 "$repository/target/release/kestrel" "$binary"
install -Dm644 "$repository/data/icons/$app_id.svg" "$icon_file"
mkdir -p "$(dirname -- "$desktop_file")"
# An absolute Exec works when ~/.local/bin is absent from the launcher's PATH.
desktop_temp="$(mktemp "$desktop_file.XXXXXX")"
trap 'rm -f "$desktop_temp"' EXIT
sed -e "s|^Exec=kestrel\$|Exec=$binary|" \
  -e "s|^Exec=kestrel |Exec=$binary |" \
  "$repository/data/$app_id.desktop" >"$desktop_temp"
chmod 644 "$desktop_temp"
mv -f "$desktop_temp" "$desktop_file"
trap - EXIT
refresh_caches

# Kestrel now keeps bounded logs; the older unbounded launch log goes.
rm -f "${XDG_STATE_HOME:-$HOME/.local/state}/kestrel/launch.log"

printf 'Installed %s (%s).\n' "$binary" "$("$binary" --version)"

launch() {
  # Kestrel keeps its own bounded logs; stdout and stderr carry nothing extra.
  setsid "$binary" >/dev/null 2>&1 </dev/null &
  for _ in $(seq 1 50); do
    running && return 0
    sleep 0.1
  done
  printf 'Did not start; see %s.\n' \
    "${XDG_STATE_HOME:-$HOME/.local/state}/kestrel/logs" >&2
  return 1
}

if [ "${1:-}" = "--restart" ]; then
  if running; then
    "$binary" --command app.quit || true
    for _ in $(seq 1 50); do
      running || break
      sleep 0.1
    done
    if running; then
      printf 'The running copy did not quit; close it and start it again.\n' >&2
      exit 1
    fi
  fi
  launch
  printf 'Started Kestrel.\n'
elif running; then
  printf 'The previous build is still running; rerun with --restart or quit and reopen it.\n'
fi
