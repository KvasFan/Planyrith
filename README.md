# Planyrith

**Planyrith** is a cross-platform, open-source 3D design and modeling application for
architecture, interiors, landscape, woodworking and product mock-ups. It is written
entirely in Rust, runs on Linux, macOS, Windows and in the browser (WebAssembly), and is
built *CLI-first*: every modeling tool exists as a command before it gets a button.

> **Status: version 0.0.0 — specification only.**
> This repository currently contains the complete design documents and an empty
> workspace scaffold. No feature is implemented yet. Implementation follows the
> milestones in [`docs/13-roadmap.md`](docs/13-roadmap.md).

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
| `crates/planyrith-cli` | `planyrith` binary: every tool as a subcommand, scripting, headless export, JSON-RPC server. |
| `crates/planyrith-gui` | `planyrith-gui` binary: egui/eframe shell with a wgpu viewport; desktop and WebAssembly. |
| `AGENTS.md` | Working instructions for AI agents and contributors implementing the spec. |
| `deny.toml` | Licensing and "pure Rust" dependency policy enforced by `cargo-deny`. |

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

## Building (scaffold)

```sh
cargo check --workspace
cargo run -p planyrith-cli      # prints the version
cargo run -p planyrith-gui      # prints the version
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Contributions are accepted under the same terms.
