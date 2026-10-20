//! Implements docs/10-extensions-api.md §2 (scripting binding), compiled only
//! with the `scripting` feature. The `rhai` crate is not pinned yet (docs/02
//! §1, docs/13 milestone 0.8), so this module declares the contract only:
//!
//! - one script function per registered command, generated from
//!   `commands::CommandRegistry` (kebab-case → snake_case, params as a map);
//! - `query`, `select`, `context`, `enter`, `exit_context`;
//! - unit helpers `mm`, `cm`, `m`, `inch`, `ft`, `deg`, `rad` (bare numbers are
//!   millimeters; angles default to radians);
//! - read-only document views, attribute access, `group(label, fn)` for one
//!   undo step, progress/cancel;
//! - operation/memory/time limits with deterministic `time`/`random`
//!   capabilities under `--deterministic`.
//!
//! Owner key: `later`.

/// `true` once the embedded runtime is linked.
pub const RUNTIME_AVAILABLE: bool = false;
