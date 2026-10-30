# SKP fidelity worklist

The user confirmed 2017-and-later interchange, with `.plyr` as the principal
working format. This is an implementation audit, not a certification claim.
The acceptance contract is docs/13 0.11 and docs/09 Decisions.

## Current evidence (2026-10-30)

| Release | Evidence | Current result |
|---|---|---|
| 2017 | Original blank-template cube; generated models; external house inspection | Established geometry/hierarchy/material/UV scope; unchanged whole-model save is byte-exact. Editable fidelity still has the documented gaps. |
| 2018 | No committed release fixture | Uncertified. |
| 2019 | Independent SDK-saved squares and generated nested model | Positive native geometry import, 254 mm scale, hierarchy, unchanged byte-exact restoration. |
| 2020 | Independent SDK-saved squares, generated arcs/nesting/textures | Positive native geometry/curve/hierarchy/texture import and unchanged byte-exact restoration. |
| 2021–2024 | No committed release fixtures | Uncertified. |
| 2025 header | Independent generated arcs/nesting probes (filenames refer to the 2021+ container family) | Analytic curves and hierarchy survive native save/reopen; unchanged save restores the exact original. Not retail certification. |
| 2026 | Original blank-template cube and generated feature-rich models | Geometry plus native annotation, image, curve, appearance and scene tests; unchanged source byte identity retained. |

All committed fixtures have pinned download scripts and licensing/provenance.
The previously failing SDK-saved 2019/2020 cases now have positive tests.
The calibrated classic reader is pure Rust; it walks object maps, persistent ids,
attributes and nested definitions without modifying version headers. It also
reads fonts, text, dimensions and guides. Unsupported objects return errors.
A structurally parsed empty model is valid; a failed empty fallback is not.

## Editable import implemented in this increment

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

## Required before complete-support claims

1. Obtain genuine 2018/2021–2024 release fixtures, validate more complex classic
   archives and extend unsupported object/layout coverage from real evidence.
2. Verify exact external font faces (native display currently substitutes its
   available font), custom stroke rendering, luminance-mask watermarks and
   remaining camera/annotation presentation details. Retained source metadata
   alone does not establish editable or visual equivalence.
3. Extend **edited SKP export**: native annotations/images/styles/sun/watermarks
   and some behavior/attribute types still produce conversion notices; curve
   export remains segmented. This request prioritized import; it does not close
   the complete open/save acceptance contract.
4. Verify original → `.plyr` → reopen → edit → SKP → independent target reader
   per supported release, including entity addition/deletion and renamed
   definitions/materials. Check data and appearance, not just file acceptance.
5. Exercise CLI/RPC and GUI with the same original feature-rich models. Browser
   SKP conversion remains unavailable. Native snapshots, recovery, history and
   Sheets must retain their existing behavior.

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
