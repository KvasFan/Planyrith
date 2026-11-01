#!/usr/bin/env bash
# The commands of the CI jobs (docs/12 §4). `.github/workflows/ci.yml` and
# `tools/ci-local.sh` both call this file, so the two cannot drift apart.
# Usage: tools/ci-steps.sh <step>...
set -euo pipefail

cd "$(dirname "$0")/.."
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
export CARGO_INCREMENTAL=0
export RUSTFLAGS="${RUSTFLAGS--D warnings}"

boundaries() {
  local fail=0 forbidden deps
  for forbidden in egui eframe egui-wgpu egui_tiles winit wgpu web-sys; do
    if grep -Eq "^${forbidden}\s*=" crates/planyrith-core/Cargo.toml; then
      echo "planyrith-core must not depend on ${forbidden}"; fail=1
    fi
  done
  for forbidden in egui eframe egui-wgpu egui_tiles winit; do
    if grep -Eq "^${forbidden}\s*=" crates/planyrith-render/Cargo.toml; then
      echo "planyrith-render must not depend on ${forbidden}"; fail=1
    fi
  done
  for forbidden in egui eframe egui-wgpu egui_tiles wgpu; do
    if grep -Eq "^${forbidden}\s*=" crates/planyrith-cli/Cargo.toml; then
      echo "planyrith-cli must not depend on ${forbidden}"; fail=1
    fi
  done
  deps="$(cargo tree -p planyrith-core -p planyrith-cli -p planyrith-render -p planyrith-gui --edges normal --prefix none --format '{p}')"
  if grep -Eq '^(openskp|planyrith-skp) v' <<<"$deps"; then
    echo "optional GPL converters must never be linked into the applications"; fail=1
  fi
  return "$fail"
}

step() {
  case "$1" in
    fmt) cargo fmt --all --check ;;
    clippy) cargo clippy --workspace --all-targets --features planyrith-cli/render -- -D clippy::all ;;
    doc) RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps ;;
    boundaries) boundaries ;;
    # The optional GPL converter stays a separate process; test it on each OS.
    converter) cargo build -p planyrith-skp ;;
    test)
      PLANYRITH_REQUIRE_GPU=1 PLANYRITH_REQUIRE_SKP=1 \
        cargo test --workspace --features planyrith-cli/render
      ;;
    wasm) cargo check -p planyrith-core --target wasm32-unknown-unknown ;;
    web-clippy)
      cargo clippy -p planyrith-gui -p planyrith-render --target wasm32-unknown-unknown \
        --all-targets -- -D warnings
      ;;
    web-build) tools/build-web.sh dist/web ;;
    web-test)
      # Debug sections substantially increase wasm-bindgen's peak memory.
      CHROMEDRIVER="${CHROMEDRIVER:-${CHROMEWEBDRIVER:?}/chromedriver}" \
        CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
        CARGO_PROFILE_TEST_DEBUG=0 \
        cargo test -p planyrith-gui --target wasm32-unknown-unknown --test web
      ;;
    *) echo "unknown CI step: $1" >&2; return 2 ;;
  esac
}

for name in "$@"; do
  echo "::: $name"
  step "$name"
done
