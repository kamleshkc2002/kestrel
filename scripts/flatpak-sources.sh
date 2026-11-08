#!/usr/bin/env bash
# Regenerates packaging/flatpak/cargo-sources.json from Cargo.lock with
# flatpak-builder-tools' cargo generator. The generator is downloaded into a
# temporary directory; this repository does not vendor it.
#
# Usage: flatpak-sources.sh [--check]
#   --check writes nothing and fails when the committed file is stale.
#
# Needs curl, git, sed, and uv (the generator declares its Python packages).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="$root/packaging/flatpak/cargo-sources.json"
generator_url="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py"

check=false
if [ "${1:-}" = "--check" ]; then
  check=true
elif [ "$#" -gt 0 ]; then
  printf 'usage: %s [--check]\n' "$0" >&2
  exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl -fsSL "$generator_url" -o "$work/generator.py"
# The tray crate uses desktop-shell sources as references; the build does not
# need them, one contains a symlink loop, and copying them would add hundreds
# of megabytes. The generator clones submodules into its cache and scans the
# whole repository, so skip the submodule checkout.
if ! grep -q 'git("submodule", "update", "--init", "--recursive")' "$work/generator.py"; then
  printf 'the cargo generator changed; review the submodule patch in %s\n' "$0" >&2
  exit 1
fi
sed -i \
  -e 's|git("submodule", "sync", "--recursive")|pass|' \
  -e 's|git("submodule", "update", "--init", "--recursive")|pass|' \
  "$work/generator.py"

# A per-run cache keeps the clone free of submodules left by an earlier run.
XDG_CACHE_HOME="$work/cache" uv run --quiet "$work/generator.py" \
  "$root/Cargo.lock" -o "$work/cargo-sources.json"

if [ "$check" = true ]; then
  if ! diff -u "$output" "$work/cargo-sources.json"; then
    printf 'packaging/flatpak/cargo-sources.json does not match Cargo.lock; run %s\n' "$0" >&2
    exit 1
  fi
else
  cp "$work/cargo-sources.json" "$output"
  printf 'wrote %s\n' "${output#"$root"/}"
fi
