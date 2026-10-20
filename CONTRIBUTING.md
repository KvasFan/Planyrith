# Contributing to Planyrith

Thank you for your interest. Planyrith is at version 0.0.0: the specification is being
finalized and the code base is an empty scaffold. Contributions at this stage are most
valuable as **spec reviews**, **design discussions** and **prototype proposals** that
follow the roadmap.

## Ground rules

- Read [`AGENTS.md`](AGENTS.md); it applies to humans too.
- Rust only, permissive open-source dependencies only (`deny.toml`).
- The core crate stays headless. The CLI exposes everything. The GUI is a client.
- Do not reference other software products in docs or code.
- Be kind and specific in reviews. Assume good faith.

## Workflow

1. Open an issue describing the change and the spec section it affects.
2. For spec changes, edit the relevant `docs/*.md` and add a dated entry to its
   "Decisions" subsection explaining the rationale.
3. For code (from 0.1.0 onward), follow the tool checklist in `AGENTS.md` §4.
4. Ensure `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test --workspace` and `cargo deny check` pass.
5. Open a pull request referencing the issue. Keep PRs focused on one spec section.

## Licensing of contributions

By contributing you agree that your contributions are licensed under the project's
dual license, MIT OR Apache-2.0, without additional terms.
