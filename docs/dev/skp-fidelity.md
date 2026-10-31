# SKP fidelity worklist

The user expanded the import minimum to 2013 on 2026-10-31 and requested direct
SKP/SKB workflows with `.plyr` as the default working format. Edited writers
currently target 2017/2026; unchanged historical sources retain their original
version. This is an implementation audit, not a certification claim.
The acceptance contract is docs/13 0.11 and docs/09 Decisions.

## Current evidence (2026-10-31)

| Release | Evidence | Current result |
|---|---|---|
| 2013 | Original 1 m box and independently generated nested model | Positive editable geometry/hierarchy, native save/reopen, byte-exact source restoration, edited 2017 regeneration; SKB import undo/redo and native-first CLI/RPC/GUI tests. |
| 2014–2016 | Original 1 m boxes, plus a nested 2014 layout probe | Positive geometry/scale and original-version restoration; import/export through backup extensions. Edited writing in these historical versions is not implemented. |
| 2017 | Original blank-template cube; generated models; external house inspection | Established geometry/hierarchy/material/UV scope; unchanged whole-model save is byte-exact. Editable fidelity still has the documented gaps. |
| 2018 | Real saved model (4,690 faces) and a real layout model | Full chain passes; tags, guides and overlong root lists covered. |
| 2019 | Independent SDK-saved squares, generated nested model and private 95 MiB model | Positive geometry/hierarchy/assets; the large model has 193,201 native faces, 130 images and zero validation problems. Native save/reopen and unchanged SKP restoration pass. |
| 2020 | Independent SDK-saved squares, generated arcs/nesting/textures | Positive native geometry/curve/hierarchy/texture import and unchanged byte-exact restoration. |
| 2021–2022 | Real saved models in both container families | Full chain passes (2021 classic archive, 2021 and 2022 containers). |
| 2023–2024 | No public redistributable file found | Uncertified; same container family as the certified 2021, 2022 and 2026 files. |
| 2025 header | Independent generated arcs/nesting probes (filenames refer to the 2021+ container family) | Analytic curves and hierarchy survive native save/reopen; unchanged save restores the exact original. Not retail certification. |
| 2026 | Original blank-template cube and generated feature-rich models | Geometry plus native annotation, image, curve, appearance and scene tests; unchanged source byte identity retained. |

All committed fixtures have pinned download scripts and licensing/provenance.
The previously failing SDK-saved 2019/2020 cases now have positive tests.
The calibrated classic reader is pure Rust; it walks object maps, persistent ids,
attributes and nested definitions without modifying version headers. It also
reads fonts, text, dimensions and guides. Unsupported objects return errors.
A structurally parsed empty model is valid; a failed empty fallback is not.

## Public corpus survey (2026-10-31)

`tools/check-skp-corpus.py` runs every 2013-or-later file below a directory
through the chain of docs/13: open, native save/reopen, unchanged restoration,
edit (add a line, delete a face, rename a material, tag, scene and component),
regenerate with both writers, reopen, compare counts and names. The surveyed files
are the five complete public repositories named in
`tests/fixtures/*/README.md` and `tools/fetch-skp-fixtures.sh`: 680 files, most
of them minimal feature pairs saved by 2017 and 2026 and copies of one model
across language packages, the others real or test models from 2013 to 2022 and
one future header. Only files small enough for a fixture are committed.

| Check | First survey | Now (680 files) |
|---|---|---|
| Open without error | 257 of the first 266 | 680 |
| Identical after native save/reopen | not measured | 680 |
| Byte-exact unchanged restoration | not measured | 680 |
| Saved and reopened by both writers | not measured | 680 |
| Counts retained after regeneration | not measured | 679 |

The exception is `PrusaI3_Extruder2`: self-touching facets refacet on the second
import, 579 → 584 faces, with no moved vertices. The 99 MB private model opens in
22 seconds with 193,201 faces and no validation problems, restores byte-exactly
and now regenerates, with 10 placements reported as closest fits.

With `--oracle`, surfaces are compared in world space against COLLADA files
exported by the original application for 47 models, each saved by 2017 and
2026. For the import and each writer's regenerated file, 88 of those 94 match in
bounds (0.05 mm), area (1 part in 10,000) and vertices (0.03 mm). The 6 others
are 3 models in both years, none with a surface missing from the native
document: placed images are surfaces in that export and not in OBJ (`house-plus`,
`feature-pack`), and `house` holds coincident faces the comparison counts once.
Texture coordinates are evaluated per side at every corner the document places
explicitly and compared with the same exports. No value contradicts them except
projectively pinned faces, 6 of the 94 rows (`pin-one`, `pin-fixed-distort`,
`feature-pack` in both years): that exporter bakes such faces into per-face
images with bounding-box coordinates, which the format notes document. A unit
test instead checks one real pinned face against the pins stored in the file,
through import and export. Corners of placed images and of faces inheriting a
group's material are not evaluated. This evidence does not cover annotations.

A field-by-field comparison of the canonical document before and after
regeneration covered the 2017 and 2026 corpora, every fixture and the real solid
models. Materials, tags, names, scenes, styles, slide settings and annotations
agree. What differs, none of it geometry or appearance:

- persistent ids and component UUIDs, which are assigned on import;
- names the 2026 writer gives to unnamed section planes, and `ModelProperties`
  values both writers add;
- leader text colors written where the source relied on the default.

That comparison exposed display settings shifted on classic releases, decoded as
X-ray on a transparent background. They are now refused and defaults apply, so
styles saved in 2013–2016, 2019, 2020 and classic 2021 files are not imported
(docs/09 Decisions).

CLI, RPC and GUI are tested on five real models: the same edits give an equal
canonical document and byte-identical SKP files (`gui/tests/skp.rs`).

Models before 2013 are refused by name. Two non-SKP files named
`.skp` are refused as unsupported containers.

## Editable import and 0.11 alpha exchange

- Native text and linear/radial dimensions, font metadata and sizes, screen
  positions, pushpin offsets, arrow styles and leader visibility.
- Persistent vertex/edge anchors through nested instances, arbitrary edge
  parameters, curve centres/angles and two-edge intersections. Merge rebinds
  complete paths, including reused definitions and overlapping pid ranges.
- Analytic circular arcs/circles; spatial polylines and sampled other curves.
- Placed image entities with physical dimensions, transformations and assets.
- Native styles, fog/backgrounds, watermarks, sun settings and scene appearance.
  Classifications, custom stroke images and the decoded settings catalogue are
  also retained in native assets/attributes, independently of the original blob.

Tests cover both 2017/2026 writers → import → native save/reopen, attached labels
following edited geometry, radial measurement/leader offsets, nested merge reuse
and undo/redo, independent text/stroke samples and malformed annotation positions.

## 0.11.0-alpha.1 additions

`label-style` edits font requests, pt/model height, arrow, text color and leader
visibility without changing content/attachments/placement. Entity Info dispatches
it once from the appearance form; CLI/RPC/Rhai expose the same command. Dimension
colors affect both GPU line groups and label text. Screen-only labels remain in
screen space after styling. Goldens, atomic-error/locked-context checks,
fuzz/undo and CLI/GUI/RPC tests cover this addition.

Edited native -> SKP -> native regressions now retain text/linear and radial
measurements, fonts/colors/arrows/live anchors, analytic circles and nested
text paths in both writers. Image entity/assets/size/placement round-trip three
times without accumulating unused native definitions. Styles, watermarks,
geographic sun directions and scene appearance also round-trip. The complete
original still restores byte-identically when unchanged; native-only source
catalogs are not duplicated as giant exported attribute strings.

## 0.11.0-alpha.2 additions

Positive 2013–2016 bodies replace earlier empty fallback results. Pre-2017
object-map slots provide stable reference identities; definition/layer prefixes
and the pre-2014 instance tail are read according to the actual header.
`.skb` shares the model reader/writer and all desktop entry points with `.skp`.
First saving defaults to a native `.plyr` copy; GUI asks through Save As and
CLI/RPC refuse implicit overwriting of an existing native sibling. Explicit
foreign choices persist for GUI/RPC sessions, including attached clients.
Read-only opening leaves source files intact. There is no manual conversion
step. Per-release certification and edited historical writers remain open.

## Required before complete-support claims

1. **Target application.** Regenerated files were only read back by this converter.
   They must be opened in the target application; data and appearance both count.
   This cannot be done on the development machine.
2. **2023 and 2024.** Repositories and code search found no public redistributable
   file from either release. A user-provided save of the same model in each
   would close this.
3. Confirm the axes of kind 7 face anchors against a source of known positions.
   Calibrate display settings for 2013–2016, 2019 and 2020, which needs files
   saved with known non-default styles in those releases.
4. Verify exact external font faces (native display substitutes its available
   font), custom stroke rendering, luminance-mask watermarks and remaining camera
   presentation. Retained source metadata does not establish equivalence.
5. Edited-output limits the format cannot hold or the writers do not accept:
   manual sun directions, text at arbitrary curve angles, linear dimension offsets
   along the measuring direction, radial dimensions across nested occurrences,
   polygon edit metadata, per-scene transition times, multi-pin placements
   (written as the closest single one), custom strokes/masks and foreign records.
   Tag attribute dictionaries are not native, so they remain only in the source.
   Whole-model native working copies retain the original; notices explain the
   boundary. Edited writing of 2013–2016 streams is not implemented.
6. Browser SKP conversion remains unavailable.

## Source preservation already implemented

Whole-model decoding stores the source and a baseline digest in native `ext/`
parts. Native open/save and autosave carry them without a converter. Whole-model
SKP save restores the exact original when the editable document matches that
baseline. History, consumed PID allocation and manifest timestamps do not count
as edits, so a fully undone change can restore the original. Document,
camera, images and Sheets do. After edits, regeneration writes current state
with compatibility notices, rather than silently returning the old source.
Unchanged explicit version changes still use the requested writer, not an
original file with a different version.

This protects otherwise uninterpreted source data for unchanged whole-model
round-trips; it does not provide editable support for that data. Merge import,
registered command export and remote snapshot flows do not carry the whole-model
source as editable document state. Browser SKP conversion remains unavailable.
Do not mark the 0.11 milestone complete on the strength of source preservation.
