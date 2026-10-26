//! Editing tools (docs/05 §4): Push/Pull, Offset, Move, Rotate, Scale, Eraser
//! and Paint. They act inside the edit context, on faces under the cursor or
//! on the selection (the hovered entity when nothing is selected).

use egui::Color32;
use glam::{DVec2, DVec3};
use planyrith_core::document::EntityPath;
use planyrith_core::ids::EntityId;
use planyrith_core::inference::Constraint;
use planyrith_core::math::{Aabb, Axis, Plane, Ray};
use planyrith_core::units::ValueFieldInput;
use serde_json::{Value, json};

use super::{BAND, Cx, Drag, Flow, Pointer, Shape, Tool, along, angle, length, parse, pt, targets};

/// A face of the edit context's container under the cursor: its pid, world
/// plane and world outer loop.
#[derive(Debug, Clone)]
struct PickedFace {
    pid: u64,
    plane: Plane,
    outer: Vec<DVec3>,
}

fn picked_face(p: &Pointer, cx: &Cx<'_>) -> Option<PickedFace> {
    let element = p.face.as_ref()?.canonical(cx.doc);
    if element.instances != cx.context.path.canonical(cx.doc) {
        return None;
    }
    let resolved = cx.doc.resolve(&element).ok()?;
    let EntityId::Face(f) = resolved.entity else {
        return None;
    };
    let mesh = cx.doc.container(resolved.container)?.mesh();
    let face = mesh.face(f)?;
    let outer = mesh
        .loop_vertices(face.outer)
        .ok()?
        .into_iter()
        .filter_map(|v| mesh.position(v).ok())
        .map(|q| resolved.world.transform_point3(q))
        .collect();
    Some(PickedFace {
        pid: resolved.pid.0,
        plane: face.plane.transformed(&resolved.world),
        outer,
    })
}

fn tol() -> planyrith_core::Tolerances {
    planyrith_core::Tolerances::default()
}

/// A typed value takes the direction the pointer gave, or the last
/// command's; a typed negative value reverses it (docs/05 §1).
fn signed(typed: f64, direction: Option<f64>) -> f64 {
    match direction {
        Some(d) if d < 0.0 => -typed,
        _ => typed,
    }
}

/// The signed angle from `from` to `to` about `axis` through `center`.
fn signed_angle(center: DVec3, axis: DVec3, from: DVec3, to: DVec3) -> f64 {
    let (a, b) = (from - center, to - center);
    a.cross(b).dot(axis).atan2(a.dot(b))
}

fn point(v: &Value) -> Option<DVec3> {
    serde_json::from_value::<[f64; 3]>(v.clone())
        .ok()
        .map(DVec3::from)
}

// ----- Push/Pull ----------------------------------------------------------------------------

/// The Push/Pull tool (docs/05 §4.3): click a face, move along its normal
/// (snapping to points anywhere: "to this point"), click; the value field
/// takes the distance; double-click repeats the last distance.
#[derive(Debug, Default)]
pub struct PushPullTool {
    face: Option<(PickedFace, DVec3, bool)>,
    cursor: Option<DVec3>,
    last: Option<f64>,
    /// Tool Options: keep the start face (as Ctrl does).
    keep: bool,
}

impl PushPullTool {
    fn distance(&self) -> Option<f64> {
        let (face, start, _) = self.face.as_ref()?;
        Some((self.cursor? - *start).dot(face.plane.normal))
    }

    fn params(&self, distance: f64) -> Option<Value> {
        let (face, _, keep) = self.face.as_ref()?;
        (distance.abs() > tol().length).then(|| {
            json!({ "face": { "Pid": face.pid }, "distance": distance, "keep_start_face": keep })
        })
    }
}

impl Tool for PushPullTool {
    fn command(&self) -> &'static str {
        "push-pull"
    }

    fn label(&self) -> &'static str {
        "Push/Pull"
    }

    fn step(&self) -> usize {
        usize::from(self.face.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        match &self.face {
            Some((face, start, _)) => super::Setup {
                constraint: Constraint::Line(Ray::new(*start, face.plane.normal)),
                reference: Some(*start),
            },
            None => super::Setup::default(),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if self.face.is_none() {
            if let Some(face) = picked_face(p, cx) {
                self.face = Some((face, p.inference.point, p.modifiers.command || self.keep));
            }
            return Flow::Continue;
        }
        self.cursor = Some(p.inference.point);
        match self.distance().and_then(|d| self.params(d).map(|v| (d, v))) {
            Some((d, params)) => {
                self.last = Some(d);
                self.face = None;
                Flow::run("push-pull", params)
            }
            None => Flow::Continue,
        }
    }

    fn double_click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        // Repeat the last distance on the face under the cursor.
        let (Some(d), Some(face)) = (self.last, picked_face(p, cx)) else {
            return Flow::Continue;
        };
        self.face = Some((face, p.inference.point, false));
        let flow = self
            .params(d)
            .map_or(Flow::Continue, |v| Flow::run("push-pull", v));
        self.face = None;
        flow
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let d = signed(length(text, cx)?, self.distance());
        let params = self.params(d).ok_or("pick a face first")?;
        self.last = Some(d);
        self.face = None;
        Ok(Flow::run("push-pull", params))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        (last.0 == "push-pull").then_some(())?;
        let mut params = last.1.clone();
        params["distance"] = json!(signed(length(text, cx).ok()?, last.1["distance"].as_f64()));
        Some(params)
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.checkbox(&mut self.keep, "Keep the start face (also Ctrl)");
    }

    fn escape(&mut self) -> bool {
        self.face.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some((face, start, _)), Some(d)) = (&self.face, self.distance()) {
            out.push(Shape::Line {
                a: *start,
                b: *start + face.plane.normal * d,
                color: BAND,
                dashed: true,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        Some(("push-pull".into(), self.params(self.distance()?)?))
    }
}

// ----- Offset -------------------------------------------------------------------------------

/// The Offset tool (docs/05 §4.7): click a face, move (inside = inward),
/// click; the value field takes the distance.
#[derive(Debug, Default)]
pub struct OffsetTool {
    face: Option<PickedFace>,
    cursor: Option<DVec3>,
    last: Option<f64>,
}

impl OffsetTool {
    /// Signed distance from the cursor to the face's outer boundary
    /// (negative inside).
    fn distance(&self) -> Option<f64> {
        let face = self.face.as_ref()?;
        let frame = face.plane.frame();
        let q = frame.to_2d(face.plane.project(self.cursor?));
        let ring: Vec<DVec2> = face.outer.iter().map(|p| frame.to_2d(*p)).collect();
        let n = ring.len();
        let mut nearest = f64::INFINITY;
        let mut inside = false;
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let ab = b - a;
            let t = ((q - a).dot(ab) / ab.length_squared().max(f64::MIN_POSITIVE)).clamp(0.0, 1.0);
            nearest = nearest.min((a + ab * t).distance(q));
            if (a.y > q.y) != (b.y > q.y) && q.x < a.x + (q.y - a.y) / (b.y - a.y) * (b.x - a.x) {
                inside = !inside;
            }
        }
        Some(if inside { -nearest } else { nearest })
    }

    fn params(&self, d: f64) -> Option<Value> {
        let face = self.face.as_ref()?;
        (d.abs() > tol().length).then(|| json!({ "face": { "Pid": face.pid }, "distance": d }))
    }
}

impl Tool for OffsetTool {
    fn command(&self) -> &'static str {
        "offset"
    }

    fn label(&self) -> &'static str {
        "Offset"
    }

    fn step(&self) -> usize {
        usize::from(self.face.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        match &self.face {
            Some(face) => super::Setup {
                constraint: Constraint::Plane(face.plane),
                reference: None,
            },
            None => super::Setup::default(),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if self.face.is_none() {
            self.face = picked_face(p, cx);
            return Flow::Continue;
        }
        self.cursor = Some(p.inference.point);
        match self.distance().and_then(|d| self.params(d).map(|v| (d, v))) {
            Some((d, params)) => {
                self.last = Some(d);
                self.face = None;
                Flow::run("offset", params)
            }
            None => Flow::Continue,
        }
    }

    fn double_click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let (Some(d), Some(face)) = (self.last, picked_face(p, cx)) else {
            return Flow::Continue;
        };
        self.face = Some(face);
        let flow = self
            .params(d)
            .map_or(Flow::Continue, |v| Flow::run("offset", v));
        self.face = None;
        flow
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let d = signed(length(text, cx)?, self.distance());
        let params = self.params(d).ok_or("pick a face first")?;
        self.last = Some(d);
        self.face = None;
        Ok(Flow::run("offset", params))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        (last.0 == "offset").then_some(())?;
        let mut params = last.1.clone();
        params["distance"] = json!(signed(length(text, cx).ok()?, last.1["distance"].as_f64()));
        Some(params)
    }

    fn escape(&mut self) -> bool {
        self.face.take().is_some()
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        Some(("offset".into(), self.params(self.distance()?)?))
    }
}

// ----- Move ---------------------------------------------------------------------------------

/// Copies typed after a copy-move (docs/05 §4.4): `*N`/`Nx` repeat the
/// spacing, `/N` divides it.
fn array_params(last: &Value, input: &ValueFieldInput) -> Option<Value> {
    let (copies, spacing) = match input {
        ValueFieldInput::Multiple(n) => (*n, "between"),
        ValueFieldInput::Division(n) => (*n, "total"),
        _ => return None,
    };
    let mut params = last.clone();
    params["copy"] = json!(true);
    params["copies"] = json!(copies.max(1));
    params["spacing"] = json!(spacing);
    Some(params)
}

/// The Move tool (docs/05 §4.4): the selection (or the hovered entity) moves
/// from a grab point to a destination; Ctrl copies; the value field takes a
/// distance or coordinates, and `*N` or `/N` after a copy.
#[derive(Debug, Default)]
pub struct MoveTool {
    grab: Option<(DVec3, String)>,
    cursor: Option<DVec3>,
    /// Tool Options: copy (as Ctrl does).
    copy: bool,
}

impl MoveTool {
    fn params(&self, to: DVec3, copy: bool) -> Option<Value> {
        let (from, targets) = self.grab.as_ref()?;
        (from.distance(to) > tol().length)
            .then(|| json!({ "targets": targets, "from": pt(*from), "to": pt(to), "copy": copy }))
    }
}

impl Tool for MoveTool {
    fn command(&self) -> &'static str {
        "move"
    }

    fn label(&self) -> &'static str {
        "Move"
    }

    fn step(&self) -> usize {
        usize::from(self.grab.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        super::Setup {
            constraint: Constraint::Free,
            reference: self.grab.as_ref().map(|(p, _)| *p),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if self.grab.is_none() {
            if let Some(t) = targets(cx, p.target.as_ref()) {
                self.grab = Some((p.inference.point, t));
            }
            return Flow::Continue;
        }
        match self.params(p.inference.point, p.modifiers.command || self.copy) {
            Some(params) => {
                self.grab = None;
                Flow::run("move", params)
            }
            None => Flow::Continue,
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let (from, _) = self.grab.clone().ok_or("pick the grab point first")?;
        let to = match parse(text, cx)? {
            ValueFieldInput::Coordinate { absolute, xyz } => {
                let base = if absolute { DVec3::ZERO } else { from };
                let c =
                    |i: usize| xyz[i].map_or(if absolute { from[i] } else { 0.0 }, |l| l.as_mm());
                base + DVec3::new(c(0), c(1), c(2))
            }
            _ => along(
                from,
                self.cursor.ok_or("move the cursor first")?,
                length(text, cx)?,
            )
            .ok_or("move the cursor to give the direction")?,
        };
        let params = self.params(to, false).ok_or("that is no move")?;
        self.grab = None;
        Ok(Flow::run("move", params))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        (last.0 == "move").then_some(())?;
        let input = parse(text, cx).ok()?;
        if let Some(params) = array_params(&last.1, &input) {
            return Some(params);
        }
        let from = serde_json::from_value::<[f64; 3]>(last.1["from"].clone())
            .ok()
            .map(DVec3::from)?;
        let to = serde_json::from_value::<[f64; 3]>(last.1["to"].clone())
            .ok()
            .map(DVec3::from)?;
        let mut params = last.1.clone();
        params["to"] = pt(along(from, to, input.as_length(cx.unit)?.as_mm())?);
        Some(params)
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.checkbox(&mut self.copy, "Copy (also Ctrl while clicking)");
    }

    fn escape(&mut self) -> bool {
        self.grab.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some((a, _)), Some(b)) = (&self.grab, self.cursor) {
            out.push(Shape::Line {
                a: *a,
                b,
                color: BAND,
                dashed: true,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        Some(("move".into(), self.params(self.cursor?, false)?))
    }
}

// ----- Rotate -------------------------------------------------------------------------------

/// The Rotate tool (docs/05 §4.5): center (protractor plane from the face
/// under it, an arrow key or the horizontal plane), first reference point,
/// second reference point; the value field takes the angle, `*N`/`/N` after
/// a copy; Ctrl copies.
#[derive(Debug, Default)]
pub struct RotateTool {
    center: Option<(DVec3, Plane, String)>,
    from: Option<DVec3>,
    cursor: Option<DVec3>,
    forced: Option<Axis>,
    /// Tool Options: copy (as Ctrl does).
    copy: bool,
}

impl RotateTool {
    fn params(&self, to: DVec3, copy: bool) -> Option<Value> {
        let (center, plane, targets) = self.center.as_ref()?;
        let from = plane.project(self.from?);
        let to = plane.project(to);
        (from.distance(*center) > tol().length && to.distance(*center) > tol().length).then(|| {
            json!({
                "targets": targets, "center": pt(*center), "axis": pt(plane.normal),
                "from_point": pt(from), "to_point": pt(to), "copy": copy,
            })
        })
    }
}

impl Tool for RotateTool {
    fn command(&self) -> &'static str {
        "rotate"
    }

    fn label(&self) -> &'static str {
        "Rotate"
    }

    fn step(&self) -> usize {
        match (&self.center, self.from) {
            (None, _) => 0,
            (Some(_), None) => 1,
            _ => 2,
        }
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        match &self.center {
            Some((c, plane, _)) => super::Setup {
                constraint: Constraint::Plane(*plane),
                reference: Some(*c),
            },
            None => super::Setup::default(),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let point = p.inference.point;
        match (&self.center, self.from) {
            (None, _) => {
                if let Some(t) = targets(cx, p.target.as_ref()) {
                    let plane = super::drawing_plane(&p.inference, self.forced, &cx.camera);
                    self.center = Some((point, plane, t));
                }
                Flow::Continue
            }
            (Some(_), None) => {
                self.from = Some(point);
                Flow::Continue
            }
            (Some(_), Some(_)) => match self.params(point, p.modifiers.command || self.copy) {
                Some(params) => {
                    self.center = None;
                    self.from = None;
                    self.forced = None;
                    Flow::run("rotate", params)
                }
                None => Flow::Continue,
            },
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let (center, plane, targets) = self.center.clone().ok_or("pick the center first")?;
        let current = self.from.zip(self.cursor).map(|(from, to)| {
            signed_angle(center, plane.normal, plane.project(from), plane.project(to))
        });
        let radians = signed(angle(text, cx)?, current);
        let params = json!({
            "targets": targets, "center": pt(center), "axis": pt(plane.normal), "angle": radians,
        });
        self.center = None;
        self.from = None;
        Ok(Flow::run("rotate", params))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        (last.0 == "rotate").then_some(())?;
        let input = parse(text, cx).ok()?;
        if let Some(params) = array_params(&last.1, &input) {
            return Some(params);
        }
        let previous = match last.1["angle"].as_f64() {
            Some(a) => Some(a),
            None => {
                let center = point(&last.1["center"])?;
                let axis = point(&last.1["axis"])?;
                let from = point(&last.1["from_point"])?;
                let to = point(&last.1["to_point"])?;
                Some(signed_angle(center, axis, from, to))
            }
        };
        let mut params = last.1.clone();
        if let Some(map) = params.as_object_mut() {
            map.remove("from_point");
            map.remove("to_point");
        }
        params["angle"] = json!(signed(input.as_angle()?.as_radians(), previous));
        Some(params)
    }

    fn arrow(&mut self, axis: Axis) -> bool {
        if self.center.is_some() {
            return false;
        }
        self.forced = if self.forced == Some(axis) {
            None
        } else {
            Some(axis)
        };
        true
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.checkbox(&mut self.copy, "Copy (also Ctrl while clicking)");
    }

    fn escape(&mut self) -> bool {
        self.forced = None;
        self.from = None;
        self.center.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let Some((c, plane, _)) = &self.center {
            for p in [self.from, self.cursor].into_iter().flatten() {
                out.push(Shape::Line {
                    a: *c,
                    b: plane.project(p),
                    color: BAND,
                    dashed: true,
                });
            }
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        Some(("rotate".into(), self.params(self.cursor?, false)?))
    }
}

// ----- Scale --------------------------------------------------------------------------------

/// The Scale tool (docs/05 §4.6): the selection's bounding box shows grips
/// (corners, edge midpoints, face centers); drag-click one, move, click.
/// Corner grips scale uniformly and edge/face grips along their axes;
/// Shift toggles uniform; Ctrl scales about the center. The value field
/// takes a factor, `fx,fy`, or a length with units for the grip's
/// dimension.
#[derive(Debug, Default)]
pub struct ScaleTool {
    grip: Option<ScaleGrip>,
    cursor: Option<DVec3>,
    hovered: Option<DVec3>,
}

#[derive(Debug, Clone)]
struct ScaleGrip {
    targets: String,
    pivot: DVec3,
    grip: DVec3,
    uniform: bool,
}

fn selection_bounds(cx: &Cx<'_>, hovered: Option<&EntityPath>) -> Option<(Aabb, String)> {
    let targets = targets(cx, hovered)?;
    let bounds = if cx.selection.is_empty() {
        cx.doc.occurrence_aabb(hovered?)?
    } else {
        cx.selection
            .iter()
            .filter_map(|p| cx.doc.occurrence_aabb(p))
            .fold(Aabb::EMPTY, |a, b| a.union(&b))
    };
    (!bounds.is_empty()).then_some((bounds, targets))
}

/// The 26 grips of a box (corners, edge midpoints, face centers).
fn grips(b: &Aabb) -> Vec<DVec3> {
    let c = b.center();
    let h = b.size() * 0.5;
    let mut out = Vec::new();
    for x in [-1.0, 0.0, 1.0] {
        for y in [-1.0, 0.0, 1.0] {
            for z in [-1.0, 0.0, 1.0] {
                if (x, y, z) != (0.0, 0.0, 0.0) {
                    out.push(c + DVec3::new(x * h.x, y * h.y, z * h.z));
                }
            }
        }
    }
    out
}

impl ScaleGrip {
    fn factors(&self, to: DVec3) -> DVec3 {
        let span = self.grip - self.pivot;
        if self.uniform {
            let f = (to - self.pivot).dot(span) / span.length_squared().max(f64::MIN_POSITIVE);
            return DVec3::splat(f);
        }
        let axis = |i: usize| {
            if span[i].abs() <= tol().length {
                1.0
            } else {
                (to[i] - self.pivot[i]) / span[i]
            }
        };
        DVec3::new(axis(0), axis(1), axis(2))
    }

    fn params(&self, factors: DVec3) -> Option<Value> {
        let degenerate = factors.to_array().iter().any(|f| f.abs() <= 1e-6);
        (!degenerate).then(|| {
            json!({ "targets": self.targets, "factors": [factors.x, factors.y, factors.z], "pivot": pt(self.pivot) })
        })
    }
}

impl Tool for ScaleTool {
    fn takes_points(&self) -> bool {
        false
    }

    fn command(&self) -> &'static str {
        "scale"
    }

    fn label(&self) -> &'static str {
        "Scale"
    }

    fn step(&self) -> usize {
        0
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        match &self.grip {
            Some(g) => super::Setup {
                constraint: Constraint::Line(Ray::new(
                    g.pivot,
                    (g.grip - g.pivot).try_normalize().unwrap_or(DVec3::X),
                )),
                reference: Some(g.pivot),
            },
            None => super::Setup::default(),
        }
    }

    fn hover(&mut self, p: &Pointer, cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
        // The grip under the cursor (within 10 points).
        self.hovered = selection_bounds(cx, p.target.as_ref()).and_then(|(b, _)| {
            grips(&b)
                .into_iter()
                .filter_map(|g| cx.to_screen(g).map(|s| (g, s.distance(p.screen))))
                .filter(|(_, d)| *d <= 10.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(g, _)| g)
        });
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if self.grip.is_none() {
            let (Some(grip), Some((bounds, targets))) =
                (self.hovered, selection_bounds(cx, p.target.as_ref()))
            else {
                return Flow::Continue;
            };
            let center = bounds.center();
            let pivot = if p.modifiers.command {
                center
            } else {
                center * 2.0 - grip
            };
            let corner = (0..3).all(|i| (grip[i] - center[i]).abs() > tol().length);
            self.grip = Some(ScaleGrip {
                targets,
                pivot,
                grip,
                uniform: corner != p.modifiers.shift,
            });
            return Flow::Continue;
        }
        let Some(grip) = self.grip.clone() else {
            return Flow::Continue;
        };
        match grip.params(grip.factors(p.inference.point)) {
            Some(params) => {
                self.grip = None;
                Flow::run("scale", params)
            }
            None => Flow::Continue,
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let grip = self.grip.clone().ok_or("pick a grip first")?;
        let factors = match parse(text, cx)? {
            ValueFieldInput::Count(n) => DVec3::splat(f64::from(n)),
            ValueFieldInput::Number(x) => DVec3::splat(x),
            // Factors are plain numbers: undo the bare-number unit.
            ValueFieldInput::Pair(a, b) => {
                let unit = cx.unit.millimeters();
                DVec3::new(a.as_mm() / unit, b.as_mm() / unit, 1.0)
            }
            ValueFieldInput::Coordinate { xyz, .. } => {
                let unit = cx.unit.millimeters();
                let f = |i: usize| xyz[i].map_or(1.0, |l| l.as_mm() / unit);
                DVec3::new(f(0), f(1), f(2))
            }
            ValueFieldInput::Length(l) => {
                // An absolute size for the grip's dimension.
                let span = (grip.grip - grip.pivot).length();
                DVec3::splat(l.as_mm() / span.max(f64::MIN_POSITIVE))
            }
            _ => return Err(format!("`{text}` is not a factor")),
        };
        let params = grip.params(factors).ok_or("a factor of zero")?;
        self.grip = None;
        Ok(Flow::run("scale", params))
    }

    fn escape(&mut self) -> bool {
        self.grip.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let Some(g) = &self.hovered {
            out.push(Shape::Marker {
                p: *g,
                color: Color32::from_rgb(0, 170, 0),
            });
        }
        if let (Some(g), Some(c)) = (&self.grip, self.cursor) {
            out.push(Shape::Line {
                a: g.pivot,
                b: c,
                color: BAND,
                dashed: true,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        let g = self.grip.as_ref()?;
        Some(("scale".into(), g.params(g.factors(self.cursor?))?))
    }
}

// ----- Eraser -------------------------------------------------------------------------------

/// The Eraser (docs/05 §4.2): click or drag over edges; Ctrl softens, Shift
/// hides; groups and components clicked are deleted.
#[derive(Debug, Default)]
pub struct EraserTool {
    stroke: Vec<EntityPath>,
}

impl EraserTool {
    fn erase(paths: &[EntityPath], cx: &Cx<'_>, modifiers: egui::Modifiers) -> Flow {
        let mut edges = Vec::new();
        let mut objects = Vec::new();
        for path in paths {
            match cx.doc.resolve(path).map(|r| r.entity) {
                Ok(EntityId::Edge(_)) => edges.push(format!("{}", path.target)),
                Ok(EntityId::Group(_) | EntityId::Instance(_)) => {
                    objects.push(format!("{}", path.target));
                }
                _ => {}
            }
        }
        let mode = if modifiers.command {
            "soften"
        } else if modifiers.shift {
            "hide"
        } else {
            "erase"
        };
        let mut commands = Vec::new();
        if !edges.is_empty() {
            commands.push((
                "erase".to_string(),
                json!({ "targets": edges.join(" | "), "mode": mode }),
            ));
        }
        if !objects.is_empty() && mode == "erase" {
            commands.push((
                "delete".to_string(),
                json!({ "targets": objects.join(" | ") }),
            ));
        }
        if commands.is_empty() {
            Flow::Continue
        } else {
            Flow::Run(commands)
        }
    }
}

impl Tool for EraserTool {
    fn takes_points(&self) -> bool {
        false
    }

    fn command(&self) -> &'static str {
        "erase"
    }

    fn label(&self) -> &'static str {
        "Eraser"
    }

    fn step(&self) -> usize {
        0
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let hit: Vec<EntityPath> = p.target.iter().cloned().collect();
        Self::erase(&hit, cx, p.modifiers)
    }

    fn drag(&mut self, phase: Drag, p: &Pointer, _screen: egui::Pos2, cx: &Cx<'_>) -> Flow {
        if let Some(t) = &p.target
            && !self.stroke.contains(t)
        {
            self.stroke.push(t.clone());
        }
        if phase == Drag::End {
            let stroke = std::mem::take(&mut self.stroke);
            return Self::erase(&stroke, cx, p.modifiers);
        }
        Flow::Continue
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err("the Eraser takes no value".into())
    }

    fn escape(&mut self) -> bool {
        let busy = !self.stroke.is_empty();
        self.stroke.clear();
        busy
    }
}

// ----- Paint --------------------------------------------------------------------------------

/// Paint (docs/05 §5.8): click a face, group or component with the chosen
/// material; Ctrl paints connected faces, Shift faces with the same
/// material, Alt picks the clicked face's material.
#[derive(Debug, Default)]
pub struct PaintTool {
    material: Option<String>,
}

/// The side of the face under the pointer that faces the eye.
fn visible_side(p: &Pointer, cx: &Cx<'_>) -> planyrith_core::document::FrontOrBack {
    use planyrith_core::document::FrontOrBack;
    let plane = p.face.as_ref().and_then(|element| {
        let r = cx.doc.resolve(element).ok()?;
        let EntityId::Face(f) = r.entity else {
            return None;
        };
        let face = cx.doc.container(r.container)?.mesh().face(f)?;
        Some(face.plane.transformed(&r.world))
    });
    match plane {
        // In parallel views the eye is far along the view direction too.
        Some(plane) if plane.signed_distance(cx.camera.eye) < 0.0 => FrontOrBack::Back,
        _ => FrontOrBack::Front,
    }
}

impl PaintTool {
    /// Paints with `material`.
    pub fn with_material(material: impl Into<String>) -> Self {
        PaintTool {
            material: Some(material.into()),
        }
    }
}

impl Tool for PaintTool {
    fn takes_points(&self) -> bool {
        false
    }

    fn command(&self) -> &'static str {
        "material-apply"
    }

    fn label(&self) -> &'static str {
        "Paint"
    }

    fn step(&self) -> usize {
        0
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        // The face (or the group or component holding it), not a nearby edge.
        let Some(target) = p
            .face
            .as_ref()
            .and_then(|f| f.selectable_in(cx.doc, &cx.context.path))
        else {
            return Flow::Continue;
        };
        // The side of the face the eye sees is the one painted or sampled.
        let side = visible_side(p, cx);
        if p.modifiers.alt {
            // Sample the material of the face under the cursor.
            if let Some(element) = &p.face
                && let Ok(r) = cx.doc.resolve(element)
                && let EntityId::Face(f) = r.entity
                && let Some(face) = cx.doc.container(r.container).and_then(|c| c.mesh().face(f))
            {
                let (material, side) = match side {
                    planyrith_core::document::FrontOrBack::Back => (face.sides.material_back, side),
                    planyrith_core::document::FrontOrBack::Front => {
                        (face.sides.material_front, side)
                    }
                };
                let id = cx.doc.materials.resolve(material, side);
                self.material = cx.doc.materials.get(id).map(|m| m.name.clone());
            }
            return Flow::Continue;
        }
        let Some(material) = self.material.clone().or_else(|| {
            cx.doc
                .materials
                .iter()
                .find(|(_, m)| m.is_default_for.is_none())
                .map(|(_, m)| m.name.clone())
        }) else {
            return Flow::Continue;
        };
        let fill = if p.modifiers.command {
            "connected"
        } else if p.modifiers.shift {
            "same-material"
        } else {
            "single"
        };
        let side = match side {
            planyrith_core::document::FrontOrBack::Front => "front",
            planyrith_core::document::FrontOrBack::Back => "back",
        };
        Flow::run(
            "material-apply",
            json!({ "targets": format!("{}", target.target), "material": material, "fill": fill, "side": side }),
        )
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err("Paint takes no value".into())
    }

    fn escape(&mut self) -> bool {
        false
    }

    fn options(&mut self, ui: &mut egui::Ui, cx: &Cx<'_>) {
        ui.label("Material");
        for (_, m) in cx.doc.materials.iter() {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    2.0,
                    Color32::from_rgba_unmultiplied(m.color.r, m.color.g, m.color.b, m.color.a),
                );
                if ui
                    .selectable_label(self.material.as_deref() == Some(m.name.as_str()), &m.name)
                    .clicked()
                {
                    self.material = Some(m.name.clone());
                }
            });
        }
    }
}
