# Planyrith

**Planyrith** is a cross-platform, open-source 3D design and modeling application for
architecture, interiors, landscape, woodworking and product mock-ups. It is written
entirely in Rust, runs on Linux, macOS, Windows and in the browser (WebAssembly), and is
built *CLI-first*: every modeling tool exists as a command before it gets a button.

> **Status: preview 0.11.0-alpha.1 — editable SKP exchange and annotation styles.**
> The geometry kernel, the document model, the modeling commands, the native file format, the
> `planyrith` command line (headless rendering, a JSON-RPC server, a shell) and the GUI work:
> drawing and editing tools that snap through the inference engine, exact input, previews,
> textured materials (bundled and your own images), components and component files, scenes
> with animated transitions, styles with fog and watermarks, material, component and style
> libraries, import and export of glTF/GLB, OBJ, STL, DXF, images and CSV, 2D drawings of a
> view as SVG, PDF or DXF, dimensions, texts, guides, section cuts, sun shadows, 3D text and
> multiple viewports. Solid booleans, Sweep, Intersect Faces, Soften/Smooth, STEP,
> sandboxed Rhai scripts, a Console and declarative plugins are available. The desktop GUI
> can attach to a local command server. Paper-space Sheets add scaled model views,
> dimensions, notes, tables and title blocks, with multi-page PDF and per-page SVG/DXF.
> Freehand and Rotated Rectangle complete the drawing catalogue. An optional desktop
> converter reads and writes SKP models.
> The same application runs in the browser, and both
> autosave and recover unsaved work. See [`CHANGELOG.md`](CHANGELOG.md) and the milestones in
> [`docs/13-roadmap.md`](docs/13-roadmap.md).

## What Planyrith will be

- **Direct modeling** of edges and faces: draw, push/pull, move, rotate, scale, offset,
  sweep. Faces form automatically from closed coplanar edge loops.
- **A precise inference engine**: endpoint, midpoint, intersection, on-edge, on-face,
  perpendicular, parallel, tangent and axis snapping, with axis locking.
- **Organization**: groups, reusable components, tags for visibility, saved scenes,
  section planes, guides, dimensions, text and 3D text.
- **Materials and rendering**: colors, textures, transparency, shadows from a sun
  model, line styles, section cuts, a fast `wgpu` viewport.
- **Interchange**: a versioned native container format plus OBJ, STL, glTF, DXF, STEP
  and image import/export, and 2D documentation sheets.
- **Automation**: everything scriptable from the command line, a JSON-RPC server, an
  embedded scripting language, and a plugin API.

## SKP models on desktop

**`.plyr` is the principal lossless working format**, including full application
state, Sheets, annotations and edit history. SKP interoperability complements it.
Complete open/save fidelity for 2017 and later is an in-progress target; the
current converter does not yet meet that target across all those releases.

An optional Rust converter opens, saves, imports and exports `.skp` models.
Build it with `cargo build -p planyrith-skp --release` and place `planyrith-skp`
beside the CLI/GUI executable (or set `PLANYRITH_SKP_ADAPTER` to its absolute
path). The converter is a separate **GPL-3.0-only** component; the applications
remain MIT/Apache-2.0. The browser build currently has no SKP converter.

```sh
planyrith -f model.skp info
planyrith -f model.plyr import furniture.skp
planyrith -f model.plyr export model.skp --skp-version 2017
planyrith -f model.plyr export model-2026.skp --skp-version 2026
```

The GUI's Open, Save As and Import/Export dialogs accept `.skp`. Tests cover
2017 and 2026 containers, SDK-saved 2019/2020 files and generated 2025-header
probes. Other intervening releases remain uncertified. Import and edited export
include geometry, hierarchy, textures, text and associative dimensions, analytic
circular curves, placed images, styles, geographic shadows and scene appearance.
Select text or dimensions and open **Entity Info → Annotation style** to change
font requests, sizes, arrow, color and leaders. The same operation is available
as `planyrith tool label-style` and through RPC/scripting.

This is an alpha build. Native display substitutes unavailable font faces;
manual sun directions, some annotation presentation and custom stroke/mask
rendering retain conversion limits. Keep `.plyr` as the full working document.
See [converter scope and license](crates/planyrith-skp/README.md).

Whole-model Open retains the complete SKP source through native working copies.
Saving it back without document changes restores the original bytes, including
uninterpreted data. Edited SKP output still has the documented conversion limits;
source preservation alone is not complete editable interoperability.

## Automation

Automation examples (script lengths are millimeters; angles are radians):

```sh
planyrith -f stairs.plyr tool ext:org.planyrith.stairs/stairs --steps 14 --rise 2.8m --run 4.2m
planyrith -f model.plyr script build.rhai --arg width=1200
planyrith ext list
planyrith ext check plugins/org.planyrith.stairs
planyrith -f model.plyr serve --socket 4500 --token local-secret
```

Use Extensions › Extension Manager in the GUI to install `.plyrx` archives or attach
to the server (port `4500`, its token, document `d1`). Window › Console runs scripts;
Extensions opens plugin forms. Solid tools and Sweep are in Tools. STEP exchanges
placed solid bodies; unsupported assembly placements are reported explicitly.

## Sheets

Use File › Send to Sheets or the Sheets tab to lay out model views on paper.
Pick a scene and a rational scale, add dimensions and notes, then export all pages
as PDF. `Edit Camera in Model` adjusts a viewport using the ordinary 3D controls.

```sh
planyrith -f house.plyr sheet add-page --page A101 --paper A4
planyrith -f house.plyr sheet add-view --page A101 --scene Plan --scale 1:50
planyrith -f house.plyr sheet export --pages all --out house.pdf
```

A scene named `Plan` must exist for the linked example; omit `--scene` to capture
the current camera. Paper coordinates are millimeters. See
[the Sheets command catalogue](docs/15-sheets-2d-documentation.md#51-command-surface-09).
`cargo run -p planyrith-core --example generate-sheets` reproduces the house
fixture and printable examples in `target/tmp/sheets/`.

## Repository layout

| Path | Purpose |
|---|---|
| `docs/` | The specification. Start with [`docs/00-overview.md`](docs/00-overview.md). |
| `crates/planyrith-core` | Document model, geometry kernel, commands, file I/O. No GUI or GPU dependencies. |
| `crates/planyrith-render` | `wgpu` renderer without UI, shared by the GUI viewport and the CLI's headless `render` subcommand. |
| `crates/planyrith-cli` | `planyrith` binary: every tool as a subcommand, scripting, headless export, JSON-RPC server. |
| `crates/planyrith-gui` | `planyrith-gui` binary: egui/eframe shell with a wgpu viewport; desktop and WebAssembly. |
| `AGENTS.md` | Working instructions for AI agents and contributors implementing the spec. |
| `deny.toml` | Licensing and "pure Rust" dependency policy enforced by `cargo-deny`. |
| `.github/workflows/ci.yml` | Lint, test and WebAssembly jobs from `docs/12-testing-and-quality.md`. |

## Design principles

1. **Core, CLI, GUI are separate.** The core knows nothing about windows or pixels.
   The CLI exposes the whole core. The GUI is a client of the same command registry.
2. **CLI first.** A tool is finished when it works headless with typed parameters,
   has tests, and is documented. Only then does it get interactive behavior.
3. **Rust only, open licenses only.** No vendored C/C++; dependencies must be
   MIT/Apache-2.0/BSD-style (see `deny.toml`).
4. **Deterministic documents.** The same commands on the same document always yield
   the same result, byte for byte, in the native format.
5. **Precision by default.** Geometry is computed in `f64` millimeters with explicit
   tolerances; rendering uses `f32`.

## Building and trying it

```sh
cargo build --release -p planyrith-cli -p planyrith-gui

planyrith-gui box.plyr                        # the GUI: L line, R rectangle, P push/pull, type values

planyrith new -f box.plyr
planyrith -f box.plyr tool rectangle --corner-a 0,0,0 --corner-b 1m,600mm,0
planyrith -f box.plyr tool push-pull --face '{face & last}' --distance 450mm
planyrith -f box.plyr query 'face' --fields pid,area
planyrith -f box.plyr validate
planyrith -f box.plyr render box.png           # a headless image
planyrith tool                                # every command; `planyrith tool <name> --help`
```

The web build: `tools/build-web.sh` (needs the `wasm-bindgen` CLI matching `Cargo.lock`) writes
static files to `dist/web`; serve them with any static server, e.g.
`python3 -m http.server -d dist/web`, and open the address in a browser.

Longer scripts run as one undo step with `planyrith -f room.plyr batch room.plyrb`; see
[`docs/08-cli-specification.md`](docs/08-cli-specification.md) and the acceptance script
[`crates/planyrith-cli/tests/batches/furnished-room.plyrb`](crates/planyrith-cli/tests/batches/furnished-room.plyrb).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Contributions are accepted under the same terms.
