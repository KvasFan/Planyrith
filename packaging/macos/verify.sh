#!/usr/bin/env bash
# Mounts the disk image and exercises the bundled programs: both CPU
# architectures, the property list, the format module, a native save and
# its validation (docs/16 §3).
# Usage: packaging/macos/verify.sh DMG FIXTURE VERSION
set -euo pipefail
dmg="$1" fixture="$2" version="$3"
mount="$(mktemp -d)"
hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$dmg"
trap 'hdiutil detach "$mount" -quiet || true' EXIT
app="$mount/Planyrith.app"
test -L "$mount/Applications"
plutil -lint "$app/Contents/Info.plist"
for program in planyrith planyrith-gui planyrith-skp; do
  test -x "$app/Contents/MacOS/$program"
  archs="$(lipo -archs "$app/Contents/MacOS/$program")"
  [[ "$archs" == *arm64* && "$archs" == *x86_64* ]] || { echo "$program: $archs" >&2; exit 1; }
done
cli="$app/Contents/MacOS/planyrith"
"$cli" --version | grep -F "$version"
"$cli" --json -f "$fixture" info | python3 -I -c '
import json, sys
r = json.load(sys.stdin)
assert r["ok"] and r["output"]["problems"] == 0 and r["output"]["counts"]["model"]["faces"] == 6, r
'
native="$(mktemp -d)/smoke.plyr"
"$cli" -f "$fixture" -o "$native" tool select --query all
"$cli" -f "$native" validate
