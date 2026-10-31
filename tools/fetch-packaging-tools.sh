#!/usr/bin/env bash
# Pinned deployment tools; none are linked into the Rust applications.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/target/packaging-tools}"
mkdir -p "$out"
curl --fail --location --silent --show-error \
  https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
  --output "$out/appimagetool.AppImage"
curl --fail --location --silent --show-error \
  https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64 \
  --output "$out/runtime-x86_64"
cd "$out"
sha256sum --check <<'HASHES'
ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0  appimagetool.AppImage
617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46  runtime-x86_64
HASHES
curl --fail --location --silent --show-error \
  https://raw.githubusercontent.com/AppImage/type2-runtime/fedfad51acc2860b2916a1eef32ba4a500ff31ef/LICENSE \
  --output RUNTIME-LICENSE.txt
# Retain full notices for the loader's static dependencies in the payload.
for specification in \
  'libfuse|https://raw.githubusercontent.com/libfuse/libfuse/fuse-3.15.0/LGPL2.txt' \
  'squashfuse|https://raw.githubusercontent.com/vasi/squashfuse/0.5.2/LICENSE' \
  'musl|https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT?h=v1.2.5' \
  'zstd|https://raw.githubusercontent.com/facebook/zstd/v1.5.6/LICENSE' \
  'zlib|https://raw.githubusercontent.com/madler/zlib/v1.3.1/LICENSE' \
  'mimalloc|https://raw.githubusercontent.com/microsoft/mimalloc/v2.1.7/LICENSE'; do
  printf '\n\n--- %s ---\n' "${specification%%|*}" >> RUNTIME-LICENSE.txt
  curl --fail --location --silent --show-error "${specification#*|}" >> RUNTIME-LICENSE.txt
done
chmod +x appimagetool.AppImage runtime-x86_64
