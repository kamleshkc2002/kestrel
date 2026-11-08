#!/usr/bin/env bash
# Build the pinned GTK stack from source and install it under /usr/local.
#
# The AppImage is built on Ubuntu 22.04 so every bundled object needs no glibc
# newer than 2.35. Jammy ships GTK 4.6 and libadwaita 1.1, but the app needs
# GTK 4.10 APIs and libadwaita 1.5, so this builds the same stack the old
# Ubuntu 24.04 build bundled:
#
#   wayland 1.22.0, wayland-protocols 1.34, glib 2.80.0, GTK 4.14.5,
#   libadwaita 1.5.0, gtk4-layer-shell 1.3.0.
#
# Usage:
#   bash scripts/build-gtk-stack.sh --install-dependencies   apt packages only
#   bash scripts/build-gtk-stack.sh                          download and build
#
# In a bare Ubuntu 22.04 container, as the AppImage workflow runs it:
#   docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 bash -c \
#     'bash scripts/build-gtk-stack.sh --install-dependencies &&
#      bash scripts/build-gtk-stack.sh'
#
# Source versions and checksums are pinned here. The AppImage workflow caches
# /usr/local with a key hashing this file, so a version bump here also busts
# the cache.
set -euo pipefail

prefix="${KESTREL_STACK_PREFIX:-/usr/local}"
build_root="${KESTREL_STACK_BUILD_DIR:-/tmp/kestrel-gtk-stack}"
jobs="${KESTREL_STACK_JOBS:-$(nproc)}"
sources="$build_root/src"
builds="$build_root/build"

wayland_version=1.22.0
wayland_sha256=1540af1ea698a471c2d8e9d288332c7e0fd360c8f1d12936ebb7e7cbc2425842
wayland_protocols_version=1.34
wayland_protocols_sha256=c59b27cacd85f60baf4ee5f80df5c0d15760ead6a2432b00ab7e2e0574dcafeb
glib_version=2.80.0
glib_sha256=8228a92f92a412160b139ae68b6345bd28f24434a7b5af150ebe21ff587a561d
gtk_version=4.14.5
gtk_sha256=5547f2b9f006b133993e070b87c17804e051efda3913feaca1108fa2be41e24d
libadwaita_version=1.5.0
libadwaita_sha256=fd92287df9bb95c963654fb6e70d3e082e2bcb37b147e0e3c905567167993783
layer_shell_version=1.3.0
layer_shell_sha256=1ebb01ab14e98afd1727f68f64981c37bd23305b1f131f5667c02b94cf593192

usage() {
  printf 'usage: %s [--install-dependencies]\n' "$0" >&2
  exit 2
}

install_dependencies() {
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install --no-install-recommends -y \
    build-essential ca-certificates curl dpkg-dev file gettext git \
    libappstream-dev libcairo2-dev libcups2-dev libdrm-dev libepoxy-dev \
    libexpat1-dev libffi-dev libfontconfig1-dev libfreetype-dev \
    libfribidi-dev libgdk-pixbuf-2.0-dev libgirepository1.0-dev \
    libgraphene-1.0-dev libharfbuzz-dev libjpeg-dev libmount-dev \
    libpango1.0-dev libpcre2-dev libpng-dev libselinux1-dev libtiff-dev \
    libx11-dev libxdamage-dev libxcursor-dev libxext-dev libxfixes-dev \
    libxi-dev libxinerama-dev libxkbcommon-dev libxrandr-dev libxrender-dev \
    pkg-config python3 python3-packaging python3-pip sassc xz-utils zlib1g-dev \
    dbus desktop-file-utils fonts-dejavu-core libglib2.0-bin \
    libgstreamer-plugins-base1.0-dev libgstreamer1.0-dev libpulse-dev \
    librsvg2-dev patchelf shared-mime-info sway xauth xvfb zstd
}

fetch() {
  local name="$1" url="$2" sha256="$3"
  if [ ! -f "$sources/$name" ]; then
    curl --fail --location --retry 3 --silent --show-error \
      --output "$sources/$name" "$url"
  fi
  if ! printf '%s  %s\n' "$sha256" "$sources/$name" | sha256sum --check --status; then
    printf 'checksum mismatch for %s\n' "$name" >&2
    rm -f "$sources/$name"
    exit 1
  fi
}

build_meson() {
  local name="$1" source="$2"
  shift 2
  rm -rf "${builds:?}/$name"
  meson setup "$builds/$name" "$source" \
    --prefix="$prefix" --libdir=lib --buildtype=release --wrap-mode=nodownload \
    "$@"
  meson compile -C "$builds/$name" -j "$jobs"
  meson install -C "$builds/$name" --no-rebuild
}

if [ "${1:-}" = "--install-dependencies" ]; then
  install_dependencies
  exit 0
fi
if [ "$#" -ne 0 ]; then
  usage
fi

export PATH="$prefix/bin:$PATH"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig:$prefix/share/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
# Installed tools such as glib-compile-resources run while the stack is still
# being built, before ldconfig has seen the new libraries.
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# meson >= 1.2 is required by glib 2.80; jammy's 0.61 is too old.
python3 -m pip install --no-cache-dir --disable-pip-version-check \
  'meson==1.3.2' 'ninja==1.11.1.1'

mkdir -p "$sources" "$builds"

fetch "wayland-$wayland_version.tar.xz" \
  "https://gitlab.freedesktop.org/wayland/wayland/-/releases/$wayland_version/downloads/wayland-$wayland_version.tar.xz" \
  "$wayland_sha256"
fetch "wayland-protocols-$wayland_protocols_version.tar.xz" \
  "https://gitlab.freedesktop.org/wayland/wayland-protocols/-/releases/$wayland_protocols_version/downloads/wayland-protocols-$wayland_protocols_version.tar.xz" \
  "$wayland_protocols_sha256"
fetch "glib-$glib_version.tar.xz" \
  "https://download.gnome.org/sources/glib/${glib_version%.*}/glib-$glib_version.tar.xz" \
  "$glib_sha256"
fetch "gtk-$gtk_version.tar.xz" \
  "https://download.gnome.org/sources/gtk/${gtk_version%.*}/gtk-$gtk_version.tar.xz" \
  "$gtk_sha256"
fetch "libadwaita-$libadwaita_version.tar.xz" \
  "https://download.gnome.org/sources/libadwaita/${libadwaita_version%.*}/libadwaita-$libadwaita_version.tar.xz" \
  "$libadwaita_sha256"
fetch "gtk4-layer-shell-$layer_shell_version.tar.gz" \
  "https://github.com/wmww/gtk4-layer-shell/archive/refs/tags/v$layer_shell_version.tar.gz" \
  "$layer_shell_sha256"

for archive in "$sources"/*.tar.*; do
  case "$archive" in
    *wayland-protocols-*) dir="$sources/wayland-protocols-$wayland_protocols_version" ;;
    *gtk4-layer-shell-*) dir="$sources/gtk4-layer-shell-$layer_shell_version" ;;
    *) dir="${archive%.tar.*}" ;;
  esac
  rm -rf "$dir"
  tar --extract --no-same-owner --file "$archive" --directory "$sources"
done

# wayland itself, so gtk's wayland client and wayland-egl are new enough.
build_meson wayland "$sources/wayland-$wayland_version" \
  -Ddocumentation=false -Dtests=false -Ddtd_validation=false

build_meson wayland-protocols "$sources/wayland-protocols-$wayland_protocols_version" \
  -Dtests=false

# glib first: gtk 4.14 needs >= 2.76, jammy has 2.72.
build_meson glib "$sources/glib-$glib_version" \
  -Dtests=false -Dintrospection=disabled -Ddocumentation=false \
  -Dman-pages=disabled

# No media backends, Vulkan, cloud providers, or profiler support: the app uses
# none of them and each adds libraries to bundle. Printing stays enabled.
build_meson gtk "$sources/gtk-$gtk_version" \
  -Dmedia-gstreamer=disabled -Dvulkan=disabled -Dcloudproviders=disabled \
  -Dsysprof=disabled -Dintrospection=disabled -Ddocumentation=false \
  -Dbuild-demos=false -Dbuild-tests=false -Dbuild-testsuite=false \
  -Dbuild-examples=false -Dman-pages=false

# libadwaita 1.5 needs gtk >= 4.13.4; jammy has 4.6.
build_meson libadwaita "$sources/libadwaita-$libadwaita_version" \
  -Dintrospection=disabled -Dvapi=false -Dgtk_doc=false -Dtests=false \
  -Dexamples=false

build_meson gtk4-layer-shell "$sources/gtk4-layer-shell-$layer_shell_version" \
  -Dintrospection=false -Dvapi=false -Ddocs=false -Dexamples=false \
  -Dtests=false -Dsmoke-tests=false

# GTK installs its settings schemas as XML; compile them so a binary run
# outside the AppDir also finds them.
if [ -x "$prefix/bin/glib-compile-schemas" ] &&
  [ -d "$prefix/share/glib-2.0/schemas" ]; then
  "$prefix/bin/glib-compile-schemas" "$prefix/share/glib-2.0/schemas"
fi
if command -v ldconfig >/dev/null; then
  ldconfig
fi

printf 'Installed gtk %s, libadwaita %s, glib %s, wayland %s, wayland-protocols %s, gtk4-layer-shell %s to %s\n' \
  "$gtk_version" "$libadwaita_version" "$glib_version" "$wayland_version" \
  "$wayland_protocols_version" "$layer_shell_version" "$prefix"
