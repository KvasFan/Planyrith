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
mkdir -p "$out/legacy"
for version in 2013 2014 2015 2016; do
  curl --fail --location --silent --show-error \
    "https://raw.githubusercontent.com/hew3d/openskp/$revision/corpus/legacy/box-v$version.skp" \
    --output "$out/legacy/box-$version.skp"
done
cd "$out"
sha256sum --check <<'HASHES'
1040c55ccd79b044070d9ae5945ac52238665eca30ca1f8cd5f5ffdae09253cd  box-2017.skp
82280b5a11184d06f38a8592fe1683f16e68ee739ebfbef43ad7f9bab64ada3f  box-2026.skp
b2bc194ccee1bba12cf2a4371a4f2ef42f90ad81b55fd294feb71a46fb33f83a  legacy/box-2013.skp
72f41969ff4e08cf68e5d43c0fdaa7480c8c342fb94d418e0d334fbdcc293de4  legacy/box-2014.skp
fb9bc7147754dd4bc472d618c6b04c37b05739f7784816397ff0dfb49ddcbef3  legacy/box-2015.skp
4e055123353b77d3e75d10023939f50811055f5a0bc1dfcc9a3231f8311278d9  legacy/box-2016.skp
HASHES

# Independently authored SDK-saved cases and generated examples declaring a 2025 header (MIT).
revision=c0bbeaa62191d776ea25e979555c5d6c464d54fa
mkdir -p "$out/intermediate"
for name in sdk_saved_2019.skp sdk_saved_2020.skp arcs_2021.skp nesting_2021.skp nesting_2013.skp legacy_v2014_synthetic.skp \
  arcs_2017.skp arcs_2020.skp nesting_2020.skp sdk_saved_sections_2019.skp sdk_saved_sections_2020.skp legacy_v2019_synthetic.skp legacy_v2020_textured.skp legacy_text_arrows_2017.skp \
  zero_material_custom_layer_v18.skp capilla_quiroz_v17.skp Untitled.skp nesting_v6.skp; do
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
70ec9be73f20da95384910aa58ba1537dc1600b027cac65137cb2469046e5fc5  nesting_2013.skp
63964a5fe8337812aa682f209fa92b8b835101be704dddbd092c347ae4fbebcb  legacy_v2014_synthetic.skp
48c85a5b02bd03767f0bc7ef1c2fee907d355698b0d5bb572ff4e41cb3926585  zero_material_custom_layer_v18.skp
5974c5f416ae32aaf12093b2808bdeff662efed7ee85565bab3caf7ddbd9f98e  capilla_quiroz_v17.skp
382d66f54c59f82a176d10c535b0731653bf3949a3057121cb32bac3799c7aff  Untitled.skp
85a506d3e970233806ef1ecf2decfa6e8a461004cce0845a4e4fef58ccdab2a8  nesting_v6.skp
HASHES

# Models saved by the releases themselves, from MIT-licensed repositories.
# The test suite is addressed by repository id and git blob (content), so
# its files need no path; see tests/fixtures/release/README.md.
mkdir -p "$out/release"
blob() {
  curl --fail --location --silent --show-error \
    --header 'Accept: application/vnd.github.raw' \
    "https://api.github.com/repositories/15951247/git/blobs/$1" --output "$out/release/$2"
}
blob a0e7f4fa1cbd8da316050297abf10b513627abf0 skp-vff-soft-block.skp
blob 141b5b4c72504a09b67dfb35b6d6f0aaee828dc9 skp-vff-hard-block.skp
blob cc3528a2878a877e426b3331596d97e9784a76c4 LiveComponent.skp
blob 49f85895e28098f18a738c134475b77e40f073fd missing_material.skp
blob 76281e755cf6447dd7dd0b7dbd12bb8bcdb9a497 uv_tile_at.skp
blob 32ad38d532b79da8b83d29d7e284968be1bf697b LICENSE-test-suite
raw() {
  curl --fail --location --silent --show-error "https://raw.githubusercontent.com/$1" --output "$out/release/$2"
}
quad=thomthom/quadface-tools/481e6951e1a9da67a1aaa6bffd4768a7818f5a09
raw "$quad/Test%20Models/HexCrater.skp" HexCrater.skp
raw "$quad/LICENSE" LICENSE-quad-tools
solid=thomthom/solid-inspector/e238257f3a5c27f7c611fe2515f7d88a9d0d86b5
models="Tests/Solid%20Inspector%202/TC_ErrorFinder"
raw "$solid/$models/ReversedFaces-SmallModel.skp" ReversedFaces-SmallModel.skp
raw "$solid/$models/PrusaI3_Extruder2.skp" PrusaI3_Extruder2.skp
raw "$solid/$models/box.skp" degenerate-uv-box.skp
raw "$solid/LICENSE" LICENSE-solid-tools
cd "$out/release"
sha256sum --check <<'HASHES'
640f57790551b48f35bc2a7931240aa64606e4ac8e1ff64c663c880891fca097  skp-vff-soft-block.skp
83c3f1dfdba0f0501311c059b82a873fff1e6ea95bdc49f0eab43dc58d7601f6  skp-vff-hard-block.skp
abf406909fb5656af588c9571fa1ef3c17fbbbeb1636e577aed469e4e3e12b18  LiveComponent.skp
c53614901ee0d06fb1a50920034daf44c1c1a3c6f830ed4357c91bae132c7981  missing_material.skp
928587de98a8bb8b5d5b53544863a8e6cf22a1f96ab0659f5e975c87fc277dc0  uv_tile_at.skp
1cd5970550df665cf355de497f32882b5aa1d7b7c72092d45f70a5b731c12c83  LICENSE-test-suite
fa54fd93935b8e716d627186b80e24bd971e8d9eeefe71d17aa26c3f1d7d88f2  HexCrater.skp
4b72d81a9196bf4681545dfd6aafb499ad70aad4b28cafc8ca791a8fecf65671  LICENSE-quad-tools
b615e73d9790e294b1ca917112b1d10ef3a5fc15083d985d6d0d05f6fd99a948  ReversedFaces-SmallModel.skp
9922548164b5d6c826005f6cba82640873f01b6e2f50aa63079cff1e878047af  PrusaI3_Extruder2.skp
2e5c112d64e7e362d6cd82ff1592e19b036982937288a173fc02db63a1e1bc60  degenerate-uv-box.skp
f1d7178decba08819ac5447fd80dc4f5a20c41838c74153c68528afc089ab022  LICENSE-solid-tools
HASHES
