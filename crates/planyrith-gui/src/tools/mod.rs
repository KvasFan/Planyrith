//! Interactive tools (docs/07 §3): state machines that collect a command's
//! `Params` from clicks, the inference engine (docs/03 §7) and the value
//! field, and hand the command to the application to run. Tools never change
//! the document themselves.

mod component;
mod draw;
mod edit;
mod select;

use egui::Color32;
use glam::DVec3;
use planyrith_core::document::{Camera, DepthRange, Document, EditContext, EntityPath, Selection};
use planyrith_core::inference::{Constraint, Inference};
use planyrith_core::math::{Axis, Plane};
use planyrith_core::units::{LengthUnit, ValueFieldInput, parse_value_field};
use serde_json::{Value, json};

pub use component::ComponentTool;
pub use draw::{ArcKind, ArcTool, LineTool, RectangleTool, RoundTool};
pub use edit::{EraserTool, MoveTool, OffsetTool, PaintTool, PushPullTool, RotateTool, ScaleTool};
pub use select::SelectTool;

/// The pointer, as a tool sees it.
#[derive(Debug, Clone)]
pub struct Pointer {
    /// What the inference engine snapped to.
    pub inference: Inference,
    /// Modifier keys.
    pub modifiers: egui::Modifiers,
    /// The entity a click would select in the edit context (CPU ray cast).
    pub target: Option<EntityPath>,
    /// The face under the cursor, ignoring nearby edges and vertices (the
    /// full path to it): what Push/Pull, Offset and Paint act on.
    pub face: Option<EntityPath>,
    /// The pointer in viewport points.
    pub screen: egui::Pos2,
}

/// What the tool needs from the document this frame.
pub struct Cx<'a> {
    /// The document.
    pub doc: &'a Document,
    /// The selection.
    pub selection: &'a Selection,
    /// The edit context.
    pub context: &'a EditContext,
    /// The unit of bare numbers in the value field.
    pub unit: LengthUnit,
    /// The camera (its aspect matches the viewport).
    pub camera: Camera,
    /// Its depth range for projecting hints.
    pub depth: DepthRange,
    /// The viewport rectangle, in points.
    pub rect: egui::Rect,
}

impl Cx<'_> {
    /// Where a world point appears in the viewport (points), if in front.
    pub fn to_screen(&self, p: DVec3) -> Option<egui::Pos2> {
        let ndc = self.camera.project(p, self.depth)?;
        Some(egui::pos2(
            self.rect.left() + ((ndc.x + 1.0) * 0.5) as f32 * self.rect.width(),
            self.rect.top() + ((1.0 - ndc.y) * 0.5) as f32 * self.rect.height(),
        ))
    }
}

/// What the tool asks of the inference engine this frame.
#[derive(Debug, Clone, Copy)]
pub struct Setup {
    /// The constraint.
    pub constraint: Constraint,
    /// The reference point (directional inferences start here).
    pub reference: Option<DVec3>,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            constraint: Constraint::Free,
            reference: None,
        }
    }
}

/// What happens after an input.
#[derive(Debug, Clone, PartialEq)]
pub enum Flow {
    /// Nothing to run.
    Continue,
    /// Run these commands in order (one undo step each); the tool stays
    /// active in the state it moved to.
    Run(Vec<(String, Value)>),
    /// Change the selection (the Select tool).
    Select(Selection),
    /// Open this group or component for editing.
    Enter(EntityPath),
}

impl Flow {
    fn run(name: &str, params: Value) -> Flow {
        Flow::Run(vec![(name.to_string(), params)])
    }
}

/// A drag phase (Select's window, Eraser's stroke).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    /// The button went down and moved.
    Start,
    /// Still moving.
    Move,
    /// Released.
    End,
}

/// A world-space hint drawn over the viewport.
#[derive(Debug, Clone)]
pub enum Shape {
    /// A segment.
    Line {
        /// Start.
        a: DVec3,
        /// End.
        b: DVec3,
        /// Color.
        color: Color32,
        /// Dashed.
        dashed: bool,
    },
    /// A polyline.
    Path {
        /// Points.
        points: Vec<DVec3>,
        /// Close the loop.
        closed: bool,
        /// Color.
        color: Color32,
    },
    /// A grip or handle.
    Marker {
        /// Where.
        p: DVec3,
        /// Color.
        color: Color32,
    },
    /// A screen-space rectangle (the Select window), in points.
    ScreenRect {
        /// One corner.
        a: egui::Pos2,
        /// The other corner.
        b: egui::Pos2,
        /// Crossing (dashed) or window (solid).
        crossing: bool,
    },
}

/// The rubber-band color.
pub const BAND: Color32 = Color32::from_rgb(30, 30, 30);

/// An interactive tool (docs/07 §3).
pub trait Tool {
    /// The registered command whose steps describe the tool (Tool Help).
    fn command(&self) -> &'static str;
    /// Menu label.
    fn label(&self) -> &'static str;
    /// The current step (index into the command's steps).
    fn step(&self) -> usize;
    /// Inference inputs for this frame.
    fn setup(&self, _cx: &Cx<'_>) -> Setup {
        Setup::default()
    }
    /// The pointer moved.
    fn hover(&mut self, _p: &Pointer, _cx: &Cx<'_>) {}
    /// A click.
    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow;
    /// A double-click.
    fn double_click(&mut self, _p: &Pointer, _cx: &Cx<'_>) -> Flow {
        Flow::Continue
    }
    /// A triple-click.
    fn triple_click(&mut self, _p: &Pointer, _cx: &Cx<'_>) -> Flow {
        Flow::Continue
    }
    /// A primary-button drag.
    fn drag(&mut self, _phase: Drag, _p: &Pointer, _screen: egui::Pos2, _cx: &Cx<'_>) -> Flow {
        Flow::Continue
    }
    /// Enter in the value field (docs/07 §2).
    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String>;
    /// The value field after the tool completed: the last command with the
    /// new value, to replace it (undo, then run; docs/07 §2).
    fn revalue(&self, _last: &(String, Value), _text: &str, _cx: &Cx<'_>) -> Option<Value> {
        None
    }
    /// An arrow key: tools that place a plane use it (docs/05 §3.5);
    /// `false` leaves it to the inference lock.
    fn arrow(&mut self, _axis: Axis) -> bool {
        false
    }
    /// Esc: cancel the current step; `false` when nothing was in progress.
    fn escape(&mut self) -> bool;
    /// `true` when the tool picks points, so a point typed into the value
    /// field acts as a click there (docs/05 §1).
    fn takes_points(&self) -> bool {
        true
    }
    /// Backspace with an empty value field (docs/07 §2): drop the last
    /// point of a chain; `true` when that point's command should be undone.
    fn back(&mut self) -> bool {
        false
    }
    /// Rubber bands and other hints.
    fn shapes(&self, _out: &mut Vec<Shape>) {}
    /// The command to preview on a copy of the document (docs/07 §3).
    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        None
    }
    /// Tool Options panel content (docs/07 §6 left dock).
    fn options(&mut self, _ui: &mut egui::Ui, _cx: &Cx<'_>) {}
}

/// The tools of the toolbar, in menu order, with their shortcut key
/// (docs/05 §12).
pub fn catalogue() -> Vec<(Box<dyn Tool>, Option<egui::Key>)> {
    use egui::Key;
    vec![
        (Box::new(SelectTool::default()), Some(Key::Space)),
        (Box::new(LineTool::default()), Some(Key::L)),
        (Box::new(RectangleTool::default()), Some(Key::R)),
        (Box::new(RoundTool::circle()), Some(Key::C)),
        (Box::new(RoundTool::polygon()), None),
        (Box::new(ArcTool::new(ArcKind::Bulge)), Some(Key::A)),
        (Box::new(ArcTool::new(ArcKind::Center)), None),
        (Box::new(ArcTool::new(ArcKind::ThreePoint)), None),
        (Box::new(ArcTool::new(ArcKind::Pie)), None),
        (Box::new(PushPullTool::default()), Some(Key::P)),
        (Box::new(OffsetTool::default()), Some(Key::F)),
        (Box::new(MoveTool::default()), Some(Key::M)),
        (Box::new(RotateTool::default()), Some(Key::Q)),
        (Box::new(ScaleTool::default()), Some(Key::S)),
        (Box::new(EraserTool::default()), Some(Key::E)),
        (Box::new(PaintTool::default()), Some(Key::B)),
        // Activated from the Components panel with a component to place.
        (Box::new(ComponentTool::default()), None),
    ]
}

// ----- shared helpers -------------------------------------------------------------------------

/// A point as JSON.
pub(crate) fn pt(p: DVec3) -> Value {
    json!([p.x, p.y, p.z])
}

/// A plane as the JSON of `Plane` (`{normal, d}`).
pub(crate) fn plane_json(plane: &Plane) -> Value {
    serde_json::to_value(plane).unwrap_or(Value::Null)
}

/// The drawing plane at a picked point (docs/05 §3.3): the forced axis
/// plane; else the face's plane when the inference is on a face; else, on an
/// edge or vertex off any face, the axis plane facing the camera most; else
/// the horizontal plane through the point.
pub(crate) fn drawing_plane(inference: &Inference, forced: Option<Axis>, camera: &Camera) -> Plane {
    if let Some(axis) = forced {
        return Plane::from_point_normal(inference.point, axis.direction());
    }
    match inference.plane {
        Some(plane) if plane.contains(inference.point, &planyrith_core::Tolerances::default()) => {
            plane
        }
        _ if inference.on.is_some() => {
            let view = camera.target - camera.eye;
            let axis = [Axis::X, Axis::Y, Axis::Z]
                .into_iter()
                .max_by(|a, b| {
                    view.dot(a.direction())
                        .abs()
                        .total_cmp(&view.dot(b.direction()).abs())
                })
                .unwrap_or(Axis::Z);
            Plane::from_point_normal(inference.point, axis.direction())
        }
        _ => Plane::from_point_normal(inference.point, DVec3::Z),
    }
}

/// Parses the value field (docs/05 §1).
pub(crate) fn parse(text: &str, cx: &Cx<'_>) -> Result<ValueFieldInput, String> {
    parse_value_field(text, cx.unit).map_err(|e| e.to_string())
}

/// A length typed into the value field, in mm.
pub(crate) fn length(text: &str, cx: &Cx<'_>) -> Result<f64, String> {
    parse(text, cx)?
        .as_length(cx.unit)
        .map(|l| l.as_mm())
        .ok_or_else(|| format!("`{text}` is not a length"))
}

/// An angle typed into the value field, in radians.
pub(crate) fn angle(text: &str, cx: &Cx<'_>) -> Result<f64, String> {
    parse(text, cx)?
        .as_angle()
        .map(|a| a.as_radians())
        .ok_or_else(|| format!("`{text}` is not an angle"))
}

/// `from + (to - from)` scaled to `length` (the typed length along the
/// inferred direction, docs/05 §3.1).
pub(crate) fn along(from: DVec3, to: DVec3, length: f64) -> Option<DVec3> {
    (to - from).try_normalize().map(|d| from + d * length)
}

/// The selection as a query, or the hovered target when nothing is
/// selected (docs/05 §4.4 "auto-select").
pub(crate) fn targets(cx: &Cx<'_>, hovered: Option<&EntityPath>) -> Option<String> {
    if !cx.selection.is_empty() {
        return Some("selection".into());
    }
    hovered.map(|p| format!("{}", p.target))
}
