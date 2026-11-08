#!/usr/bin/env bash

set -euo pipefail

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
version="${1:-0.1.0}"
architecture="${ARCH:-x86_64}"
build_dir="$root_dir/build/appimage"
appdir="$build_dir/AppDir"
tools_dir="$build_dir/tools"
dist_dir="$root_dir/dist"
linuxdeploy="$tools_dir/linuxdeploy-${architecture}.AppImage"
gtk_plugin="$tools_dir/linuxdeploy-plugin-gtk.sh"
plugin_revision="3b67a1d1c1b0c8268f57f2bce40fe2d33d409cea"
desktop_file="$root_dir/data/io.github.kamleshkc2002.Kestrel.desktop"
icon_file="$root_dir/data/icons/io.github.kamleshkc2002.Kestrel.svg"
artifact="$dist_dir/Kestrel-${version}-${architecture}.AppImage"

case "$architecture" in
  x86_64|aarch64) ;;
  *)
    printf 'unsupported AppImage architecture: %s\n' "$architecture" >&2
    exit 1
    ;;
esac

rm -rf "$build_dir"
mkdir -p "$appdir" "$tools_dir" "$dist_dir"
rm -f "$artifact" "$artifact.sha256"

KESTREL_LAYER_SHELL=required cargo build --locked --release --package kestrel --package kestrel-record

curl --fail --location --retry 3 \
  --output "$linuxdeploy" \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${architecture}.AppImage"
curl --fail --location --retry 3 \
  --output "$gtk_plugin" \
  "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/${plugin_revision}/linuxdeploy-plugin-gtk.sh"
chmod +x "$linuxdeploy" "$gtk_plugin"

# Keep GStreamer host plugins matched to the core library; report missing plugins from the host registry.
# linuxdeploy inspects only kestrel first; the helper joins the AppDir
# afterwards, so both its scans exclude libgst and the helper loads the host
# runtime at run time.
env DEPLOY_GTK_VERSION=4 \
  "$linuxdeploy" --appimage-extract-and-run \
  --appdir "$appdir" \
  --executable "$root_dir/target/release/kestrel" \
  --desktop-file "$desktop_file" \
  --icon-file "$icon_file" \
  --exclude-library 'libgst*' \
  --plugin gtk
if ! find "$appdir/usr/lib" -name 'libgtk4-layer-shell.so.0*' -print -quit | grep -q .; then
  printf 'linuxdeploy did not bundle libgtk4-layer-shell.so.0\n' >&2
  exit 1
fi

# The helper must sit beside kestrel so current_exe finds it.
install -Dm755 "$root_dir/target/release/kestrel-record" "$appdir/usr/bin/kestrel-record"

# Libadwaita owns its stylesheet; GTK selects the user's Wayland or X11 session.
sed -i '/^export GTK_THEME=/d; /^export GDK_BACKEND=/d' \
  "$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
cat >>"$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh" <<'EOF'

# Use GTK's built-in input-method fallback when the bundled module is absent.
if [ -n "${GTK_IM_MODULE:-}" ] &&
  ! find "$APPDIR/usr/lib/gtk-4.0" "$APPDIR/usr/local/lib/gtk-4.0" \
      -type f -name "libim-${GTK_IM_MODULE}.so" -print -quit 2>/dev/null |
    grep -q .; then
  unset GTK_IM_MODULE
fi
EOF
# Keep the media backend out of the bundle; the module can arrive under the
# distro layout or the source stack's prefix.
find "$appdir/usr/lib/gtk-4.0" "$appdir/usr/local/lib/gtk-4.0" \
  -name 'libmedia-gstreamer.so' -delete 2>/dev/null || true

# The GTK plugin resolves the source-built stack's module directory relative
# to the distro libdir, which lands in local/lib. Move the modules into the
# AppDir's lib directory: linuxdeploy deploys their dependencies from there,
# and the hook's GTK_PATH then points at the usual place.
if [ -d "$appdir/local/lib/gtk-4.0" ]; then
  mkdir -p "$appdir/usr/lib"
  cp -a "$appdir/local/lib/gtk-4.0/." "$appdir/usr/lib/gtk-4.0/"
  rm -rf "$appdir/local"
  sed -i 's|^export GTK_PATH=.*|export GTK_PATH="$APPDIR/usr/lib/gtk-4.0"|' \
    "$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
fi

env ARCH="$architecture" VERSION="$version" \
  "$linuxdeploy" --appimage-extract-and-run \
  --appdir "$appdir" \
  --exclude-library 'libgst*' \
  --output appimage

generated="$(find "$root_dir" -maxdepth 1 -type f -name '*.AppImage' -print -quit)"
if [ -z "$generated" ]; then
  printf 'linuxdeploy did not produce an AppImage\n' >&2
  exit 1
fi
mv "$generated" "$artifact"
chmod +x "$artifact"
(
  cd "$dist_dir"
  sha256sum "$(basename "$artifact")" >"$(basename "$artifact").sha256"
)

printf '%s\n' "$artifact"
