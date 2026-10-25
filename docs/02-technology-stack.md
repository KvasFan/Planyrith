# 02 — Technology stack

**Audience:** implementing agents. Normative for dependency choices; alternatives are recorded so
future decisions do not re-litigate them without new evidence.

Constraints that drove every choice: **Rust only** (no vendored C/C++), **permissive open
licenses**, **one code base for desktop and WebAssembly**, **performance of a 3D viewport with
high-frequency interaction**.

## 1. Decision table

| Concern | Decision | License | Alternatives considered | Why |
|---|---|---|---|---|
| GUI toolkit (desktop + web) | **egui + eframe** | MIT/Apache-2.0 | Dioxus, iced, Slint | See §2.1. Immediate-mode fits tool/viewport interaction; first-class wgpu integration; mature wasm. |
| GUI ↔ GPU integration | **egui-wgpu** (+ `egui-winit` via eframe) | MIT/Apache-2.0 | custom glue | Official, maintained with egui. |
| Docking / panel layout | **egui_tiles** | MIT/Apache-2.0 | egui_dock | Simpler model (tree of tiles), maintained by egui's author; supports tabs, splits, drag. |
| File open/save dialogs | **egui-file-dialog** | MIT | rfd (native dialogs), own widget | Pure egui, so it renders identically on desktop and in the browser and needs no platform dialog bindings. |
| 3D rendering | **wgpu** | MIT/Apache-2.0 | glow (OpenGL), vulkano, ash | Portable (Vulkan/Metal/DX12/GL/WebGPU/WebGL2), pure Rust, the standard. |
| Windowing/input | **winit** (via eframe) | Apache-2.0 | sdl2 (C) | Pure Rust, wasm support. |
| Linear algebra | **glam** (`f64` types `DVec3/DMat4/DAffine3` in core; `f32` in render) | MIT/Apache-2.0 | nalgebra, cgmath | SIMD-fast, simple API, both precisions, serde support. `nalgebra` only if a solver is needed (feature-gated). |
| Transcendental functions | **libm** (pure-Rust port of a portable C math library), also enabled as glam's `libm` feature | MIT/Apache-2.0 | `std` `f64::sin` etc. | `std` calls the platform math library, whose last-bit rounding differs between OSes; byte-identical output across platforms (docs/08 §10) needs one implementation everywhere. |
| Exact CAD kernel (B-rep/NURBS) | **truck** family (`truck-geometry`, `truck-topology`, `truck-modeling`, `truck-meshalgo`, `truck-stepio`) behind feature `brep` | Apache-2.0 | OpenCascade (C++, excluded), fornjot kernel (early) | Only mature pure-Rust B-rep kernel; STEP I/O; used for exact solids, fillets later, STEP exchange. |
| Interactive mesh representation | **custom half-edge mesh** in core | — | plexus, baby_shark, truck polymesh | Direct modeling needs face finding/splitting/merging on planar polygon faces with holes; no crate offers exactly that with stable ids. |
| Triangulation of planar polygons with holes | **earcutr** (ear clipping) with a fallback to own implementation | ISC | lyon (2D path tessellation, MIT/Apache), spade (CDT, MIT/Apache) | Small, robust, fast for CAD polygons. `spade` used when constrained Delaunay quality is required (texture-mapped large faces). |
| Picking / spatial queries | **parry3d-f64** (ray casts, AABB, `Qbvh`) | Apache-2.0 | bvh crate, own BVH | Battle-tested, f64 variant, queries we need (ray/AABB/segment/triangle). |
| Serialization | **serde** | MIT/Apache-2.0 | — | Standard. |
| Native binary encoding | **rmp-serde** (MessagePack) | MIT | bincode, postcard, CBOR | Self-describing (forward-compatible field skipping), compact, fast, many tools can inspect it. |
| Native container | **zip** crate (`deflate-flate2` feature + `flate2` `rust_backend`, i.e. `miniz_oxide`) | MIT | tar, custom | Random access to parts, textures as separate members, previews readable by any zip tool. |
| JSON (CLI output, RPC, schemas) | **serde_json**, **schemars** | MIT/Apache-2.0 | — | Standard. |
| CLI parsing | **clap** (derive) | MIT/Apache-2.0 | argh, lexopt | Rich help, shell completions, mirrors `Params` structs. |
| Logging/diagnostics | **tracing**, **tracing-subscriber** | MIT | log | Structured spans for profiling commands. |
| Errors | **thiserror** (core), **anyhow** (binaries) | MIT/Apache-2.0 | — | Standard. |
| Parallelism (desktop) | **rayon** behind feature `parallel` | MIT/Apache-2.0 | std threads | Data-parallel algorithms; must be deterministic (see `docs/01 §6`). |
| Images / textures | **image** (PNG, JPEG, WebP decode via pure-Rust codecs) | MIT/Apache-2.0 | — | Pure Rust. |
| glTF | **gltf** | MIT/Apache-2.0 | own | Complete, maintained. |
| OBJ | **tobj** (import); own writer | MIT | obj-rs | Simple and robust. |
| STL | own (format is trivial) | — | stl_io | Avoid a dependency for 30 lines. |
| DXF | **dxf** crate | MIT | own | Covers entities we need (LINE, LWPOLYLINE, ARC, CIRCLE, 3DFACE, INSERT, TEXT, DIMENSION). |
| STEP | **truck-stepio** (feature `brep`) | Apache-2.0 | — | Only pure-Rust option. |
| PDF export (sheets) | **printpdf** or **pdf-writer** | MIT/Apache-2.0 | — | Pure Rust; `pdf-writer` is lower level but lean; choose at milestone 0.9. |
| SVG export | own writer (simple) | — | svg crate | Trivial. |
| Text shaping in viewport | **cosmic-text** or egui's own text via `epaint` | MIT/Apache-2.0 | glyphon | Labels/dimensions rendered through egui painter layer; 3D text uses glyph outlines from `ttf-parser` (MIT/Apache-2.0) extruded by the core. |
| Scripting language | **rhai** behind feature `scripting` | MIT/Apache-2.0 | mlua (C), rune, koto, wasmtime plugins | Pure Rust, sandboxed, wasm-compatible, simple syntax, serde interop. WASM plugins via `wasmtime` (Apache-2.0 w/ LLVM exception) are a desktop-only extension for a later milestone. |
| WebAssembly glue | **wasm-bindgen**, **web-sys**, **js-sys**, **wasm-bindgen-futures** | MIT/Apache-2.0 | — | Standard; eframe uses them. |
| Browser storage | **rexie** (IndexedDB wrapper) behind a `Storage` trait | MIT/Apache-2.0 | idb crate, raw web-sys | Ergonomic async API; alternative `idb` acceptable. |
| Compression inside containers | **flate2** with default `miniz_oxide` backend; **zstd** excluded (C) | MIT/Apache-2.0 | lz4_flex (MIT) for history journal | Pure Rust. |
| Hashing / content ids | **blake3** (pure Rust build) | CC0/Apache-2.0 | sha2 | Fast content hashes for texture dedup. |
| Ids | own newtypes over a slot map (`slotmap` crate acceptable) | MIT/Apache-2.0 | uuid | Dense, cache-friendly, stable within a document; UUIDs only for component definitions shared across files (`uuid` crate). |
| Property testing | **proptest** | MIT/Apache-2.0 | quickcheck | Richer strategies. |
| CLI tests | **assert_cmd**, **predicates** | MIT/Apache-2.0 | — | Standard. |
| Benchmarks | **criterion** | MIT/Apache-2.0 | divan | Standard. |
| Fuzzing | **cargo-fuzz** / **arbitrary** | MIT/Apache-2.0 | — | Importers and the mesh kernel. |
| License enforcement | **cargo-deny** (`deny.toml`) | — | cargo-about | Fails CI on disallowed licenses or C `-sys` crates. |

## 2. Evaluations

### 2.1 GUI: egui vs Dioxus (requested evaluation)

**Chosen: egui + eframe.**

Dioxus is an excellent framework for document-style and form-heavy UIs with a React-like
component model, and it was the initial proposal. It was rejected for Planyrith's main
application for these reasons:

1. **Viewport integration.** Planyrith's central widget is a GPU viewport receiving pointer
   events at 120+ Hz with per-frame custom drawing (inference marks, rubber-band previews,
   dimension text). egui's painter and `egui-wgpu` callbacks render the 3D scene *inside* the
   same frame and same input stream as the panels. With Dioxus Desktop (a WebView renderer) or
   Dioxus Native (Blitz, still maturing), the viewport would be a separate surface bridged over
   an event boundary, adding latency and complexity on three platforms plus web.
2. **Performance model.** Immediate-mode UI redraws only when input or data changes and has no
   virtual-DOM diffing; for hundreds of property widgets bound to a large selection this is
   simpler and faster to keep correct.
3. **Docking.** `egui_tiles` provides dockable, tabbed, draggable panels today. There is no
   equivalent in the Dioxus ecosystem without building one.
4. **WebAssembly parity.** eframe's web runner renders the whole UI on a canvas via WebGPU/WebGL2,
   so desktop and web are pixel-identical and share 100 % of UI code.
5. **Ecosystem fit.** Several production 3D/graphics tools in Rust use egui + wgpu, so the
   integration patterns (custom paint callbacks, texture handling, HiDPI) are well documented.

Dioxus remains a reasonable choice for *auxiliary* web pages (a documentation site, a
component-library browser) if ever needed; it is **not** used in the application binaries.

**iced** was considered (Elm architecture, wgpu backend) and rejected because custom widgets
with heavy per-frame painting and docking are more work than in egui. **Slint** was rejected
because its GPL/commercial dual licensing conflicts with the permissive-only policy.

### 2.2 Math: glam vs nalgebra

Both are permissive and pure Rust. Planyrith needs two precisions: `f64` for the geometry
kernel (millimeter coordinates for buildings hundreds of meters wide, with 1e-3 mm merge
tolerance) and `f32` for GPU buffers. `glam` provides both (`DVec3`, `DMat4`, `DAffine3` and
`Vec3`, `Mat4`) with a small, fast API and serde support. `nalgebra` is more general (any
dimension, decompositions) but heavier to compile and more verbose. Decision: `glam`
everywhere; `nalgebra` may be added behind a feature if an algorithm requires a linear solver
(e.g. plane fitting by SVD — but a covariance/eigen approach in `glam` suffices for 3×3).

The `truck` kernel uses `cgmath` internally. The `geom::brep` bridge converts at the boundary;
`cgmath` is therefore an indirect dependency only under the `brep` feature.

### 2.3 Kernel strategy: half-edge mesh first, B-rep when exactness matters

Direct modeling of planar faces (the 95 % case) is best served by a **polygonal half-edge mesh**
with stable ids, because the core operations — find faces from edge loops, split a face by an
edge, merge coplanar faces when an edge is erased, push/pull with automatic merge — are graph
operations, not surface-evaluation problems. Curves (arcs, circles) are stored as *curve
descriptors* attached to polyline edges ("curve groups") so they segment at adjustable
resolution yet still behave as single edges for selection and inference.

`truck` is used, behind the `brep` feature, for:
- exact solid booleans when the half-edge boolean (`geom::boolean`) reports a degenerate case,
- STEP import/export,
- future exact fillets/chamfers and surface-surface intersections.

Conversion both ways (`HalfEdgeMesh ⇄ truck::Solid`) is in `geom::brep` and is lossless for
planar solids.

### 2.4 Native format encoding: rmp-serde + zip

MessagePack via `rmp-serde` is compact and self-describing, so old readers can skip unknown
fields and new readers can supply defaults. Wrapping it in a zip container gives random access
to textures and previews and lets other tools inspect the file. `bincode`/`postcard` were
rejected because they are not self-describing (forward compatibility would require manual
versioning of every struct). A pure-JSON format is offered as an *export* (`planyrith export
--format json`) for debugging and interoperability, not as the native format.

### 2.5 Picking: parry3d vs GPU id buffer

Both are implemented. CPU ray casting via `parry3d-f64`'s `Qbvh` is authoritative (exact, works
headless, same code for inference). The GPU id buffer (`docs/06 §8`) is an acceleration used
by the GUI for hover highlighting on very large models; its result is always confirmed by a CPU
cast against the candidate's geometry.

### 2.6 Scripting: rhai vs Lua vs WASM

`rhai` is pure Rust (no C), sandboxed (operation limits, no filesystem unless provided), runs
in wasm and binds serde types directly, so the command registry can be exposed automatically.
Lua via `mlua` requires a C runtime (excluded). WASM component plugins via `wasmtime` are
powerful but add ~10 MB and do not run inside the web build; they are a post-1.0 desktop feature.

## 3. Licensing policy

- Project license: **MIT OR Apache-2.0**.
- Allowed dependency licenses: MIT, Apache-2.0 (with or without LLVM exception), BSD-2-Clause,
  BSD-3-Clause, ISC, Zlib, BSL-1.0, CC0-1.0, Unicode-3.0/Unicode-DFS-2016, MPL-2.0 (file-level
  copyleft; acceptable), OFL-1.1 and Ubuntu Font License for bundled fonts.
- **GPL, LGPL, AGPL**: not allowed in `planyrith-core`, `planyrith-render`, `planyrith-cli` or
  `planyrith-gui`. They may be used only in optional, separately distributed plugins and only
  with an explicit `deny.toml` allow entry and a note here.
- "Rust only": any crate that compiles or links vendored C/C++ (typically `*-sys`) is banned
  (`deny.toml [bans]`). Pure-Rust bindings to OS-provided APIs (Vulkan via `ash`, Metal via
  `metal`, Win32 via `windows`, Web APIs via `web-sys`) are allowed.
- Bundled fonts must be OFL/Ubuntu-licensed; bundled icons must be MIT/CC0/Apache.
- `cargo deny check licenses bans sources` runs in CI on every PR.

## 4. Version pinning

- Use caret requirements (`"0.x"`) in `Cargo.toml` and commit `Cargo.lock` for binaries.
- Upgrade `wgpu`/`egui` together (they are released in lockstep) at most once per milestone.
- MSRV: Rust 1.95 (edition 2024), dictated by the egui 0.36 release line. Bump only at minor
  releases, together with the egui/wgpu upgrade.

## 5. Platform notes

| Platform | Graphics backend (wgpu) | Notes |
|---|---|---|
| Linux | Vulkan (GL fallback) | X11 and Wayland via winit. |
| macOS | Metal | Universal binaries (x86_64 + aarch64). |
| Windows | DX12 (Vulkan/GL fallback) | MSVC toolchain. |
| Web | WebGPU, WebGL2 fallback | WebGL2 path disables MSAA > 4 and compute-based features. |

## Decisions

- 2026-10-20 — egui + eframe chosen over Dioxus for the application UI (§2.1).
- 2026-10-20 — glam for all math; nalgebra only behind a feature if needed (§2.2).
- 2026-10-20 — Custom half-edge mesh is the primary kernel; truck behind `brep` (§2.3).
- 2026-10-20 — rmp-serde inside a zip container for `.plyr` (§2.4).
- 2026-10-20 — rhai for scripting; WASM plugins post-1.0 (§2.6).
- 2026-10-20 — Copyleft dependencies excluded from all shipped crates (§3).
- 2026-10-20 — First pinned GUI line is egui/eframe/egui-wgpu 0.36 with wgpu 30, egui_tiles 0.17
  and egui-file-dialog 0.15. egui 0.37 was current but egui-file-dialog had not yet followed it;
  both lines use wgpu 30, so the upgrade to 0.37 is a lockstep bump with no renderer change.
- 2026-10-20 — glam is pinned to the 0.33 line because parry3d-f64 0.31 reaches glam through
  `glamx` ^0.3 (glam ^0.33); one glam copy lets the core pass vectors to the picking kernel
  without conversions. Move to 0.34 when parry does.
- 2026-10-20 — MSRV raised from 1.85 to 1.95 (§4): every egui 0.36/0.37 crate requires it.
- 2026-10-20 — `flate2` is a direct dependency of the core with `rust_backend` only, because the
  zip crate declares flate2 without a backend and its default `deflate` feature selects
  `zlib-rs` plus `zopfli`; the explicit feature keeps the container on `miniz_oxide`.
- 2026-10-20 — BSL-1.0 (Boost Software License, permissive) added to the allow-list: eframe's
  Windows clipboard path pulls `clipboard-win` and `error-code` under it (§3).
- 2026-10-20 — `truck-*` and `rhai` are not pinned yet; the `brep` and `scripting` features are
  declared as no-ops until their milestones.
- 2026-10-21 — **Deterministic math.** The core computes `sin`, `cos`, `tan`, `atan2`, `pow` and
  the other transcendental functions with the `libm` crate, and glam is built with its `libm`
  feature, so the same input gives the same bits on every OS (docs/08 §10). Square roots and
  arithmetic are exactly rounded by IEEE 754 everywhere and need nothing. A crate-level
  `crates/planyrith-core/clippy.toml` forbids the `std` methods in the core.
- 2026-10-25 — `deny.toml` ignores four "unmaintained" advisories (RUSTSEC-2026-0192 `ttf-parser`,
  -0370 `zvariant`, -0371 `zbus_names`, -0372 `zvariant_derive`; no known vulnerability) that only
  the GUI placeholder pulls in through eframe 0.36 (window decorations and Linux accessibility).
  Accepted for 0.1, where the GUI is not implemented; to be reviewed with the 0.2 GUI work
  (replace or keep, with a new dated entry).
