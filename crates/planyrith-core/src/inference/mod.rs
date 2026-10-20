//! Implements docs/03-geometry-kernel.md §7 (inference engine) and the shared
//! constraint model of docs/05 §2: snap candidates, priorities, references,
//! axis locks. Visuals live in the GUI (docs/07 §5); the CLI exposes the engine
//! as `planyrith infer` (docs/08 §12).
//!
//! Owner key: `later` (milestone 0.3).

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::{Document, EntityPath};
use crate::math::{Axis, Plane, Ray};
use crate::spatial::DocumentIndex;
use crate::tolerance::Tolerances;

/// Inference kinds in priority order — a lower discriminant wins when several
/// candidates are within pixel tolerance (docs/03 §7).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum InferenceKind {
    /// Vertex — green.
    Endpoint,
    /// Edge midpoint — cyan.
    Midpoint,
    /// Edge×edge (screen space) or edge×face — black/red.
    Intersection,
    /// Arc/circle center — blue.
    Center,
    /// On an edge — red.
    OnEdge,
    /// On a face — blue.
    OnFace,
    /// Tangent to an edge/arc — magenta.
    Tangent,
    /// Perpendicular to an edge — magenta.
    Perpendicular,
    /// Parallel to an edge — magenta.
    Parallel,
    /// Axis alignment with a remembered reference point — dotted colored line.
    FromPoint,
    /// On a drawing axis — red/green/blue.
    OnAxis,
    /// On the ground or constraint plane.
    OnPlane,
    /// Free point on the ground/constraint plane.
    Free,
}

impl InferenceKind {
    /// Numeric priority (0 = highest), docs/03 §7 order.
    pub const fn priority(self) -> u8 {
        self as u8
    }

    /// Default status-bar label (docs/07 §5).
    pub const fn label(self) -> &'static str {
        match self {
            InferenceKind::Endpoint => "Endpoint",
            InferenceKind::Midpoint => "Midpoint",
            InferenceKind::Intersection => "Intersection",
            InferenceKind::Center => "Center",
            InferenceKind::OnEdge => "On Edge",
            InferenceKind::OnFace => "On Face",
            InferenceKind::Tangent => "Tangent",
            InferenceKind::Perpendicular => "Perpendicular",
            InferenceKind::Parallel => "Parallel",
            InferenceKind::FromPoint => "From Point",
            InferenceKind::OnAxis => "On Axis",
            InferenceKind::OnPlane => "On Plane",
            InferenceKind::Free => "",
        }
    }
}

/// The current tool's input constraint (docs/03 §7 "constraint").
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Constraint {
    /// No constraint (ground plane for first points).
    Free,
    /// Points are projected onto this plane.
    Plane(Plane),
    /// Points are projected onto this axis through `origin`.
    Axis {
        /// Line origin.
        origin: DVec3,
        /// Axis.
        axis: Axis,
    },
    /// Points are projected onto an arbitrary line.
    Line(Ray),
}

/// Perspective/parallel scale information needed to convert a pixel tolerance
/// to a world distance at a given depth (docs/03 §7).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewMetrics {
    /// `true` for perspective projections.
    pub perspective: bool,
    /// World mm per pixel at depth 1 mm (perspective) or everywhere (parallel).
    pub mm_per_pixel_at_unit_depth: f64,
}

impl ViewMetrics {
    /// World-space tolerance for `pixels` at ray depth `t`.
    pub fn world_tolerance(&self, pixels: f64, t: f64) -> f64 {
        if self.perspective {
            pixels * self.mm_per_pixel_at_unit_depth * t.max(0.0)
        } else {
            pixels * self.mm_per_pixel_at_unit_depth
        }
    }
}

/// One inference query (docs/03 §7 "Input").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InferenceRequest {
    /// Screen ray.
    pub ray: Ray,
    /// Pixel tolerance (default 8 px).
    pub pixel_tolerance: f64,
    /// View scale.
    pub view: ViewMetrics,
    /// Tool constraint.
    pub constraint: Constraint,
    /// Reference point for "from point" inferences and relative lengths.
    pub reference_point: Option<DVec3>,
    /// Last placed point (depth fallback when the ray misses the ground).
    pub last_point: Option<DVec3>,
}

impl InferenceRequest {
    /// Default pixel tolerance (docs/03 §7).
    pub const DEFAULT_PIXEL_TOLERANCE: f64 = 8.0;
}

/// The engine's answer (docs/03 §7 "Output").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Inference {
    /// Snapped point in world space.
    #[schemars(with = "[f64; 3]")]
    pub point: DVec3,
    /// Kind.
    pub kind: InferenceKind,
    /// Entity the inference came from.
    pub on: Option<EntityPath>,
    /// `true` while an axis/inference lock is active.
    pub locked: bool,
    /// Status label.
    pub label: String,
    /// Axis for `OnAxis` / `FromPoint` (drives the color).
    pub axis: Option<Axis>,
}

/// Lock state (docs/03 §7 "Axis lock", docs/05 §2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Lock {
    /// Shift: lock the current inference's constraint.
    Current,
    /// Arrow keys: lock to a drawing axis.
    Axis(Axis),
    /// `↓`: parallel or perpendicular to the hovered edge.
    ParallelPerpendicular,
}

/// Inference engine state: hover references and the active lock (docs/03 §7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InferenceEngine {
    references: Vec<EntityPath>,
    lock: Option<(Lock, Constraint)>,
}

impl InferenceEngine {
    /// Maximum remembered references (docs/03 §7 "max 3 references").
    pub const MAX_REFERENCES: usize = 3;

    /// Fresh engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current references, oldest first.
    pub fn references(&self) -> &[EntityPath] {
        &self.references
    }

    /// Adds a hover/`--ref` reference, evicting the oldest beyond the maximum.
    pub fn add_reference(&mut self, path: EntityPath) {
        if let Some(pos) = self.references.iter().position(|p| *p == path) {
            self.references.remove(pos);
        }
        if self.references.len() == Self::MAX_REFERENCES {
            self.references.remove(0);
        }
        self.references.push(path);
    }

    /// Forgets all references.
    pub fn clear_references(&mut self) {
        self.references.clear();
    }

    /// Active lock, if any.
    pub fn lock(&self) -> Option<Lock> {
        self.lock.map(|(l, _)| l)
    }

    /// Engages a lock relative to the given current inference.
    pub fn engage_lock(&mut self, lock: Lock, current: &Inference, req: &InferenceRequest) {
        todo!("docs/03 §7 axis lock")
    }

    /// Releases the lock.
    pub fn release_lock(&mut self) {
        self.lock = None;
    }

    /// Computes the best inference for a request (docs/03 §7 rules; ties broken
    /// by priority, then smaller `t`, then lower id — deterministic).
    pub fn infer(
        &self,
        doc: &Document,
        index: &DocumentIndex,
        req: &InferenceRequest,
    ) -> Inference {
        todo!("docs/03 §7 infer")
    }

    /// All candidates within tolerance, sorted best first (debugging, goldens).
    pub fn candidates(
        &self,
        doc: &Document,
        index: &DocumentIndex,
        req: &InferenceRequest,
    ) -> Vec<Inference> {
        todo!("docs/03 §7 candidate enumeration")
    }

    /// Free-point depth resolution: ray–constraint plane, else ray–ground, else a
    /// vertical plane through `last_point` facing the camera (docs/03 §7).
    pub fn resolve_free_point(req: &InferenceRequest, tol: &Tolerances) -> DVec3 {
        todo!("docs/03 §7 free point depth resolution")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::EntityKey;
    use crate::ids::Pid;

    #[test]
    fn priorities_follow_spec_order() {
        assert!(InferenceKind::Endpoint.priority() < InferenceKind::Midpoint.priority());
        assert!(InferenceKind::OnEdge.priority() < InferenceKind::OnFace.priority());
        assert_eq!(InferenceKind::Free.priority(), 12);
    }

    #[test]
    fn references_are_capped_at_three() {
        let mut e = InferenceEngine::new();
        for i in 1..=4 {
            e.add_reference(EntityPath::in_model(EntityKey::Pid(Pid(i))));
        }
        assert_eq!(e.references().len(), 3);
        assert_eq!(e.references()[0].target, EntityKey::Pid(Pid(2)));
    }

    #[test]
    fn view_metrics_scale_with_depth() {
        let v = ViewMetrics {
            perspective: true,
            mm_per_pixel_at_unit_depth: 0.001,
        };
        assert!((v.world_tolerance(8.0, 1000.0) - 8.0).abs() < 1e-12);
        let p = ViewMetrics {
            perspective: false,
            mm_per_pixel_at_unit_depth: 2.0,
        };
        assert_eq!(p.world_tolerance(8.0, 1000.0), 16.0);
    }
}
