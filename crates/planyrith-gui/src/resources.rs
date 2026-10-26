//! The Materials, Components and Styles panels (docs/07 §6, milestone 0.4).
//! Like every panel they read the document and dispatch commands; files are
//! asked of the application's file dialog through [`FileRequest`], tools
//! through [`ResourceState::activate`].

use egui::{Color32, RichText};
use planyrith_core::document::{EdgeColorMode, FaceMode, Rgba8, WatermarkPlacement};
use serde_json::{Value, json};

use crate::session::Session;

/// A file a panel needs picked, and what it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRequest {
    /// An image to texture this material with.
    Texture(String),
    /// An image to add as a watermark of this style.
    Watermark(String),
    /// A `.plyrm` to load.
    MaterialLibraryLoad,
    /// A `.plyrm` to write.
    MaterialLibrarySave,
    /// A `.plyrc` to insert.
    ComponentInsert,
    /// A `.plyrc` to write this component to.
    ComponentSave(String),
    /// A `.plyrc` to reload this component from.
    ComponentReload(String),
    /// A `.plyrs` to load.
    StyleLibraryLoad,
    /// A `.plyrs` to write.
    StyleLibrarySave,
    /// An image to insert as an image entity.
    Image,
    /// A file to import (docs/09 §5).
    Import,
    /// A 3D model file to export the model to.
    Export,
    /// A CSV file for the component report.
    Report,
}

impl FileRequest {
    /// Title, file type name and extensions, and whether it saves.
    pub fn dialog(&self) -> (&'static str, &'static str, &'static [&'static str], bool) {
        const IMAGES: &[&str] = &["png", "jpg", "jpeg", "webp"];
        match self {
            FileRequest::Texture(_) => ("Texture image", "Images", IMAGES, false),
            FileRequest::Watermark(_) => ("Watermark image", "Images", IMAGES, false),
            FileRequest::MaterialLibraryLoad => {
                ("Load materials", "Material libraries", &["plyrm"], false)
            }
            FileRequest::MaterialLibrarySave => {
                ("Save materials", "Material libraries", &["plyrm"], true)
            }
            FileRequest::ComponentInsert => ("Insert component", "Components", &["plyrc"], false),
            FileRequest::ComponentSave(_) => ("Save component", "Components", &["plyrc"], true),
            FileRequest::ComponentReload(_) => {
                ("Reload component", "Components", &["plyrc"], false)
            }
            FileRequest::StyleLibraryLoad => ("Load styles", "Style libraries", &["plyrs"], false),
            FileRequest::StyleLibrarySave => ("Save styles", "Style libraries", &["plyrs"], true),
            FileRequest::Image => ("Insert image", "Images", IMAGES, false),
            FileRequest::Import => (
                "Import",
                "Importable files",
                &[
                    "glb", "gltf", "obj", "stl", "plyr", "plyrc", "png", "jpg", "jpeg", "webp",
                    "csv",
                ],
                false,
            ),
            FileRequest::Export => (
                "Export 3D model",
                "3D models",
                &["glb", "gltf", "obj", "stl", "plyr"],
                true,
            ),
            FileRequest::Report => ("Export component report", "CSV files", &["csv"], true),
        }
    }

    /// The command that uses the picked `path`.
    pub fn command(&self, path: &str) -> (&'static str, Value) {
        match self {
            FileRequest::Texture(m) => ("material-edit", json!({ "material": m, "texture": path })),
            FileRequest::Watermark(s) => (
                "style-watermark-add",
                json!({ "style": s, "image": path, "placement": "anchored" }),
            ),
            FileRequest::MaterialLibraryLoad => ("material-library-load", json!({ "path": path })),
            FileRequest::MaterialLibrarySave => ("material-library-save", json!({ "path": path })),
            FileRequest::ComponentInsert => ("component-insert", json!({ "path": path })),
            FileRequest::ComponentSave(d) => (
                "component-save-as",
                json!({ "definition": d, "path": path }),
            ),
            FileRequest::ComponentReload(d) => {
                ("component-reload", json!({ "definition": d, "path": path }))
            }
            FileRequest::StyleLibraryLoad => ("style-library-load", json!({ "path": path })),
            FileRequest::StyleLibrarySave => ("style-library-save", json!({ "path": path })),
            FileRequest::Image => ("image-insert", json!({ "path": path })),
            FileRequest::Import => ("import", json!({ "path": path })),
            FileRequest::Export | FileRequest::Report => ("export", json!({ "path": path })),
        }
    }
}

/// A tool a panel wants active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRequest {
    /// Paint with this material.
    Paint(String),
    /// Place this component.
    Place(String),
}

/// Panel state kept between frames.
#[derive(Default)]
pub struct ResourceState {
    /// Thumbnails: bundled materials by name, document images by hash.
    thumbnails: std::collections::HashMap<String, egui::TextureHandle>,
    selected_material: Option<String>,
    new_material: String,
    selected_component: Option<String>,
    new_style: String,
    /// A file to pick, for the application.
    pub file: Option<FileRequest>,
    /// A tool to activate, for the application.
    pub activate: Option<ToolRequest>,
}

/// Runs a command; a failure goes to `error`.
fn run(session: &mut Session, error: &mut Option<String>, name: &str, params: Value) {
    if let Err(e) = session.execute(name, params) {
        *error = Some(e);
    }
}

fn color32(c: Rgba8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, 255)
}

fn hex(c: Rgba8) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

/// A 48-pixel thumbnail of an RGB image as an egui texture.
fn thumbnail_of(ctx: &egui::Context, name: &str, image: &image::RgbImage) -> egui::TextureHandle {
    let small = image::imageops::thumbnail(image, 48, 48);
    let pixels: Vec<egui::Color32> = small
        .pixels()
        .map(|p| egui::Color32::from_rgb(p[0], p[1], p[2]))
        .collect();
    ctx.load_texture(
        name,
        egui::ColorImage::new([small.width() as usize, small.height() as usize], pixels),
        egui::TextureOptions::LINEAR,
    )
}

/// The thumbnail of a document image (decoded once).
fn image_thumbnail(
    ui: &egui::Ui,
    state: &mut ResourceState,
    session: &Session,
    image: planyrith_core::document::ImageRef,
) -> Option<egui::TextureHandle> {
    let key = format!("{image:?}");
    if let Some(t) = state.thumbnails.get(&key) {
        return Some(t.clone());
    }
    let data = session.doc().images.get(image)?;
    let decoded = image::load_from_memory(&data.bytes).ok()?.to_rgb8();
    let t = thumbnail_of(ui.ctx(), &key, &decoded);
    state.thumbnails.insert(key, t.clone());
    Some(t)
}

fn swatch(ui: &mut egui::Ui, color: Rgba8, textured: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 3.0, color32(color));
    if textured {
        // A small checker marks a texture.
        let q = rect.shrink(4.0);
        let half = q.width() / 2.0;
        for (x, y) in [(0.0, 0.0), (half, half)] {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(q.min + egui::vec2(x, y), egui::vec2(half, half)),
                0.0,
                Color32::from_white_alpha(170),
            );
        }
    }
    ui.painter().rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
}

/// A color shown as a swatch with an editable hex code; `Some` when a new
/// valid code was entered.
fn color_field(ui: &mut egui::Ui, key: &str, color: Rgba8) -> Option<String> {
    let id = ui.id().with(key);
    let mut text: String = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| hex(color));
    let mut out = None;
    ui.horizontal(|ui| {
        swatch(ui, color, false);
        let edit = ui.add(egui::TextEdit::singleline(&mut text).desired_width(70.0));
        if edit.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, text.clone()));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
        if edit.lost_focus() && text != hex(color) && text.starts_with('#') && text.len() == 7 {
            out = Some(text.clone());
        }
    });
    out
}

/// A number edited by dragging or typing; `Some` once the edit is done.
fn number_field(
    ui: &mut egui::Ui,
    key: &str,
    value: f64,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
    suffix: &str,
) -> Option<f64> {
    let id = ui.id().with(key);
    let mut v: f64 = ui.data(|d| d.get_temp(id)).unwrap_or(value);
    // Whole steps show no decimals.
    let decimals = if speed >= 0.05 && suffix != " m" {
        0
    } else {
        2
    };
    let r = ui.add(
        egui::DragValue::new(&mut v)
            .range(range)
            .speed(speed)
            .fixed_decimals(decimals)
            .suffix(suffix),
    );
    if r.dragged() || r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, v));
    } else {
        ui.data_mut(|d| d.remove::<f64>(id));
    }
    let done = r.drag_stopped() || r.lost_focus();
    (done && (v - value).abs() > f64::EPSILON).then_some(v)
}

// ----- Materials ----------------------------------------------------------------------------

/// The Materials panel.
pub fn materials(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut ResourceState,
    error: &mut Option<String>,
) {
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let doc = session.doc();
    let list: Vec<(String, Rgba8, bool, bool)> = doc
        .materials
        .iter()
        .map(|(_, m)| {
            (
                m.name.clone(),
                m.color,
                m.texture.is_some(),
                m.is_default_for.is_some(),
            )
        })
        .collect();
    let images: Vec<Option<planyrith_core::document::ImageRef>> = doc
        .materials
        .iter()
        .map(|(_, m)| m.texture.as_ref().map(|t| t.image))
        .collect();
    let thumbs: Vec<Option<egui::TextureHandle>> = images
        .iter()
        .map(|i| i.and_then(|i| image_thumbnail(ui, state, session, i)))
        .collect();
    let doc = session.doc();
    egui::ScrollArea::vertical()
        .id_salt("materials")
        .max_height(ui.available_height() * 0.45)
        .show(ui, |ui| {
            for ((name, color, textured, _), thumb) in list.iter().zip(&thumbs) {
                ui.horizontal(|ui| {
                    match thumb {
                        Some(t) => {
                            ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(18.0, 18.0)));
                        }
                        None => swatch(ui, *color, *textured),
                    }
                    let selected = state.selected_material.as_deref() == Some(name.as_str());
                    let label = ui.selectable_label(selected, name);
                    if label.clicked() {
                        state.selected_material = Some(name.clone());
                    }
                    if label.double_clicked() {
                        state.activate = Some(ToolRequest::Paint(name.clone()));
                    }
                    label.on_hover_text("Double-click to paint with it");
                });
            }
        });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.new_material)
                .hint_text("New material")
                .desired_width(120.0),
        );
        if ui.button("Create").clicked() {
            let name = if state.new_material.trim().is_empty() {
                (1..)
                    .map(|n| format!("Material {n}"))
                    .find(|n| !list.iter().any(|(m, ..)| m == n))
                    .unwrap_or_default()
            } else {
                state.new_material.trim().to_string()
            };
            actions.push((
                "material-create",
                json!({ "name": name, "color": "#B0B0B0" }),
            ));
            state.selected_material = Some(name);
            state.new_material.clear();
        }
    });
    ui.horizontal(|ui| {
        if ui.button("Load library…").clicked() {
            state.file = Some(FileRequest::MaterialLibraryLoad);
        }
        if ui.button("Save library…").clicked() {
            state.file = Some(FileRequest::MaterialLibrarySave);
        }
        if ui
            .button("Purge unused")
            .on_hover_text("Delete the materials nothing uses")
            .clicked()
        {
            actions.push(("material-purge", json!({})));
        }
    });
    egui::CollapsingHeader::new("Bundled")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for b in crate::bundled::catalogue() {
                    let key = format!("bundled:{}", b.name);
                    let thumb = state
                        .thumbnails
                        .entry(key.clone())
                        .or_insert_with(|| thumbnail_of(ui.ctx(), &key, &(b.generate)()))
                        .clone();
                    let button = egui::Button::image(
                        egui::Image::new(&thumb).fit_to_exact_size(egui::vec2(40.0, 40.0)),
                    );
                    if ui
                        .add(button)
                        .on_hover_text(format!("{} — click to paint with it", b.name))
                        .clicked()
                    {
                        let existing = list.iter().any(|(m, ..)| m == b.name);
                        if !existing {
                            match crate::bundled::texture_path(&b) {
                                Ok(path) => actions.push((
                                    "material-create",
                                    json!({
                                        "name": b.name,
                                        "color": b.color,
                                        "texture": path.to_string_lossy(),
                                        "texture_width": b.size_mm.0,
                                        "texture_height": b.size_mm.1,
                                    }),
                                )),
                                Err(e) => *error = Some(e),
                            }
                        }
                        state.selected_material = Some(b.name.to_string());
                        state.activate = Some(ToolRequest::Paint(b.name.to_string()));
                    }
                }
            });
        });

    // The selected material's editor.
    let selected = state
        .selected_material
        .clone()
        .filter(|s| list.iter().any(|(m, ..)| m == s));
    if let Some(name) = selected
        && let Some(m) = doc
            .materials
            .by_name(&name)
            .and_then(|id| doc.materials.get(id))
    {
        ui.separator();
        ui.heading(&name);
        let is_default = m.is_default_for.is_some();
        egui::Grid::new("material editor")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Color");
                if let Some(c) = color_field(ui, "material color", m.color) {
                    actions.push(("material-edit", json!({ "material": name, "color": c })));
                }
                ui.end_row();
                ui.label("Opacity");
                if let Some(o) =
                    number_field(ui, "opacity", f64::from(m.opacity), 0.0..=1.0, 0.01, "")
                {
                    actions.push(("material-edit", json!({ "material": name, "opacity": o })));
                }
                ui.end_row();
                ui.label("Texture");
                ui.horizontal(|ui| {
                    if ui
                        .button(if m.texture.is_some() { "Replace…" } else { "Load…" })
                        .clicked()
                    {
                        state.file = Some(FileRequest::Texture(name.clone()));
                    }
                    if m.texture.is_some() && ui.button("Remove").clicked() {
                        actions.push((
                            "material-edit",
                            json!({ "material": name, "texture": "none" }),
                        ));
                    }
                });
                ui.end_row();
                if let Some(t) = &m.texture {
                    ui.label("Tile");
                    ui.horizontal(|ui| {
                        if let Some(w) = number_field(
                            ui,
                            "tile width",
                            t.size_mm.x,
                            1.0..=1.0e6,
                            5.0,
                            " mm",
                        ) {
                            actions.push((
                                "material-edit",
                                json!({ "material": name, "texture_width": w }),
                            ));
                        }
                        ui.label("×");
                        if let Some(h) = number_field(
                            ui,
                            "tile height",
                            t.size_mm.y,
                            1.0..=1.0e6,
                            5.0,
                            " mm",
                        ) {
                            actions.push((
                                "material-edit",
                                json!({ "material": name, "texture_width": t.size_mm.x, "texture_height": h }),
                            ));
                        }
                    });
                    ui.end_row();
                    ui.label("Colorize");
                    let current = match t.colorize {
                        None => "none",
                        Some(planyrith_core::document::ColorizeMode::Shift) => "shift",
                        Some(planyrith_core::document::ColorizeMode::Tint) => "tint",
                    };
                    egui::ComboBox::from_id_salt("colorize")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for choice in ["none", "shift", "tint"] {
                                if ui.selectable_label(current == choice, choice).clicked()
                                    && current != choice
                                {
                                    actions.push((
                                        "material-edit",
                                        json!({ "material": name, "colorize": choice }),
                                    ));
                                }
                            }
                        });
                    ui.end_row();
                }
            });
        ui.horizontal(|ui| {
            if ui.button("Paint with it").clicked() {
                state.activate = Some(ToolRequest::Paint(name.clone()));
            }
            if !is_default && ui.button("Delete").clicked() {
                actions.push(("material-delete", json!({ "material": name })));
                state.selected_material = None;
            }
        });
    }
    for (name, params) in actions {
        run(session, error, name, params);
    }
}

// ----- Components ---------------------------------------------------------------------------

/// The Components panel ("In Model").
pub fn components(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut ResourceState,
    error: &mut Option<String>,
) {
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let doc = session.doc();
    let list: Vec<(String, String, u32)> = doc
        .definition_order
        .iter()
        .filter_map(|d| doc.definition(*d))
        .filter(|d| !d.is_internal_group)
        .map(|d| (d.name.clone(), d.description.clone(), d.usage_count))
        .collect();
    if list.is_empty() {
        ui.weak("No components yet: select geometry and use Edit › Make Component.");
    }
    egui::ScrollArea::vertical()
        .id_salt("components")
        .max_height(ui.available_height() * 0.5)
        .show(ui, |ui| {
            for (name, description, count) in &list {
                let selected = state.selected_component.as_deref() == Some(name.as_str());
                let text = RichText::new(format!("{name}  ·  {count} in model"));
                let label = ui.selectable_label(selected, text);
                if label.clicked() {
                    state.selected_component = Some(name.clone());
                }
                if label.double_clicked() {
                    state.activate = Some(ToolRequest::Place(name.clone()));
                }
                let hover = if description.is_empty() {
                    "Double-click to place it".to_string()
                } else {
                    format!("{description}\nDouble-click to place it")
                };
                label.on_hover_text(hover);
            }
        });
    ui.horizontal(|ui| {
        if ui.button("Insert from file…").clicked() {
            state.file = Some(FileRequest::ComponentInsert);
        }
        if ui
            .add_enabled(
                !session.engine.selection().is_empty(),
                egui::Button::new("Make Unique"),
            )
            .on_hover_text("Give the selected instances their own copy")
            .clicked()
        {
            actions.push(("make-unique", json!({ "targets": "selection" })));
        }
    });
    let selected = state
        .selected_component
        .clone()
        .filter(|s| list.iter().any(|(n, ..)| n == s));
    if let Some(name) = selected
        && let Some(def) = doc
            .definition_by_name(&name)
            .and_then(|d| doc.definition(d))
    {
        ui.separator();
        ui.heading(&name);
        let b = &def.behavior;
        let glue = format!("{:?}", b.glue_to).to_lowercase();
        ui.weak(format!(
            "Glues to: {glue}{}{}",
            if b.cuts_opening {
                " · cuts an opening"
            } else {
                ""
            },
            if b.always_face_camera {
                " · faces the camera"
            } else {
                ""
            }
        ));
        ui.horizontal(|ui| {
            if ui.button("Place").clicked() {
                state.activate = Some(ToolRequest::Place(name.clone()));
            }
            if ui.button("Select instances").clicked() {
                actions.push((
                    "select",
                    json!({ "query": format!("definition:\"{name}\"") }),
                ));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Save as…").clicked() {
                state.file = Some(FileRequest::ComponentSave(name.clone()));
            }
            if ui
                .button("Reload…")
                .on_hover_text("Replace its contents with a component file's")
                .clicked()
            {
                state.file = Some(FileRequest::ComponentReload(name.clone()));
            }
        });
    }
    for (name, params) in actions {
        run(session, error, name, params);
    }
}

// ----- Styles -------------------------------------------------------------------------------

/// The Styles panel: the list, the active style's editor, watermarks and
/// fog.
pub fn styles(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut ResourceState,
    error: &mut Option<String>,
) {
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let doc = session.doc();
    let active = doc.styles.active();
    let names: Vec<(String, bool)> = doc
        .styles
        .iter()
        .map(|(id, s)| (s.name.clone(), id == active))
        .collect();
    for (name, is_active) in &names {
        if ui.selectable_label(*is_active, name).clicked() && !is_active {
            actions.push(("style-activate", json!({ "style": name })));
        }
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.new_style)
                .hint_text("New style")
                .desired_width(120.0),
        );
        if ui
            .button("Create")
            .on_hover_text("A copy of the active style")
            .clicked()
        {
            let name = if state.new_style.trim().is_empty() {
                (1..)
                    .map(|n| format!("Style {n}"))
                    .find(|n| !names.iter().any(|(s, _)| s == n))
                    .unwrap_or_default()
            } else {
                state.new_style.trim().to_string()
            };
            actions.push(("style-create", json!({ "name": name })));
            state.new_style.clear();
        }
    });
    ui.horizontal(|ui| {
        if ui.button("Load library…").clicked() {
            state.file = Some(FileRequest::StyleLibraryLoad);
        }
        if ui.button("Save library…").clicked() {
            state.file = Some(FileRequest::StyleLibrarySave);
        }
    });
    let Some(style) = doc.styles.active_style().cloned() else {
        return;
    };
    let name = style.name.clone();
    let edit = |fields: Value| {
        let mut params = json!({ "style": name });
        if let (Some(p), Some(f)) = (params.as_object_mut(), fields.as_object()) {
            p.extend(f.clone());
        }
        ("style-edit", params)
    };
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("style editor")
        .show(ui, |ui| {
            egui::CollapsingHeader::new("Edges")
                .default_open(true)
                .show(ui, |ui| {
                    let e = &style.edges;
                    let mut show = e.show;
                    if ui.checkbox(&mut show, "Edges").changed() {
                        actions.push(edit(json!({ "edges": show })));
                    }
                    let mut back = e.back_edges;
                    if ui.checkbox(&mut back, "Back edges (dashed)").changed() {
                        actions.push(edit(json!({ "back_edges": back })));
                    }
                    ui.horizontal(|ui| {
                        ui.label("Profiles");
                        if let Some(w) = number_field(
                            ui,
                            "profiles",
                            f64::from(e.profiles.unwrap_or(0)),
                            0.0..=10.0,
                            0.05,
                            " px",
                        ) {
                            actions.push(edit(json!({ "profiles": w.round() as u8 })));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        for (label, mode, key) in [
                            ("One color", EdgeColorMode::All, "all"),
                            ("By material", EdgeColorMode::ByMaterial, "by-material"),
                            ("By axis", EdgeColorMode::ByAxis, "by-axis"),
                        ] {
                            if ui.radio(e.color == mode, label).clicked() && e.color != mode {
                                actions.push(edit(json!({ "edge_color": key })));
                            }
                        }
                    });
                });
            egui::CollapsingHeader::new("Faces")
                .default_open(true)
                .show(ui, |ui| {
                    let f = &style.faces;
                    egui::ComboBox::from_id_salt("face mode")
                        .selected_text(face_mode_label(f.mode))
                        .show_ui(ui, |ui| {
                            for (mode, key) in [
                                (FaceMode::Wireframe, "wireframe"),
                                (FaceMode::HiddenLine, "hidden-line"),
                                (FaceMode::Shaded, "shaded"),
                                (FaceMode::ShadedTextured, "shaded-textured"),
                                (FaceMode::Monochrome, "monochrome"),
                                (FaceMode::XRay, "x-ray"),
                            ] {
                                if ui
                                    .selectable_label(f.mode == mode, face_mode_label(mode))
                                    .clicked()
                                    && f.mode != mode
                                {
                                    actions.push(edit(json!({ "faces": key })));
                                }
                            }
                        });
                    egui::Grid::new("face colors")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label("Front");
                            if let Some(c) = color_field(ui, "front", f.front_default) {
                                actions.push(edit(json!({ "front_color": c })));
                            }
                            ui.end_row();
                            ui.label("Back");
                            if let Some(c) = color_field(ui, "back", f.back_default) {
                                actions.push(edit(json!({ "back_color": c })));
                            }
                            ui.end_row();
                        });
                });
            egui::CollapsingHeader::new("Background")
                .default_open(true)
                .show(ui, |ui| {
                    let b = &style.background;
                    egui::Grid::new("background").num_columns(2).show(ui, |ui| {
                        ui.label("Color");
                        if let Some(c) = color_field(ui, "background", b.color) {
                            actions.push(edit(json!({ "background": c })));
                        }
                        ui.end_row();
                        let mut sky = b.sky.is_some();
                        if ui.checkbox(&mut sky, "Sky").changed() {
                            let value = if sky {
                                "#CEE1F2".to_string()
                            } else {
                                "none".into()
                            };
                            actions.push(edit(json!({ "sky": value })));
                        }
                        if let Some(c) = b.sky
                            && let Some(new) = color_field(ui, "sky", c)
                        {
                            actions.push(edit(json!({ "sky": new })));
                        }
                        ui.end_row();
                        let mut ground = b.ground.is_some();
                        if ui.checkbox(&mut ground, "Ground").changed() {
                            let value = if ground {
                                "#B8C4A8".to_string()
                            } else {
                                "none".into()
                            };
                            actions.push(edit(json!({ "ground": value })));
                        }
                        if let Some((c, opacity)) = b.ground {
                            ui.horizontal(|ui| {
                                if let Some(new) = color_field(ui, "ground", c) {
                                    actions.push(edit(json!({ "ground": new })));
                                }
                                if let Some(o) = number_field(
                                    ui,
                                    "ground opacity",
                                    f64::from(opacity),
                                    0.0..=1.0,
                                    0.01,
                                    "",
                                ) {
                                    actions.push(edit(json!({ "ground_opacity": o })));
                                }
                            });
                        }
                        ui.end_row();
                    });
                });
            egui::CollapsingHeader::new("Watermarks").show(ui, |ui| {
                for (i, w) in style.watermarks.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let place = match w.placement {
                            WatermarkPlacement::Stretched => "stretched",
                            WatermarkPlacement::Tiled => "tiled",
                            WatermarkPlacement::Centered => "centered",
                            WatermarkPlacement::Anchored(..) => "anchored",
                        };
                        ui.label(format!(
                            "{} · {place}{}",
                            i + 1,
                            if w.behind_model { " · behind" } else { "" }
                        ));
                        if ui.small_button("Remove").clicked() {
                            actions.push((
                                "style-watermark-remove",
                                json!({ "style": name, "index": i }),
                            ));
                        }
                    });
                }
                if ui.button("Add image…").clicked() {
                    state.file = Some(FileRequest::Watermark(name.clone()));
                }
            });
            egui::CollapsingHeader::new("Fog").show(ui, |ui| {
                let on = session.doc().rendering.fog_enabled;
                let mut enabled = on;
                if ui.checkbox(&mut enabled, "Fog").changed() {
                    actions.push(("fog-set", json!({ "enabled": enabled })));
                }
                let (start, end) = style
                    .fog
                    .map_or((10_000.0, 100_000.0), |f| (f.start, f.end));
                ui.horizontal(|ui| {
                    ui.label("From");
                    if let Some(v) =
                        number_field(ui, "fog start", start / 1000.0, 0.0..=10_000.0, 0.1, " m")
                    {
                        actions.push(("fog-set", json!({ "start": v * 1000.0 })));
                    }
                    ui.label("to");
                    if let Some(v) =
                        number_field(ui, "fog end", end / 1000.0, 0.01..=10_000.0, 0.1, " m")
                    {
                        actions.push(("fog-set", json!({ "end": v * 1000.0 })));
                    }
                });
            });
            egui::CollapsingHeader::new("Modeling").show(ui, |ui| {
                let m = &style.modeling;
                egui::Grid::new("modeling").num_columns(2).show(ui, |ui| {
                    ui.label("Selection");
                    if let Some(c) = color_field(ui, "selection", m.selection_color) {
                        actions.push(edit(json!({ "selection_color": c })));
                    }
                    ui.end_row();
                    ui.label("Fade outside group");
                    if let Some(v) =
                        number_field(ui, "fade", f64::from(m.inactive_fade), 0.0..=1.0, 0.01, "")
                    {
                        actions.push(edit(json!({ "inactive_fade": v })));
                    }
                    ui.end_row();
                });
            });
            let other_styles = names.len() > 1;
            if other_styles {
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button("Delete style")
                        .on_hover_text("Activates another style first")
                        .clicked()
                        && let Some((other, _)) = names.iter().find(|(n, _)| *n != name)
                    {
                        actions.push(("style-activate", json!({ "style": other })));
                        actions.push(("style-delete", json!({ "style": name })));
                    }
                });
            }
        });
    for (name, params) in actions {
        run(session, error, name, params);
    }
}

fn face_mode_label(mode: FaceMode) -> &'static str {
    match mode {
        FaceMode::Wireframe => "Wireframe",
        FaceMode::HiddenLine => "Hidden line",
        FaceMode::Shaded => "Shaded",
        FaceMode::ShadedTextured => "Shaded with textures",
        FaceMode::Monochrome => "Monochrome",
        FaceMode::XRay => "X-ray",
    }
}
