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

# Independently authored SDK-saved cases and generated examples declaring a 2025 header (MIT).
revision=c0bbeaa62191d776ea25e979555c5d6c464d54fa
mkdir -p "$out/intermediate"
for name in sdk_saved_2019.skp sdk_saved_2020.skp arcs_2021.skp nesting_2021.skp; do
  curl --fail --location --silent --show-error \
    "https://raw.githubusercontent.com/iamahsanmehmood/openskp/$revision/packages/python/tests/fixtures/$name" \
    --output "$out/intermediate/$name"
done
curl --fail --location --silent --show-error \
  "https://raw.githubusercontent.com/iamahsanmehmood/openskp/$revision/LICENSE" \
  --output "$out/intermediate/LICENSE"
cd "$out/intermediate"
sha256sum --check <<'HASHES'
5e7e538e15e8b7d3032adcfe83c3ed8ca085d3a1475247e1604d70b68ae41b32  arcs_2021.skp
083eda2e4da943addc93cacc383056195a0d93f68e3ce1252afddcb914ccfbb2  nesting_2021.skp
919210cb1b04c4c20c8eb13983056cc997d2df4e6ea3691835978eca002a8bcf  sdk_saved_2019.skp
a06f9441f372ff1fa3584de759b631b96dac7cbd27c66a91ee50240c800bd81e  sdk_saved_2020.skp
7c70ac0b7916390744a5f6361c59008cada08149a54348530b767953e050dbc6  LICENSE
HASHES
