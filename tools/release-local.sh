#!/usr/bin/env bash
# Builds every release package this Linux machine can make for one tag, without
# GitHub's runners (docs/16 Decisions, 2026-11-09 and 2026-11-17): Linux
# packages in an Ubuntu 22.04 container (the glibc 2.35 baseline), the Flatpak
# on the host, Windows programs cross-built with mingw-w64 and packaged with
# NSIS (from a container) and wixl. The macOS disk image needs Apple's SDK and
# hardware and is not made here.
#
#   tools/release-local.sh v0.12.0-alpha.1            build and check
#   tools/release-local.sh v0.12.0-alpha.1 --publish  also create the release
#
# The tag must exist and tools/ci-local.sh must have passed on its commit. The
# files to upload end up in target/release-local/<tag>/publish with SHA256SUMS.
# Needs podman, wine, wixl, zip, flatpak with org.freedesktop.Sdk//26.08, the
# MinGW-w64 toolchain, and gh for --publish.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tag="${1:?usage: tools/release-local.sh TAG [--publish]}"
publish="${2:-}"
sha="$(git -C "$root" rev-parse --verify "$tag^{commit}")"
version="${tag#v}"
state="$root/target/release-local/$tag"
tree="$state/tree"
out="$tree/dist/release"
jobs="${PLANYRITH_CI_JOBS:-6}"
memory="${PLANYRITH_CI_MEMORY:-24g}"

test -e "$root/target/ci-local/passed/$sha" || {
  echo "release-local: tools/ci-local.sh has not passed on $sha" >&2
  exit 1
}

echo "::: source tree at $tag ($sha)"
if [ ! -d "$tree/.git" ]; then
  mkdir -p "$state"
  git clone -q --no-hardlinks "$root" "$tree"
fi
git -C "$tree" fetch -q "$root" "refs/tags/$tag:refs/tags/$tag"
git -C "$tree" checkout -q --detach "$sha"
git -C "$tree" clean -q -fdx -e target
actual="$(cd "$tree" && cargo metadata --locked --no-deps --format-version 1 |
  jq -r '.packages[] | select(.name == "planyrith-cli") | .version')"
test "$actual" = "$version" || {
  echo "release-local: $tag does not match version $actual" >&2
  exit 1
}

echo "::: container images"
# The toolchain layer is rebuilt when the host's stable compiler changes, so
# the packages are built with the compiler tools/ci-local.sh checked.
podman build -q -t planyrith-release-linux --build-arg "RUST=$(rustc +stable --version)" \
  -f - "$state" > /dev/null <<'IMAGE'
FROM docker.io/library/ubuntu:22.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl git jq pkg-config dpkg-dev zstd desktop-file-utils file \
    libxkbcommon-dev libwayland-dev libx11-dev libxi-dev libxrandr-dev libxcursor-dev \
    libegl1-mesa-dev libvulkan1 mesa-vulkan-drivers xz-utils squashfs-tools \
    && rm -rf /var/lib/apt/lists/*
ARG RUST
RUN echo "$RUST" && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
    --default-toolchain stable --component rustfmt,clippy,rust-src --target wasm32-unknown-unknown
ENV PATH=/root/.cargo/bin:$PATH
IMAGE
podman build -q -t planyrith-nsis -f - "$state" > /dev/null <<'IMAGE'
FROM docker.io/library/ubuntu:22.04
RUN apt-get update && apt-get install -y --no-install-recommends nsis && rm -rf /var/lib/apt/lists/*
IMAGE
cat > "$state/makensis" <<EOF
#!/usr/bin/env bash
exec podman run --rm --userns=keep-id -v "$tree:$tree" -w "\$PWD" planyrith-nsis makensis "\$@"
EOF
chmod +x "$state/makensis"

echo "::: Linux packages and source in Ubuntu 22.04"
mkdir -p "$state/cargo-registry"
podman run --rm -v "$tree:/work" -v "$state/cargo-registry:/root/.cargo/registry" -w /work \
  -e CARGO_BUILD_JOBS="$jobs" --memory="$memory" --memory-swap="$memory" \
  planyrith-release-linux bash -euc '
git config --global --add safe.directory /work
cargo build --workspace --release --locked --features planyrith-cli/render
tools/fetch-packaging-tools.sh
cargo xtask linux --appimagetool "$PWD/target/packaging-tools/appimagetool.AppImage" \
  --runtime "$PWD/target/packaging-tools/runtime-x86_64"
packaging/linux/verify.sh
cargo xtask source
tools/bundle-runtime-sources.sh
src="dist/release/planyrith-'"$version"'-source"
(cd "$src" && cargo metadata --locked --offline --no-deps --format-version 1 > /dev/null)
test -f "$src/cargo-vendor/openskp/Cargo.toml"
test -f "$src/vendor/truck-shapeops/Cargo.toml"'

echo "::: Flatpak"
(cd "$tree" && CARGO_BUILD_JOBS="$jobs" cargo xtask flatpak --bin-dir target/release)
flatpak install --user -y --noninteractive "$out/Planyrith-$version-x86_64.flatpak"
flatpak run --command=planyrith org.planyrith.Planyrith --version | grep -F "$version"
flatpak uninstall --user -y --noninteractive org.planyrith.Planyrith

echo "::: Windows programs, setup, portable archive and MSI"
win="$tree/target/x86_64-pc-windows-gnu/release"
(cd "$tree" && RUSTFLAGS="-C target-feature=+crt-static" CARGO_BUILD_JOBS="$jobs" \
  cargo build --workspace --release --locked --target x86_64-pc-windows-gnu \
  --features planyrith-cli/render)
# Only system DLLs: no MinGW runtime may be needed next to the programs.
for exe in planyrith planyrith-gui planyrith-skp; do
  dlls="$(x86_64-w64-mingw32-objdump -p "$win/$exe.exe" | awk '/DLL Name/ {print tolower($3)}')"
  if grep -E '^(libgcc|libstdc|libwinpthread)' <<< "$dlls"; then
    echo "release-local: $exe.exe needs a MinGW runtime DLL" >&2
    exit 1
  fi
done
(cd "$tree" && MAKENSIS="$state/makensis" cargo xtask windows --bin-dir "$win")
(cd "$tree" && WIXL=wixl cargo xtask msi --bin-dir "$win")
WINEDEBUG=-all WINEPREFIX="$state/wine" wine "$win/planyrith.exe" --version | grep -F "$version"

echo "::: files to publish"
files=(
  "Planyrith-$version-windows-x86_64-setup.exe"
  "planyrith-$version-windows-x86_64-portable.zip"
  "Planyrith-$version-windows-x86_64.msi"
  "Planyrith-$version-x86_64.AppImage"
  "Planyrith-$version-x86_64.flatpak"
  "planyrith_${version}_amd64.deb"
  "planyrith-$version-linux-x86_64.tar.gz"
  "planyrith-$version-source.tar.gz"
  planyrith-appimage-runtime-source.tar.gz
)
rm -rf "$state/publish"
mkdir -p "$state/publish"
for f in "${files[@]}"; do cp "$out/$f" "$state/publish/"; done
arch=("$out"/planyrith-*-x86_64.pkg.tar.zst)
test "${#arch[@]}" -eq 1
cp "${arch[0]}" "$state/publish/"
(cd "$state/publish" &&
  find . -maxdepth 1 -type f ! -name SHA256SUMS -printf '%f\0' | sort -z | xargs -0 sha256sum > SHA256SUMS &&
  sha256sum --check --quiet SHA256SUMS)
ls -la "$state/publish"

if [ "$publish" = --publish ]; then
  notes="$root/docs/releases/$tag.md"
  test -f "$notes"
  flags=()
  if [[ "$version" == *-* ]]; then flags+=(--prerelease); fi
  gh release create "$tag" --verify-tag "${flags[@]}" --title "Planyrith $version" \
    --notes-file "$notes" "$state/publish"/*
fi
echo "release-local: $tag built${publish:+ and published}"
