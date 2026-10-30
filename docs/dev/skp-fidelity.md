# SKP fidelity worklist

The user confirmed 2017-and-later interchange, with `.plyr` as the principal
working format. This is an implementation audit, not a certification claim.
The acceptance contract is docs/13 0.11 and docs/09 Decisions.

## Current evidence (2026-10-30)

| Release | Evidence | Current result |
|---|---|---|
| 2017 | Original blank-template cube; generated models; external house inspection | Established geometry/hierarchy/material/UV scope; unchanged whole-model save is byte-exact. Editable fidelity still has the documented gaps. |
| 2018 | No committed release fixture | Uncertified. |
| 2019 | Independent SDK-saved file in `fixtures/intermediate` | Dependency recovers no geometry; opening now fails explicitly. |
| 2020 | Independent SDK-saved file in `fixtures/intermediate` | Dependency recovers no geometry; opening now fails explicitly. |
| 2021–2024 | No committed release fixtures | Uncertified. |
| 2025 header | Independent generated arcs and nesting probes (filenames refer to the 2021+ container family) | Geometry/hierarchy round-trip through both writers; unchanged save restores the exact original. Not retail-application certification. |
| 2026 | Original blank-template cube; generated models; external house inspection | Established geometry/hierarchy/material/UV scope; unchanged whole-model save is byte-exact. Editable fidelity still has the documented gaps. |

All committed fixtures have pinned download scripts and licensing/provenance.
2019/2020 refusal tests deliberately expose missing support. Replace those
expectations with positive data/editing tests when the decoder supports them.

## Required before complete-support claims

1. Implement or integrate a pure-Rust decoder for the 2019/2020 failures and
   obtain actual 2018/2021–2024 files. A locally isolated attempt to route the
   failed files through the dependency's release-17 grammar still failed on
   object-reference and persistent-id layouts; it was not shipped. Altering
   header versions is not a supported decoding strategy.
2. Transfer editable annotations (text/fonts, leaders, linear/radial dimensions
   and references), placed images, curve descriptors and spatial polylines.
3. Transfer styles, rendering/shadows/watermarks, per-scene appearance, component
   behavior details and currently unrepresentable attributes. Inventory unknown
   records and references so regeneration does not discard them unnoticed.
4. Verify original → `.plyr` → reopen → edit → SKP → independent target reader
   for each supported release, including added/deleted entities and renamed
   definitions/materials. Check data and appearance, not merely file acceptance.
5. Exercise CLI/RPC and GUI with the same models. Native snapshots, recovery,
   history, Sheets and unsupported source data must remain recoverable.

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
