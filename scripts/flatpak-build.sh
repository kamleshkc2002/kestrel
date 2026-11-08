#!/usr/bin/env bash
# Builds the Flatpak bundle from this checkout.
#
# The committed manifest names the tagged release as its source, so it is the
# manifest Flathub builds. This script rewrites that source to a `dir` source
# pointing at the checkout, so a pull request and a local build compile their
# own tree. Output goes to build/flatpak/ and dist/.
#
# Usage: flatpak-build.sh [--install]
#   --install adds the bundle to the user installation after building.
#
# Needs flatpak, flatpak-builder (or the org.flatpak.Builder application), and
# jq. The Flathub builder container used by CI provides all of them.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/packaging/flatpak/io.github.kamleshkc2002.Kestrel.yml"
work="$root/build/flatpak"
app_id=io.github.kamleshkc2002.Kestrel

install=false
if [ "${1:-}" = "--install" ]; then
  install=true
elif [ "$#" -gt 0 ]; then
  printf 'usage: %s [--install]\n' "$0" >&2
  exit 2
fi

if command -v flatpak-builder >/dev/null 2>&1; then
  builder=(flatpak-builder)
elif flatpak info org.flatpak.Builder >/dev/null 2>&1; then
  # The Builder application runs flatpak on the host; without the user
  # installation path it looks in its own private data directory instead.
  user_dir="${XDG_DATA_HOME:-$HOME/.local/share}/flatpak"
  builder=(flatpak run --env="FLATPAK_USER_DIR=$user_dir" \
    --command=flatpak-builder org.flatpak.Builder)
else
  printf 'flatpak-build needs flatpak-builder or the org.flatpak.Builder application\n' >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  printf 'flatpak-build needs jq\n' >&2
  exit 1
fi

version="$(grep -m1 '^version = ' "$root/apps/kestrel/Cargo.toml" | cut -d '"' -f 2)"
arch="$(flatpak --default-arch)"
bundle="$root/dist/Kestrel-$version-$arch.flatpak"

mkdir -p "$work" "$root/dist"

# Canonical JSON avoids parsing the YAML a second time.
"${builder[@]}" --show-manifest "$manifest" >"$work/manifest.json"
jq --arg path "$root" '
  .modules |= map(
    if .name == "kestrel" then
      .sources |= map(
        if (.type == "git") and ((.url // "") | test("kamleshkc2002/kestrel\\.git$"))
        then {
          "type": "dir",
          "path": $path,
          "skip": ["target", "build", "dist", "squashfs-root", ".git"]
        }
        else . end
      )
    else . end
  )
' "$work/manifest.json" >"$work/manifest-checkout.json"

# The Flathub builder container has no machine-id, so rofiles-fuse cannot
# start there; the build cache works without it.
"${builder[@]}" \
  --force-clean \
  --disable-rofiles-fuse \
  --state-dir="$work/state" \
  --repo="$work/repo" \
  "$work/app" \
  "$work/manifest-checkout.json"

flatpak build-bundle "$work/repo" "$bundle" "$app_id" master
printf 'built %s\n' "${bundle#"$root"/}"

if [ "$install" = true ]; then
  flatpak install --user --noninteractive "$bundle"
fi
