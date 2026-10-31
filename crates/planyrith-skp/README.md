# Optional SKP converter

This is a **separate GPL-3.0-only executable**, using `openskp` 0.5.0. It is
excluded from the default workspace build and is never linked into Planyrith's
core, CLI, renderer or GUI. Build it explicitly with
`cargo build -p planyrith-skp --release` and place it beside the application,
or set `PLANYRITH_SKP_ADAPTER` to its absolute executable path. It runs on the
desktop; the browser backend reports that no converter is available.

The process reads all input from stdin. Operations are `decode` (SKP → native),
`encode-preserve` (whole-model save), `encode-2017` and `encode-2026` (explicit
native → SKP conversion). Stdout uses the versioned
byte-stream protocol in `planyrith-core::io::adapter`; stderr is diagnostic text.
Inputs/outputs are limited to 512 MiB. A fixed authoring time and seed make
exports reproducible. No proprietary SDK, C/C++ library or network service is used.

`.plyr` remains the primary working format. Whole-model decoding embeds the
original SKP in preserved native extension parts, together with a document
baseline. Unchanged saves restore those source bytes exactly, even after a
native save/reopen; session history and manifest timestamps do not invalidate
them. Document/camera/assets/Sheets edits invalidate restoration, so saving
cannot silently revert edits to the original file. A regenerated save uses
2017 for source releases before 2026, otherwise 2026, with notices for changed
writer versions and data conversion limits. Explicit export remains versioned.
The native working copy retains the original even when edited SKP output cannot.

Import transfers polygon faces/holes, loose edges, soft/smooth/hidden flags,
groups, shared/nested components and transforms, front/back materials, embedded
textures and representable per-face UV placement, tag visibility, cameras,
scenes/sections, guides, units, drawing axes and attribute dictionaries. It also
creates editable native text and linear/radial dimensions, with persistent
nested anchors, edge parameters, curve centres and edge intersections. Font
family/weight/size, screen positions, pushpin offsets, leader visibility and
arrow types survive native saves. Frontends substitute the bundled font when
needed. Circular arcs/circles retain analytic descriptors; spatial polylines
and other curves retain their sampled chains. Placed images retain their
material, texture asset, size and transform.

The classic reader accepts packed color attributes and trailing component
definitions, including extended archive tags. Embedded BMP assets become
lossless PNG images. Warped polygons become facets with unchanged vertices;
facets below native tolerance retain boundary wires with a notice. A face whose
own rings cross or touch becomes facets, as no grouping can separate edges of
one face; its source vertices are unchanged.
Overlapping edges and coincident vertices in separate faces/wires are retained
in private groups, and interrupted curves become separate analytic/sample
chains. Converter failure messages describe the cause without automatically
declaring the source file corrupt.

Styles, fog/backgrounds, watermarks, geographic sun settings and per-scene
appearance transfer to native settings. Source classifications and custom
stroke images remain inspectable native data; decoded settings without a
renderer equivalent are retained under `org.planyrith.skp` attributes. Custom
stroke rendering, luminance-mask watermarks, exact external font faces and
some camera/annotation presentation details still have display limitations.
The original source is retained by whole-model Open as described above.

Edited SKP export now regenerates text and linear/radial dimensions with
representable persistent anchors, fonts/colors and leaders, analytic circular
curves and welded sampled chains, image entities/assets, styles/watermarks,
geographic shadows and per-scene appearance. Repeated image conversions do not
create unused native definitions. Temporary SKP ids do not consume native ids.

Texture tints transfer in both writers. A face whose texture samples fit no
single placement takes the closest one, reported once with a count, so one face
cannot stop a save; an image entity that lost its material keeps its rectangle.

Remaining conversion limits include manual sun directions, text attached at
arbitrary analytic curve angles, sideways radial-leader components, a linear
dimension offset along its measuring direction, radial dimensions across nested
occurrences, some attribute/behavior types, custom strokes/masks, Sheets and edit
history. Geographic shadow dates must fit the format's unsigned 32-bit epoch;
detached or unrepresentable radial references fail explicitly. Fractional font
point sizes and intermediate watermark grid positions are rounded with notices.
Polygon edit metadata remains native-only; its welded geometry transfers.
Regenerated files may omit foreign/unrecognized records; the complete original
remains in a whole-model native working copy. Native-only source catalogs are
not duplicated into exported SKP attribute strings.

Conflicting face sides remain in private surface groups (at most 4096 per
source container), including scene references to copied edges. Projective UV
frames survive native saves; WebP textures/watermarks export as PNG. Selected-only
export is rejected. Keep a native copy for full document fidelity.

Compatibility tests cover original 2017/2026 files, independent SDK-saved
2019/2020 files and generated probes declaring a 2025 header. The calibrated
Rust classic-archive reader now recovers the previously missing 2019/2020
geometry, curves, nesting and textures. It supports both short and escaped
object references and checks persistent-id layouts without changing headers.
It covers classic archives of any release from 2013; those of earlier releases
are refused by name. `tests/fixtures/release` adds files saved by real 2018, 2021
and 2022 releases. 2023 and 2024 have no public fixture; unknown classic objects
fail explicitly. `tools/check-skp-corpus.py` runs the whole exchange chain over a
local corpus and compares surfaces with COLLADA exports of the original
application (docs/dev/skp-fidelity.md). Exported files are checked by the
converter's readers and 2017 grammar; opening them in the target application
itself is still required.

`cargo test -p planyrith-skp` exercises genuine files, generated round-trips,
texture coordinates, scale, repeated/nested components, holes, section planes,
attributes, malformed inputs, process framing and core import/export/undo.
The native golden counterpart in `planyrith-core/tests/fixtures/skp` is generated
by `tools/generate-skp-fixture.rs`, independently of this converter.

## Fixture provenance

`tests/fixtures/box-2017.skp` and `box-2026.skp` are the upstream author's
`blank-template-box.skp`, saved from a blank template with no scale figure or
bundled materials. Both contain only the author's 1 m cube: 8 vertices, 12
edges, 6 faces, no component definitions or texture images. They are covered
by the upstream GPL-3.0-only grant and kept only in this GPL component.

Source: <https://github.com/hew3d/openskp/tree/a6788afa6386cfd4bc781998dca99c591b3fb069/corpus>.
The script `tools/fetch-skp-fixtures.sh` reproduces both downloads and verifies
their hashes. The dependency and this component's license are in `LICENSE`.

`tests/fixtures/release` has its own README and MIT grants. The classic reader
is a Rust port of the MIT-licensed format reference in
`iamahsanmehmood/openskp` at revision
`c0bbeaa62191d776ea25e979555c5d6c464d54fa`. Its copyright and grant are retained
in `tests/fixtures/intermediate/LICENSE` and embedded in the executable's
`--license` output. No interpreter or new runtime dependency is introduced.
