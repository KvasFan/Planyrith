//! Implements docs/05 §8.21: drawing a tube path with inference and one
//! shared `route` command at completion.

use super::{BAND, Cx, Flow, Pointer, Setup, Shape, Tool, along, length, parse, pt};
use crate::i18n::{en, t};
use glam::DVec3;
use planyrith_core::units::ValueFieldInput;
use serde_json::{Value, json};

/// Collects a world-space centerline for `route`.
#[derive(Debug, Default)]
pub struct RouteTool {
    params: Value,
    points: Vec<DVec3>,
    cursor: Option<DVec3>,
}

impl RouteTool {
    /// Starts a path using the Routing window's section and bend settings.
    pub fn new(params: Value) -> Self {
        Self {
            params,
            ..Default::default()
        }
    }

    fn add(&mut self, point: DVec3, cx: &Cx<'_>) {
        if self
            .points
            .last()
            .is_none_or(|p| !cx.doc.info.tolerances.same_point(*p, point))
        {
            self.points.push(point);
        }
    }

    fn request(&self, points: &[DVec3]) -> Value {
        let mut params = if self.params.is_object() {
            self.params.clone()
        } else {
            json!({})
        };
        params["points"] = json!(points.iter().copied().map(pt).collect::<Vec<_>>());
        params
    }

    fn finish(&mut self) -> Flow {
        if self.points.len() < 2 {
            return Flow::Continue;
        }
        let params = self.request(&self.points);
        self.points.clear();
        Flow::Run(vec![("route".into(), params)])
    }
}

impl Tool for RouteTool {
    fn command(&self) -> &'static str {
        "route"
    }
    fn label(&self) -> &'static str {
        en("Route Tube")
    }
    fn step(&self) -> usize {
        usize::from(!self.points.is_empty())
    }
    fn setup(&self, _: &Cx<'_>) -> Setup {
        Setup {
            reference: self.points.last().copied(),
            ..Default::default()
        }
    }
    fn hover(&mut self, p: &Pointer, _: &Cx<'_>) {
        self.cursor = Some(p.inference.point);
    }
    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        self.add(p.inference.point, cx);
        Flow::Continue
    }
    fn double_click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        self.add(p.inference.point, cx);
        self.finish()
    }
    fn confirm(&mut self, p: Option<&Pointer>, cx: &Cx<'_>) -> Flow {
        if self.points.len() == 1
            && let Some(p) = p
        {
            self.add(p.inference.point, cx);
        }
        self.finish()
    }
    fn value(&mut self, text: &str, cx: &Cx<'_>) -> Result<Flow, String> {
        let point = match parse(text, cx)? {
            ValueFieldInput::Coordinate { absolute, xyz } => {
                let base = if absolute {
                    DVec3::ZERO
                } else {
                    self.points.last().copied().unwrap_or_default()
                };
                let current = if absolute {
                    self.cursor.unwrap_or_default()
                } else {
                    DVec3::ZERO
                };
                base + DVec3::new(
                    xyz[0].map_or(current.x, |v| v.as_mm()),
                    xyz[1].map_or(current.y, |v| v.as_mm()),
                    xyz[2].map_or(current.z, |v| v.as_mm()),
                )
            }
            _ => {
                let from = self
                    .points
                    .last()
                    .copied()
                    .ok_or("pick the start point first")?;
                along(
                    from,
                    self.cursor.ok_or("move the pointer to give a direction")?,
                    length(text, cx)?,
                )
                .ok_or("move the pointer to give a direction")?
            }
        };
        self.add(point, cx);
        Ok(Flow::Continue)
    }
    fn back(&mut self) -> bool {
        self.points.pop();
        false
    }
    fn escape(&mut self) -> bool {
        let had = !self.points.is_empty();
        self.points.clear();
        had
    }
    fn shapes(&self, out: &mut Vec<Shape>) {
        let mut points = self.points.clone();
        if let Some(p) = self.cursor
            && !points.is_empty()
        {
            points.push(p);
        }
        out.push(Shape::Path {
            points,
            closed: false,
            color: BAND,
        });
    }
    fn preview(&self, cx: &Cx<'_>) -> Option<(String, Value)> {
        if self.points.is_empty() {
            return None;
        }
        let mut points = self.points.clone();
        if let Some(p) = self.cursor
            && points
                .last()
                .is_some_and(|last| !cx.doc.info.tolerances.same_point(*last, p))
        {
            points.push(p);
        }
        (points.len() >= 2).then(|| ("route".into(), self.request(&points)))
    }
    fn options(&mut self, ui: &mut egui::Ui, _: &Cx<'_>) {
        ui.label(t(
            "Click tube path points; Enter or double-click finishes. Backspace removes a point.",
        ));
    }
}
