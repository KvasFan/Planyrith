# 00 — Overview

**Audience:** AI agents and engineers implementing Planyrith.
**Status:** normative for version 0.x. Changes go through the "Decisions" section at the end.

## 1. What Planyrith is

Planyrith is a cross-platform 3D design application for people who think in walls, boards,
rooms and objects rather than in NURBS patches. Its modeling paradigm is **direct edge-and-face
modeling**: the user draws edges; closed coplanar loops of edges become faces automatically;
faces are pushed and pulled into volumes; everything is edited by grabbing geometry and moving
it with precise, visually explained snapping ("inference").

Primary domains: architecture (concept through construction documentation), interior design,
landscape, carpentry/woodworking, stage and exhibition design, product mock-ups, 3D printing
preparation, and education.

Planyrith ships as:

| Deliverable | Binary / target | Description |
|---|---|---|
| Core library | `planyrith-core` (crate) | Document model, geometry kernel, commands, I/O. Headless. |
| Command-line tool | `planyrith` | Every operation as a subcommand; scripting; headless import/export/render; JSON-RPC server. |
| Desktop app | `planyrith-gui` | Linux (X11/Wayland), macOS, Windows. egui/eframe shell, wgpu viewport. |
| Web app | `planyrith-gui` built for `wasm32-unknown-unknown` | Same UI in the browser via WebGPU (WebGL2 fallback), documents stored in IndexedDB. |

## 2. Goals

1. **Fast to learn, fast to use.** A new user builds a furnished room within an hour. An expert
   user never leaves the keyboard-mouse flow: type a length, hit Enter, continue.
2. **Precise.** `f64` geometry in millimeters, explicit tolerances, exact axis locks, numeric
   entry for every interactive tool.
3. **CLI first, GUI second.** Every tool is a headless command with typed parameters. The GUI is
   one client of that command set; scripts, tests and other programs are others.
4. **Cross-platform from day one.** One code base for Linux, macOS, Windows and the browser.
5. **Open.** MIT/Apache-2.0 dual license; only permissive, pure-Rust dependencies.
6. **Interoperable.** Lossless native format; robust import/export of the formats people
   actually exchange (OBJ, STL, glTF, DXF, STEP, images); 2D documentation output (PDF/SVG).
7. **Extensible.** Scripting and plugins can do anything the GUI can do, because they call the
   same commands.

## 3. Non-goals (for 1.0)

- Parametric history trees / feature-based CAD. Planyrith is direct modeling with undo.
- Photorealistic path-traced rendering (an export hook for external renderers is in scope).
- Building Information Modeling semantics (IFC import/export is a post-1.0 item).
- Mesh sculpting, subdivision surfaces, character animation, physics simulation.
- Collaboration server / real-time multi-user editing (file-level sharing only).
- Mobile touch-first UI (the web build must *work* on tablets; optimizing for them is later).

## 4. Personas

- **Dana, architect.** Models a house massing in an afternoon, iterates with a client live,
  exports elevations and a plan sheet to PDF. Needs: speed, inference, scenes, sections,
  dimensions, sheets.
- **Marco, carpenter.** Models a cabinet from board dimensions typed in millimeters, needs a cut
  list and a dimensioned drawing. Needs: numeric entry, components, copy arrays, 2D sheets,
  CSV export of component counts.
- **Ines, educator.** Runs the web build in a classroom on Chromebooks. Needs: zero install,
  autosave to the browser, small example files, a forgiving undo.
- **Priya, pipeline engineer.** Converts hundreds of models nightly and renders thumbnails.
  Needs: a deterministic CLI, JSON output, exit codes, batch scripts, headless rendering.
- **Kenji, plugin author.** Writes a stair generator. Needs: a stable scripting API that mirrors
  the command set, a way to add a toolbar button and a parameter panel.

## 5. Feature map (1.0)

### 5.1 Geometry and modeling
- Edges, faces (planar polygons with holes), automatic face creation and splitting, face
  merging on edge deletion, front/back face orientation, soft/smooth/hidden edge flags.
- Tools: Select, Eraser, Line, Freehand, Rectangle, Rotated Rectangle, Circle, Polygon,
  Arc (2-point, 3-point, pie), Push/Pull, Move (with copy and linear/radial arrays), Rotate,
  Scale, Offset, Sweep (profile along a path), Intersect Faces, Solid tools (Union,
  Subtract, Trim, Intersect, Split, Outer Shell), Flip, Soften/Smooth, Hide/Unhide.
- Guides: Tape Measure (guide lines/points), Protractor (angular guides), Axes (relocate drawing axes).
- Annotations: Dimension (linear, aligned, radial), Text (screen-anchored and leader), 3D Text.

### 5.2 Organization
- Groups, components (definition + instances, nested, mirrored, scaled), component library
  (local folders + files), "make unique", explode.
- Tags (visibility/style categories) with folders; per-entity tag; tag on instances.
- Scenes: saved camera, tag visibility, section plane state, style, shadow settings, with
  transition animation and an export-to-video/frames path.
- Section planes (active cut, fill caps, section lines export to 2D).
- Attribute dictionaries on any entity (typed key/value, used by plugins and the sheets module).

### 5.3 Appearance
- Materials: color, opacity, texture (PNG/JPEG/WebP), UV mapping per face (position, scale,
  rotate, skew, project), front and back face materials, default material colors.
- Styles: edge display (profiles, extensions, endpoints, jitter), face display (shaded,
  shaded with textures, hidden line, wireframe, monochrome, X-ray), background/sky/ground,
  watermarks, fog.
- Shadows: sun position from date, time and geographic location (or manual azimuth/altitude),
  light/dark sliders, shadows on faces and ground.
- Camera: perspective (field of view), parallel projection, two-point perspective, standard
  views, walk/look-around, position camera at eye height.

### 5.4 Files and interchange
- Native container `.plyr` (versioned, lossless, embeds textures, optional edit history).
- Component files `.plyrc`, material libraries `.plyrm`, style files `.plyrs`.
- Import: OBJ, STL (ASCII/binary), glTF 2.0/GLB, DXF (2D/3D entities), STEP (via B-rep kernel),
  PNG/JPEG/WebP (as textures or image entities), CSV (points), `.plyr/.plyrc`.
- Export: OBJ, STL, glTF/GLB, DXF, STEP, PNG/JPEG/WebP renders at arbitrary resolution,
  SVG/PDF of 2D views and sheets, CSV of component counts, JSON of the document.

### 5.5 Automation and extension
- `planyrith` CLI with text and JSON output; command scripts; a `serve` mode exposing JSON-RPC
  over stdio or a local socket.
- Embedded scripting language (rhai) with an API that mirrors the command registry.
- Plugin manifests that contribute tools, menus, toolbars and panels declaratively.

### 5.6 2D documentation ("Sheets")
- Paper-space sheets containing model viewports (scene + scale + style), dimensions, text,
  tables, images, title blocks; export to PDF/SVG/DXF. Specified in `docs/15-sheets-2d-documentation.md`.

## 6. Quality targets

| Target | Requirement |
|---|---|
| Startup to first interaction | < 1.5 s desktop, < 4 s web (cold cache, mid-range laptop) |
| Viewport frame time | < 8 ms at 1080p for a 1M-triangle, 200k-edge model (integrated GPU) |
| Inference latency | < 4 ms per cursor move for 200k edges (spatial index) |
| Undo/redo | O(size of change), never full-document copies |
| File save/load | 100 MB `.plyr` in < 3 s on NVMe; streaming, never loads textures twice |
| Determinism | `planyrith` on the same inputs yields byte-identical `.plyr` output across OSes |
| Memory | Core document memory ≤ 2× the on-disk uncompressed size |

## 7. Document conventions

- Normative words: **MUST**, **SHOULD**, **MAY** as in RFC 2119.
- Each tool/command has a stable kebab-case name used by the CLI, the registry, the scripting
  API and the docs (e.g. `push-pull`).
- Units in docs are SI unless stated; the internal unit is the millimeter (`docs/03`).
- Every doc ends with a **Decisions** list: dated entries recording resolved ambiguities.

## 8. Index of documents

| # | File | Content |
|---|---|---|
| 00 | `00-overview.md` | This document |
| 01 | `01-architecture.md` | Crate layout, command system, data flow, concurrency, error model |
| 02 | `02-technology-stack.md` | Approved crates, alternatives considered, licensing policy |
| 03 | `03-geometry-kernel.md` | Half-edge mesh, tolerances, face finding, booleans, B-rep bridge |
| 04 | `04-document-model.md` | Entities, hierarchy, materials, tags, scenes, attributes, ids |
| 05 | `05-modeling-tools.md` | Full tool catalogue with parameters, algorithms and tests |
| 06 | `06-rendering-and-viewport.md` | wgpu pipeline, styles, shadows, sections, picking, camera |
| 07 | `07-interaction-and-ui.md` | egui shell, docking, inference UX, keyboard model, panels |
| 08 | `08-cli-specification.md` | Subcommands, addressing, units, JSON output, JSON-RPC server |
| 09 | `09-file-formats.md` | Native container, component/material files, importers/exporters |
| 10 | `10-extensions-api.md` | Scripting language binding, plugin manifests, sandboxing |
| 11 | `11-persistence-and-web.md` | Storage abstraction, IndexedDB adapter, wasm build, autosave |
| 12 | `12-testing-and-quality.md` | Test strategy, golden files, fuzzing, CI matrix, benchmarks |
| 13 | `13-roadmap.md` | Milestones 0.1 → 1.0 with acceptance criteria |
| 14 | `14-glossary.md` | Canonical vocabulary |
| 15 | `15-sheets-2d-documentation.md` | Paper-space documentation module |

## Decisions

- 2026-10-20 — Product name fixed as **Planyrith**; native extension `.plyr`; CLI binary `planyrith`.
- 2026-10-20 — Direct modeling only; no parametric history for 1.0 (see §3).
- 2026-10-20 — Internal length unit is the millimeter in `f64` (see `docs/03`).
