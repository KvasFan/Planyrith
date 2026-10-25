//! Dock panels (docs/07 §6): Entity Info, Tags, Scenes, Outliner and Tool
//! Options. Panels read the document and dispatch commands; they never
//! change it directly.

use egui::{Color32, RichText};
use planyrith_core::document::{
    ContainerRef, Document, EditContext, EntityKey, EntityPath, EntityView, InstancePath, QueryEnv,
    Rgba8, Selection, measure_occurrence,
};
use planyrith_core::ids::EntityId;
use serde_json::{Value, json};

use crate::session::Session;

/// Text being edited in the panels.
#[derive(Default)]
pub struct PanelState {
    new_tag: String,
    new_scene: String,
    renaming_tag: Option<(String, String)>,
    renaming_scene: Option<(String, String)>,
    selected_scene: Option<String>,
}

/// Runs a command; a failure goes to `error`.
fn run(session: &mut Session, error: &mut Option<String>, name: &str, params: Value) -> bool {
    match session.execute(name, params) {
        Ok(_) => true,
        Err(e) => {
            *error = Some(e);
            false
        }
    }
}

fn color32(c: Rgba8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

fn swatch(ui: &mut egui::Ui, color: Rgba8) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color32(color));
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Inside,
    );
}

fn kind_name(id: EntityId) -> &'static str {
    match id {
        EntityId::Edge(_) => "Edge",
        EntityId::Face(_) => "Face",
        EntityId::Group(_) => "Group",
        EntityId::Instance(_) => "Component",
        EntityId::Guide(_) => "Guide",
        EntityId::Dimension(_) => "Dimension",
        EntityId::Text(_) => "Text",
        EntityId::Image(_) => "Image",
        EntityId::SectionPlane(_) => "Section Plane",
    }
}

// ----- Entity Info --------------------------------------------------------------------------

/// Entity Info (docs/07 §6): the selection's kinds, measurements, tag,
/// material and visibility.
pub fn entity_info(ui: &mut egui::Ui, session: &mut Session, error: &mut Option<String>) {
    let doc = session.doc();
    let selection = session.engine.selection().clone();
    if selection.is_empty() {
        ui.weak("No selection");
        ui.separator();
        let counts = doc.model.counts();
        ui.label(format!(
            "Model: {} faces, {} edges, {} groups, {} components",
            counts.faces, counts.edges, counts.groups, counts.instances
        ));
        return;
    }
    // Kinds and counts.
    let mut kinds: std::collections::BTreeMap<&'static str, usize> = Default::default();
    let mut resolved = Vec::new();
    for path in selection.iter() {
        if let Ok(r) = doc.resolve(path) {
            *kinds.entry(kind_name(r.entity)).or_default() += 1;
            resolved.push((path.clone(), r));
        }
    }
    let summary: Vec<String> = kinds.iter().map(|(k, n)| format!("{n} {k}")).collect();
    ui.heading(summary.join(", "));
    ui.separator();

    let env = QueryEnv {
        selection: &selection,
        last: session.engine.last_created(),
        context: session.engine.context(),
    };
    let (mut length, mut area, mut volume) = (0.0, 0.0, 0.0);
    for (path, _) in &resolved {
        if let Some(m) = measure_occurrence(doc, &env, path) {
            length += m.length.unwrap_or(0.0);
            area += m.area.unwrap_or(0.0);
            volume += m.volume.unwrap_or(0.0);
        }
    }
    egui::Grid::new("entity info")
        .num_columns(2)
        .show(ui, |ui| {
            if let [(path, r)] = resolved.as_slice() {
                ui.label("Entity");
                ui.label(format!("{} @{}", kind_name(r.entity), r.pid.0));
                ui.end_row();
                if let Some(entities) = doc.container(r.container) {
                    match entities.view(r.entity) {
                        Some(EntityView::Group(g)) => {
                            ui.label("Name");
                            ui.label(if g.name.is_empty() { "—" } else { &g.name });
                            ui.end_row();
                        }
                        Some(EntityView::Instance(i)) => {
                            let def = doc.definition(i.definition);
                            ui.label("Definition");
                            ui.label(def.map_or("?", |d| d.name.as_str()));
                            ui.end_row();
                            ui.label("Instances");
                            ui.label(def.map_or(0, |d| d.usage_count).to_string());
                            ui.end_row();
                        }
                        Some(EntityView::Face(face)) => {
                            for (label, id, side) in [
                                (
                                    "Front",
                                    face.sides.material_front,
                                    planyrith_core::document::FrontOrBack::Front,
                                ),
                                (
                                    "Back",
                                    face.sides.material_back,
                                    planyrith_core::document::FrontOrBack::Back,
                                ),
                            ] {
                                ui.label(label);
                                ui.horizontal(|ui| {
                                    if let Some(m) =
                                        doc.materials.get(doc.materials.resolve(id, side))
                                    {
                                        swatch(ui, m.color);
                                        ui.label(&m.name);
                                    }
                                });
                                ui.end_row();
                            }
                        }
                        _ => {}
                    }
                }
                let _ = path;
            }
            if length > 0.0 {
                ui.label("Length");
                ui.label(format!("{length:.1} mm"));
                ui.end_row();
            }
            if area > 0.0 {
                ui.label("Area");
                ui.label(format!("{:.4} m²", area / 1e6));
                ui.end_row();
            }
            if volume > 0.0 {
                ui.label("Volume");
                ui.label(format!("{:.4} m³", volume / 1e9));
                ui.end_row();
            }
        });
    ui.separator();

    // Tag (docs/05 §5.6) for the whole selection.
    let tags: Vec<(String, Rgba8)> = doc
        .tags
        .iter()
        .map(|(_, t)| (t.name.clone(), t.color))
        .collect();
    let current: Option<String> = {
        let mut names = resolved.iter().filter_map(|(_, r)| {
            let entities = doc.container(r.container)?;
            let tag = entities
                .tag_of(r.entity)
                .unwrap_or_else(|| doc.tags.untagged());
            doc.tags.get(tag).map(|t| t.name.clone())
        });
        let first = names.next();
        if names.all(|n| Some(&n) == first.as_ref()) {
            first
        } else {
            None
        }
    };
    let hidden = resolved.iter().all(|(_, r)| {
        doc.container(r.container)
            .is_some_and(|e| e.is_hidden(r.entity))
    });
    let locked = resolved.iter().all(|(_, r)| {
        doc.container(r.container)
            .is_some_and(|e| e.is_locked(r.entity))
    });
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.label("Tag");
        egui::ComboBox::from_id_salt("entity tag")
            .selected_text(current.clone().unwrap_or_else(|| "(mixed)".into()))
            .show_ui(ui, |ui| {
                for (name, color) in &tags {
                    ui.horizontal(|ui| {
                        swatch(ui, *color);
                        if ui
                            .selectable_label(current.as_deref() == Some(name.as_str()), name)
                            .clicked()
                        {
                            chosen = Some(name.clone());
                        }
                    });
                }
            });
    });
    let (mut hide, mut lock) = (hidden, locked);
    ui.horizontal(|ui| {
        ui.checkbox(&mut hide, "Hidden");
        ui.checkbox(&mut lock, "Locked");
    });
    if let Some(tag) = chosen {
        run(
            session,
            error,
            "tag-assign",
            json!({ "targets": "selection", "tag": tag }),
        );
    }
    if hide != hidden {
        let name = if hide { "hide" } else { "unhide" };
        run(session, error, name, json!({ "targets": "selection" }));
    }
    if lock != locked {
        let name = if lock { "lock" } else { "unlock" };
        run(session, error, name, json!({ "targets": "selection" }));
    }
}

// ----- Tags ---------------------------------------------------------------------------------

/// Tags (docs/07 §6): visibility, color, add, rename, delete, assign.
pub fn tags(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut PanelState,
    error: &mut Option<String>,
) {
    let doc = session.doc();
    let untagged = doc.tags.untagged();
    let rows: Vec<(String, Rgba8, bool, bool)> = doc
        .tags
        .iter()
        .map(|(id, t)| (t.name.clone(), t.color, t.visible, id == untagged))
        .collect();
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    egui::ScrollArea::vertical()
        .max_height(ui.available_height() - 60.0)
        .show(ui, |ui| {
            for (name, color, visible, is_untagged) in &rows {
                ui.horizontal(|ui| {
                    let mut v = *visible;
                    if ui.checkbox(&mut v, "").on_hover_text("Visible").changed() {
                        actions.push(("tag-set-visible", json!({ "tag": name, "visible": v })));
                    }
                    swatch(ui, *color);
                    match &mut state.renaming_tag {
                        Some((old, new)) if old == name => {
                            let edit = ui.text_edit_singleline(new);
                            if edit.lost_focus() {
                                if ui.input(|i| i.key_pressed(egui::Key::Enter)) && new != old {
                                    actions.push((
                                        "tag-rename",
                                        json!({ "tag": old, "new_name": new }),
                                    ));
                                }
                                state.renaming_tag = None;
                            } else {
                                edit.request_focus();
                            }
                        }
                        _ => {
                            let label = ui.label(name);
                            if !is_untagged && label.double_clicked() {
                                state.renaming_tag = Some((name.clone(), name.clone()));
                            }
                        }
                    }
                    if !is_untagged {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("🗑").on_hover_text("Delete tag").clicked() {
                                actions.push(("tag-delete", json!({ "tag": name })));
                            }
                            if ui
                                .small_button("⮕")
                                .on_hover_text("Assign the selection to this tag")
                                .clicked()
                            {
                                actions.push((
                                    "tag-assign",
                                    json!({ "targets": "selection", "tag": name }),
                                ));
                            }
                        });
                    }
                });
            }
        });
    ui.separator();
    ui.horizontal(|ui| {
        let edit = ui.add(egui::TextEdit::singleline(&mut state.new_tag).hint_text("New tag"));
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("Add").clicked() || enter) && !state.new_tag.trim().is_empty() {
            actions.push(("tag-create", json!({ "name": state.new_tag.trim() })));
            state.new_tag.clear();
        }
    });
    for (name, params) in actions {
        run(session, error, name, params);
    }
}

// ----- Scenes -------------------------------------------------------------------------------

/// Scenes (docs/07 §6): activate, add, update, rename, reorder, delete.
pub fn scenes(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut PanelState,
    error: &mut Option<String>,
) {
    let names: Vec<String> = session
        .doc()
        .scenes
        .iter()
        .map(|(_, s)| s.name.clone())
        .collect();
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    egui::ScrollArea::vertical()
        .max_height(ui.available_height() - 90.0)
        .show(ui, |ui| {
            for name in &names {
                match &mut state.renaming_scene {
                    Some((old, new)) if old == name => {
                        let edit = ui.text_edit_singleline(new);
                        if edit.lost_focus() {
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && new != old {
                                actions.push((
                                    "scene-rename",
                                    json!({ "scene": old, "new_name": new }),
                                ));
                                state.selected_scene = Some(new.clone());
                            }
                            state.renaming_scene = None;
                        } else {
                            edit.request_focus();
                        }
                    }
                    _ => {
                        let selected = state.selected_scene.as_deref() == Some(name.as_str());
                        let label = ui.selectable_label(selected, name);
                        if label.clicked() {
                            state.selected_scene = Some(name.clone());
                            actions.push(("scene-activate", json!({ "scene": name })));
                        }
                        if label.double_clicked() {
                            state.renaming_scene = Some((name.clone(), name.clone()));
                        }
                    }
                }
            }
            if names.is_empty() {
                ui.weak("No scenes yet: add one to remember this view.");
            }
        });
    ui.separator();
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut state.new_scene).hint_text("Scene name"));
        if ui
            .button("Add")
            .on_hover_text("Capture the current view")
            .clicked()
        {
            let name = if state.new_scene.trim().is_empty() {
                (1..)
                    .map(|n| format!("Scene {n}"))
                    .find(|n| !names.contains(n))
                    .unwrap_or_default()
            } else {
                state.new_scene.trim().to_string()
            };
            actions.push(("scene-add", json!({ "name": name })));
            state.selected_scene = Some(name);
            state.new_scene.clear();
        }
    });
    if let Some(selected) = state.selected_scene.clone().filter(|s| names.contains(s)) {
        let index = names.iter().position(|n| *n == selected).unwrap_or(0);
        ui.horizontal(|ui| {
            if ui
                .button("Update")
                .on_hover_text("Capture the current view into it")
                .clicked()
            {
                actions.push(("scene-update", json!({ "scene": selected })));
            }
            if ui.add_enabled(index > 0, egui::Button::new("▲")).clicked() {
                actions.push((
                    "scene-reorder",
                    json!({ "scene": selected, "index": index - 1 }),
                ));
            }
            if ui
                .add_enabled(index + 1 < names.len(), egui::Button::new("▼"))
                .clicked()
            {
                actions.push((
                    "scene-reorder",
                    json!({ "scene": selected, "index": index + 1 }),
                ));
            }
            if ui.button("Delete").clicked() {
                actions.push(("scene-delete", json!({ "scene": selected })));
                state.selected_scene = None;
            }
        });
    }
    for (name, params) in actions {
        run(session, error, name, params);
    }
}

// ----- Outliner -----------------------------------------------------------------------------

/// Outliner (docs/07 §6): the tree of groups and components; click selects,
/// double-click opens for editing, eye and lock toggle.
pub fn outliner(ui: &mut egui::Ui, session: &mut Session, error: &mut Option<String>) {
    let mut actions: Vec<Outline> = Vec::new();
    egui::ScrollArea::vertical().show(ui, |ui| {
        let doc = session.doc();
        let selection = session.engine.selection().clone();
        let context = session.engine.context().path.canonical(doc);
        let model = ui.selectable_label(context.is_root(), "Model");
        if model.clicked() {
            actions.push(Outline::Context(InstancePath::root()));
        }
        outline_level(
            ui,
            doc,
            ContainerRef::Model,
            &InstancePath::root(),
            &selection,
            &mut actions,
            0,
        );
    });
    for action in actions {
        match action {
            Outline::Select(path) => {
                session.engine.set_selection(Selection::single(path));
            }
            Outline::Context(path) => match session.engine.set_context(EditContext { path }) {
                Ok(()) => session.engine.set_selection(Selection::new()),
                Err(e) => *error = Some(e.to_string()),
            },
            Outline::Command(name, path) => {
                let mut params = json!({ "targets": format!("{}", path.target) });
                if !path.instances.is_root()
                    && let Some((last, parent)) = path.instances.steps.split_last()
                {
                    let context = EntityPath::new(
                        InstancePath {
                            steps: parent.to_vec(),
                        },
                        *last,
                    );
                    params["context"] = serde_json::to_value(&context).unwrap_or(Value::Null);
                }
                run(session, error, name, params);
            }
        }
    }
}

enum Outline {
    Select(EntityPath),
    Context(InstancePath),
    Command(&'static str, EntityPath),
}

fn outline_level(
    ui: &mut egui::Ui,
    doc: &Document,
    container: ContainerRef,
    path: &InstancePath,
    selection: &Selection,
    actions: &mut Vec<Outline>,
    depth: usize,
) {
    let Some(entities) = doc.container(container) else {
        return;
    };
    if depth > 32 {
        return;
    }
    for id in entities.iter().filter(|id| id.is_container_instance()) {
        let Some(pid) = entities.pid_of(id) else {
            continue;
        };
        let key = EntityKey::Pid(pid);
        let item = EntityPath::new(path.clone(), key);
        let (label, def) = match entities.view(id) {
            Some(EntityView::Group(g)) => (
                if g.name.is_empty() {
                    format!("Group @{}", pid.0)
                } else {
                    g.name.clone()
                },
                g.definition,
            ),
            Some(EntityView::Instance(i)) => {
                let def_name = doc
                    .definition(i.definition)
                    .map_or("?", |d| d.name.as_str());
                (
                    i.name
                        .clone()
                        .unwrap_or_else(|| format!("{def_name} @{}", pid.0)),
                    i.definition,
                )
            }
            _ => continue,
        };
        let hidden = entities.is_hidden(id);
        let locked = entities.is_locked(id);
        let selected = selection.iter().any(|p| p.canonical(doc) == item);
        let child_path = path.child(key);
        let has_children = doc
            .definition(def)
            .is_some_and(|d| d.entities.iter().any(|c| c.is_container_instance()));
        let row = |ui: &mut egui::Ui, actions: &mut Vec<Outline>| {
            ui.horizontal(|ui| {
                let text = if hidden {
                    RichText::new(&label).weak()
                } else {
                    RichText::new(&label)
                };
                let response = ui.selectable_label(selected, text);
                if response.clicked() {
                    actions.push(Outline::Select(item.clone()));
                }
                if response.double_clicked() {
                    actions.push(Outline::Context(child_path.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button(if locked { "🔒" } else { "🔓" })
                        .on_hover_text(if locked { "Unlock" } else { "Lock" })
                        .clicked()
                    {
                        actions.push(Outline::Command(
                            if locked { "unlock" } else { "lock" },
                            item.clone(),
                        ));
                    }
                    if ui
                        .small_button(if hidden { "◌" } else { "👁" })
                        .on_hover_text(if hidden { "Unhide" } else { "Hide" })
                        .clicked()
                    {
                        actions.push(Outline::Command(
                            if hidden { "unhide" } else { "hide" },
                            item.clone(),
                        ));
                    }
                });
            });
        };
        if has_children {
            let id_salt = ui.make_persistent_id(("outline", format!("{child_path}")));
            egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id_salt,
                false,
            )
            .show_header(ui, |ui| row(ui, actions))
            .body(|ui| {
                outline_level(
                    ui,
                    doc,
                    ContainerRef::Definition(def),
                    &child_path,
                    selection,
                    actions,
                    depth + 1,
                );
            });
        } else {
            row(ui, actions);
        }
    }
}

// ----- Tool Options -------------------------------------------------------------------------

/// Tool Options (docs/07 §6, left dock): the active tool's help. The
/// interactive tools arrive in 0.3; 0.2 has selection and navigation.
pub fn tool_options(ui: &mut egui::Ui) {
    ui.heading("Select");
    ui.label("Click selects; Ctrl adds, Shift toggles, Ctrl+Shift removes.");
    ui.label("Double-click a group or component to edit inside it; Esc leaves it.");
    ui.label("Delete removes the selection.");
    ui.separator();
    ui.heading("Navigate");
    ui.label("Middle-drag orbits about the point under the cursor.");
    ui.label("Shift + middle-drag pans; the wheel zooms toward the cursor.");
    ui.label("Shift+Z fits the model; the Camera menu has the standard views.");
}
