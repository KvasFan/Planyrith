//! Placing components (docs/07 §6 Components panel: "drag into viewport =
//! insert-component with glue behavior"): each click inserts an instance
//! of the chosen component where the inference puts the cursor; a gluing
//! component over a face it accepts lies on that face.

use planyrith_core::document::GlueTo;
use planyrith_core::ids::EntityId;
use serde_json::{Value, json};

use super::{Cx, Flow, Pointer, Tool, pt};

/// The component placement tool.
#[derive(Debug, Default)]
pub struct ComponentTool {
    definition: Option<String>,
    request: Option<Value>,
}

impl ComponentTool {
    /// Places instances of `definition` (a component name).
    pub fn new(definition: impl Into<String>) -> Self {
        ComponentTool {
            definition: Some(definition.into()),
            request: None,
        }
    }

    /// The `component-insert` parameters for the pointer.
    fn params(&self, p: &Pointer, cx: &Cx<'_>) -> Option<Value> {
        let name = self.definition.as_ref()?;
        let d = cx.doc.definition_by_name(name)?;
        let glue = cx.doc.definition(d)?.behavior.glue_to;
        let mut params = json!({ "definition": name, "position": pt(p.inference.point) });
        // Glue onto the face under the cursor when the component accepts it.
        if glue != GlueTo::None
            && let Some(face) = &p.face
            && let Ok(r) = cx.doc.resolve(face)
            && let EntityId::Face(f) = r.entity
            && let Some(plane) = cx
                .doc
                .container(r.container)
                .and_then(|c| c.mesh().face(f))
                .map(|x| x.plane.transformed(&r.world))
            && glues(glue, plane.normal)
            && face.canonical(cx.doc).instances == cx.context.path.canonical(cx.doc)
            && let Some(pid) = cx
                .doc
                .container(r.container)
                .and_then(|c| c.pid_of(EntityId::Face(f)))
        {
            params["on_face"] = json!({ "Pid": pid.0 });
        }
        Some(params)
    }
}

fn glues(glue: GlueTo, normal: glam::DVec3) -> bool {
    let z = normal.normalize_or_zero().z.abs();
    match glue {
        GlueTo::None => false,
        GlueTo::Any => true,
        GlueTo::Horizontal => z > 1.0 - 1e-6,
        GlueTo::Vertical => z < 1e-6,
        GlueTo::Sloped => (1e-6..=1.0 - 1e-6).contains(&z),
    }
}

impl Tool for ComponentTool {
    fn command(&self) -> &'static str {
        "component-insert"
    }

    fn label(&self) -> &'static str {
        "Component"
    }

    fn step(&self) -> usize {
        0
    }

    fn hover(&mut self, p: &Pointer, cx: &Cx<'_>) {
        self.request = self.params(p, cx);
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        match self.params(p, cx) {
            Some(params) => Flow::run("component-insert", params),
            None => Flow::Continue,
        }
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err("type a point (x,y,z) to place the component there".into())
    }

    fn escape(&mut self) -> bool {
        false
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        self.request
            .clone()
            .map(|params| ("component-insert".to_string(), params))
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        match &self.definition {
            Some(name) => {
                ui.label(format!("Placing `{name}`"));
                ui.weak("Click to place; gluing components lie on matching faces. Space ends.");
            }
            None => {
                ui.weak("Choose a component in the Components panel.");
            }
        }
    }
}
