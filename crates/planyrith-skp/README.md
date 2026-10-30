# Optional SKP converter

This is a **separate GPL-3.0-only executable**, using `openskp` 0.5.0. It is
excluded from the default workspace build and is never linked into Planyrith's
core, CLI, renderer or GUI. Build it explicitly with
`cargo build -p planyrith-skp --release` and place it beside the application,
or set `PLANYRITH_SKP_ADAPTER` to its absolute executable path. It runs on the
desktop; the browser backend reports that no converter is available.

The process reads all input from stdin. Operations are `decode` (SKP → native),
`encode-2017` and `encode-2026` (native → SKP). Stdout uses the versioned
byte-stream protocol in `planyrith-core::io::adapter`; stderr is diagnostic text.
Inputs/outputs are limited to 512 MiB. A fixed authoring time and seed make
exports reproducible. No proprietary SDK, C/C++ library or network service is used.

Transferred: polygon faces/holes, loose edges, soft/smooth/hidden flags,
groups, shared/nested components and transforms, front/back materials, embedded
textures and representable per-face UV placement, tag visibility, camera, scene
cameras/visibility/sections, section planes, guide points/lines, display units,
drawing axes, and representable attribute dictionaries. Unsupported data produces
explicit warnings: Sheets, edit history, annotations, placed images, rendering
styles/shadows/watermarks, some attribute types and component behavior details.
Curves retain their segmented geometry. Conflicting face sides are preserved in
private surface groups (at most 4096 per source container), including scene
visibility references to copied edges. Projective UV frames survive native saves;
WebP textures export as PNG without changing decoded pixels. Selected-only export
is rejected.
Keep a native copy for full document fidelity.

Compatibility tests cover the 2017 and 2026 containers. Intermediate releases
are not certified. Exported files are checked by the converter's readers and
2017 grammar; testing in the user's target application is still valuable.

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
