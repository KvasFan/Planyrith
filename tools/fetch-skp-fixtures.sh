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
for name in sdk_saved_2019.skp sdk_saved_2020.skp arcs_2021.skp nesting_2021.skp \
  arcs_2017.skp arcs_2020.skp nesting_2020.skp sdk_saved_sections_2019.skp sdk_saved_sections_2020.skp legacy_v2019_synthetic.skp legacy_v2020_textured.skp legacy_text_arrows_2017.skp; do
  curl --fail --location --silent --show-error \
    "https://raw.githubusercontent.com/iamahsanmehmood/openskp/$revision/packages/python/tests/fixtures/$name" \
    --output "$out/intermediate/$name"
done
curl --fail --location --silent --show-error \
  "https://raw.githubusercontent.com/iamahsanmehmood/openskp/$revision/packages/cpp/tests/fixtures/style_watermark.skp" \
  --output "$out/intermediate/style_watermark.skp"
curl --fail --location --silent --show-error \
  "https://raw.githubusercontent.com/iamahsanmehmood/openskp/$revision/LICENSE" \
  --output "$out/intermediate/LICENSE"
cd "$out/intermediate"
sha256sum --check <<'HASHES'
5e7e538e15e8b7d3032adcfe83c3ed8ca085d3a1475247e1604d70b68ae41b32  arcs_2021.skp
083eda2e4da943addc93cacc383056195a0d93f68e3ce1252afddcb914ccfbb2  nesting_2021.skp
919210cb1b04c4c20c8eb13983056cc997d2df4e6ea3691835978eca002a8bcf  sdk_saved_2019.skp
a06f9441f372ff1fa3584de759b631b96dac7cbd27c66a91ee50240c800bd81e  sdk_saved_2020.skp
1f51510b75d35e6de9441f145e6372883a28d430143666ed7d14d6a75cdc783f  arcs_2017.skp
f8422dadfa4f5b1dad014eb73113295dc5eb357fb9139f06862b0beafcf8c00b  arcs_2020.skp
34dc6e2cc20b9ea0303b17c9944292573875dda2b341f4d56998ad3998e8ba89  nesting_2020.skp
0831dd4bd8c2fef8ec0c27f0c859a8e115ad765e1743666ac6e9c0125f3f3918  sdk_saved_sections_2019.skp
830bc1bfaadc9ed8553805c21e25e67fe4eb79883d90e540295b116dd7860bd6  sdk_saved_sections_2020.skp
b95db425794b4c3a874b20755818ce098427a0425d4415e72d2324910ff5117d  legacy_v2019_synthetic.skp
c8e99018aad18dea12c90d407435c54d0a15a9610b6e66ff2952c4eeeb9bef45  legacy_v2020_textured.skp
45f5d45d53dd5925f68f81cbd2458658e23ee2630085fd9a9f1dc23b6a420311  legacy_text_arrows_2017.skp
470d540432636acb5a35184393f9bed1c5e246037532da701209340d847bc728  style_watermark.skp
7c70ac0b7916390744a5f6361c59008cada08149a54348530b767953e050dbc6  LICENSE
HASHES
