//! Implements docs/05-modeling-tools.md §4.21–§4.23 interactively
//! (milestone 1.42): Mirror, Revolve and Pattern pick their plane, axis or
//! direction in the viewport and dispatch the same commands as the
//! Tools-menu windows.

use glam::DVec3;
use planyrith_core::inference::Constraint;
use planyrith_core::math::Plane;
use serde_json::{Value, json};

use super::{BAND, Cx, Flow, Pointer, Shape, Tool, angle, length, parse, pt, targets};
use crate::i18n::{en, t};

fn close(a: DVec3, b: DVec3) -> bool {
    a.distance(b) <= planyrith_core::Tolerances::default().length
}

// ----- mirror -------------------------------------------------------------------------------

/// The Mirror tool: two points of a line in the drawing plane; the mirror
/// plane holds the line and stands square to the drawing plane.
#[derive(Debug)]
pub struct MirrorTool {
    first: Option<(DVec3, Plane, String)>,
    cursor: Option<DVec3>,
    /// Tool Options: keep the originals (the default).
    copy: bool,
}

impl Default for MirrorTool {
    fn default() -> Self {
        MirrorTool {
            first: None,
            cursor: None,
            copy: true,
        }
    }
}

impl MirrorTool {
    fn params(&self, to: DVec3) -> Option<Value> {
        let (a, plane, targets) = self.first.as_ref()?;
        let b = plane.project(to);
        let normal = (b - *a).cross(plane.normal).try_normalize()?;
        (!close(*a, b)).then(|| {
            json!({ "targets": targets, "point": pt(*a), "normal": pt(normal), "copy": self.copy })
        })
    }
}

impl Tool for MirrorTool {
    fn command(&self) -> &'static str {
        "mirror"
    }

    fn label(&self) -> &'static str {
        en("Mirror")
    }

    fn step(&self) -> usize {
        usize::from(self.first.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        match &self.first {
            Some((a, plane, _)) => super::Setup {
                constraint: Constraint::Plane(*plane),
                reference: Some(*a),
            },
            None => super::Setup::default(),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let point = p.inference.point;
        match &self.first {
            None => {
                if let Some(t) = targets(cx, p.target.as_ref()) {
                    let plane = super::drawing_plane(&p.inference, None, &cx.camera);
                    self.first = Some((plane.project(point), plane, t));
                }
                Flow::Continue
            }
            Some(_) => match self.params(point) {
                Some(params) => {
                    self.first = None;
                    Flow::run("mirror", params)
                }
                None => Flow::Continue,
            },
        }
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err(t("Pick two points of the mirror line"))
    }

    fn takes_points(&self) -> bool {
        true
    }

    fn escape(&mut self) -> bool {
        self.first.take().is_some()
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.checkbox(&mut self.copy, t("Keep the originals (mirrored copies)"));
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some((a, plane, _)), Some(c)) = (&self.first, self.cursor) {
            out.push(Shape::Line {
                a: *a,
                b: plane.project(c),
                color: BAND,
                dashed: true,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        Some(("mirror".into(), self.params(self.cursor?)?))
    }
}

// ----- revolve ------------------------------------------------------------------------------

/// The Revolve tool: the profile face, then two points of the axis; the
/// turn comes from Tool Options or the value field.
#[derive(Debug)]
pub struct RevolveTool {
    profile: Option<String>,
    axis_point: Option<DVec3>,
    cursor: Option<DVec3>,
    /// Tool Options: the turn, degrees.
    degrees: f64,
}

impl Default for RevolveTool {
    fn default() -> Self {
        RevolveTool {
            profile: None,
            axis_point: None,
            cursor: None,
            degrees: 360.0,
        }
    }
}

impl RevolveTool {
    fn params(&self, to: DVec3, radians: f64) -> Option<Value> {
        let profile = self.profile.as_ref()?;
        let a = self.axis_point?;
        let axis = (to - a).try_normalize()?;
        Some(json!({
            "profile": profile, "axis_point": pt(a), "axis": pt(axis), "angle": radians,
        }))
    }
}

impl Tool for RevolveTool {
    fn command(&self) -> &'static str {
        "revolve"
    }

    fn label(&self) -> &'static str {
        en("Revolve")
    }

    fn step(&self) -> usize {
        match (&self.profile, self.axis_point) {
            (None, _) => 0,
            (Some(_), None) => 1,
            _ => 2,
        }
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        super::Setup {
            constraint: Constraint::Free,
            reference: self.axis_point,
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, _cx: &Cx<'_>) -> Flow {
        let point = p.inference.point;
        match (&self.profile, self.axis_point) {
            (None, _) => {
                if let Some(face) = &p.face {
                    self.profile = Some(format!("{}", face.target));
                }
                Flow::Continue
            }
            (Some(_), None) => {
                self.axis_point = Some(point);
                Flow::Continue
            }
            (Some(_), Some(_)) => match self.params(point, self.degrees.to_radians()) {
                Some(params) => {
                    self.profile = None;
                    self.axis_point = None;
                    Flow::run("revolve", params)
                }
                None => Flow::Continue,
            },
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let radians = angle(text, cx)?;
        let cursor = self.cursor.ok_or("pick the axis first")?;
        let params = self
            .params(cursor, radians)
            .ok_or_else(|| t("Pick the profile face and two points of the axis"))?;
        self.profile = None;
        self.axis_point = None;
        Ok(Flow::run("revolve", params))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        (last.0 == "revolve").then_some(())?;
        let mut params = last.1.clone();
        params["angle"] = json!(parse(text, cx).ok()?.as_angle()?.as_radians());
        Some(params)
    }

    fn takes_points(&self) -> bool {
        self.profile.is_some()
    }

    fn escape(&mut self) -> bool {
        if self.axis_point.take().is_some() {
            return true;
        }
        self.profile.take().is_some()
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.horizontal(|ui| {
            ui.label(t("Turn"));
            ui.add(
                egui::DragValue::new(&mut self.degrees)
                    .range(1.0..=360.0)
                    .suffix("°"),
            );
        });
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some(a), Some(c)) = (self.axis_point, self.cursor) {
            out.push(Shape::Line {
                a,
                b: c,
                color: BAND,
                dashed: true,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        Some((
            "revolve".into(),
            self.params(self.cursor?, self.degrees.to_radians())?,
        ))
    }
}

// ----- pattern ------------------------------------------------------------------------------

/// The Pattern tool: linear (a base point, then a point at the spacing) or
/// circular (the center, about the drawing plane's normal); the count comes
/// from Tool Options, or `5x` in the value field afterwards.
#[derive(Debug)]
pub struct PatternTool {
    base: Option<(DVec3, Plane, String)>,
    cursor: Option<DVec3>,
    /// Tool Options: circular instead of linear.
    circular: bool,
    /// Tool Options: instances, the original included.
    count: u32,
}

impl Default for PatternTool {
    fn default() -> Self {
        PatternTool {
            base: None,
            cursor: None,
            circular: false,
            count: 3,
        }
    }
}

impl PatternTool {
    fn params(&self, to: DVec3, spacing: Option<f64>) -> Option<Value> {
        let (a, plane, targets) = self.base.as_ref()?;
        if self.circular {
            return Some(json!({
                "targets": targets, "kind": "circular", "count": self.count,
                "center": pt(*a), "axis": pt(plane.normal),
            }));
        }
        let d = to - *a;
        let direction = d.try_normalize()?;
        Some(json!({
            "targets": targets, "kind": "linear", "count": self.count,
            "direction": pt(direction), "spacing": spacing.unwrap_or(d.length()),
        }))
    }
}

impl Tool for PatternTool {
    fn command(&self) -> &'static str {
        "pattern"
    }

    fn label(&self) -> &'static str {
        en("Pattern")
    }

    fn step(&self) -> usize {
        usize::from(self.base.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        super::Setup {
            constraint: Constraint::Free,
            reference: self.base.as_ref().map(|b| b.0),
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let point = p.inference.point;
        match &self.base {
            None => {
                if let Some(t) = targets(cx, p.target.as_ref()) {
                    let plane = super::drawing_plane(&p.inference, None, &cx.camera);
                    self.base = Some((point, plane, t));
                }
                Flow::Continue
            }
            Some((a, _, _)) => {
                if !self.circular && close(*a, point) {
                    return Flow::Continue;
                }
                match self.params(point, None) {
                    Some(params) => {
                        self.base = None;
                        Flow::run("pattern", params)
                    }
                    None => Flow::Continue,
                }
            }
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        // While picking the second point: the spacing along the pointer.
        let cursor = self.cursor.ok_or("pick the base point first")?;
        let spacing = length(text, cx)?;
        let params = self
            .params(cursor, Some(spacing))
            .ok_or_else(|| t("Pick the base point, then the direction"))?;
        self.base = None;
        Ok(Flow::run("pattern", params))
    }

    fn revalue(&self, last: &(String, Value), text: &str, cx: &Cx<'_>) -> Option<Value> {
        (last.0 == "pattern").then_some(())?;
        let input = parse(text, cx).ok()?;
        let mut params = last.1.clone();
        // `5x` or `*5`: the count; a length: the spacing (linear).
        let raw = text
            .trim()
            .trim_start_matches('*')
            .trim_end_matches(['x', 'X']);
        if text.contains(['x', 'X', '*'])
            && let Ok(n) = raw.trim().parse::<u32>()
        {
            params["count"] = json!(n.max(2));
            return Some(params);
        }
        if params["kind"] == "linear" {
            params["spacing"] = json!(input.as_length(cx.unit)?.as_mm());
            return Some(params);
        }
        None
    }

    fn takes_points(&self) -> bool {
        true
    }

    fn escape(&mut self) -> bool {
        self.base.take().is_some()
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.circular, false, t("Linear"));
            ui.selectable_value(&mut self.circular, true, t("Circular"));
        });
        ui.horizontal(|ui| {
            ui.label(t("Instances"));
            ui.add(egui::DragValue::new(&mut self.count).range(2..=1000));
        });
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some((a, _, _)), Some(c)) = (&self.base, self.cursor) {
            out.push(Shape::Line {
                a: *a,
                b: c,
                color: BAND,
                dashed: true,
            });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        let cursor = self.cursor?;
        if !self.circular && self.base.as_ref().is_some_and(|b| close(b.0, cursor)) {
            return None;
        }
        Some(("pattern".into(), self.params(cursor, None)?))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::viewport::Viewport;
    use planyrith_core::document::{EntityKey, EntityPath, InstancePath};
    use planyrith_core::ids::Pid;
    use planyrith_core::inference::{Inference, InferenceKind};

    fn pointer(p: [f64; 3], face: Option<u64>) -> Pointer {
        let point = DVec3::from_array(p);
        Pointer {
            inference: Inference {
                point,
                kind: InferenceKind::Free,
                on: None,
                locked: false,
                label: String::new(),
                axis: None,
                from: None,
                direction: None,
                plane: Some(Plane::from_point_normal(point, DVec3::Z)),
                via: None,
            },
            modifiers: egui::Modifiers::NONE,
            target: None,
            face: face.map(|pid| EntityPath::new(InstancePath::root(), EntityKey::Pid(Pid(pid)))),
            screen: egui::Pos2::ZERO,
        }
    }

    fn run(session: &mut Session, flow: Flow) -> Value {
        let Flow::Run(commands) = flow else {
            panic!("{flow:?}");
        };
        let mut out = Value::Null;
        for (name, params) in commands {
            out = session.execute(&name, params).unwrap();
        }
        out
    }

    fn boxed(session: &mut Session) {
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 30 }),
            ),
            (
                "make-group",
                json!({ "targets": "edge|face", "name": "Box" }),
            ),
            ("select", json!({ "query": "group" })),
        ] {
            session.execute(name, params).unwrap();
        }
    }

    fn x_range(session: &Session) -> (f64, f64) {
        let doc = session.doc();
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for (_, g) in doc.model.groups() {
            for (_, v) in doc
                .definition(g.definition)
                .unwrap()
                .entities
                .mesh()
                .vertices()
            {
                let x = g.transform.transform_point3(v.position).x;
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
        (lo, hi)
    }

    #[test]
    fn mirror_across_a_picked_line() {
        let mut session = Session::new();
        boxed(&mut session);
        let viewport = Viewport::default();
        let mut tool = MirrorTool::default();
        let cx = viewport.cx(&session);
        assert_eq!(
            tool.click(&pointer([200.0, 0.0, 0.0], None), &cx),
            Flow::Continue
        );
        assert_eq!(tool.step(), 1);
        tool.hover(&pointer([200.0, 100.0, 0.0], None), &cx);
        let (name, preview) = tool.preview(&cx).unwrap();
        assert_eq!(name, "mirror");
        assert_eq!(preview["normal"], json!([1.0, 0.0, 0.0]));
        let flow = tool.click(&pointer([200.0, 100.0, 0.0], None), &cx);
        run(&mut session, flow);
        // The copy lies from 300 to 400; the original stays.
        assert_eq!(session.doc().model.groups().count(), 2);
        assert_eq!(x_range(&session), (0.0, 400.0));
        assert_eq!(tool.step(), 0);
    }

    #[test]
    fn revolve_a_picked_face_about_a_picked_axis() {
        let mut session = Session::new();
        session
            .execute(
                "line",
                json!({ "points": [[10, 0, 0], [30, 0, 0], [30, 0, 20], [10, 0, 20]], "close": true }),
            )
            .unwrap();
        let face = {
            let model = &session.doc().model;
            let (f, _) = model.mesh().faces().next().unwrap();
            model.pid_of(f.into()).unwrap().0
        };
        let viewport = Viewport::default();
        let mut tool = RevolveTool::default();
        let cx = viewport.cx(&session);
        tool.click(&pointer([20.0, 0.0, 10.0], Some(face)), &cx);
        tool.click(&pointer([0.0, 0.0, 0.0], None), &cx);
        assert_eq!(tool.step(), 2);
        tool.hover(&pointer([0.0, 0.0, 50.0], None), &cx);
        // A typed turn completes it.
        let flow = tool.value("90", &cx).unwrap();
        let out = run(&mut session, flow);
        assert!(out["volume_mm3"].as_f64().unwrap() > 0.0, "{out}");
        // Typing again replaces the turn.
        let last = (
            "revolve".to_string(),
            json!({ "profile": "@1", "axis_point": [0, 0, 0], "axis": [0, 0, 1], "angle": 1.0 }),
        );
        let cx = viewport.cx(&session);
        let again = tool.revalue(&last, "180", &cx).unwrap();
        assert!((again["angle"].as_f64().unwrap() - std::f64::consts::PI).abs() < 1e-12);
    }

    #[test]
    fn linear_and_circular_patterns() {
        let mut session = Session::new();
        boxed(&mut session);
        let viewport = Viewport::default();
        let mut tool = PatternTool::default();
        let cx = viewport.cx(&session);
        tool.click(&pointer([0.0, 0.0, 0.0], None), &cx);
        // Clicking the base point again does not finish.
        assert_eq!(
            tool.click(&pointer([0.0, 0.0, 0.0], None), &cx),
            Flow::Continue
        );
        let flow = tool.click(&pointer([150.0, 0.0, 0.0], None), &cx);
        let Flow::Run(commands) = &flow else { panic!() };
        let last = commands[0].clone();
        run(&mut session, flow);
        assert_eq!(x_range(&session), (0.0, 400.0));
        // `5x` afterwards asks for five.
        let cx = viewport.cx(&session);
        let again = tool.revalue(&last, "5x", &cx).unwrap();
        assert_eq!(again["count"], 5);
        let again = tool.revalue(&last, "200", &cx).unwrap();
        assert_eq!(again["spacing"], 200.0);
        // Circular: about the drawing plane's normal through the center.
        tool.circular = true;
        tool.count = 4;
        tool.click(&pointer([-100.0, 0.0, 0.0], None), &cx);
        tool.hover(&pointer([0.0, 0.0, 0.0], None), &cx);
        let (_, params) = tool.preview(&cx).unwrap();
        assert_eq!(params["kind"], "circular");
        assert_eq!(params["axis"], json!([0.0, 0.0, 1.0]));
        assert_eq!(params["center"], json!([-100.0, 0.0, 0.0]));
    }
}
