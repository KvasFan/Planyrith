//! Implements docs/03-geometry-kernel.md §9 (B-rep bridge), compiled only with
//! the `brep` feature. The `truck` crates are not pinned yet (docs/02 §1,
//! docs/13 milestone 0.8), so this module currently declares the contract only:
//!
//! - `to_solid(&HalfEdgeMesh) -> truck::Solid` — planar faces become planar
//!   surfaces, loops become wires of line/arc edges using curve descriptors.
//! - `from_solid(&truck::Solid, tess_res) -> HalfEdgeMesh` — curved faces are
//!   tessellated with curve descriptors retained for cylinders/arcs.
//! - `boolean(...)` — exact fallback for `geom::boolean` (`NonRobust` cases).
//!
//! Round trip of a planar solid must be exact within `tolerances.length`.
//! Owner key: `later`.

/// `true` once the exact kernel is linked; frontends use it to decide whether a
/// `NonRobust` boolean can be retried.
pub const KERNEL_AVAILABLE: bool = false;
