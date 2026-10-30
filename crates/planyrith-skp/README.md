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

Compatibility tests cover the 2017 and 2026 containers and generated probes
declaring a 2025 header.
The SDK-saved 2019/2020 probes in `tests/fixtures/intermediate` are rejected:
the dependency fails to recover their geometry. Such failures must not open a
successful empty model. Other intermediate releases are not certified. Complete
2017-and-later editable fidelity is still in progress. Exported files are checked
by the converter's readers and 2017 grammar; testing in the user's target application is still valuable.

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
