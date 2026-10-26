//! Dock panels (docs/07 §6): Entity Info, Tags, Scenes, Outliner, Tool
//! Options and Tool Help. Panels read the document and dispatch commands; they never
//! change it directly.

use egui::{Color32, RichText};
use planyrith_core::document::{
    ContainerRef, Document, EditContext, EntityKey, EntityPath, EntityView, InstancePath, QueryEnv,
    Rgba8, Selection, measure_occurrence,
};
use planyrith_core::ids::EntityId;
use serde_json::{Value, json};

use crate::icons::{self, Icon};
use crate::session::Session;
use crate::tools::{Cx, Tool};

/// Text being edited in the panels.
#[derive(Default)]
pub struct PanelState {
    new_tag: String,
    new_scene: String,
    renaming_tag: Option<(String, String)>,
    renaming_scene: Option<(String, String)>,
    selected_scene: Option<String>,
    scene_description: Option<(String, String)>,
    /// Play (`true`) or stop (`false`) the scene animation, for the app.
    pub play: Option<bool>,
    /// Whether the animation is playing (set by the app each frame).
    pub playing: bool,
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
    let rows: Vec<TagRow> = doc
        .tags
        .iter()
        .map(|(id, t)| TagRow {
            name: t.name.clone(),
            color: t.color,
            visible: t.visible,
            untagged: id == untagged,
            folder: t.folder,
        })
        .collect();
    let folders: Vec<(
        planyrith_core::ids::TagFolderId,
        String,
        bool,
        Option<planyrith_core::ids::TagFolderId>,
    )> = doc
        .tags
        .folders()
        .map(|(id, f)| (id, f.name.clone(), f.visible, f.parent))
        .collect();
    let folder_names: Vec<String> = folders.iter().map(|(_, n, ..)| n.clone()).collect();
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    egui::ScrollArea::vertical()
        .max_height(ui.available_height() - 90.0)
        .show(ui, |ui| {
            for row in rows.iter().filter(|r| r.folder.is_none()) {
                tag_row(ui, row, &folder_names, state, &mut actions);
            }
            for (id, _, _, parent) in &folders {
                if parent.is_none() {
                    folder_tree(ui, *id, &folders, &rows, &folder_names, state, &mut actions);
                }
            }
        });
    ui.separator();
    ui.horizontal(|ui| {
        let edit = ui.add(
            egui::TextEdit::singleline(&mut state.new_tag)
                .hint_text("New tag or folder")
                .desired_width(130.0),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let name = state.new_tag.trim().to_string();
        if (ui.button("Add tag").clicked() || enter) && !name.is_empty() {
            actions.push(("tag-create", json!({ "name": name })));
            state.new_tag.clear();
        } else if ui.button("Add folder").clicked() && !name.is_empty() {
            actions.push(("tag-folder-create", json!({ "name": name })));
            state.new_tag.clear();
        }
    });
    for (name, params) in actions {
        run(session, error, name, params);
    }
}

/// One tag of the Tags panel.
struct TagRow {
    name: String,
    color: Rgba8,
    visible: bool,
    untagged: bool,
    folder: Option<planyrith_core::ids::TagFolderId>,
}

/// A folder of the Tags panel with its visibility, its tags and its
/// sub-folders.
#[allow(clippy::type_complexity)]
fn folder_tree(
    ui: &mut egui::Ui,
    id: planyrith_core::ids::TagFolderId,
    folders: &[(
        planyrith_core::ids::TagFolderId,
        String,
        bool,
        Option<planyrith_core::ids::TagFolderId>,
    )],
    rows: &[TagRow],
    folder_names: &[String],
    state: &mut PanelState,
    actions: &mut Vec<(&'static str, Value)>,
) {
    let Some((_, name, visible, _)) = folders.iter().find(|(f, ..)| *f == id) else {
        return;
    };
    ui.horizontal(|ui| {
        let mut v = *visible;
        if ui
            .checkbox(&mut v, "")
            .on_hover_text("Folder visible")
            .changed()
        {
            actions.push((
                "tag-folder-set-visible",
                json!({ "name": name, "visible": v }),
            ));
        }
        let empty = !rows.iter().any(|r| r.folder == Some(id))
            && !folders.iter().any(|(_, _, _, p)| *p == Some(id));
        egui::CollapsingHeader::new(format!("📁 {name}"))
            .id_salt(("tag folder", name))
            .default_open(true)
            .show(ui, |ui| {
                for row in rows.iter().filter(|r| r.folder == Some(id)) {
                    tag_row(ui, row, folder_names, state, actions);
                }
                for (child, ..) in folders.iter().filter(|(_, _, _, p)| *p == Some(id)) {
                    folder_tree(ui, *child, folders, rows, folder_names, state, actions);
                }
                if empty && ui.small_button("Delete folder").clicked() {
                    actions.push(("tag-folder-delete", json!({ "name": name })));
                }
            });
    });
}

fn tag_row(
    ui: &mut egui::Ui,
    row: &TagRow,
    folder_names: &[String],
    state: &mut PanelState,
    actions: &mut Vec<(&'static str, Value)>,
) {
    let name = &row.name;
    ui.horizontal(|ui| {
        let mut v = row.visible;
        if ui.checkbox(&mut v, "").on_hover_text("Visible").changed() {
            actions.push(("tag-set-visible", json!({ "tag": name, "visible": v })));
        }
        swatch(ui, row.color);
        match &mut state.renaming_tag {
            Some((old, new)) if old == name => {
                let edit = ui.text_edit_singleline(new);
                if edit.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) && new != old {
                        actions.push(("tag-rename", json!({ "tag": old, "new_name": new })));
                    }
                    state.renaming_tag = None;
                } else {
                    edit.request_focus();
                }
            }
            _ => {
                let label = ui.label(name);
                if !row.untagged && label.double_clicked() {
                    state.renaming_tag = Some((name.clone(), name.clone()));
                }
            }
        }
        if !row.untagged {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("🗑").on_hover_text("Delete tag").clicked() {
                    actions.push(("tag-delete", json!({ "tag": name })));
                }
                if ui
                    .small_button("⮕")
                    .on_hover_text("Assign the selection to this tag")
                    .clicked()
                {
                    actions.push(("tag-assign", json!({ "targets": "selection", "tag": name })));
                }
                if !folder_names.is_empty() {
                    ui.menu_button("📁", |ui| {
                        if ui.button("Top level").clicked() {
                            actions.push(("tag-move", json!({ "tag": name })));
                            ui.close();
                        }
                        for f in folder_names {
                            if ui.button(f).clicked() {
                                actions.push(("tag-move", json!({ "tag": name, "folder": f })));
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Move to a folder");
                }
            });
        }
    });
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
        // The selected scene's properties (docs/07 §6).
        let doc = session.doc();
        if let Some(scene) = doc
            .scenes
            .by_name(&selected)
            .and_then(|id| doc.scenes.get(id))
        {
            let mut seconds = scene.transition_s;
            let mut include = scene.include_in_animation;
            ui.horizontal(|ui| {
                ui.label("Transition");
                let drag = ui.add(
                    egui::DragValue::new(&mut seconds)
                        .range(0.0..=60.0)
                        .speed(0.05)
                        .suffix(" s"),
                );
                if drag.drag_stopped() || (drag.changed() && !drag.dragged()) {
                    actions.push((
                        "scene-update",
                        json!({ "scene": selected, "properties_only": true, "transition_s": seconds }),
                    ));
                }
            });
            if ui.checkbox(&mut include, "Include in animation").changed() {
                actions.push((
                    "scene-update",
                    json!({ "scene": selected, "properties_only": true, "include_in_animation": include }),
                ));
            }
            let editing = state
                .scene_description
                .get_or_insert_with(|| (selected.clone(), scene.description.clone()));
            if editing.0 != selected {
                *editing = (selected.clone(), scene.description.clone());
            }
            let edit = ui.add(egui::TextEdit::singleline(&mut editing.1).hint_text("Description"));
            if edit.lost_focus() && editing.1 != scene.description {
                actions.push((
                    "scene-update",
                    json!({ "scene": selected, "properties_only": true, "description": editing.1 }),
                ));
            }
        }
    }
    if !names.is_empty() {
        ui.separator();
        let label = if state.playing {
            "■ Stop"
        } else {
            "▶ Play animation"
        };
        if ui.button(label).clicked() {
            state.play = Some(!state.playing);
        }
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

/// Tool Options (docs/07 §6, left dock): the active tool's parameters that
/// clicks do not collect.
pub fn tool_options(ui: &mut egui::Ui, tool: &mut dyn Tool, cx: &Cx<'_>, prompt: &str) {
    ui.horizontal(|ui| {
        if let Some(icon) = Icon::for_command(tool.command()) {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            icons::paint(ui.painter(), rect, icon, ui.visuals().strong_text_color());
        }
        ui.heading(tool.label());
    });
    if !prompt.is_empty() {
        ui.label(RichText::new(prompt).weak());
    }
    ui.separator();
    tool.options(ui, cx);
}

/// Tool Help (docs/07 §4, §6): the active tool's steps from its command's
/// metadata, the current one highlighted, and the shared keys.
pub fn tool_help(ui: &mut egui::Ui, session: &Session, tool: &dyn Tool) {
    ui.heading(tool.label());
    if let Some(command) = session.engine.registry().get(tool.command()) {
        let meta = command.meta();
        ui.weak(format!("Command `{}`", meta.name));
        for (i, step) in meta.steps.iter().enumerate() {
            let mut text = RichText::new(format!("{}. {}", i + 1, step.prompt));
            if i == tool.step() {
                text = text.strong();
            }
            ui.label(text);
            if !step.value_field.is_empty() {
                ui.weak(format!("    Value field: {}", step.value_field));
            }
            if !step.inference.is_empty() {
                let kinds: Vec<String> = step
                    .inference
                    .iter()
                    .map(|k| {
                        serde_json::to_value(k)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default()
                    })
                    .collect();
                ui.weak(format!("    Snaps: {}", kinds.join(", ")));
            }
        }
    }
    ui.separator();
    ui.label("Shift locks the current inference; arrow keys lock an axis (→ X, ← Y, ↑ Z, ↓ parallel/perpendicular).");
    ui.label("Type a value and press Enter; right after a tool completes, a new value replaces the last one.");
    ui.label("Esc cancels the step, then clears the selection, then leaves the open group.");
    ui.separator();
    ui.heading("Navigate");
    ui.label("Middle-drag orbits about the point under the cursor.");
    ui.label("Shift + middle-drag pans; the wheel zooms toward the cursor.");
    ui.label("Shift+Z fits the model; the Camera menu has the standard views.");
}
