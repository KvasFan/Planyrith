#!/usr/bin/env bash
# docs/11 §4: builds the web application into a folder of static files and
# checks the size budget (≤ 12 MB with brotli, docs/13 0.7).
#   1. cargo builds `planyrith-gui` for wasm32-unknown-unknown (release);
#   2. wasm-bindgen (the CLI matching the `wasm-bindgen` crate in Cargo.lock)
#      writes the JavaScript glue;
#   3. wasm-opt shrinks the module when it is installed (optional);
#   4. the page and the service worker (both stamped with the build) and
#      the manifest are copied next to them.
# Serve the folder with any static file server, e.g.
# `python3 -m http.server -d dist/web`.
# Usage: tools/build-web.sh [out-dir]   (default: dist/web)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/dist/web}"
budget=$((12 * 1024 * 1024))

if ! command -v wasm-bindgen >/dev/null; then
  version="$(grep -A1 '^name = "wasm-bindgen"$' "$root/Cargo.lock" | sed -n 's/^version = "\(.*\)"$/\1/p')"
  echo "wasm-bindgen is missing: cargo install wasm-bindgen-cli --version $version" >&2
  exit 1
fi

cargo build --manifest-path "$root/Cargo.toml" --release -p planyrith-gui \
  --bin planyrith-gui --target wasm32-unknown-unknown
mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "$root/target/wasm32-unknown-unknown/release/planyrith-gui.wasm"
if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int \
    --enable-sign-ext --enable-mutable-globals --enable-reference-types \
    -o "$out/planyrith-gui_bg.wasm" "$out/planyrith-gui_bg.wasm"
fi
web="$root/crates/planyrith-gui/web"
cp "$web/manifest.webmanifest" "$web/icon.svg" "$out/"
build="$(sha256sum "$out/planyrith-gui_bg.wasm" | cut -c1-16)"
sed "s/@BUILD@/$build/" "$web/sw.js" >"$out/sw.js"
sed "s/@BUILD@/$build/" "$web/index.html" >"$out/index.html"

# The size budget, compressed as a server would send it.
total=0
for f in "$out"/*; do
  size="$(brotli -c -q 11 "$f" | wc -c)"
  printf '%10d  %s\n' "$size" "$(basename "$f")"
  total=$((total + size))
done
printf '%10d  total with brotli (budget %d)\n' "$total" "$budget"
if [ "$total" -gt "$budget" ]; then
  echo "the web build is over its size budget" >&2
  exit 1
fi
