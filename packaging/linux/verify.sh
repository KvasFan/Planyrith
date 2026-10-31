#!/usr/bin/env bash
# Validate payloads from their extracted installed locations, not the build tree.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="${1:-$root/dist/release}"
fixture="$root/crates/planyrith-skp/tests/fixtures/legacy/box-2013.skp"
mkdir -p "$out/verify"
for kind in deb arch portable; do
  dest="$out/verify/$kind"
  mkdir -p "$dest"
  case "$kind" in
    deb) dpkg-deb -x "$out"/*.deb "$dest"; bins="$dest/usr/bin" ;;
    arch) tar --zstd -xf "$out"/*.pkg.tar.zst -C "$dest"; bins="$dest/usr/bin" ;;
    portable) tar -xzf "$out"/*-linux-x86_64.tar.gz -C "$dest"; bins="$dest/bin" ;;
  esac
  test -x "$bins/planyrith-gui"
  test -x "$bins/planyrith-skp"
  "$bins/planyrith" --json -f "$fixture" info > "$dest/info.json"
  jq -e '.ok and .output.problems == 0 and .output.counts.model.faces == 6' "$dest/info.json"
  "$bins/planyrith" -f "$fixture" -o "$dest/working.plyr" tool select --query all
  "$bins/planyrith" -f "$dest/working.plyr" validate
  "$bins/planyrith" ext list > "$dest/extensions.txt"
done
appimage=("$out"/*.AppImage)
"${appimage[0]}" --appimage-extract-and-run --cli --json -f "$fixture" info > "$out/verify/appimage-info.json"
jq -e '.ok and .output.problems == 0 and .output.counts.model.faces == 6' "$out/verify/appimage-info.json"
desktop-file-validate "$out/deb-root/usr/share/applications/org.planyrith.Planyrith.desktop"
