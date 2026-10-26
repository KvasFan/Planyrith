//! Implements docs/03-geometry-kernel.md §7 (inference engine) and the shared
//! constraint model of docs/05 §2: snap candidates, priorities, references,
//! axis locks. Visuals live in the GUI (docs/07 §5); the CLI exposes the engine
//! as `planyrith infer` (docs/08 §12).
//!
//! Candidates come from one ray cast (vertices, edges, the front face, with
//! occluded elements dropped), edge midpoints, curve centers, edge × edge
//! intersections, and directional lines from the reference point (axes,
//! parallel/perpendicular to remembered edges, tangents, alignment with
//! remembered points); the tool's constraint or the active lock then
//! projects them. Ordering: point snaps, then directional inferences, then
//! the face, the constraint plane and the free point (docs/03 Decisions).

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::{Camera, Document, EntityPath, Projection};
use crate::geom::CurveDescriptor;
use crate::math::{Axis, Plane, Ray, Segment};
use crate::spatial::{DocumentIndex, HitFilter, HitKind};
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
    /// Every kind in priority order.
    pub const ALL: [InferenceKind; 13] = [
        InferenceKind::Endpoint,
        InferenceKind::Midpoint,
        InferenceKind::Intersection,
        InferenceKind::Center,
        InferenceKind::OnEdge,
        InferenceKind::OnFace,
        InferenceKind::Tangent,
        InferenceKind::Perpendicular,
        InferenceKind::Parallel,
        InferenceKind::FromPoint,
        InferenceKind::OnAxis,
        InferenceKind::OnPlane,
        InferenceKind::Free,
    ];

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

impl InferenceRequest {
    /// The request with an axis lock along the document's drawing axes
    /// (`axes`) instead of the world's.
    pub fn in_axes(&self, axes: &glam::DAffine3) -> InferenceRequest {
        let mut out = *self;
        if let Constraint::Axis { origin, axis } = self.constraint
            && !axes.matrix3.abs_diff_eq(glam::DMat3::IDENTITY, 1e-12)
        {
            out.constraint = Constraint::Line(Ray::new(
                origin,
                (axes.matrix3 * axis.direction()).normalize_or(axis.direction()),
            ));
        }
        out
    }
}

impl Constraint {
    /// The line of an `Axis` or `Line` constraint.
    fn line(&self) -> Option<(DVec3, DVec3)> {
        match *self {
            Constraint::Axis { origin, axis } => Some((origin, axis.direction())),
            Constraint::Line(r) => Some((r.origin, r.dir)),
            _ => None,
        }
    }

    /// `p` moved onto the constraint (along the plane normal, or to the
    /// nearest point of the line).
    fn project(&self, p: DVec3) -> DVec3 {
        match *self {
            Constraint::Free => p,
            Constraint::Plane(plane) => plane.project(p),
            _ => self.line().map_or(p, |(o, d)| o + d * (p - o).dot(d)),
        }
    }
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

    /// The metrics of a camera drawing into a viewport `height_px` pixels high.
    pub fn for_camera(camera: &Camera, height_px: f64) -> Self {
        let height_px = height_px.max(1.0);
        match camera.projection {
            Projection::Parallel { height_mm } => ViewMetrics {
                perspective: false,
                mm_per_pixel_at_unit_depth: height_mm / height_px,
            },
            Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => {
                ViewMetrics {
                    perspective: true,
                    mm_per_pixel_at_unit_depth: 2.0 * libm::tan(fov_y_deg.to_radians() * 0.5)
                        / height_px,
                }
            }
        }
    }

    /// A fixed world tolerance for `pixels` (headless queries, `infer`).
    pub fn fixed(tolerance_mm: f64, pixels: f64) -> Self {
        ViewMetrics {
            perspective: false,
            mm_per_pixel_at_unit_depth: tolerance_mm / pixels.max(f64::MIN_POSITIVE),
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
    /// Where the guide line of a directional inference starts (the reference
    /// point; the remembered point for `FromPoint`).
    #[schemars(with = "Option<[f64; 3]>")]
    pub from: Option<DVec3>,
    /// Direction of the edge or guide line (locks, overlays).
    #[schemars(with = "Option<[f64; 3]>")]
    pub direction: Option<DVec3>,
    /// The face or constraint plane the point lies on (locks, tools).
    pub plane: Option<Plane>,
    /// The remembered point a perpendicular projection comes from: the
    /// segment from it to `point` is perpendicular to `direction`.
    #[serde(default)]
    #[schemars(with = "Option<[f64; 3]>")]
    pub via: Option<DVec3>,
}

impl Inference {
    fn new(point: DVec3, kind: InferenceKind) -> Self {
        Inference {
            point,
            kind,
            on: None,
            locked: false,
            label: kind.label().to_string(),
            axis: None,
            from: None,
            direction: None,
            plane: None,
            via: None,
        }
    }

    /// `true` for the kinds that constrain a direction from a reference.
    pub const fn is_directional(&self) -> bool {
        matches!(
            self.kind,
            InferenceKind::Tangent
                | InferenceKind::Perpendicular
                | InferenceKind::Parallel
                | InferenceKind::FromPoint
                | InferenceKind::OnAxis
        )
    }
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

/// A remembered hover reference (docs/03 §7 "Hover memory"): the entity, the
/// point hovered on it and, for an edge, its world direction.
#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    /// The hovered occurrence.
    pub path: EntityPath,
    /// The point hovered (an endpoint, a point on the edge or face).
    pub point: DVec3,
    /// The edge's world direction (parallel/perpendicular inferences).
    pub direction: Option<DVec3>,
}

/// Inference engine state: hover references and the active lock (docs/03 §7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InferenceEngine {
    references: Vec<Reference>,
    lock: Option<(Lock, Constraint)>,
}

/// The pixel-distance class of a candidate: point snaps beat directional
/// inferences, which beat surfaces (docs/03 Decisions, inference order).
fn rank(kind: InferenceKind) -> u8 {
    match kind {
        InferenceKind::Endpoint
        | InferenceKind::Midpoint
        | InferenceKind::Intersection
        | InferenceKind::Center
        | InferenceKind::OnEdge => kind.priority(),
        InferenceKind::Tangent
        | InferenceKind::Perpendicular
        | InferenceKind::Parallel
        | InferenceKind::FromPoint
        | InferenceKind::OnAxis => kind.priority() - 1,
        InferenceKind::OnFace => InferenceKind::OnAxis.priority(),
        InferenceKind::OnPlane | InferenceKind::Free => kind.priority(),
    }
}

/// The point of the line `(o, d)` nearest to the ray, and its distance to it.
fn line_near_ray(o: DVec3, d: DVec3, ray: &Ray) -> Option<(DVec3, f64)> {
    let w = o - ray.origin;
    let b = d.dot(ray.dir);
    let denom = 1.0 - b * b;
    if denom <= 1e-12 {
        return None; // parallel to the view ray
    }
    let s = (b * w.dot(ray.dir) - w.dot(d)) / denom;
    let p = o + d * s;
    Some((p, ray.distance_to_point(p)))
}

/// Closest points of two infinite lines; `None` when they are parallel.
fn lines_closest(o1: DVec3, d1: DVec3, o2: DVec3, d2: DVec3) -> Option<(DVec3, DVec3)> {
    let r = o1 - o2;
    let (a, e, b) = (d1.dot(d1), d2.dot(d2), d1.dot(d2));
    let (c, f) = (d1.dot(r), d2.dot(r));
    let denom = a * e - b * b;
    if denom <= 1e-12 * a * e {
        return None;
    }
    let s = (b * f - c * e) / denom;
    let t = (a * f - b * c) / denom;
    Some((o1 + d1 * s, o2 + d2 * t))
}

/// The label of a drawing axis line.
fn axis_label(axis: Axis) -> &'static str {
    match axis {
        Axis::X => "Red Axis",
        Axis::Y => "Green Axis",
        Axis::Z => "Blue Axis",
    }
}

/// Closest points of two segments (for edge × edge intersections).
fn segments_closest(a: &Segment, b: &Segment) -> (DVec3, DVec3) {
    let d1 = a.b - a.a;
    let d2 = b.b - b.a;
    let r = a.a - b.a;
    let (aa, ee, f) = (d1.dot(d1), d2.dot(d2), d2.dot(r));
    if aa <= f64::MIN_POSITIVE || ee <= f64::MIN_POSITIVE {
        return (a.a, b.a);
    }
    let c = d1.dot(r);
    let bb = d1.dot(d2);
    let denom = aa * ee - bb * bb;
    let mut s = if denom > 1e-12 * aa * ee {
        ((bb * f - c * ee) / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut t = (bb * s + f) / ee;
    if t < 0.0 {
        t = 0.0;
        s = (-c / aa).clamp(0.0, 1.0);
    } else if t > 1.0 {
        t = 1.0;
        s = ((bb - c) / aa).clamp(0.0, 1.0);
    }
    (a.a + d1 * s, b.a + d2 * t)
}

/// A direction perpendicular to `d` in the horizontal plane (or along X when
/// `d` is vertical).
fn horizontal_perpendicular(d: DVec3) -> DVec3 {
    d.cross(DVec3::Z).try_normalize().unwrap_or(DVec3::X)
}

impl InferenceEngine {
    /// Maximum remembered references (docs/03 §7 "max 3 references").
    pub const MAX_REFERENCES: usize = 3;

    /// Fresh engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current references, oldest first.
    pub fn references(&self) -> &[Reference] {
        &self.references
    }

    /// Adds a hover/`--ref` reference, evicting the oldest beyond the maximum.
    /// `direction` is the edge's world direction for edge references.
    pub fn add_reference(&mut self, path: EntityPath, point: DVec3, direction: Option<DVec3>) {
        if let Some(pos) = self.references.iter().position(|r| r.path == path) {
            self.references.remove(pos);
        }
        if self.references.len() == Self::MAX_REFERENCES {
            self.references.remove(0);
        }
        self.references.push(Reference {
            path,
            point,
            direction: direction.and_then(DVec3::try_normalize),
        });
    }

    /// Adds the inference's entity as a reference (the GUI's hover memory).
    /// Only inferences on the entity itself count: an alignment with a
    /// remembered point names that point's entity but lies elsewhere.
    pub fn remember(&mut self, inference: &Inference) {
        let on_entity = matches!(
            inference.kind,
            InferenceKind::Endpoint
                | InferenceKind::Midpoint
                | InferenceKind::Intersection
                | InferenceKind::Center
                | InferenceKind::OnEdge
                | InferenceKind::OnFace
        );
        if let Some(path) = inference.on.as_ref().filter(|_| on_entity) {
            let direction = match inference.kind {
                InferenceKind::OnEdge | InferenceKind::Midpoint => inference.direction,
                _ => None,
            };
            self.add_reference(path.clone(), inference.point, direction);
        }
    }

    /// Forgets all references.
    pub fn clear_references(&mut self) {
        self.references.clear();
    }

    /// Active lock, if any.
    pub fn lock(&self) -> Option<Lock> {
        self.lock.map(|(l, _)| l)
    }

    /// The constraint the lock imposes, if any.
    pub fn lock_constraint(&self) -> Option<Constraint> {
        self.lock.map(|(_, c)| c)
    }

    /// Engages a lock relative to the given current inference (docs/03 §7):
    /// an axis through the reference point; for `Current`, the line of a
    /// directional or edge inference, the plane of a face, or the direction
    /// from the reference to the point; for `ParallelPerpendicular`, the
    /// direction of the last referenced edge (or its horizontal
    /// perpendicular, whichever the current direction is closer to).
    pub fn engage_lock(&mut self, lock: Lock, current: &Inference, req: &InferenceRequest) {
        let origin = req.reference_point.unwrap_or(current.point);
        let line = |dir: DVec3, through: DVec3| {
            dir.try_normalize()
                .map(|d| Constraint::Line(Ray::new(through, d)))
        };
        let constraint = match lock {
            Lock::Axis(axis) => Some(Constraint::Axis { origin, axis }),
            Lock::Current => match current.kind {
                _ if current.is_directional() => current
                    .direction
                    .and_then(|d| line(d, current.from.unwrap_or(origin))),
                InferenceKind::OnFace | InferenceKind::OnPlane => {
                    current.plane.map(Constraint::Plane)
                }
                InferenceKind::OnEdge => current.direction.and_then(|d| line(d, current.point)),
                _ => line(current.point - origin, origin),
            },
            Lock::ParallelPerpendicular => {
                let edge = self.references.iter().rev().find_map(|r| r.direction);
                edge.and_then(|u| {
                    let v = horizontal_perpendicular(u);
                    let current_dir = (current.point - origin).normalize_or_zero();
                    let dir = if current_dir.dot(u).abs() >= current_dir.dot(v).abs() {
                        u
                    } else {
                        v
                    };
                    line(dir, origin)
                })
            }
        };
        self.lock = constraint.map(|c| (lock, c));
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
        self.candidates(doc, index, req)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                let mut free = Inference::new(
                    Self::resolve_free_point(req, doc.tolerances()),
                    InferenceKind::Free,
                );
                free.locked = self.lock.is_some();
                free
            })
    }

    /// All candidates within tolerance, sorted best first (debugging, goldens).
    pub fn candidates(
        &self,
        doc: &Document,
        index: &DocumentIndex,
        req: &InferenceRequest,
    ) -> Vec<Inference> {
        let tol = doc.tolerances();
        // The drawing axes of the document (docs/05 §5.10).
        let axes = doc.info.axes;
        let axes_origin = axes.translation;
        let dir_of = |axis: Axis| (axes.matrix3 * axis.direction()).normalize_or(axis.direction());
        let req = &req.in_axes(&axes);
        let constraint = self.lock_constraint().unwrap_or(req.constraint);
        let locked = self.lock.is_some();
        let ray = &req.ray;
        let world_tol = |t: f64| {
            req.view
                .world_tolerance(req.pixel_tolerance, t)
                .max(tol.length)
        };
        let within = |p: DVec3| ray.distance_to_point(p) <= world_tol(ray.closest_param(p));

        // Broad phase with the tolerance at the model's far side; each hit is
        // then checked at its own depth.
        let bounds = doc.aabb();
        let far = if bounds.is_empty() {
            1.0e6
        } else {
            bounds
                .corners()
                .iter()
                .map(|c| (*c - ray.origin).length())
                .fold(1.0, f64::max)
        };
        let filter = HitFilter {
            proximity: world_tol(far),
            ..HitFilter::default()
        };
        let hits = index.ray_cast(doc, ray, &filter);
        // Elements behind the first face are hidden from view.
        let front = hits
            .iter()
            .filter(|h| matches!(h.kind, HitKind::Face(_)))
            .map(|h| h.t)
            .fold(f64::INFINITY, f64::min);
        let visible = |t: f64| t <= front + world_tol(t);

        let mut out: Vec<Inference> = Vec::new();
        let mut edges: Vec<(Segment, EntityPath)> = Vec::new();
        let mut face_done = false;
        for hit in &hits {
            match hit.kind {
                HitKind::Vertex(_) if visible(hit.t) && within(hit.point) => {
                    let mut i = Inference::new(hit.point, InferenceKind::Endpoint);
                    i.on = Some(hit.path.clone());
                    out.push(i);
                }
                HitKind::Edge(e) if visible(hit.t) => {
                    let Some((seg, center)) = edge_world(doc, &hit.path, e) else {
                        continue;
                    };
                    let direction = seg.direction();
                    if within(hit.point) {
                        let mut i = Inference::new(hit.point, InferenceKind::OnEdge);
                        i.on = Some(hit.path.clone());
                        i.direction = Some(direction);
                        out.push(i);
                    }
                    let mid = seg.midpoint();
                    if within(mid) {
                        let mut i = Inference::new(mid, InferenceKind::Midpoint);
                        i.on = Some(hit.path.clone());
                        i.direction = Some(direction);
                        out.push(i);
                    }
                    if let Some(c) = center
                        && within(c)
                    {
                        let mut i = Inference::new(c, InferenceKind::Center);
                        i.on = Some(hit.path.clone());
                        out.push(i);
                    }
                    if edges.len() < 8 {
                        edges.push((seg, hit.path.clone()));
                    }
                }
                HitKind::Face(f) if !face_done => {
                    face_done = true;
                    let mut i = Inference::new(hit.point, InferenceKind::OnFace);
                    i.on = Some(hit.path.clone());
                    i.plane = face_world_plane(doc, &hit.path, f);
                    out.push(i);
                    // The centers of the curves bounding the face under the
                    // cursor (a circle, a polygon, an arc's pie), without
                    // hovering the curve first.
                    for (c, edge) in face_curve_centers(doc, &hit.path, f) {
                        if within(c) {
                            let mut i = Inference::new(c, InferenceKind::Center);
                            i.on = Some(edge);
                            out.push(i);
                        }
                    }
                }
                _ => {}
            }
        }
        // The origin of the drawing axes snaps like an endpoint unless a face
        // hides it (docs/03 Decisions).
        if within(axes_origin) && visible(ray.closest_param(axes_origin)) {
            let mut i = Inference::new(axes_origin, InferenceKind::Endpoint);
            i.label = "Origin".into();
            out.push(i);
        }
        // Edge × edge intersections near the cursor.
        for (i, (a, pa)) in edges.iter().enumerate() {
            for (b, _) in &edges[i + 1..] {
                let (p, q) = segments_closest(a, b);
                if p.distance(q) <= tol.length && within(p) {
                    let mut x = Inference::new(p, InferenceKind::Intersection);
                    x.on = Some(pa.clone());
                    out.push(x);
                }
            }
        }
        // Centers of remembered curves.
        for r in &self.references {
            if let Ok(res) = doc.resolve(&r.path)
                && let crate::ids::EntityId::Edge(e) = res.entity
                && let Some((_, Some(c))) = edge_world(doc, &r.path, e)
                && within(c)
            {
                let mut i = Inference::new(c, InferenceKind::Center);
                i.on = Some(r.path.clone());
                out.push(i);
            }
        }

        // Directional inferences from the reference point.
        if let Some(origin) = req.reference_point
            && constraint.line().is_none()
        {
            let mut lines: Vec<(InferenceKind, DVec3, Option<Axis>, Option<EntityPath>)> =
                Vec::new();
            for r in &self.references {
                if let Some(u) = r.direction {
                    lines.push((InferenceKind::Parallel, u, None, Some(r.path.clone())));
                    lines.push((
                        InferenceKind::Perpendicular,
                        horizontal_perpendicular(u),
                        None,
                        Some(r.path.clone()),
                    ));
                }
            }
            for (kind, path, dir) in tangents_at(doc, index, origin, tol) {
                lines.push((kind, dir, None, Some(path)));
            }
            for axis in Axis::ALL {
                lines.push((InferenceKind::OnAxis, dir_of(axis), Some(axis), None));
            }
            for (kind, dir, axis, path) in &lines {
                let (kind, dir, axis) = (*kind, *dir, *axis);
                if let Some((p, _)) = line_near_ray(origin, dir, ray)
                    && within(p)
                    && p.distance(origin) > tol.length
                {
                    let mut i = Inference::new(p, kind);
                    i.from = Some(origin);
                    i.direction = Some(dir);
                    i.axis = axis;
                    i.on = path.clone();
                    i.label = match (kind, axis) {
                        (InferenceKind::OnAxis, Some(Axis::X)) => "On Red Axis".into(),
                        (InferenceKind::OnAxis, Some(Axis::Y)) => "On Green Axis".into(),
                        (InferenceKind::OnAxis, Some(Axis::Z)) => "On Blue Axis".into(),
                        (InferenceKind::Parallel, _) => "Parallel to Edge".into(),
                        (InferenceKind::Perpendicular, _) => "Perpendicular to Edge".into(),
                        (InferenceKind::Tangent, _) => "Tangent at Vertex".into(),
                        _ => kind.label().into(),
                    };
                    out.push(i);
                }
            }
            for (_, dir, axis, path) in &lines {
                // Where the line crosses the drawing axes.
                for drawing_axis in Axis::ALL {
                    if let Some((p, q)) =
                        lines_closest(origin, *dir, axes_origin, dir_of(drawing_axis))
                        && p.distance(q) <= tol.length
                        && p.distance(origin) > tol.length
                        && within(p)
                    {
                        let mut i = Inference::new(p, InferenceKind::Intersection);
                        i.from = Some(origin);
                        i.direction = Some(*dir);
                        i.axis = *axis;
                        i.on = path.clone();
                        i.label = format!("Intersection with {}", axis_label(drawing_axis));
                        out.push(i);
                    }
                }
                // The perpendicular projection of each remembered point onto
                // the line.
                for r in &self.references {
                    let foot = origin + *dir * (r.point - origin).dot(*dir);
                    if foot.distance(origin) > tol.length
                        && foot.distance(r.point) > tol.length
                        && within(foot)
                    {
                        let mut i = Inference::new(foot, InferenceKind::FromPoint);
                        i.from = Some(origin);
                        i.direction = Some(*dir);
                        i.axis = *axis;
                        i.via = Some(r.point);
                        i.on = Some(r.path.clone());
                        i.label = "Perpendicular from Point".into();
                        out.push(i);
                    }
                }
            }
            // Alignment with remembered points along the axes.
            for r in &self.references {
                for axis in Axis::ALL {
                    if let Some((p, _)) = line_near_ray(r.point, dir_of(axis), ray)
                        && within(p)
                        && p.distance(r.point) > tol.length
                    {
                        let mut i = Inference::new(p, InferenceKind::FromPoint);
                        i.from = Some(r.point);
                        i.direction = Some(dir_of(axis));
                        i.axis = Some(axis);
                        i.on = Some(r.path.clone());
                        out.push(i);
                    }
                }
            }
        }

        // The drawing axes (docs/03 Decisions): points on them near the
        // cursor (not through faces, and on the constraint plane if any),
        // and where nearby model edges cross them.
        let on_constraint = |p: DVec3| match constraint {
            Constraint::Free => true,
            Constraint::Plane(plane) => plane.contains(p, tol),
            _ => false,
        };
        for axis in Axis::ALL {
            let d = dir_of(axis);
            if let Some((p, _)) = line_near_ray(axes_origin, d, ray)
                && within(p)
                && visible(ray.closest_param(p))
                && on_constraint(p)
                && p.distance(axes_origin) > tol.length
            {
                let mut i = Inference::new(p, InferenceKind::OnAxis);
                i.axis = Some(axis);
                i.from = Some(axes_origin);
                i.direction = Some(d);
                i.label = format!("On {}", axis_label(axis));
                out.push(i);
            }
            for (seg, path) in &edges {
                let along = seg.b - seg.a;
                if let Some((p, q)) = lines_closest(seg.a, along, axes_origin, d)
                    && p.distance(q) <= tol.length
                    && (p - seg.a).dot(along) >= -tol.length * along.length()
                    && (p - seg.b).dot(along) <= tol.length * along.length()
                    && within(p)
                    && on_constraint(p)
                {
                    let mut i = Inference::new(p, InferenceKind::Intersection);
                    i.on = Some(path.clone());
                    i.axis = Some(axis);
                    i.label = format!("Intersection with {}", axis_label(axis));
                    out.push(i);
                }
            }
        }

        // On a constraint line (a lock, Push/Pull's normal): the
        // perpendicular projections of remembered points and where the line
        // crosses the drawing axes.
        if let Some((o, d)) = constraint.line() {
            for r in &self.references {
                let foot = o + d * (r.point - o).dot(d);
                if foot.distance(r.point) > tol.length && within(foot) {
                    let mut i = Inference::new(foot, InferenceKind::FromPoint);
                    i.from = Some(o);
                    i.direction = Some(d);
                    i.via = Some(r.point);
                    i.on = Some(r.path.clone());
                    i.label = "Perpendicular from Point".into();
                    out.push(i);
                }
            }
            for axis in Axis::ALL {
                if let Some((p, q)) = lines_closest(o, d, axes_origin, dir_of(axis))
                    && p.distance(q) <= tol.length
                    && within(p)
                {
                    let mut i = Inference::new(p, InferenceKind::Intersection);
                    i.label = format!("Intersection with {}", axis_label(axis));
                    out.push(i);
                }
            }
        }

        // The constraint plane, and the free point.
        if let Constraint::Plane(plane) = constraint
            && let Some(t) = ray.intersect_plane(&plane, tol)
            && t > 0.0
        {
            let mut i = Inference::new(ray.at(t), InferenceKind::OnPlane);
            i.plane = Some(plane);
            out.push(i);
        }
        let mut free = Inference::new(
            Self::resolve_free_point(&InferenceRequest { constraint, ..*req }, tol),
            InferenceKind::Free,
        );
        if matches!(constraint, Constraint::Free) && free.point.z.abs() <= tol.length {
            free.plane = Some(Plane::from_point_normal(DVec3::ZERO, DVec3::Z));
        }
        out.push(free);

        // Onto the constraint: points elsewhere give their projection ("to
        // this point"); surfaces give the line's point nearest the cursor.
        let line = constraint.line();
        for i in &mut out {
            i.locked = locked;
            match (constraint, line) {
                (Constraint::Free, _) => {}
                (_, Some((o, d))) => {
                    if matches!(
                        i.kind,
                        InferenceKind::OnFace | InferenceKind::OnPlane | InferenceKind::Free
                    ) {
                        if let Some((p, _)) = line_near_ray(o, d, ray) {
                            i.point = p;
                        } else {
                            i.point = constraint.project(i.point);
                        }
                    } else {
                        i.point = constraint.project(i.point);
                    }
                    i.from.get_or_insert(o);
                    i.direction.get_or_insert(d);
                }
                (Constraint::Plane(plane), _) => {
                    i.point = plane.project(i.point);
                    if i.plane.is_none() {
                        i.plane = Some(plane);
                    }
                }
                _ => {}
            }
        }

        // Best first: kind class, then depth along the ray, then the order
        // the hits came in (stable sort).
        let mut keyed: Vec<(u8, f64, usize, Inference)> = out
            .into_iter()
            .enumerate()
            .map(|(n, i)| (rank(i.kind), ray.closest_param(i.point), n, i))
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        keyed.into_iter().map(|(_, _, _, i)| i).collect()
    }

    /// Free-point depth resolution: ray–constraint plane, else ray–ground, else a
    /// vertical plane through `last_point` facing the camera (docs/03 §7).
    pub fn resolve_free_point(req: &InferenceRequest, tol: &Tolerances) -> DVec3 {
        let ray = &req.ray;
        match req.constraint {
            Constraint::Plane(plane) => {
                if let Some(t) = ray.intersect_plane(&plane, tol)
                    && t > 0.0
                {
                    return ray.at(t);
                }
                plane.project(ray.at(1000.0))
            }
            Constraint::Axis { .. } | Constraint::Line(_) => {
                let (o, d) = req.constraint.line().unwrap_or((ray.origin, ray.dir));
                line_near_ray(o, d, ray).map_or(o, |(p, _)| p)
            }
            Constraint::Free => {
                let ground = Plane::from_point_normal(DVec3::ZERO, DVec3::Z);
                if let Some(t) = ray.intersect_plane(&ground, tol)
                    && t > 0.0
                {
                    return ray.at(t);
                }
                let anchor = req.last_point.or(req.reference_point);
                let facing = DVec3::new(-ray.dir.x, -ray.dir.y, 0.0).try_normalize();
                if let (Some(anchor), Some(normal)) = (anchor, facing) {
                    let wall = Plane::from_point_normal(anchor, normal);
                    if let Some(t) = ray.intersect_plane(&wall, tol)
                        && t > 0.0
                    {
                        return ray.at(t);
                    }
                }
                ray.at(1000.0)
            }
        }
    }
}

/// An edge occurrence's world segment and, for curve edges, the curve's
/// world center.
fn edge_world(
    doc: &Document,
    path: &EntityPath,
    e: crate::ids::EdgeId,
) -> Option<(Segment, Option<DVec3>)> {
    let resolved = doc.resolve(path).ok()?;
    let mesh = doc.container(resolved.container)?.mesh();
    let seg = mesh.edge_segment(e).ok()?.transformed(&resolved.world);
    let center = mesh
        .edge(e)
        .and_then(|edge| edge.curve)
        .and_then(|c| mesh.curve(c))
        .and_then(CurveDescriptor::center)
        .map(|c| resolved.world.transform_point3(c));
    Some((seg, center))
}

/// The centers of the curves (circles, arcs, polygons) on a face
/// occurrence's boundary that lie in its plane, one per curve, with an
/// edge of each.
fn face_curve_centers(
    doc: &Document,
    path: &EntityPath,
    f: crate::ids::FaceId,
) -> Vec<(DVec3, EntityPath)> {
    let Ok(resolved) = doc.resolve(path) else {
        return Vec::new();
    };
    let Some(mesh) = doc.container(resolved.container).map(|c| c.mesh()) else {
        return Vec::new();
    };
    let Some(plane) = mesh.face(f).map(|face| face.plane) else {
        return Vec::new();
    };
    let tol = doc.tolerances();
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for e in mesh.face_edges(f).unwrap_or_default() {
        let Some(curve) = mesh.edge(e).and_then(|edge| edge.curve) else {
            continue;
        };
        if seen.contains(&curve) {
            continue;
        }
        seen.push(curve);
        // Only centers in the face's plane: a cylinder's side facets do not
        // offer the center of its cap through themselves.
        if let Some(c) = mesh
            .curve(curve)
            .and_then(CurveDescriptor::center)
            .filter(|c| plane.contains(*c, tol))
        {
            let edge = EntityPath::new(
                path.instances.clone(),
                crate::document::EntityKey::Id(crate::ids::EntityId::Edge(e)),
            );
            out.push((resolved.world.transform_point3(c), edge));
        }
    }
    out
}

/// The world plane of a face occurrence.
fn face_world_plane(doc: &Document, path: &EntityPath, f: crate::ids::FaceId) -> Option<Plane> {
    let resolved = doc.resolve(path).ok()?;
    let face = doc.container(resolved.container)?.mesh().face(f)?;
    Some(face.plane.transformed(&resolved.world))
}

/// Tangent directions at `origin`: for each curve edge (arc, circle) ending
/// at `origin`, the direction continuing it (docs/05 §3.7 "Tangent at
/// vertex").
fn tangents_at(
    doc: &Document,
    index: &DocumentIndex,
    origin: DVec3,
    tol: &Tolerances,
) -> Vec<(InferenceKind, EntityPath, DVec3)> {
    let mut out = Vec::new();
    let probe = crate::math::Aabb::from_points([origin]).expanded(tol.length);
    for leaf in index.aabb_query(&probe) {
        let Some(entities) = doc.container(leaf.container) else {
            continue;
        };
        let mesh = entities.mesh();
        let local = leaf.world.inverse().transform_point3(origin);
        for (e, edge) in mesh.edges() {
            if edge.curve.is_none() {
                continue;
            }
            let Ok(seg) = mesh.edge_segment(e) else {
                continue;
            };
            let dir = if tol.same_point(seg.b, local) {
                seg.b - seg.a
            } else if tol.same_point(seg.a, local) {
                seg.a - seg.b
            } else {
                continue;
            };
            let Some(pid) = entities.pid_of(crate::ids::EntityId::Edge(e)) else {
                continue;
            };
            let Some(dir) = leaf.world.transform_vector3(dir).try_normalize() else {
                continue;
            };
            let path = EntityPath::new(leaf.path.clone(), crate::document::EntityKey::Pid(pid));
            out.push((InferenceKind::Tangent, path, dir));
        }
    }
    // Pid order, not slot order (deterministic tie-breaks).
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
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
            e.add_reference(
                EntityPath::in_model(EntityKey::Pid(Pid(i))),
                DVec3::ZERO,
                None,
            );
        }
        assert_eq!(e.references().len(), 3);
        assert_eq!(e.references()[0].path.target, EntityKey::Pid(Pid(2)));
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

    use std::sync::Arc;

    use serde_json::json;

    use crate::commands::{CommandRegistry, Engine};

    fn scene() -> Engine {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let mut run = |name: &str, params: serde_json::Value| {
            e.execute_erased(name, params)
                .unwrap_or_else(|err| panic!("{name}: {err}"));
        };
        run(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 1000, 0] }),
        );
        run(
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 1000.0 }),
        );
        // Crossing lines in different containers (in one container the
        // second would split the first at a shared vertex).
        run("line", json!({ "points": [[2000, 0, 0], [3000, 1000, 0]] }));
        run("make-group", json!({ "targets": "last" }));
        run(
            "line",
            json!({ "points": [[2000, 800, 0], [3200, 200, 0]] }),
        );
        run(
            "circle",
            json!({ "center": [5000, 0, 0], "radius": 500.0, "normal": [0, 0, 1] }),
        );
        e
    }

    #[test]
    fn the_origin_snaps_like_an_endpoint() {
        let e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let engine = InferenceEngine::new();
        let i = infer(&e, &engine, &request(down(4.0, -3.0)));
        assert_eq!(i.kind, InferenceKind::Endpoint);
        assert_eq!(i.label, "Origin");
        assert!(close(i.point, DVec3::ZERO));
        // Away from it, a free point.
        let i = infer(&e, &engine, &request(down(400.0, -300.0)));
        assert_eq!(i.kind, InferenceKind::Free);
    }

    fn down(x: f64, y: f64) -> Ray {
        Ray::new(DVec3::new(x, y, 10_000.0), DVec3::NEG_Z)
    }

    fn request(ray: Ray) -> InferenceRequest {
        InferenceRequest {
            ray,
            pixel_tolerance: InferenceRequest::DEFAULT_PIXEL_TOLERANCE,
            view: ViewMetrics::fixed(20.0, 8.0),
            constraint: Constraint::Free,
            reference_point: None,
            last_point: None,
        }
    }

    fn infer(e: &Engine, engine: &InferenceEngine, req: &InferenceRequest) -> Inference {
        let index = DocumentIndex::build(e.document());
        engine.infer(e.document(), &index, req)
    }

    fn close(a: DVec3, b: DVec3) -> bool {
        a.distance(b) < 1e-6
    }

    #[test]
    fn point_snaps_on_a_box() {
        let e = scene();
        let engine = InferenceEngine::new();
        let i = infer(&e, &engine, &request(down(1005.0, 995.0)));
        assert_eq!(i.kind, InferenceKind::Endpoint);
        assert!(
            close(i.point, DVec3::new(1000.0, 1000.0, 1000.0)),
            "{}",
            i.point
        );
        assert!(i.on.is_some());

        let i = infer(&e, &engine, &request(down(503.0, 6.0)));
        assert_eq!(i.kind, InferenceKind::Midpoint);
        assert!(close(i.point, DVec3::new(500.0, 0.0, 1000.0)));

        let i = infer(&e, &engine, &request(down(300.0, 5.0)));
        assert_eq!(i.kind, InferenceKind::OnEdge);
        assert!(close(i.point, DVec3::new(300.0, 0.0, 1000.0)));
        assert!(i.direction.is_some());

        let i = infer(&e, &engine, &request(down(400.0, 400.0)));
        assert_eq!(i.kind, InferenceKind::OnFace);
        assert!(close(i.point, DVec3::new(400.0, 400.0, 1000.0)));
        assert!(i.plane.is_some_and(|p| p.normal.z.abs() > 0.99));

        let i = infer(&e, &engine, &request(down(8000.0, 8000.0)));
        assert_eq!(i.kind, InferenceKind::Free);
        assert!(close(i.point, DVec3::new(8000.0, 8000.0, 0.0)));

        // Crossing lines and a circle's center (after hovering the circle).
        let i = infer(&e, &engine, &request(down(2537.0, 530.0)));
        assert_eq!(i.kind, InferenceKind::Intersection);
        assert!(
            close(i.point, DVec3::new(7600.0 / 3.0, 1600.0 / 3.0, 0.0)),
            "{}",
            i.point
        );
        // Over the circle's face its center snaps without hovering the
        // circle first.
        let i = infer(&e, &engine, &request(down(5006.0, 4.0)));
        assert_eq!(i.kind, InferenceKind::Center, "{i:?}");
        assert!(close(i.point, DVec3::new(5000.0, 0.0, 0.0)));
        let mut hovered = InferenceEngine::new();
        let on_circle = infer(&e, &hovered, &request(down(5498.8, 34.9)));
        assert_eq!(on_circle.kind, InferenceKind::OnEdge, "{on_circle:?}");
        assert!(on_circle.on.is_some());
        hovered.remember(&on_circle);
        let i = infer(&e, &hovered, &request(down(5006.0, 4.0)));
        assert_eq!(i.kind, InferenceKind::Center);
        assert!(close(i.point, DVec3::new(5000.0, 0.0, 0.0)));
    }

    #[test]
    fn directions_from_the_reference_point() {
        let e = scene();
        let mut engine = InferenceEngine::new();
        let mut req = request(down(9000.0, 7.0));
        req.reference_point = Some(DVec3::new(7000.0, 0.0, 0.0));
        let i = infer(&e, &engine, &req);
        assert_eq!(i.kind, InferenceKind::OnAxis);
        assert_eq!(i.axis, Some(Axis::X));
        assert_eq!(i.label, "On Red Axis");
        assert!(close(i.point, DVec3::new(9000.0, 0.0, 0.0)));
        assert_eq!(i.from, Some(DVec3::new(7000.0, 0.0, 0.0)));

        // Parallel to a remembered diagonal edge.
        let diagonal = infer(&e, &engine, &request(down(2300.0, 297.0)));
        assert_eq!(diagonal.kind, InferenceKind::OnEdge);
        engine.remember(&diagonal);
        let mut req = request(down(9000.0 + 600.0, 600.0 + 4.0));
        req.reference_point = Some(DVec3::new(9000.0, 0.0, 0.0));
        let i = infer(&e, &engine, &req);
        assert_eq!(i.kind, InferenceKind::Parallel, "{i:?}");
        assert!(
            close(i.point, DVec3::new(9602.0, 602.0, 0.0)),
            "{}",
            i.point
        );

        // Above the box's top face the axis wins over the face.
        let mut req = request(down(700.0, 503.0));
        req.reference_point = Some(DVec3::new(200.0, 500.0, 1000.0));
        let i = infer(&e, &engine, &req);
        assert_eq!(i.kind, InferenceKind::OnAxis, "{i:?}");
        assert!(close(i.point, DVec3::new(700.0, 500.0, 1000.0)));
    }

    #[test]
    fn the_drawing_axes_snap_and_cross() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased(
            "line",
            json!({ "points": [[3000, -500, 0], [3000, 1500, 0]] }),
        )
        .unwrap();
        let mut engine = InferenceEngine::new();

        // On an axis away from any geometry.
        let i = infer(&e, &engine, &request(down(-2000.0, 6.0)));
        assert_eq!(i.kind, InferenceKind::OnAxis, "{i:?}");
        assert_eq!(i.label, "On Red Axis");
        assert!(close(i.point, DVec3::new(-2000.0, 0.0, 0.0)));

        // Where an edge crosses an axis.
        let i = infer(&e, &engine, &request(down(3004.0, 5.0)));
        assert_eq!(i.kind, InferenceKind::Intersection, "{i:?}");
        assert_eq!(i.label, "Intersection with Red Axis");
        assert!(close(i.point, DVec3::new(3000.0, 0.0, 0.0)));

        // Drawing along green from (1000, 2000, 0), the line crosses the red
        // axis.
        let mut req = request(down(1004.0, 3.0));
        req.reference_point = Some(DVec3::new(1000.0, 2000.0, 0.0));
        let i = infer(&e, &engine, &req);
        assert_eq!(i.kind, InferenceKind::Intersection, "{i:?}");
        assert!(close(i.point, DVec3::new(1000.0, 0.0, 0.0)));

        // Perpendicular projection of a point passed over: drawing along
        // red from (-1000, 2500, 0), level with the line's end (3000, 1500).
        let end = infer(&e, &engine, &request(down(3002.0, 1498.0)));
        assert_eq!(end.kind, InferenceKind::Endpoint);
        engine.remember(&end);
        let mut req = request(down(3005.0, 2504.0));
        req.reference_point = Some(DVec3::new(-1000.0, 2500.0, 0.0));
        let i = infer(&e, &engine, &req);
        assert_eq!(i.kind, InferenceKind::FromPoint, "{i:?}");
        assert!(
            close(i.point, DVec3::new(3000.0, 2500.0, 0.0)),
            "{}",
            i.point
        );
        assert_eq!(i.via, Some(DVec3::new(3000.0, 1500.0, 0.0)));

        // The same on a locked line.
        let mut req = request(down(3005.0, 2504.0));
        req.reference_point = Some(DVec3::new(-1000.0, 2500.0, 0.0));
        req.constraint = Constraint::Axis {
            origin: DVec3::new(-1000.0, 2500.0, 0.0),
            axis: Axis::X,
        };
        let i = infer(&e, &engine, &req);
        assert_eq!(i.kind, InferenceKind::FromPoint, "{i:?}");
        assert!(close(i.point, DVec3::new(3000.0, 2500.0, 0.0)));
    }

    #[test]
    fn locks_and_constraints_project_the_snaps() {
        let e = scene();
        let mut engine = InferenceEngine::new();
        let mut req = request(down(1003.0, 998.0));
        req.reference_point = Some(DVec3::new(0.0, 0.0, 0.0));
        let current = infer(&e, &engine, &req);
        engine.engage_lock(Lock::Axis(Axis::Z), &current, &req);
        assert_eq!(engine.lock(), Some(Lock::Axis(Axis::Z)));
        // The corner's height along the locked axis ("to this point").
        let i = infer(&e, &engine, &req);
        assert_eq!(i.kind, InferenceKind::Endpoint);
        assert!(i.locked);
        assert!(close(i.point, DVec3::new(0.0, 0.0, 1000.0)), "{}", i.point);
        engine.release_lock();

        // Shift on a face locks its plane.
        let mut req = request(down(400.0, 400.0));
        let on_face = infer(&e, &engine, &req);
        engine.engage_lock(Lock::Current, &on_face, &req);
        req.ray = down(8000.0, 8000.0);
        let i = infer(&e, &engine, &req);
        assert!(
            close(i.point, DVec3::new(8000.0, 8000.0, 1000.0)),
            "{}",
            i.point
        );
        engine.release_lock();

        // A plane constraint projects an endpoint onto it.
        let mut req = request(down(1003.0, 998.0));
        req.constraint = Constraint::Plane(Plane::from_point_normal(
            DVec3::new(0.0, 0.0, 500.0),
            DVec3::Z,
        ));
        let i = infer(&e, &engine, &req);
        assert_eq!(i.kind, InferenceKind::Endpoint);
        assert!(close(i.point, DVec3::new(1000.0, 1000.0, 500.0)));
    }

    #[test]
    fn free_points_without_ground_use_a_wall_through_the_last_point() {
        let tol = Tolerances::default();
        let mut req = request(Ray::new(
            DVec3::new(0.0, -5000.0, 100.0),
            DVec3::new(0.0, 1.0, 0.2).normalize(),
        ));
        req.last_point = Some(DVec3::new(0.0, 0.0, 0.0));
        let p = InferenceEngine::resolve_free_point(&req, &tol);
        assert!(p.y.abs() < 1e-6 && p.z > 100.0, "{p}");
        req.constraint = Constraint::Axis {
            origin: DVec3::ZERO,
            axis: Axis::X,
        };
        let p = InferenceEngine::resolve_free_point(&req, &tol);
        assert!(p.y.abs() < 1e-9 && p.z.abs() < 1e-9);
    }
}
