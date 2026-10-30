//! Implements docs/05 §§3.2, 3.4: planar strokes and oriented rectangles.
use super::{
    BAND, Cx, Drag, Flow, Pointer, Setup, Shape, Tool, along, angle, drawing_plane, length, pt,
};
use glam::DVec3;
use planyrith_core::{
    inference::Constraint,
    math::{Axis, Plane},
    units::Angle,
};
use serde_json::{Value, json};

/// Press-drag-release Freehand tool; one core command per stroke.
#[derive(Debug)]
pub struct FreehandTool {
    points: Vec<DVec3>,
    plane: Option<Plane>,
    screen: Option<egui::Pos2>,
    tolerance: f64,
}
impl Default for FreehandTool {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            plane: None,
            screen: None,
            tolerance: 0.5,
        }
    }
}
impl FreehandTool {
    fn begin(&mut self, p: &Pointer) {
        let plane = p
            .inference
            .plane
            .unwrap_or_else(|| Plane::from_point_normal(p.inference.point, DVec3::Z));
        self.points = vec![plane.project(p.inference.point)];
        self.plane = Some(plane);
        self.screen = Some(p.screen);
    }
    fn sample(&mut self, p: &Pointer, force: bool) {
        if !force
            && self
                .screen
                .is_some_and(|last| last.distance(p.screen) < 2.0)
        {
            return;
        }
        if self.points.len() >= 65535 {
            return;
        }
        if let Some(plane) = self.plane {
            let point = plane.project(p.inference.point);
            if self
                .points
                .last()
                .is_none_or(|last| last.distance(point) > 1e-3)
            {
                self.points.push(point);
                self.screen = Some(p.screen);
            }
        }
    }
}
impl Tool for FreehandTool {
    fn command(&self) -> &'static str {
        "freehand"
    }
    fn label(&self) -> &'static str {
        "Freehand"
    }
    fn step(&self) -> usize {
        0
    }
    fn takes_points(&self) -> bool {
        false
    }
    fn setup(&self, _: &Cx<'_>) -> Setup {
        Setup {
            constraint: self.plane.map_or(Constraint::Free, Constraint::Plane),
            reference: self.points.last().copied(),
        }
    }
    fn press(&mut self, p: &Pointer, _: &Cx<'_>) {
        self.begin(p);
    }
    fn click(&mut self, _: &Pointer, _: &Cx<'_>) -> Flow {
        self.escape();
        Flow::Continue
    }
    fn drag(&mut self, phase: Drag, p: &Pointer, _: egui::Pos2, _: &Cx<'_>) -> Flow {
        if self.points.is_empty() {
            self.begin(p);
        }
        self.sample(p, phase == Drag::End);
        if phase != Drag::End {
            return Flow::Continue;
        }
        self.plane = None;
        self.screen = None;
        let points = std::mem::take(&mut self.points);
        if points.len() < 2 {
            return Flow::Continue;
        }
        Flow::run(
            "freehand",
            json!({"points":points.iter().map(|p|pt(*p)).collect::<Vec<_>>(),"simplify_tolerance":self.tolerance}),
        )
    }
    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let tolerance = length(text, cx)?;
        if !tolerance.is_finite() || tolerance < 0.0 {
            return Err("simplification tolerance must be nonnegative".into());
        }
        self.tolerance = tolerance;
        Ok(Flow::Continue)
    }
    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        if last.0 != "freehand" {
            return None;
        }
        let tolerance = length(text, cx).ok()?;
        if tolerance < 0.0 {
            return None;
        }
        let mut params = last.1.clone();
        params["simplify_tolerance"] = json!(tolerance);
        Some(params)
    }
    fn escape(&mut self) -> bool {
        let active = !self.points.is_empty();
        self.points.clear();
        self.plane = None;
        self.screen = None;
        active
    }
    fn shapes(&self, out: &mut Vec<Shape>) {
        if self.points.len() > 1 {
            out.push(Shape::Path {
                points: self.points.clone(),
                closed: false,
                color: BAND,
            });
        }
    }
    fn options(&mut self, ui: &mut egui::Ui, _: &Cx<'_>) {
        ui.label("Simplification tolerance (mm)");
        ui.add(
            egui::DragValue::new(&mut self.tolerance)
                .range(0.0..=10000.0)
                .speed(0.1),
        );
    }
}

/// Three-point rotated rectangle with exact edge length and width/angle input.
#[derive(Debug, Default)]
pub struct RotatedRectangleTool {
    first: Option<(DVec3, Plane)>,
    edge: Option<DVec3>,
    cursor: Option<DVec3>,
    forced: Option<Axis>,
}
impl RotatedRectangleTool {
    fn values(&self) -> Option<(f64, f64)> {
        let (origin, plane) = self.first?;
        let edge = self.edge?;
        let d = plane.project(self.cursor?) - edge;
        let u = (edge - origin).try_normalize()?;
        let v = plane.normal.cross(u);
        Some((d.length(), d.dot(v).atan2(d.dot(u))))
    }
    fn params(&self, width: f64, angle: f64) -> Option<Value> {
        let (origin, plane) = self.first?;
        let edge = self.edge?;
        if width <= 1e-3 || angle.sin().abs() <= 1e-6 {
            return None;
        }
        Some(
            json!({"origin":pt(origin),"edge_end":pt(edge),"width":width,"angle":angle,"normal":pt(plane.normal)}),
        )
    }
    fn typed(text: &str, cx: &Cx<'_>, previous: f64) -> Result<(f64, f64), String> {
        match text.split_once(',') {
            Some((w, a)) => Ok((length(w.trim(), cx)?, angle(a.trim(), cx)?)),
            None => Ok((
                length(text, cx)?,
                if previous.sin().abs() > 1e-6 {
                    previous
                } else {
                    Angle::RIGHT.as_radians()
                },
            )),
        }
    }
}
impl Tool for RotatedRectangleTool {
    fn command(&self) -> &'static str {
        "rotated-rectangle"
    }
    fn label(&self) -> &'static str {
        "Rotated Rectangle"
    }
    fn step(&self) -> usize {
        if self.edge.is_some() {
            2
        } else {
            usize::from(self.first.is_some())
        }
    }
    fn setup(&self, _: &Cx<'_>) -> Setup {
        match self.first {
            Some((a, plane)) => Setup {
                constraint: Constraint::Plane(plane),
                reference: Some(self.edge.unwrap_or(a)),
            },
            None => Setup::default(),
        }
    }
    fn hover(&mut self, p: &Pointer, _: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }
    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        self.cursor = Some(p.inference.point);
        let Some((a, plane)) = self.first else {
            self.first = Some((
                p.inference.point,
                drawing_plane(&p.inference, self.forced, &cx.camera),
            ));
            return Flow::Continue;
        };
        if self.edge.is_none() {
            let b = plane.project(p.inference.point);
            if b.distance(a) > 1e-3 {
                self.edge = Some(b);
            }
            return Flow::Continue;
        }
        if let Some((w, angle)) = self.values()
            && let Some(params) = self.params(w, angle)
        {
            self.escape();
            return Flow::run("rotated-rectangle", params);
        }
        Flow::Continue
    }
    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let (a, plane) = self.first.ok_or("pick the first corner")?;
        if self.edge.is_none() {
            let b = plane.project(self.cursor.unwrap_or(a + plane.frame().u));
            let l = length(text, cx)?;
            if l <= 1e-3 {
                return Err("first edge must have positive length".into());
            }
            self.edge =
                Some(along(a, b, l).ok_or("move the pointer to set the first edge direction")?);
            return Ok(Flow::Continue);
        }
        let (w, angle) = Self::typed(
            text,
            cx,
            self.values().map_or(Angle::RIGHT.as_radians(), |(_, a)| a),
        )?;
        let params = self
            .params(w, angle)
            .ok_or("give a positive width and a non-collinear angle")?;
        self.escape();
        Ok(Flow::run("rotated-rectangle", params))
    }
    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        if last.0 != "rotated-rectangle" {
            return None;
        }
        let mut params = last.1.clone();
        let (w, angle) = Self::typed(text, cx, params["angle"].as_f64()?).ok()?;
        if w <= 1e-3 || angle.sin().abs() <= 1e-6 {
            return None;
        }
        params["width"] = json!(w);
        params["angle"] = json!(angle);
        Some(params)
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
        let active = self.first.take().is_some();
        self.edge = None;
        self.cursor = None;
        self.forced = None;
        active
    }
    fn shapes(&self, out: &mut Vec<Shape>) {
        if let Some((a, plane)) = self.first {
            if let Some(b) = self.edge {
                if let Some(c) = self.cursor {
                    let d = plane.project(c) - b;
                    out.push(Shape::Path {
                        points: vec![a, b, b + d, a + d],
                        closed: true,
                        color: BAND,
                    });
                }
            } else if let Some(b) = self.cursor {
                out.push(Shape::Line {
                    a,
                    b: plane.project(b),
                    color: BAND,
                    dashed: false,
                });
            }
        }
    }
    fn preview(&self, _: &Cx<'_>) -> Option<(String, Value)> {
        let (w, a) = self.values()?;
        Some(("rotated-rectangle".into(), self.params(w, a)?))
    }
}
