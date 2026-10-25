//! Drawing tools (docs/05 §3): Line, Rectangle, Circle, Polygon and the arcs.
//! Points come from the inference engine; planes from the face under the
//! first click, an arrow key, or the horizontal plane through the point.

use glam::DVec3;
use planyrith_core::inference::Constraint;
use planyrith_core::math::{Axis, Plane};
use planyrith_core::units::ValueFieldInput;
use serde_json::{Value, json};

use super::{
    BAND, Cx, Flow, Pointer, Shape, Tool, along, drawing_plane, length, parse, plane_json, pt,
};

fn same(a: DVec3, b: DVec3) -> bool {
    a.distance(b) <= planyrith_core::Tolerances::default().length
}

fn point_of(v: &Value) -> Option<DVec3> {
    serde_json::from_value::<[f64; 3]>(v.clone())
        .ok()
        .map(DVec3::from)
}

// ----- Line ---------------------------------------------------------------------------------

/// The Line tool (docs/05 §3.1): click a start point, then each next point;
/// every segment is one `line` command (one undo step); closing on the start
/// or Esc ends the chain.
#[derive(Debug, Default)]
pub struct LineTool {
    chain: Vec<DVec3>,
    cursor: Option<DVec3>,
}

impl LineTool {
    fn segment(&mut self, to: DVec3) -> Flow {
        let Some(&from) = self.chain.last() else {
            self.chain.push(to);
            return Flow::Continue;
        };
        if same(from, to) {
            return Flow::Continue;
        }
        let closes = self.chain.len() >= 2 && same(self.chain[0], to);
        let flow = Flow::run("line", json!({ "points": [pt(from), pt(to)] }));
        if closes {
            self.chain.clear();
        } else {
            self.chain.push(to);
        }
        flow
    }
}

impl Tool for LineTool {
    fn command(&self) -> &'static str {
        "line"
    }

    fn label(&self) -> &'static str {
        "Line"
    }

    fn step(&self) -> usize {
        usize::from(!self.chain.is_empty())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        super::Setup {
            constraint: Constraint::Free,
            reference: self.chain.last().copied(),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, _cx: &Cx<'_>) -> Flow {
        self.segment(p.inference.point)
    }

    fn double_click(&mut self, _p: &Pointer, _cx: &Cx<'_>) -> Flow {
        self.chain.clear();
        Flow::Continue
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let from = self.chain.last().copied();
        let to = match parse(text, cx)? {
            ValueFieldInput::Coordinate { absolute, xyz } => {
                let base = if absolute {
                    DVec3::ZERO
                } else {
                    from.unwrap_or(DVec3::ZERO)
                };
                let current = if absolute {
                    self.cursor.unwrap_or(DVec3::ZERO)
                } else {
                    DVec3::ZERO
                };
                let c = |i: usize| xyz[i].map_or(current[i], |l| l.as_mm());
                base + DVec3::new(c(0), c(1), c(2))
            }
            _ => {
                let (Some(from), Some(cursor)) = (from, self.cursor) else {
                    return Err("pick the start point first".into());
                };
                along(from, cursor, length(text, cx)?)
                    .ok_or("move the cursor to give the direction")?
            }
        };
        Ok(self.segment(to))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        if last.0 != "line" {
            return None;
        }
        let points = last.1["points"].as_array()?;
        let (a, b) = (point_of(&points[0])?, point_of(points.last()?)?);
        let b = along(a, b, length(text, cx).ok()?)?;
        Some(json!({ "points": [pt(a), pt(b)] }))
    }

    fn escape(&mut self) -> bool {
        let busy = !self.chain.is_empty();
        self.chain.clear();
        busy
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some(&a), Some(b)) = (self.chain.last(), self.cursor) {
            out.push(Shape::Line {
                a,
                b,
                color: BAND,
                dashed: false,
            });
        }
    }
}

// ----- Rectangle ----------------------------------------------------------------------------

/// The Rectangle tool (docs/05 §3.3): first corner (its plane from the face
/// under it, an arrow key or the horizontal plane), then the opposite
/// corner; value field `w,h` or `w`.
#[derive(Debug, Default)]
pub struct RectangleTool {
    first: Option<(DVec3, Plane)>,
    cursor: Option<DVec3>,
    forced: Option<Axis>,
}

/// The rectangle's corners in its plane's frame for opposite corner `b`.
fn rectangle_corners(a: DVec3, plane: &Plane, b: DVec3) -> [DVec3; 4] {
    let frame = plane.frame_at(a);
    let q = frame.to_2d(plane.project(b));
    [
        a,
        frame.from_2d(glam::DVec2::new(q.x, 0.0)),
        frame.from_2d(q),
        frame.from_2d(glam::DVec2::new(0.0, q.y)),
    ]
}

impl RectangleTool {
    fn params(a: DVec3, plane: &Plane, b: DVec3) -> Option<Value> {
        let corners = rectangle_corners(a, plane, b);
        let tol = planyrith_core::Tolerances::default();
        (corners[1].distance(a) > tol.length && corners[3].distance(a) > tol.length).then(
            || json!({ "corner_a": pt(a), "corner_b": pt(corners[2]), "plane": plane_json(plane) }),
        )
    }

    /// The opposite corner for typed `w,h` (or `w`, keeping the height), the
    /// signs following the cursor.
    fn sized(
        a: DVec3,
        plane: &Plane,
        current: DVec3,
        input: &ValueFieldInput,
        cx: &Cx<'_>,
    ) -> Option<DVec3> {
        let frame = plane.frame_at(a);
        let q = frame.to_2d(plane.project(current));
        let sign = |v: f64| if v < 0.0 { -1.0 } else { 1.0 };
        let (w, h) = match input {
            ValueFieldInput::Pair(w, h) => (w.as_mm(), h.as_mm()),
            other => (other.as_length(cx.unit)?.as_mm(), q.y.abs()),
        };
        Some(frame.from_2d(glam::DVec2::new(sign(q.x) * w, sign(q.y) * h)))
    }
}

impl Tool for RectangleTool {
    fn command(&self) -> &'static str {
        "rectangle"
    }

    fn label(&self) -> &'static str {
        "Rectangle"
    }

    fn step(&self) -> usize {
        usize::from(self.first.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        match self.first {
            Some((a, plane)) => super::Setup {
                constraint: Constraint::Plane(plane),
                reference: Some(a),
            },
            None => super::Setup::default(),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        match self.first {
            None => {
                self.first = Some((
                    p.inference.point,
                    drawing_plane(&p.inference, self.forced, &cx.camera),
                ));
                Flow::Continue
            }
            Some((a, plane)) => match Self::params(a, &plane, p.inference.point) {
                Some(params) => {
                    self.first = None;
                    self.forced = None;
                    Flow::run("rectangle", params)
                }
                None => Flow::Continue,
            },
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let (a, plane) = self.first.ok_or("pick the first corner")?;
        let input = parse(text, cx)?;
        let b = Self::sized(a, &plane, self.cursor.unwrap_or(a + DVec3::ONE), &input, cx)
            .ok_or_else(|| format!("`{text}` is not w,h"))?;
        let params = Self::params(a, &plane, b).ok_or("the rectangle would be empty")?;
        self.first = None;
        Ok(Flow::run("rectangle", params))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        if last.0 != "rectangle" {
            return None;
        }
        let a = point_of(&last.1["corner_a"])?;
        let b = point_of(&last.1["corner_b"])?;
        let plane: Plane = serde_json::from_value(last.1["plane"].clone()).ok()?;
        let b = Self::sized(a, &plane, b, &parse(text, cx).ok()?, cx)?;
        Self::params(a, &plane, b)
    }

    fn arrow(&mut self, axis: Axis) -> bool {
        if self.first.is_some() {
            return false;
        }
        self.forced = if self.forced == Some(axis) {
            None
        } else {
            Some(axis)
        };
        true
    }

    fn escape(&mut self) -> bool {
        self.forced = None;
        self.first.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some((a, plane)), Some(c)) = (self.first, self.cursor) {
            out.push(Shape::Path {
                points: rectangle_corners(a, &plane, c).to_vec(),
                closed: true,
                color: BAND,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        let (a, plane) = self.first?;
        Some(("rectangle".into(), Self::params(a, &plane, self.cursor?)?))
    }
}

// ----- Circle and Polygon ---------------------------------------------------------------------

/// The Circle and Polygon tools (docs/05 §3.5–§3.6): center (plane from the
/// face, an arrow key or the horizontal plane), then the radius; `Ns` in the
/// value field sets the segment or side count.
#[derive(Debug)]
pub struct RoundTool {
    polygon: bool,
    center: Option<(DVec3, Plane)>,
    cursor: Option<DVec3>,
    forced: Option<Axis>,
    count: u32,
}

impl RoundTool {
    /// The Circle tool.
    pub fn circle() -> Self {
        RoundTool {
            polygon: false,
            center: None,
            cursor: None,
            forced: None,
            count: 24,
        }
    }

    /// The Polygon tool.
    pub fn polygon() -> Self {
        RoundTool {
            polygon: true,
            count: 6,
            ..RoundTool::circle()
        }
    }

    fn name(&self) -> &'static str {
        if self.polygon { "polygon" } else { "circle" }
    }

    fn params(&self, radius: f64) -> Option<(String, Value)> {
        let (center, plane) = self.center?;
        if radius <= planyrith_core::Tolerances::default().length {
            return None;
        }
        let count = if self.polygon { "sides" } else { "segments" };
        Some((
            self.name().into(),
            json!({ "center": pt(center), "radius": radius, "normal": pt(plane.normal), count: self.count }),
        ))
    }

    fn radius(&self) -> Option<f64> {
        let (center, plane) = self.center?;
        Some(plane.project(self.cursor?).distance(center))
    }
}

impl Tool for RoundTool {
    fn command(&self) -> &'static str {
        self.name()
    }

    fn label(&self) -> &'static str {
        if self.polygon { "Polygon" } else { "Circle" }
    }

    fn step(&self) -> usize {
        usize::from(self.center.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        match self.center {
            Some((c, plane)) => super::Setup {
                constraint: Constraint::Plane(plane),
                reference: Some(c),
            },
            None => super::Setup::default(),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if self.center.is_none() {
            self.center = Some((
                p.inference.point,
                drawing_plane(&p.inference, self.forced, &cx.camera),
            ));
            return Flow::Continue;
        }
        self.cursor = Some(p.inference.point);
        match self.radius().and_then(|r| self.params(r)) {
            Some((name, params)) => {
                self.center = None;
                self.forced = None;
                Flow::run(&name, params)
            }
            None => Flow::Continue,
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let r = match parse(text, cx)? {
            ValueFieldInput::Segments(n) => {
                self.count = n.clamp(3, 999);
                return Ok(Flow::Continue);
            }
            ValueFieldInput::Radius(r) => r.as_mm(),
            _ => length(text, cx)?,
        };
        let (name, params) = self.params(r).ok_or("pick the center first")?;
        self.center = None;
        Ok(Flow::run(&name, params))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        if last.0 != self.name() {
            return None;
        }
        let mut params = last.1.clone();
        match parse(text, cx).ok()? {
            ValueFieldInput::Segments(n) => {
                let key = if self.polygon { "sides" } else { "segments" };
                params[key] = json!(n.clamp(3, 999));
            }
            ValueFieldInput::Radius(r) => params["radius"] = json!(r.as_mm()),
            _ => params["radius"] = json!(length(text, cx).ok()?),
        }
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

    fn escape(&mut self) -> bool {
        self.forced = None;
        self.center.take().is_some()
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        let label = if self.polygon { "Sides" } else { "Segments" };
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::DragValue::new(&mut self.count).range(3..=999));
        });
        ui.weak(format!(
            "Type `{}s` in the value field to set it.",
            self.count
        ));
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some((c, plane)), Some(p)) = (self.center, self.cursor) {
            out.push(Shape::Line {
                a: c,
                b: plane.project(p),
                color: BAND,
                dashed: true,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        self.params(self.radius()?)
    }
}

// ----- Arcs ---------------------------------------------------------------------------------

/// Which arc command an [`ArcTool`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcKind {
    /// `arc-bulge`: start, end, bulge (the default Arc tool, docs/05 §3.7).
    Bulge,
    /// `arc-2point`: center, start, end.
    Center,
    /// `arc-3point`: start, a point on the arc, end.
    ThreePoint,
    /// `pie`: center, start, end (a closed sector).
    Pie,
}

/// The arc tools (docs/05 §3.7): three picks whose meaning depends on the
/// kind; the plane is the first pick's (face, arrow key or horizontal). The
/// value field takes the radius or chord length at the second pick, the
/// bulge, a radius (`Rr`) or the sweep angle at the third, and `Ns` for
/// segments.
#[derive(Debug)]
pub struct ArcTool {
    kind: ArcKind,
    picks: Vec<DVec3>,
    plane: Option<Plane>,
    cursor: Option<DVec3>,
    forced: Option<Axis>,
    segments: u32,
}

impl ArcTool {
    /// An arc tool of `kind`.
    pub fn new(kind: ArcKind) -> Self {
        ArcTool {
            kind,
            picks: Vec::new(),
            plane: None,
            cursor: None,
            forced: None,
            segments: 12,
        }
    }

    fn name(&self) -> &'static str {
        match self.kind {
            ArcKind::Bulge => "arc-bulge",
            ArcKind::Center => "arc-2point",
            ArcKind::ThreePoint => "arc-3point",
            ArcKind::Pie => "pie",
        }
    }

    /// The command for a third point `c` (projected onto the plane).
    fn params(&self, c: DVec3) -> Option<(String, Value)> {
        let [a, b] = [*self.picks.first()?, *self.picks.get(1)?];
        let plane = self.plane?;
        let c = plane.project(c);
        let tol = planyrith_core::Tolerances::default();
        if same(a, b) || same(b, c) || same(a, c) {
            return None;
        }
        let normal = pt(plane.normal);
        let params = match self.kind {
            ArcKind::Bulge => {
                json!({ "start": pt(a), "end": pt(b), "through": pt(c), "normal": normal, "segments": self.segments })
            }
            ArcKind::ThreePoint => {
                if tol.collinear(a, b, c) {
                    return None;
                }
                json!({ "a": pt(a), "b": pt(b), "c": pt(c), "segments": self.segments })
            }
            ArcKind::Center | ArcKind::Pie => {
                // The end lies on the radius circle, in the direction of c.
                let r = b.distance(a);
                let end = along(a, c, r)?;
                json!({ "center": pt(a), "start": pt(b), "end": pt(end), "normal": normal, "segments": self.segments })
            }
        };
        Some((self.name().into(), params))
    }
}

impl Default for ArcTool {
    fn default() -> Self {
        ArcTool::new(ArcKind::Bulge)
    }
}

impl Tool for ArcTool {
    fn command(&self) -> &'static str {
        self.name()
    }

    fn label(&self) -> &'static str {
        match self.kind {
            ArcKind::Bulge => "Arc",
            ArcKind::Center => "2 Point Arc",
            ArcKind::ThreePoint => "3 Point Arc",
            ArcKind::Pie => "Pie",
        }
    }

    fn step(&self) -> usize {
        self.picks.len().min(2)
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        match (self.picks.last(), self.plane) {
            (Some(&last), Some(plane)) => super::Setup {
                constraint: Constraint::Plane(plane),
                reference: Some(last),
            },
            _ => super::Setup::default(),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let point = p.inference.point;
        if self.picks.is_empty() {
            self.plane = Some(drawing_plane(&p.inference, self.forced, &cx.camera));
            self.picks.push(point);
            return Flow::Continue;
        }
        let plane = self
            .plane
            .unwrap_or_else(|| drawing_plane(&p.inference, None, &cx.camera));
        if self.picks.len() == 1 {
            let point = plane.project(point);
            if !same(point, self.picks[0]) {
                self.picks.push(point);
            }
            return Flow::Continue;
        }
        match self.params(point) {
            Some((name, params)) => {
                self.picks.clear();
                self.forced = None;
                Flow::run(&name, params)
            }
            None => Flow::Continue,
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        if let ValueFieldInput::Segments(n) = parse(text, cx)? {
            self.segments = n.clamp(1, 999);
            return Ok(Flow::Continue);
        }
        let cursor = self.cursor.ok_or("move the cursor first")?;
        match self.picks.len() {
            1 => {
                // Second pick at the typed distance toward the cursor.
                let plane = self.plane.ok_or("pick the first point")?;
                let b = along(self.picks[0], plane.project(cursor), length(text, cx)?)
                    .ok_or("move the cursor to give the direction")?;
                self.picks.push(b);
                Ok(Flow::Continue)
            }
            2 => {
                let (a, b) = (self.picks[0], self.picks[1]);
                let plane = self.plane.ok_or("pick the first point")?;
                let c = match (self.kind, parse(text, cx)?) {
                    (ArcKind::Center | ArcKind::Pie, input) => {
                        // A sweep angle from the start.
                        let sweep = input
                            .as_angle()
                            .map(|a| a.as_radians())
                            .ok_or_else(|| format!("`{text}` is not an angle"))?;
                        let rotation = glam::DQuat::from_axis_angle(plane.normal, sweep);
                        a + rotation * (b - a)
                    }
                    (_, ValueFieldInput::Radius(r)) => {
                        // The bulge of an arc of radius r over the chord.
                        let half = a.distance(b) * 0.5;
                        let r = r.as_mm().max(half);
                        let sagitta = r - (r * r - half * half).max(0.0).sqrt();
                        bulge_point(a, b, &plane, cursor, sagitta)
                    }
                    _ => bulge_point(a, b, &plane, cursor, length(text, cx)?),
                };
                let (name, params) = self.params(c).ok_or("that arc is degenerate")?;
                self.picks.clear();
                Ok(Flow::run(&name, params))
            }
            _ => Err("pick the first point".into()),
        }
    }

    fn arrow(&mut self, axis: Axis) -> bool {
        if !self.picks.is_empty() {
            return false;
        }
        self.forced = if self.forced == Some(axis) {
            None
        } else {
            Some(axis)
        };
        true
    }

    fn escape(&mut self) -> bool {
        self.forced = None;
        let busy = !self.picks.is_empty();
        self.picks.clear();
        busy
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some(&a), Some(c)) = (self.picks.first(), self.cursor) {
            let b = self.picks.get(1).copied().unwrap_or(c);
            out.push(Shape::Line {
                a,
                b,
                color: BAND,
                dashed: true,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        self.params(self.cursor?)
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.horizontal(|ui| {
            ui.label("Segments");
            ui.add(egui::DragValue::new(&mut self.segments).range(1..=999));
        });
        ui.weak(format!(
            "Type `{}s` in the value field to set it.",
            self.segments
        ));
    }
}

/// The point at `sagitta` from the chord midpoint, on the cursor's side.
fn bulge_point(a: DVec3, b: DVec3, plane: &Plane, cursor: DVec3, sagitta: f64) -> DVec3 {
    let mid = (a + b) * 0.5;
    let side = plane.normal.cross(b - a).normalize_or_zero();
    let sign = if (plane.project(cursor) - mid).dot(side) < 0.0 {
        -1.0
    } else {
        1.0
    };
    mid + side * sign * sagitta
}
