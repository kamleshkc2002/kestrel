#!/usr/bin/env bash
# Publishes the GitHub release for a tag from the signed files in dist/.
# Usage: publish-release.sh vX.Y.Z[-suffix]
# A version with a suffix (0.9.0-preview.1, 1.0.0-rc.2) is a prerelease; a
# plain version (1.0.0) is a stable release and becomes the latest one.
set -euo pipefail

tag="${1:?usage: publish-release.sh <tag>}"
version="${tag#v}"
case "$version" in
  *-*) channel=(--prerelease) ;;
  *) channel=(--latest) ;;
esac

gh release create "$tag" \
  dist/*.AppImage dist/*.sha256 dist/*.minisig \
  --repo "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}" \
  --title "Kestrel $version" \
  --generate-notes \
  "${channel[@]}" \
  --verify-tag
