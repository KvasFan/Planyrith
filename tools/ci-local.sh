#!/usr/bin/env bash
# Runs a commit through CI before it reaches GitHub (docs/12 Decisions,
# 2026-11-02), and records the pass for `.githooks/pre-push`.
#
# It checks the commit, not the working tree: a clean clone shows files that were
# never committed. The Linux steps run in the image CI uses, with its software
# renderer, because most failures seen on GitHub came from that renderer and
# cannot appear on a development machine's GPU. Windows and macOS are compiled
# but not run; docs/12 lists what only GitHub can still catch.
#
# Usage: tools/ci-local.sh [commit]     (default: HEAD)
# Needs podman, cargo-deny, chromium and chromedriver. State is in target/ci-local.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
sha="$(git -C "$root" rev-parse --verify "${1:-HEAD}^{commit}")"
state="$root/target/ci-local"
tree="$state/tree"
image=planyrith-ci
mkdir -p "$state/passed" "$state/cargo" "$state/rustup" "$state/tools"
exec 9>"$state/lock"
flock 9

if [ -e "$state/passed/$sha" ]; then
  echo "ci-local: $sha already passed"
  exit 0
fi
if [ "$sha" = "$(git -C "$root" rev-parse HEAD)" ] && [ -n "$(git -C "$root" status --porcelain)" ]; then
  echo "ci-local: uncommitted changes are not part of this check"
fi

[ -d "$tree/.git" ] || git clone -q --shared --no-checkout "$root" "$tree"
git -C "$tree" checkout -q --force --detach "$sha"
git -C "$tree" clean -ffdxq -e /target

podman build -q -t "$image" -f - "$state" >/dev/null <<'IMAGE'
FROM docker.io/library/ubuntu:26.04
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl git pkg-config mesa-vulkan-drivers \
    libvulkan1 libxkbcommon0 libxkbcommon-x11-0 libx11-6 libxcursor1 libxi6 \
    libxrandr2 libwayland-client0 libegl1 libgl1 jq
IMAGE

# container [podman option... --] command...
container() {
  local options=()
  if [[ "$1" == --* ]]; then
    while [ "$1" != -- ]; do options+=("$1"); shift; done
    shift
  fi
  podman run --rm --init --userns=keep-id -w /work "${options[@]}" \
    -v "$tree:/work" -v "$state/cargo:/opt/cargo" -v "$state/rustup:/opt/rustup" \
    -e HOME=/tmp -e CARGO_HOME=/opt/cargo -e RUSTUP_HOME=/opt/rustup \
    -e PATH=/opt/cargo/bin:/usr/bin:/bin -e CI=true \
    "$image" "$@"
}

# CI tracks the current stable release, so a new one can bring new lints.
container bash -euc '
  command -v rustup >/dev/null ||
    curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain none
  rustup -q update stable --no-self-update
'
container tools/ci-steps.sh fmt clippy doc boundaries wasm web-clippy converter test-build
# GitHub's Linux runners have four cores. Races between tests appear with that
# few and stay hidden on more: 14 % of snapshot runs crashed on four, none on 16.
cores=--cpuset-cpus=0-3
container "$cores" -- tools/ci-steps.sh test snapshots

# The remaining steps use tools of this machine, and its toolchain.
export CARGO_TARGET_DIR="$state/host"
export PATH="$state/tools/bin:$PATH"
cd "$tree"
bindgen="$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"$/\1/p')"
if [ "$(wasm-bindgen --version 2>/dev/null)" != "wasm-bindgen $bindgen" ]; then
  env -u CARGO_TARGET_DIR cargo install -q --locked --force --root "$state/tools" \
    --target-dir "$state/tools/build" wasm-bindgen-cli --version "$bindgen"
fi
cargo deny check licenses bans sources
tools/ci-steps.sh web-build
CHROMEDRIVER="$(command -v chromedriver)" tools/ci-steps.sh web-test
for target in x86_64-pc-windows-msvc aarch64-apple-darwin; do
  echo "::: check $target"
  rustup target add "$target" >/dev/null 2>&1
  RUSTFLAGS="-D warnings" CARGO_INCREMENTAL=0 cargo check --workspace --all-targets \
    --features planyrith-cli/render --target "$target"
done

touch "$state/passed/$sha"
echo "ci-local: $sha passed"
