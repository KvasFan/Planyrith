#!/usr/bin/env bash
# Corresponding source and relinking inputs for the optional AppImage runtime.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/dist/release}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
stage="$out/appimage-runtime-sources"
mkdir -p "$stage"
curl --fail --location --silent --show-error \
  https://codeload.github.com/AppImage/type2-runtime/tar.gz/fedfad51acc2860b2916a1eef32ba4a500ff31ef \
  --output "$stage/type2-runtime-fedfad51.tar.gz"
curl --fail --location --silent --show-error \
  https://github.com/libfuse/libfuse/releases/download/fuse-3.15.0/fuse-3.15.0.tar.xz \
  --output "$stage/fuse-3.15.0.tar.xz"
curl --fail --location --silent --show-error \
  https://codeload.github.com/vasi/squashfuse/tar.gz/refs/tags/0.5.2 \
  --output "$stage/squashfuse-0.5.2.tar.gz"
cd "$stage"
sha256sum --check <<'HASHES'
70589cfd5e1cff7ccd6ac91c86c01be340b227285c5e200baa284e401eea2ca0  fuse-3.15.0.tar.xz
db0238c5981dabbd80ee09ae15387f390091668ca060a7bc38047912491443d3  squashfuse-0.5.2.tar.gz
HASHES
cp "$root/packaging/appimage/SOURCE-README.txt" README.txt
sha256sum ./*.tar.* > SHA256SUMS
tar -czf "$out/planyrith-appimage-runtime-source.tar.gz" -C "$out" appimage-runtime-sources
