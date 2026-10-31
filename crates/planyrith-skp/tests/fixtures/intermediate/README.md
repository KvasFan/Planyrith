# Intermediate-version and feature probes

These small files come from the MIT-licensed repository
<https://github.com/iamahsanmehmood/openskp/tree/c0bbeaa62191d776ea25e979555c5d6c464d54fa/packages/python/tests/fixtures>.
`style_watermark.skp` comes from `packages/cpp/tests/fixtures` at the same revision.
The copyright notice and grant are in `LICENSE` beside the files.
`tools/fetch-skp-fixtures.sh` reproduces downloads and verifies their hashes.
No runtime dependency on that project's implementation languages is added.

| Files | Regression |
|---|---|
| `nesting_2013.skp` | 3 definitions, 3 faces; nested instances without trailing instance GUIDs. |
| `legacy_v2014_synthetic.skp` | 4 definitions, 4 faces; pre-2017 entity preambles and definition prefix. |
| `sdk_saved_2019.skp`, `sdk_saved_2020.skp` | Positive import: 4 vertices, 4 edges, one 254 mm square; formerly empty decoder fallback. |
| `sdk_saved_sections_2019.skp`, `sdk_saved_sections_2020.skp` | Same square with additional saved file sections/settings; these are not section-plane entities. |
| `arcs_2017.skp`, `arcs_2020.skp`, `arcs_2021.skp` | 2 root faces and 3 analytic curve records. |
| `nesting_2020.skp`, `nesting_2021.skp` | 3 definitions, nested hierarchy and shared geometry. |
| `legacy_v2019_synthetic.skp` | 4 definitions, 4 faces, 16 vertices. |
| `legacy_v2020_textured.skp` | One textured face; native material/image retention. |
| `legacy_text_arrows_2017.skp` | 3 leader texts with different arrow codes and a font with a multibyte persistent id. |
| `style_watermark.skp` | Saved styles with embedded watermark images. |
| `zero_material_custom_layer_v18.skp` | 2018 layout model (`{18.0.16975}`, strings redacted by the source): a custom tag listed ahead of the default layer, an overlong root list, two guides and no faces. |
| `capilla_quiroz_v17.skp` | Real 2017 model: leader text attached to a roof face (anchor kind 7). |
| `Untitled.skp` | `{25.0.575}` model whose faces hold self-crossing rings and crossing holes. |
| `nesting_v6.skp` | A release before the 2013 minimum, refused by name. |

The SDK-saved cases are named by their author. The files named `*_2021.skp`
select the 2021+ container family; their actual header is `{25.0.575}`. Generated
cases are not certification against a retail application. Source restoration
and editable data checks supplement topology counts. Files saved by real 2018,
2021 and 2022 releases are in `../release/`.
