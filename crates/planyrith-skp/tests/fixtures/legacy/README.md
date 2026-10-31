# Historical classic archives

These four 1 m box models were saved by releases 2013–2016 and come from the
GPL-3.0-only format-reader corpus at pinned revision
`hew3d/openskp@a6788afa6386cfd4bc781998dca99c591b3fb069`, paths
`corpus/legacy/box-v2013.skp` through `box-v2016.skp`. Their original headers
are `{13.0.1}`, `{14.0.1}`, `{15.0.1}` and `{16.0.1}`. Headers are not edited.
The optional component's GPL grant is in `../../../LICENSE`.

`tools/fetch-skp-fixtures.sh` reproduces the downloads and checks SHA-256.
Regressions require six editable faces, twelve edges, exact 1 m dimensions,
valid native topology, unchanged-source byte restoration and edited 2017
export/reimport. A process test imports the same bytes under `.SKB` names,
checks byte-exact undo/redo and exports to a 2026 backup. Application tests
exercise direct open/save, the native default and overwrite protection.

The MIT fixtures under `../intermediate/` also cover nested 2013 components
and the 2014 definition layout. These probes do not certify every entity type
in every historical release or writing edited files in the 2013–2016 format.
