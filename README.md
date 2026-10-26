# Planyrith

**Planyrith** is a cross-platform, open-source 3D design and modeling application for
architecture, interiors, landscape, woodworking and product mock-ups. It is written
entirely in Rust, runs on Linux, macOS, Windows and in the browser (WebAssembly), and is
built *CLI-first*: every modeling tool exists as a command before it gets a button.

> **Status: version 0.4.0 — organization and materials.**
> The geometry kernel, the document model, the modeling commands, the native file format, the
> `planyrith` command line (headless rendering, a JSON-RPC server, a shell) and the GUI work:
> drawing and editing tools that snap through the inference engine, exact input, previews,
> textured materials (bundled and your own images), components and component files, scenes
> with animated transitions, styles with fog and watermarks, and material, component and style
> libraries. See [`CHANGELOG.md`](CHANGELOG.md) and the milestones in
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

Longer scripts run as one undo step with `planyrith -f room.plyr batch room.plyrb`; see
[`docs/08-cli-specification.md`](docs/08-cli-specification.md) and the acceptance script
[`crates/planyrith-cli/tests/batches/furnished-room.plyrb`](crates/planyrith-cli/tests/batches/furnished-room.plyrb).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Contributions are accepted under the same terms.
