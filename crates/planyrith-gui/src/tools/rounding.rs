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
