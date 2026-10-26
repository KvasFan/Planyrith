//! Annotation and guide tools (docs/05 §3.8, §5.10, §5.11, §6, §7):
//! Dimension, Text, Tape Measure, Protractor, Section Plane, Axes and 3D
//! Text. Like every tool they only collect parameters and hand the
//! application one command (docs/07 §3).

use glam::{DAffine3, DMat3, DVec3};
use planyrith_core::document::EntityKey;
use planyrith_core::ids::EntityId;
use planyrith_core::math::{Axis, Plane};
use serde_json::{Value, json};

use super::{BAND, Cx, Flow, Pointer, Shape, Tool, angle, length, parse, plane_json, pt};

const GUIDE: egui::Color32 = egui::Color32::from_rgb(90, 90, 90);
const SECTION: egui::Color32 = egui::Color32::from_rgb(235, 120, 20);

/// The pid of an edge under the pointer that belongs to an arc or circle.
fn curve_edge(p: &Pointer, cx: &Cx<'_>) -> Option<u64> {
    let path = p.target.as_ref()?;
    let resolved = cx.doc.resolve(path).ok()?;
    let EntityId::Edge(e) = resolved.entity else {
        return None;
    };
    let entities = cx.doc.container(resolved.container)?;
    entities.mesh().edge(e)?.curve?;
    match path.target {
        EntityKey::Pid(pid) => Some(pid.0),
        EntityKey::Id(id) => entities.pid_of(id).map(|p| p.0),
    }
}

// ----- Dimension ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
enum DimState {
    #[default]
    Idle,
    Start(DVec3),
    Ends(DVec3, DVec3),
    Radial(u64, DVec3),
}

/// The Dimension tool (docs/05 §7.1): two points then the line's place; an
/// arc or circle gives a radius dimension (diameter with Ctrl).
#[derive(Debug, Default)]
pub struct DimensionTool {
    state: DimState,
    cursor: Option<DVec3>,
}

/// The offset from the line `a`→`b` to `c`, across the line.
fn across(a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let d = (b - a).normalize_or_zero();
    let v = c - a;
    v - d * v.dot(d)
}

impl Tool for DimensionTool {
    fn command(&self) -> &'static str {
        "dimension"
    }

    fn label(&self) -> &'static str {
        "Dimension"
    }

    fn step(&self) -> usize {
        match self.state {
            DimState::Idle => 0,
            DimState::Start(_) | DimState::Radial(..) => 1,
            DimState::Ends(..) => 2,
        }
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        super::Setup {
            reference: match self.state {
                DimState::Start(a) => Some(a),
                _ => None,
            },
            ..Default::default()
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let at = p.inference.point;
        match self.state {
            DimState::Idle => {
                self.state = match curve_edge(p, cx) {
                    Some(pid) => DimState::Radial(pid, at),
                    None => DimState::Start(at),
                };
                Flow::Continue
            }
            DimState::Start(a) => {
                if a.distance(at) > 1e-6 {
                    self.state = DimState::Ends(a, at);
                }
                Flow::Continue
            }
            DimState::Ends(a, b) => {
                self.state = DimState::Idle;
                Flow::run(
                    "dimension",
                    json!({ "start": pt(a), "end": pt(b), "offset": pt(across(a, b, at)) }),
                )
            }
            DimState::Radial(pid, q) => {
                self.state = DimState::Idle;
                let kind = if p.modifiers.ctrl {
                    "diameter"
                } else {
                    "radius"
                };
                Flow::run(
                    "dimension",
                    json!({ "kind": kind, "curve": { "Pid": pid }, "offset": pt(at - q) }),
                )
            }
        }
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err("the Dimension tool measures; place it with the mouse".into())
    }

    fn escape(&mut self) -> bool {
        let busy = !matches!(self.state, DimState::Idle);
        self.state = DimState::Idle;
        busy
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        let Some(c) = self.cursor else { return };
        match self.state {
            DimState::Idle => {}
            DimState::Start(a) => out.push(Shape::Line {
                a,
                b: c,
                color: BAND,
                dashed: true,
            }),
            DimState::Ends(a, b) => {
                let o = across(a, b, c);
                for (from, to) in [(a, a + o), (b, b + o), (a + o, b + o)] {
                    out.push(Shape::Line {
                        a: from,
                        b: to,
                        color: BAND,
                        dashed: false,
                    });
                }
            }
            DimState::Radial(_, q) => out.push(Shape::Line {
                a: q,
                b: c,
                color: BAND,
                dashed: false,
            }),
        }
    }
}

// ----- Text ---------------------------------------------------------------------------------

/// The Text tool (docs/05 §7.2): click what the text points to (empty space:
/// screen text), click where the text goes. The text comes from Tool
/// Options; empty, it is what the point measures.
#[derive(Debug, Default)]
pub struct TextTool {
    text: String,
    anchor: Option<(DVec3, egui::Pos2)>,
    cursor: Option<DVec3>,
}

impl Tool for TextTool {
    fn command(&self) -> &'static str {
        "text"
    }

    fn label(&self) -> &'static str {
        "Text"
    }

    fn step(&self) -> usize {
        usize::from(self.anchor.is_some())
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, _cx: &Cx<'_>) -> Flow {
        let text = (!self.text.trim().is_empty()).then(|| self.text.clone());
        match self.anchor.take() {
            None if p.target.is_none() && p.face.is_none() => {
                // Empty space: screen text.
                Flow::run(
                    "text",
                    json!({ "text": text.unwrap_or_else(|| "Text".into()) }),
                )
            }
            None => {
                self.anchor = Some((p.inference.point, p.screen));
                Flow::Continue
            }
            Some((at, screen)) => {
                let d = p.screen - screen;
                let mut params = json!({ "at": pt(at), "screen_offset": [d.x, -d.y] });
                if let Some(t) = text {
                    params["text"] = json!(t);
                }
                Flow::run("text", params)
            }
        }
    }

    fn value(&mut self, text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        self.text = text.to_string();
        Ok(Flow::Continue)
    }

    fn takes_points(&self) -> bool {
        false
    }

    fn escape(&mut self) -> bool {
        self.anchor.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some((a, _)), Some(c)) = (self.anchor, self.cursor) {
            out.push(Shape::Line {
                a,
                b: c,
                color: BAND,
                dashed: false,
            });
        }
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.label("Text (empty: what it points to measures)");
        ui.text_edit_multiline(&mut self.text);
    }
}

// ----- Tape Measure ---------------------------------------------------------------------------

/// The Tape Measure tool (docs/05 §6.1): measures between two points and
/// leaves a guide; typing a length right after a measurement resizes the
/// model so that measurement becomes it. Ctrl measures without a guide.
#[derive(Debug, Default)]
pub struct TapeMeasureTool {
    from: Option<DVec3>,
    cursor: Option<DVec3>,
    last: Option<f64>,
}

impl Tool for TapeMeasureTool {
    fn command(&self) -> &'static str {
        "tape-measure"
    }

    fn label(&self) -> &'static str {
        "Tape Measure"
    }

    fn step(&self) -> usize {
        usize::from(self.from.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        super::Setup {
            reference: self.from,
            ..Default::default()
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, _cx: &Cx<'_>) -> Flow {
        let at = p.inference.point;
        match self.from.take() {
            None => {
                self.from = Some(at);
                self.last = None;
                Flow::Continue
            }
            Some(from) => {
                self.last = Some(from.distance(at));
                Flow::run(
                    "tape-measure",
                    json!({ "from": pt(from), "to": pt(at), "create_guide": !p.modifiers.ctrl }),
                )
            }
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let target = length(text, cx)?;
        match (self.from, self.last) {
            (Some(from), _) => {
                let to = super::along(from, self.cursor.ok_or("move the cursor first")?, target)
                    .ok_or("move the cursor to give the direction")?;
                self.from = None;
                self.last = Some(target);
                Ok(Flow::run(
                    "tape-measure",
                    json!({ "from": pt(from), "to": pt(to) }),
                ))
            }
            (None, Some(measured)) if measured > 0.0 => {
                self.last = None;
                Ok(Flow::run(
                    "resize-model",
                    json!({ "measured": measured, "target": target }),
                ))
            }
            _ => Err("measure something first".into()),
        }
    }

    fn escape(&mut self) -> bool {
        self.from.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let (Some(a), Some(b)) = (self.from, self.cursor) {
            out.push(Shape::Line {
                a,
                b,
                color: GUIDE,
                dashed: true,
            });
        }
    }
}

// ----- Protractor -----------------------------------------------------------------------------

/// The Protractor tool (docs/05 §6.2): center, a point on the first leg, then
/// the angle (a point, or a typed angle); leaves an angular guide line.
#[derive(Debug, Default)]
pub struct ProtractorTool {
    center: Option<DVec3>,
    first: Option<DVec3>,
    axis: Option<Axis>,
    cursor: Option<(DVec3, Option<Plane>)>,
}

impl ProtractorTool {
    fn normal(&self) -> DVec3 {
        self.axis.map_or(DVec3::Z, Axis::direction)
    }
}

impl Tool for ProtractorTool {
    fn command(&self) -> &'static str {
        "protractor"
    }

    fn label(&self) -> &'static str {
        "Protractor"
    }

    fn step(&self) -> usize {
        usize::from(self.center.is_some()) + usize::from(self.first.is_some())
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        super::Setup {
            reference: self.center,
            ..Default::default()
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some((p.inference.point, p.inference.plane));
    }

    fn arrow(&mut self, axis: Axis) -> bool {
        self.axis = Some(axis);
        true
    }

    fn click(&mut self, p: &Pointer, _cx: &Cx<'_>) -> Flow {
        let at = p.inference.point;
        match (self.center, self.first) {
            (None, _) => {
                self.center = Some(at);
                if self.axis.is_none() {
                    self.axis = p.inference.plane.and_then(|pl| {
                        [Axis::X, Axis::Y, Axis::Z]
                            .into_iter()
                            .find(|a| pl.normal.dot(a.direction()).abs() > 0.999)
                    });
                }
                Flow::Continue
            }
            (Some(_), None) => {
                self.first = Some(at);
                Flow::Continue
            }
            (Some(center), Some(first)) => {
                let normal = self.normal();
                self.center = None;
                self.first = None;
                self.axis = None;
                Flow::run(
                    "protractor",
                    json!({ "center": pt(center), "axis": pt(normal), "from_point": pt(first), "to_point": pt(at) }),
                )
            }
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let (Some(center), Some(first)) = (self.center, self.first) else {
            return Err("pick the center and the first leg first".into());
        };
        let a = angle(text, cx)?;
        let normal = self.normal();
        self.center = None;
        self.first = None;
        self.axis = None;
        Ok(Flow::run(
            "protractor",
            json!({ "center": pt(center), "axis": pt(normal), "from_point": pt(first), "angle": a }),
        ))
    }

    fn escape(&mut self) -> bool {
        let busy = self.center.is_some();
        self.center = None;
        self.first = None;
        self.axis = None;
        busy
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        let Some(center) = self.center else { return };
        if let Some(first) = self.first {
            out.push(Shape::Line {
                a: center,
                b: first,
                color: GUIDE,
                dashed: false,
            });
        }
        if let Some((c, _)) = self.cursor {
            out.push(Shape::Line {
                a: center,
                b: c,
                color: GUIDE,
                dashed: true,
            });
        }
        out.push(Shape::Marker {
            p: center,
            color: GUIDE,
        });
    }
}

// ----- Section Plane --------------------------------------------------------------------------

/// The Section Plane tool (docs/05 §5.11): the plane follows the face under
/// the cursor (an arrow key locks an axis plane); a click places it.
#[derive(Debug, Default)]
pub struct SectionPlaneTool {
    axis: Option<Axis>,
    plane: Option<Plane>,
}

impl SectionPlaneTool {
    fn plane_at(&self, p: &Pointer) -> Plane {
        match (self.axis, p.inference.plane) {
            (Some(axis), _) => Plane::from_point_normal(p.inference.point, axis.direction()),
            (None, Some(face))
                if face.contains(p.inference.point, &planyrith_core::Tolerances::default()) =>
            {
                face
            }
            _ => Plane::from_point_normal(p.inference.point, DVec3::Z),
        }
    }
}

impl Tool for SectionPlaneTool {
    fn command(&self) -> &'static str {
        "section-plane-add"
    }

    fn label(&self) -> &'static str {
        "Section Plane"
    }

    fn step(&self) -> usize {
        0
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.plane = Some(self.plane_at(p));
    }

    fn arrow(&mut self, axis: Axis) -> bool {
        self.axis = if self.axis == Some(axis) {
            None
        } else {
            Some(axis)
        };
        true
    }

    fn click(&mut self, p: &Pointer, _cx: &Cx<'_>) -> Flow {
        let plane = self.plane_at(p);
        Flow::run("section-plane-add", json!({ "plane": plane_json(&plane) }))
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err("place the plane with the mouse".into())
    }

    fn takes_points(&self) -> bool {
        false
    }

    fn escape(&mut self) -> bool {
        self.axis.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        let Some(plane) = self.plane else { return };
        let frame = plane.frame();
        let c = frame.to_2d(frame.origin);
        let size = 600.0;
        let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .map(|(x, y)| frame.from_2d(c + glam::DVec2::new(x, y) * size));
        out.push(Shape::Path {
            points: corners.to_vec(),
            closed: true,
            color: SECTION,
        });
    }
}

// ----- Axes -----------------------------------------------------------------------------------

/// The Axes tool (docs/05 §5.10): origin, red direction, green direction.
#[derive(Debug, Default)]
pub struct AxesTool {
    points: Vec<DVec3>,
    cursor: Option<DVec3>,
}

impl Tool for AxesTool {
    fn command(&self) -> &'static str {
        "axes-set"
    }

    fn label(&self) -> &'static str {
        "Axes"
    }

    fn step(&self) -> usize {
        self.points.len()
    }

    fn setup(&self, _cx: &Cx<'_>) -> super::Setup {
        super::Setup {
            reference: self.points.first().copied(),
            ..Default::default()
        }
    }

    fn hover(&mut self, p: &Pointer, _cx: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }

    fn click(&mut self, p: &Pointer, _cx: &Cx<'_>) -> Flow {
        self.points.push(p.inference.point);
        if self.points.len() < 3 {
            return Flow::Continue;
        }
        let [o, x, y] = [self.points[0], self.points[1], self.points[2]];
        self.points.clear();
        Flow::run(
            "axes-set",
            json!({ "origin": pt(o), "x": pt(x - o), "y": pt(y - o) }),
        )
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err("pick the origin and the directions".into())
    }

    fn escape(&mut self) -> bool {
        let busy = !self.points.is_empty();
        self.points.clear();
        busy
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        let (Some(&o), Some(c)) = (self.points.first(), self.cursor) else {
            return;
        };
        let colors = [
            egui::Color32::from_rgb(200, 30, 30),
            egui::Color32::from_rgb(30, 160, 30),
        ];
        for (i, p) in self
            .points
            .iter()
            .skip(1)
            .chain(std::iter::once(&c))
            .enumerate()
        {
            out.push(Shape::Line {
                a: o,
                b: *p,
                color: colors[i.min(1)],
                dashed: false,
            });
        }
    }
}

// ----- 3D Text --------------------------------------------------------------------------------

/// The 3D Text tool (docs/05 §3.8): the text and its sizes in Tool Options;
/// a click places it — on a face, in the face's plane.
#[derive(Debug)]
pub struct Text3dTool {
    text: String,
    height: String,
    extrude: String,
}

impl Default for Text3dTool {
    fn default() -> Self {
        Text3dTool {
            text: "Text".into(),
            height: "100".into(),
            extrude: "0".into(),
        }
    }
}

impl Tool for Text3dTool {
    fn command(&self) -> &'static str {
        "text-3d"
    }

    fn label(&self) -> &'static str {
        "3D Text"
    }

    fn step(&self) -> usize {
        0
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let height = match length(&self.height, cx) {
            Ok(h) if h > 0.0 => h,
            _ => return Flow::Continue,
        };
        let extrude = length(&self.extrude, cx).unwrap_or(0.0).max(0.0);
        // On a face: in its plane, letters upright along the slope.
        let z = p.inference.plane.map_or(DVec3::Z, |pl| pl.normal);
        let x = DVec3::Z.cross(z).try_normalize().unwrap_or(DVec3::X);
        let placement =
            DAffine3::from_mat3_translation(DMat3::from_cols(x, z.cross(x), z), p.inference.point);
        Flow::run(
            "text-3d",
            json!({
                "text": self.text,
                "height": height,
                "extrude": extrude,
                "placement": serde_json::to_value(placement).unwrap_or(Value::Null),
            }),
        )
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        if parse(text, cx)?.as_length(cx.unit).is_some() {
            self.height = text.to_string();
            Ok(Flow::Continue)
        } else {
            Err("type the letter height".into())
        }
    }

    fn takes_points(&self) -> bool {
        false
    }

    fn escape(&mut self) -> bool {
        false
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.label("Text");
        ui.text_edit_multiline(&mut self.text);
        ui.horizontal(|ui| {
            ui.label("Height");
            ui.text_edit_singleline(&mut self.height);
        });
        ui.horizontal(|ui| {
            ui.label("Extrusion");
            ui.text_edit_singleline(&mut self.extrude);
        });
    }
}
