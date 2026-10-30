#!/usr/bin/env bash
# Reproduce the externally authored SKP fixtures of docs/09 §5.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/crates/planyrith-skp/tests/fixtures"
revision=a6788afa6386cfd4bc781998dca99c591b3fb069
mkdir -p "$out"
for version in 2017 2026; do
  curl --fail --location --silent --show-error \
    "https://raw.githubusercontent.com/hew3d/openskp/$revision/corpus/$version/blank-template-box.skp" \
    --output "$out/box-$version.skp"
done
cd "$out"
sha256sum --check <<'HASHES'
1040c55ccd79b044070d9ae5945ac52238665eca30ca1f8cd5f5ffdae09253cd  box-2017.skp
82280b5a11184d06f38a8592fe1683f16e68ee739ebfbef43ad7f9bab64ada3f  box-2026.skp
HASHES
