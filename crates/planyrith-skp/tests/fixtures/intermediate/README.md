# Intermediate-version probes

These four files are from the MIT-licensed repository
<https://github.com/iamahsanmehmood/openskp/tree/c0bbeaa62191d776ea25e979555c5d6c464d54fa/packages/python/tests/fixtures>.
The original copyright notice and grant are in `LICENSE` beside the files.
`tools/fetch-skp-fixtures.sh` reproduces the downloads and checks their hashes.
No runtime dependency on that project or its implementation languages is added.

| File | Current regression |
|---|---|
| `sdk_saved_2019.skp` | Decoder fails to recover geometry; opening must fail explicitly. |
| `sdk_saved_2020.skp` | Decoder fails to recover geometry; opening must fail explicitly. |
| `arcs_2021.skp` | 2 root faces; geometry round-trips through both writers; unchanged source is byte-exact. |
| `nesting_2021.skp` | 3 definitions; nested hierarchy round-trips; unchanged source is byte-exact. |

The first two are SDK-saved cases named by their author. The files named `*_2021.skp` select the 2021+ container family; their actual
version header is `{25.0.575}`. They are generated cases, not certification
against the original application. These
probes expose actual coverage; they do not establish complete support for a
release. Replace the expected refusals with positive topology, metadata and
editing checks when a suitable Rust decoder is implemented.
