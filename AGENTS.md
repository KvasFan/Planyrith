# Instructions for AI agents and contributors

This file tells an implementing agent how to work in this repository. The product
specification itself lives in `docs/`. Read this file first, then the docs in order.

## 1. Reading order

1. `docs/00-overview.md` — what Planyrith is, scope, non-goals, personas.
2. `docs/01-architecture.md` — the core/CLI/GUI split and the command system. **Everything else depends on this.**
3. `docs/02-technology-stack.md` — approved crates, evaluated alternatives, licensing policy.
4. `docs/04-document-model.md` and `docs/03-geometry-kernel.md` — the data you operate on.
5. `docs/05-modeling-tools.md` — the tool catalogue; each tool is one command.
6. `docs/08-cli-specification.md` — the user-facing command surface.
7. The remaining documents as the milestone you are working on requires (`docs/13-roadmap.md`).
8. `docs/14-glossary.md` whenever a term is unclear. Use the glossary's terms in code and docs.

## 2. Non-negotiable rules

- **Language:** Rust only. No C/C++ code, no `-sys` crates that build or link vendored C libraries (see `deny.toml`). System graphics/windowing APIs reached through pure-Rust bindings (`wgpu`, `winit`) are fine.
- **Licenses:** dependencies must be MIT, Apache-2.0, BSD-2/3, ISC, Zlib, MPL-2.0 or equivalent. Copyleft (GPL/LGPL/AGPL) only for optional, separable components and only with an explicit `deny.toml` entry and a note in `docs/02-technology-stack.md`. Run `cargo deny check` before proposing a dependency.
- **Architecture boundaries:** `planyrith-core` must never depend on a GUI toolkit, a GPU API, a windowing library or `std::fs` outside of the `io::storage` abstraction. CI will fail if it does.
- **CLI first:** a tool or operation is accepted only when it (a) is a `Command` in the core registry with serde-typed parameters, (b) is reachable from `planyrith` as a subcommand and via JSON-RPC, (c) has tests, (d) is documented in `docs/05-modeling-tools.md` or `docs/08-cli-specification.md`. Interactive GUI behavior comes after, never instead.
- **Determinism:** no randomness, no wall-clock or locale dependence in the core. Iteration order over entities must be stable (use ordered ids, not hash-map iteration order, anywhere results are observable).
- **Units and precision:** lengths are `f64` millimeters internally. Use the tolerance constants from `docs/03-geometry-kernel.md`; never compare floats with `==`.
- **No references to other software.** The documentation and code must not name, cite, or imitate the user-visible details of any existing commercial or proprietary product. Describe behavior in Planyrith's own terms using the glossary.
- **Safety:** `unsafe_code` is denied workspace-wide and forbidden in `planyrith-core`. If a GPU interop case truly requires `unsafe` in `planyrith-gui`, isolate it in one module with a `// SAFETY:` comment and a test.

## 3. Coding standards

- Edition 2024, stable toolchain (`rust-toolchain.toml`). `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace` and `cargo deny check` must pass.
- Public items have doc comments. Modules start with a `//!` summary that names the spec section they implement, e.g. `//! Implements docs/05-modeling-tools.md §4.3 Push/Pull`.
- Errors: `thiserror` enums in the core, `anyhow` only in binaries. Never `unwrap()`/`expect()` in the core outside tests.
- Entity ids are opaque newtypes (`FaceId`, `EdgeId`, …); never expose raw integers in APIs.
- Prefer small, pure functions for geometry with property-based tests (`proptest`).
- Every importer/exporter has golden-file round-trip tests under `crates/planyrith-core/tests/fixtures/`.
- Commit messages: imperative subject ≤ 72 chars, body explains *why*, reference the spec section (e.g. `Implement Offset tool (docs/05 §4.9)`).

## 4. How to add a tool (checklist)

1. Specify it (or confirm the existing spec) in `docs/05-modeling-tools.md`: purpose, parameters, interactive steps, inference used, algorithm, edge cases, undo, acceptance tests.
2. Add a `Command` implementation in `planyrith-core::commands::<tool>` with a serde `Params` struct and a `Result` struct. Register it in the command registry with a stable kebab-case name.
3. Add the subcommand mapping in `planyrith-cli` (clap derive mirrors `Params`) and verify `planyrith tool <name> --help` renders every parameter with units.
4. Add tests: unit tests on the geometry, a CLI integration test (`assert_cmd`) that runs the command on a fixture and compares the resulting native file to a golden file.
5. Only then add the interactive tool in `planyrith-gui::tools::<tool>`: a state machine that collects `Params` from mouse/keyboard/inference and dispatches the same command.
6. Update `docs/08-cli-specification.md` and the CHANGELOG.

## 5. What to do when the spec is silent or ambiguous

- Prefer the simplest behavior that is consistent with the glossary and existing tools.
- Document the decision in the relevant `docs/` file in a "Decisions" subsection, dated, with rationale.
- Do not invent user-visible features outside the roadmap milestone you are working on.

## 6. Repository conventions

- `docs/` is the source of truth. Code comments point to docs, not the other way round.
- Fixtures and golden files are small (< 1 MB each) and generated by a reproducible script in `tools/` once that directory exists.
- Version numbers follow SemVer; 0.0.0 is specification-only; the first code lands as 0.1.0.
