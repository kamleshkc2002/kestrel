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

cargo build --locked --release --package kestrel

curl --fail --location --retry 3 \
  --output "$linuxdeploy" \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${architecture}.AppImage"
curl --fail --location --retry 3 \
  --output "$gtk_plugin" \
  "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/${plugin_revision}/linuxdeploy-plugin-gtk.sh"
chmod +x "$linuxdeploy" "$gtk_plugin"

env DEPLOY_GTK_VERSION=4 \
  "$linuxdeploy" --appimage-extract-and-run \
  --appdir "$appdir" \
  --executable "$root_dir/target/release/kestrel" \
  --desktop-file "$desktop_file" \
  --icon-file "$icon_file" \
  --plugin gtk

# The GTK plugin carries GTK3-era overrides. Libadwaita must own its stylesheet,
# and GTK should select Wayland or X11 from the user's current session.
sed -i '/^export GTK_THEME=/d; /^export GDK_BACKEND=/d' \
  "$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
find "$appdir/usr/lib/gtk-4.0" -name 'libmedia-gstreamer.so' -delete 2>/dev/null || true

env ARCH="$architecture" VERSION="$version" \
  "$linuxdeploy" --appimage-extract-and-run \
  --appdir "$appdir" \
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
