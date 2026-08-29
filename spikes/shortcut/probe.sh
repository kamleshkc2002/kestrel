#!/usr/bin/env bash
#
# Disposable non-interactive probe for Kestrel Issue #7:
# "Phase 0: validate global-shortcut activation paths".
#
# This script performs read-only introspection only. It never calls
# BindShortcuts and never triggers a consent dialog. It probes:
#   1. session identity and the xdg-desktop-portal frontend version
#   2. org.freedesktop.portal.GlobalShortcuts availability/version
#   3. active portal backends and whether any exposes the
#      org.freedesktop.impl.portal.GlobalShortcuts backend interface
#   4. permission-store tables used by the GlobalShortcuts portal
#   5. COSMIC compositor-specific keybinding surfaces
#   6. X11/Xwayland reachability plus the XGrabKey fallback surface
#   7. the always-available normal-window/command-surface entry point
#
# Usage: bash spikes/shortcut/probe.sh > /tmp/kestrel-shortcut-probe.txt 2>&1
set -u

say() { printf '%s\n' "$*"; }

say "== session =="
say "XDG_SESSION_TYPE=${XDG_SESSION_TYPE:-unset}"
say "XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-unset}"
say "XDG_SESSION_DESKTOP=${XDG_SESSION_DESKTOP:-unset}"
say "WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-unset}"
say "DISPLAY=${DISPLAY:-unset}"
say "DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unset}"

say ""
say "== portal frontend =="
if command -v xdg-desktop-portal >/dev/null 2>&1; then
    say "xdg-desktop-portal_version=$(xdg-desktop-portal --version 2>/dev/null)"
elif [ -x /usr/libexec/xdg-desktop-portal ]; then
    say "xdg-desktop-portal_version=$(/usr/libexec/xdg-desktop-portal --version 2>/dev/null)"
else
    say "xdg-desktop-portal_version=not-found"
fi

say "frontend_GlobalShortcuts_version_probe:"
busctl --user get-property org.freedesktop.portal.Desktop \
    /org/freedesktop/portal/desktop \
    org.freedesktop.portal.GlobalShortcuts version 2>&1 | sed 's/^/  /'

say "frontend_GlobalShortcuts_method_probe(CreateSession):"
busctl --user call org.freedesktop.portal.Desktop \
    /org/freedesktop/portal/desktop \
    org.freedesktop.portal.GlobalShortcuts CreateSession 'a{sv}' 0 2>&1 | sed 's/^/  /'

say "frontend_portal_interface_count:"
busctl --user introspect org.freedesktop.portal.Desktop \
    /org/freedesktop/portal/desktop 2>/dev/null | grep -c 'org.freedesktop.portal.' | sed 's/^/  /'

say ""
say "== portal backends =="
for svc in org.freedesktop.impl.portal.desktop.cosmic org.freedesktop.impl.portal.desktop.gtk; do
    say "backend=$svc"
    busctl --user introspect "$svc" /org/freedesktop/portal/desktop 2>&1 \
        | grep 'org.freedesktop.impl.portal' | sed 's/^/  /'
done

say ""
say "== portal backend .portal configuration =="
for f in /usr/share/xdg-desktop-portal/portals/cosmic.portal \
         /usr/share/xdg-desktop-portal/portals/gtk.portal; do
    say "--- $f ---"
    if [ -r "$f" ]; then
        sed -n 's/^/  /p' "$f"
    else
        say "  (missing)"
    fi
done

say ""
say "== permission store (read-only List) =="
for table in shortcuts global_shortcuts global-shortcuts GlobalShortcuts; do
    printf '  List(%s) -> ' "$table"
    busctl --user call org.freedesktop.impl.portal.PermissionStore \
        /org/freedesktop/impl/portal/PermissionStore \
        org.freedesktop.impl.portal.PermissionStore List s "$table" 2>&1 | tr -d '\n'
    printf '\n'
done

say ""
say "== COSMIC compositor-specific keybinding surfaces =="
say "  well-known names (shortcut/keybind/comp/settings matches):"
busctl --user list 2>/dev/null | grep -iE 'cosmic|keybind|shortcut|comp|settings' \
    | sed 's/^/    /' | head -40
say "  cosmic_settings_daemon_present=$(
    busctl --user list 2>/dev/null | grep -q 'com.system76.CosmicSettingsDaemon' \
        && echo yes || echo no)"
say "  cosmic_comp_present=$(
    busctl --user list 2>/dev/null | grep -q 'com.system76.CosmicComp' \
        && echo yes || echo no)"
say "  cosmic_config_shortcut_files:"
find "${XDG_CONFIG_HOME:-$HOME/.config}/cosmic" -type f \
    \( -iname '*shortcut*' -o -iname '*keybind*' -o -iname '*key_bind*' \
       -o -iname '*binding*' \) 2>/dev/null | sed 's/^/    /'
say "  cosmic_config_grep_hits:"
grep -rilE 'shortcut|keybind|binding' \
    "${XDG_CONFIG_HOME:-$HOME/.config}/cosmic" 2>/dev/null | sed 's/^/    /' | head -20

say ""
say "== X11 / Xwayland reachability =="
if command -v xdpyinfo >/dev/null 2>&1; then
    say "  xdpyinfo:"
    xdpyinfo 2>&1 | grep -E 'name of display|version number|dimensions' | sed 's/^/    /'
else
    say "  xdpyinfo=missing"
fi
if command -v xprop >/dev/null 2>&1; then
    say "  xprop_root_supporting_wm:"
    xprop -root _NET_SUPPORTING_WM_CHECK 2>&1 | sed 's/^/    /'
else
    say "  xprop=missing"
fi

say ""
say "== X11 XGrabKey fallback surface =="
HERE="$(cd "$(dirname "$0")" && pwd)"
if command -v gcc >/dev/null 2>&1 && [ -f /usr/include/X11/Xlib.h ]; then
    gcc -Wall -Wextra -O2 -o "$HERE/xgrabkey_probe" "$HERE/xgrabkey_probe.c" -lX11 \
        && "$HERE/xgrabkey_probe" | sed 's/^/  /'
else
    say "  gcc or libX11 headers missing; xgrabkey_probe not run"
fi

say ""
say "== normal-window / command-surface fallback (by architecture) =="
say "  ARCHITECTURE.md: 'A normal application window and command surface are"
say "  always valid entry points.' (no tray/global shortcut required)"
say "  apps/kestrel/src/main.rs currently prints the scaffold capability only;"
say "  a GTK4/libadwaita command-surface window is a Phase 1 UI deliverable, so"
say "  the normal-window entry point is by-design Supported but not yet implemented."
