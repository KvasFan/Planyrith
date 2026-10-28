//! Implements docs/05 §4.8 and §8: Sweep and two-click solid tools.

use super::{Cx, Flow, Pointer, Tool};
use planyrith_core::document::EntityPath;
use planyrith_core::ids::EntityId;
use serde_json::{Value, json};

/// Two-click solid operation; picks are in the current edit context.
pub struct SolidTool {
    name: &'static str,
    label: &'static str,
    first: Option<EntityPath>,
    request: Option<Value>,
    hint: String,
}
impl SolidTool {
    pub fn new(name: &'static str, label: &'static str) -> Self {
        Self {
            name,
            label,
            first: None,
            request: None,
            hint: String::new(),
        }
    }
    fn pick(&mut self, p: &Pointer, cx: &Cx<'_>) -> Option<EntityPath> {
        let path = p.target.as_ref()?.canonical(cx.doc);
        if path.instances != cx.context.path.canonical(cx.doc) {
            return None;
        }
        let resolved = cx.doc.resolve(&path).ok()?;
        let entities = cx.doc.container(resolved.container)?;
        let definition = entities
            .definition_of(resolved.entity)
            .and_then(|d| cx.doc.definition(d))?;
        if !definition.entities.mesh().is_solid() {
            self.hint = "Open boundary or wire edges: choose a closed solid".into();
            return None;
        }
        self.hint = format!(
            "Solid {} ({})",
            if self.first.is_some() { 2 } else { 1 },
            resolved.pid.0
        );
        Some(path)
    }
    fn params(&self, second: &EntityPath) -> Option<Value> {
        let first = self.first.as_ref()?;
        if first == second {
            return None;
        }
        Some(json!({"a":first.to_string(),"b":second.to_string()}))
    }
}
impl Tool for SolidTool {
    fn command(&self) -> &'static str {
        self.name
    }
    fn label(&self) -> &'static str {
        self.label
    }
    fn step(&self) -> usize {
        usize::from(self.first.is_some())
    }
    fn hover(&mut self, p: &Pointer, cx: &Cx<'_>) {
        self.request = self.pick(p, cx).and_then(|path| self.params(&path));
    }
    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let Some(path) = self.pick(p, cx) else {
            return Flow::Continue;
        };
        if self.first.is_none() {
            self.first = Some(path);
            return Flow::Continue;
        }
        let Some(params) = self.params(&path) else {
            return Flow::Continue;
        };
        self.first = None;
        self.request = None;
        Flow::run(self.name, params)
    }
    fn value(&mut self, _: &str, _: &Cx<'_>) -> Result<Flow, String> {
        Err("Pick two solid groups or components".into())
    }
    fn escape(&mut self) -> bool {
        let active = self.first.take().is_some();
        self.request = None;
        active
    }
    fn takes_points(&self) -> bool {
        false
    }
    fn preview(&self, _: &Cx<'_>) -> Option<(String, Value)> {
        self.request.clone().map(|p| (self.name.into(), p))
    }
    fn options(&mut self, ui: &mut egui::Ui, _: &Cx<'_>) {
        ui.label("Pick the cutter first, then the target. Union and intersection treat the two picks symmetrically.");
        ui.label(&self.hint);
    }
}

/// Sweep: preselect path edges and pick a profile, or pick profile then path face.
#[derive(Default)]
pub struct SweepTool {
    profile: Option<EntityPath>,
    request: Option<Value>,
}
impl SweepTool {
    fn params(&self, p: &Pointer, cx: &Cx<'_>) -> Option<Value> {
        let face = p.face.as_ref()?.canonical(cx.doc);
        if face.instances != cx.context.path.canonical(cx.doc) {
            return None;
        }
        if let Some(profile) = &self.profile {
            if *profile == face {
                return None;
            }
            Some(json!({"profile":{"Path":profile},"path_face":{"Path":face}}))
        } else {
            let edges = cx
                .selection
                .iter()
                .filter_map(|path| cx.doc.resolve(path).ok())
                .filter(|r| matches!(r.entity, EntityId::Edge(_)))
                .map(|r| format!("@{}", r.pid.0))
                .collect::<Vec<_>>();
            if edges.is_empty() {
                None
            } else {
                Some(json!({"profile":{"Path":face},"path":edges.join(" | ")}))
            }
        }
    }
}
impl Tool for SweepTool {
    fn command(&self) -> &'static str {
        "sweep"
    }
    fn label(&self) -> &'static str {
        "Sweep"
    }
    fn step(&self) -> usize {
        usize::from(self.profile.is_some())
    }
    fn hover(&mut self, p: &Pointer, cx: &Cx<'_>) {
        self.request = self.params(p, cx);
    }
    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        if let Some(params) = self.params(p, cx) {
            self.profile = None;
            self.request = None;
            return Flow::run("sweep", params);
        }
        if self.profile.is_none() {
            self.profile = p.face.as_ref().map(|p| p.canonical(cx.doc));
        }
        Flow::Continue
    }
    fn value(&mut self, _: &str, _: &Cx<'_>) -> Result<Flow, String> {
        Err("Preselect path edges and click the profile, or pick profile then path face".into())
    }
    fn escape(&mut self) -> bool {
        self.request = None;
        self.profile.take().is_some()
    }
    fn takes_points(&self) -> bool {
        false
    }
    fn preview(&self, _: &Cx<'_>) -> Option<(String, Value)> {
        self.request.clone().map(|p| ("sweep".into(), p))
    }
}
