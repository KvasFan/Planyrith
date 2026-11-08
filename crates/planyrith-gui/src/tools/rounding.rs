//! Implements the interactive Fillet and Chamfer tools (docs/05 §4.17–§4.18):
//! click edges of the solid being edited (again to drop one), see the
//! result at the current size, type a size and Enter to apply.

use egui::Color32;
use glam::DVec3;
use planyrith_core::document::EntityPath;
use planyrith_core::ids::EntityId;
use serde_json::{Value, json};

use super::{Cx, Flow, Pointer, Shape, Tool, length};
use crate::i18n::{en, t};

/// Picked edges' color.
const PICKED: Color32 = Color32::from_rgb(0, 120, 215);

/// Fillet or Chamfer by picking edges.
pub struct EdgeRoundTool {
    chamfer: bool,
    /// Picked edges: the edge and its world segment.
    edges: Vec<(EntityPath, [DVec3; 2])>,
    hover: Option<[DVec3; 2]>,
    /// Radius or distance, mm.
    size: f64,
    segments: u32,
}

impl EdgeRoundTool {
    /// The Fillet tool.
    pub fn fillet() -> Self {
        EdgeRoundTool {
            chamfer: false,
            edges: Vec::new(),
            hover: None,
            size: 10.0,
            segments: 8,
        }
    }

    /// The Chamfer tool.
    pub fn chamfer() -> Self {
        EdgeRoundTool {
            chamfer: true,
            ..EdgeRoundTool::fillet()
        }
    }

    /// The edge under the pointer in the edit context, with its segment.
    fn edge(p: &Pointer, cx: &Cx<'_>) -> Option<(EntityPath, [DVec3; 2])> {
        let path = p.target.as_ref()?.canonical(cx.doc);
        if path.instances != cx.context.path.canonical(cx.doc) {
            return None;
        }
        let r = cx.doc.resolve(&path).ok()?;
        let EntityId::Edge(e) = r.entity else {
            return None;
        };
        let s = cx.doc.container(r.container)?.mesh().edge_segment(e).ok()?;
        Some((
            path,
            [r.world.transform_point3(s.a), r.world.transform_point3(s.b)],
        ))
    }

    fn params(&self, size: f64, cx: &Cx<'_>) -> Option<Value> {
        let ids: Vec<String> = self
            .edges
            .iter()
            .filter_map(|(path, _)| cx.doc.resolve(path).ok())
            .map(|r| format!("@{}", r.pid.0))
            .collect();
        if ids.is_empty() {
            return None;
        }
        Some(if self.chamfer {
            json!({ "edges": ids.join(" | "), "distance": size })
        } else {
            json!({ "edges": ids.join(" | "), "radius": size, "segments": self.segments })
        })
    }
}

impl Tool for EdgeRoundTool {
    fn command(&self) -> &'static str {
        if self.chamfer { "chamfer" } else { "fillet" }
    }

    fn label(&self) -> &'static str {
        if self.chamfer {
            en("Chamfer")
        } else {
            en("Fillet")
        }
    }

    fn step(&self) -> usize {
        0
    }

    fn hover(&mut self, p: &Pointer, cx: &Cx<'_>) {
        self.hover = Self::edge(p, cx).map(|(_, s)| s);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if let Some((path, segment)) = Self::edge(p, cx) {
            match self.edges.iter().position(|(e, _)| *e == path) {
                Some(i) => {
                    self.edges.remove(i);
                }
                None => self.edges.push((path, segment)),
            }
        }
        Flow::Continue
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let size = length(text, cx)?;
        if size <= 0.0 {
            return Err(t("Type a positive size"));
        }
        let params = self
            .params(size, cx)
            .ok_or_else(|| t("Pick the edges first"))?;
        self.size = size;
        self.edges.clear();
        Ok(Flow::run(self.command(), params))
    }

    fn escape(&mut self) -> bool {
        let busy = !self.edges.is_empty();
        self.edges.clear();
        busy
    }

    fn back(&mut self) -> bool {
        self.edges.pop();
        false
    }

    fn takes_points(&self) -> bool {
        false
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        for (_, [a, b]) in &self.edges {
            out.push(Shape::Line {
                a: *a,
                b: *b,
                color: PICKED,
                dashed: false,
            });
        }
        if let Some([a, b]) = self.hover {
            out.push(Shape::Line {
                a,
                b,
                color: PICKED,
                dashed: true,
            });
        }
    }

    fn preview(&self, cx: &Cx<'_>) -> Option<(String, Value)> {
        self.params(self.size, cx)
            .map(|p| (self.command().to_string(), p))
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        ui.label(t(
            "Click edges of the solid you are editing (again to drop one), then type the size and press Enter.",
        ));
        ui.horizontal(|ui| {
            ui.label(if self.chamfer {
                t("Distance")
            } else {
                t("Radius")
            });
            ui.add(
                egui::DragValue::new(&mut self.size)
                    .range(0.1..=100_000.0)
                    .speed(0.5)
                    .suffix(" mm"),
            );
        });
        if !self.chamfer {
            ui.horizontal(|ui| {
                ui.label(t("Segments"));
                ui.add(egui::DragValue::new(&mut self.segments).range(1..=64));
            });
        }
        ui.label(crate::i18n::t_args(
            "Edges picked: { $count }",
            &[("count", self.edges.len().to_string().into())],
        ));
    }
}

/// What a face-picking tool makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FacesKind {
    Thicken,
    Loft,
}

/// Thicken (docs/05 §4.19) or Loft (§4.20) by picking faces: Thicken takes
/// the typed thickness; Loft takes the profiles in click order and is made
/// by double-clicking the last one.
pub struct FacesTool {
    kind: FacesKind,
    /// Picked faces with their outlines (world).
    faces: Vec<(EntityPath, Vec<DVec3>)>,
    hover: Option<Vec<DVec3>>,
    /// Thickness, mm.
    size: f64,
}

impl FacesTool {
    /// The Thicken tool.
    pub fn thicken() -> Self {
        FacesTool {
            kind: FacesKind::Thicken,
            faces: Vec::new(),
            hover: None,
            size: 10.0,
        }
    }

    /// The Loft tool.
    pub fn loft() -> Self {
        FacesTool {
            kind: FacesKind::Loft,
            ..FacesTool::thicken()
        }
    }

    fn face(p: &Pointer, cx: &Cx<'_>) -> Option<(EntityPath, Vec<DVec3>)> {
        let path = p.face.as_ref()?.canonical(cx.doc);
        if path.instances != cx.context.path.canonical(cx.doc) {
            return None;
        }
        let r = cx.doc.resolve(&path).ok()?;
        let EntityId::Face(f) = r.entity else {
            return None;
        };
        let mesh = cx.doc.container(r.container)?.mesh();
        let outline = mesh
            .face_vertices(f)
            .ok()?
            .into_iter()
            .filter_map(|v| mesh.position(v).ok())
            .map(|q| r.world.transform_point3(q))
            .collect();
        Some((path, outline))
    }

    fn ids(&self, cx: &Cx<'_>) -> Vec<String> {
        self.faces
            .iter()
            .filter_map(|(path, _)| cx.doc.resolve(path).ok())
            .map(|r| format!("@{}", r.pid.0))
            .collect()
    }

    fn params(&self, cx: &Cx<'_>) -> Option<Value> {
        let ids = self.ids(cx);
        match self.kind {
            FacesKind::Thicken if !ids.is_empty() => {
                Some(json!({ "faces": ids.join(" | "), "thickness": self.size }))
            }
            FacesKind::Loft if ids.len() >= 2 => Some(json!({ "profiles": ids })),
            _ => None,
        }
    }
}

impl Tool for FacesTool {
    fn command(&self) -> &'static str {
        match self.kind {
            FacesKind::Thicken => "thicken",
            FacesKind::Loft => "loft",
        }
    }

    fn label(&self) -> &'static str {
        match self.kind {
            FacesKind::Thicken => en("Thicken"),
            FacesKind::Loft => en("Loft"),
        }
    }

    fn step(&self) -> usize {
        0
    }

    fn hover(&mut self, p: &Pointer, cx: &Cx<'_>) {
        self.hover = Self::face(p, cx).map(|(_, o)| o);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if let Some((path, outline)) = Self::face(p, cx) {
            match self.faces.iter().position(|(f, _)| *f == path) {
                Some(i) => {
                    self.faces.remove(i);
                }
                None => self.faces.push((path, outline)),
            }
        }
        Flow::Continue
    }

    fn double_click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if self.kind != FacesKind::Loft {
            return Flow::Continue;
        }
        // The double click's first click toggled the face off again.
        if let Some((path, outline)) = Self::face(p, cx)
            && !self.faces.iter().any(|(f, _)| *f == path)
        {
            self.faces.push((path, outline));
        }
        match self.params(cx) {
            Some(params) => {
                self.faces.clear();
                Flow::run("loft", params)
            }
            None => Flow::Continue,
        }
    }

    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        if self.kind == FacesKind::Loft {
            return Err(t("Double-click the last profile to make the loft"));
        }
        let size = length(text, cx)?;
        if size == 0.0 {
            return Err(t("Type a non-zero thickness"));
        }
        self.size = size;
        let params = self.params(cx).ok_or_else(|| t("Pick the faces first"))?;
        self.faces.clear();
        Ok(Flow::run("thicken", params))
    }

    fn escape(&mut self) -> bool {
        let busy = !self.faces.is_empty();
        self.faces.clear();
        busy
    }

    fn back(&mut self) -> bool {
        self.faces.pop();
        false
    }

    fn takes_points(&self) -> bool {
        false
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        for (_, outline) in &self.faces {
            out.push(Shape::Path {
                points: outline.clone(),
                closed: true,
                color: PICKED,
            });
        }
        if let Some(outline) = &self.hover {
            out.push(Shape::Path {
                points: outline.clone(),
                closed: true,
                color: PICKED,
            });
        }
    }

    fn preview(&self, cx: &Cx<'_>) -> Option<(String, Value)> {
        self.params(cx).map(|p| (self.command().to_string(), p))
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        match self.kind {
            FacesKind::Thicken => {
                ui.label(t(
                    "Click the faces of a surface (again to drop one), then type the thickness and press Enter.",
                ));
                ui.horizontal(|ui| {
                    ui.label(t("Thickness"));
                    ui.add(
                        egui::DragValue::new(&mut self.size)
                            .range(-100_000.0..=100_000.0)
                            .speed(0.5)
                            .suffix(" mm"),
                    );
                });
            }
            FacesKind::Loft => {
                ui.label(t(
                    "Click the profile faces in order and double-click the last one.",
                ));
            }
        }
        ui.label(crate::i18n::t_args(
            "Faces picked: { $count }",
            &[("count", self.faces.len().to_string().into())],
        ));
    }
}
