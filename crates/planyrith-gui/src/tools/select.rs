//! The Select tool (docs/05 §4.1): click with modifiers, double-click a face
//! (with its edges) or a group/component (to open it), triple-click for all
//! connected geometry, and window (left to right: fully inside) or crossing
//! (right to left: touching) selection by dragging.

use glam::DVec3;
use planyrith_core::document::{EntityKey, EntityPath, Selection};
use planyrith_core::ids::EntityId;
use serde_json::json;

use super::{Cx, Drag, Flow, Pointer, Shape, Tool};

/// The Select tool.
#[derive(Debug, Default)]
pub struct SelectTool {
    window: Option<(egui::Pos2, egui::Pos2)>,
}

impl SelectTool {
    fn combine(cx: &Cx<'_>, hits: &[EntityPath], modifiers: egui::Modifiers) -> Selection {
        let mut items: Vec<EntityPath> = cx.selection.iter().map(|p| p.canonical(cx.doc)).collect();
        match (modifiers.command, modifiers.shift) {
            (false, false) => items = hits.to_vec(),
            (true, false) => {
                for h in hits {
                    if !items.contains(h) {
                        items.push(h.clone());
                    }
                }
            }
            (true, true) => items.retain(|p| !hits.contains(p)),
            (false, true) => {
                for h in hits {
                    if let Some(i) = items.iter().position(|p| p == h) {
                        items.remove(i);
                    } else {
                        items.push(h.clone());
                    }
                }
            }
        }
        let mut selection = Selection::new();
        for p in items {
            selection.insert(p);
        }
        selection
    }

    /// The context's top-level entities a window or crossing rectangle
    /// selects (docs/05 §4.1).
    fn in_window(&self, cx: &Cx<'_>, a: egui::Pos2, b: egui::Pos2) -> Vec<EntityPath> {
        let rect = egui::Rect::from_two_pos(a, b);
        let crossing = b.x < a.x;
        let Ok((container, world)) = cx.doc.resolve_container(&cx.context.path) else {
            return Vec::new();
        };
        let Some(entities) = cx.doc.container(container) else {
            return Vec::new();
        };
        let mesh = entities.mesh();
        let context = cx.context.path.canonical(cx.doc);
        let mut out = Vec::new();
        for id in entities.iter() {
            let Some(pid) = entities.pid_of(id) else {
                continue;
            };
            let path = EntityPath::new(context.clone(), EntityKey::Pid(pid));
            let points: Vec<DVec3> = match id {
                EntityId::Edge(e) => mesh
                    .edge_segment(e)
                    .map(|s| vec![s.a, s.b])
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| world.transform_point3(p))
                    .collect(),
                EntityId::Face(f) => mesh
                    .face(f)
                    .and_then(|face| mesh.loop_vertices(face.outer).ok())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|v| mesh.position(v).ok())
                    .map(|p| world.transform_point3(p))
                    .collect(),
                _ => cx
                    .doc
                    .occurrence_aabb(&path)
                    .map(|b| b.corners().to_vec())
                    .unwrap_or_default(),
            };
            let screen: Vec<egui::Pos2> = points.iter().filter_map(|p| cx.to_screen(*p)).collect();
            if screen.is_empty() || screen.len() < points.len() {
                continue;
            }
            let hit = if crossing {
                screen.iter().any(|p| rect.contains(*p))
                    || rect.intersects(egui::Rect::from_points(&screen))
            } else {
                screen.iter().all(|p| rect.contains(*p))
            };
            if hit && !entities.is_hidden(id) {
                out.push(path);
            }
        }
        out
    }
}

impl Tool for SelectTool {
    fn command(&self) -> &'static str {
        "select"
    }

    fn label(&self) -> &'static str {
        "Select"
    }

    fn step(&self) -> usize {
        0
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let hits: Vec<EntityPath> = p.target.iter().cloned().collect();
        if hits.is_empty() && p.modifiers.any() {
            return Flow::Continue;
        }
        Flow::Select(Self::combine(cx, &hits, p.modifiers))
    }

    fn double_click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let Some(target) = &p.target else {
            return Flow::Continue;
        };
        match cx.doc.resolve(target).map(|r| r.entity) {
            Ok(EntityId::Group(_) | EntityId::Instance(_)) => Flow::Enter(target.clone()),
            Ok(EntityId::Face(_)) => Flow::run(
                "select",
                json!({ "query": format!("{t} | bounded-by({t})", t = target.target) }),
            ),
            _ => Flow::Continue,
        }
    }

    fn triple_click(&mut self, p: &Pointer, _cx: &Cx<'_>) -> Flow {
        match &p.target {
            Some(target) => Flow::run(
                "select",
                json!({ "query": format!("connected({})", target.target) }),
            ),
            None => Flow::Continue,
        }
    }

    fn drag(&mut self, phase: Drag, p: &Pointer, screen: egui::Pos2, cx: &Cx<'_>) -> Flow {
        match phase {
            Drag::Start => {
                self.window = Some((screen, screen));
                Flow::Continue
            }
            Drag::Move => {
                if let Some((_, end)) = &mut self.window {
                    *end = screen;
                }
                Flow::Continue
            }
            Drag::End => {
                let Some((a, _)) = self.window.take() else {
                    return Flow::Continue;
                };
                let hits = self.in_window(cx, a, screen);
                Flow::Select(Self::combine(cx, &hits, p.modifiers))
            }
        }
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err("the Select tool takes no value".into())
    }

    fn escape(&mut self) -> bool {
        self.window.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        if let Some((a, b)) = self.window {
            out.push(Shape::ScreenRect {
                a,
                b,
                crossing: b.x < a.x,
            });
        }
    }
}
