//! The application shell (docs/07 §1, §7, §9): menu bar, docks, status bar,
//! keyboard shortcuts, file dialogs and the unsaved-changes guard. Menu items
//! are commands (docs/07 §7).

use std::path::PathBuf;

use egui_file_dialog::FileDialog;
use egui_tiles::{Container, Tile, TileId, Tiles, Tree, UiResponse};
use planyrith_core::document::{EditContext, FaceMode, InstancePath};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::panels::{self, PanelState};
use crate::session::Session;
use crate::viewport::{self, Viewport};

/// A dock pane (docs/07 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pane {
    /// The 3D viewport.
    Viewport,
    /// Entity Info.
    EntityInfo,
    /// Tags.
    Tags,
    /// Scenes.
    Scenes,
    /// Outliner.
    Outliner,
    /// Tool Options (left dock).
    ToolOptions,
}

impl Pane {
    fn title(self) -> &'static str {
        match self {
            Pane::Viewport => "Viewport",
            Pane::EntityInfo => "Entity Info",
            Pane::Tags => "Tags",
            Pane::Scenes => "Scenes",
            Pane::Outliner => "Outliner",
            Pane::ToolOptions => "Tool Options",
        }
    }
}

/// The default layout: tool options | viewport | info, tags, scenes,
/// outliner.
fn default_layout() -> Tree<Pane> {
    let mut tiles = Tiles::default();
    let tools = tiles.insert_pane(Pane::ToolOptions);
    let view = tiles.insert_pane(Pane::Viewport);
    let info = tiles.insert_pane(Pane::EntityInfo);
    let tags = tiles.insert_pane(Pane::Tags);
    let scenes = tiles.insert_pane(Pane::Scenes);
    let outliner = tiles.insert_pane(Pane::Outliner);
    let upper = tiles.insert_tab_tile(vec![info, tags]);
    let lower = tiles.insert_tab_tile(vec![outliner, scenes]);
    let right = tiles.insert_vertical_tile(vec![upper, lower]);
    let root = tiles.insert_horizontal_tile(vec![tools, view, right]);
    if let Some(Tile::Container(Container::Linear(linear))) = tiles.get_mut(root) {
        linear.shares.set_share(tools, 0.18);
        linear.shares.set_share(view, 0.6);
        linear.shares.set_share(right, 0.22);
    }
    Tree::new("planyrith-docks", root, tiles)
}

/// Something that waits for the user to accept losing unsaved changes.
#[derive(Debug, Clone)]
enum Pending {
    New,
    Open(Option<PathBuf>),
    Quit,
}

/// What the open file dialog is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogPurpose {
    Open,
    SaveAs,
}

/// Persisted per user (docs/07 §1: layouts are not stored in documents).
#[derive(Default, Serialize, Deserialize)]
struct Settings {
    recent: Vec<PathBuf>,
}

const LAYOUT_KEY: &str = "planyrith.layout";
const SETTINGS_KEY: &str = "planyrith.settings";

/// The application.
pub struct App {
    session: Session,
    viewport: Viewport,
    tree: Tree<Pane>,
    panels: PanelState,
    dialog: FileDialog,
    dialog_purpose: Option<DialogPurpose>,
    pending: Option<Pending>,
    quitting: bool,
    message: Option<(String, bool)>,
    settings: Settings,
    title: String,
}

impl App {
    /// Creates the app, restoring the layout and opening `file` if given.
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> Self {
        let tree = cc
            .storage
            .and_then(|s| eframe::get_value(s, LAYOUT_KEY))
            .unwrap_or_else(default_layout);
        let settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();
        let mut app = App {
            session: Session::new(),
            viewport: Viewport::default(),
            tree,
            panels: PanelState::default(),
            dialog: FileDialog::new()
                .add_file_filter_extensions("Planyrith documents", vec!["plyr"])
                .default_file_filter("Planyrith documents"),
            dialog_purpose: None,
            pending: None,
            quitting: false,
            message: None,
            settings,
            title: String::new(),
        };
        if let Some(path) = file {
            app.open(&path);
        }
        app
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), true));
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), false));
    }

    /// Runs a command, reporting failures in the status bar.
    fn run(&mut self, name: &str, params: Value) {
        if let Err(e) = self.session.execute(name, params) {
            self.error(e);
        }
    }

    fn open(&mut self, path: &std::path::Path) {
        match Session::open(path) {
            Ok(session) => {
                self.session = session;
                self.viewport = Viewport::default();
                self.remember(path.to_path_buf());
                self.info(format!("Opened {}", path.display()));
            }
            Err(e) => self.error(format!("Could not open {}: {e}", path.display())),
        }
    }

    fn remember(&mut self, path: PathBuf) {
        self.settings.recent.retain(|p| *p != path);
        self.settings.recent.insert(0, path);
        self.settings.recent.truncate(10);
    }

    fn save(&mut self, path: Option<PathBuf>) {
        if path.is_none() && self.session.path.is_none() {
            self.dialog_purpose = Some(DialogPurpose::SaveAs);
            self.dialog.save_file();
            return;
        }
        match self.session.save(path.as_deref()) {
            Ok(path) => {
                self.remember(path.clone());
                self.info(format!("Saved {}", path.display()));
            }
            Err(e) => self.error(format!("Could not save: {e}")),
        }
    }

    /// Runs `action` now, or after asking when there are unsaved changes.
    fn guarded(&mut self, action: Pending) {
        if self.session.dirty() {
            self.pending = Some(action);
        } else {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Pending) {
        match action {
            Pending::New => {
                self.session = Session::new();
                self.viewport = Viewport::default();
            }
            Pending::Open(Some(path)) => self.open(&path),
            Pending::Open(None) => {
                self.dialog_purpose = Some(DialogPurpose::Open);
                self.dialog.pick_file();
            }
            Pending::Quit => self.quitting = true,
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let pressed = |m: Modifiers, k: Key| {
            ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)))
        };
        if (pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
            || pressed(Modifiers::COMMAND, Key::Y))
            && let Err(e) = self.session.redo()
        {
            self.error(e);
        }
        if pressed(Modifiers::COMMAND, Key::Z)
            && let Err(e) = self.session.undo()
        {
            self.error(e);
        }
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::S) {
            self.dialog_purpose = Some(DialogPurpose::SaveAs);
            self.dialog.save_file();
        }
        if pressed(Modifiers::COMMAND, Key::S) {
            self.save(None);
        }
        if pressed(Modifiers::COMMAND, Key::O) {
            self.guarded(Pending::Open(None));
        }
        if pressed(Modifiers::COMMAND, Key::N) {
            self.guarded(Pending::New);
        }
        if ctx.memory(|m| m.focused().is_some()) {
            // A text field is being edited (docs/07 §2 focus rules).
            return;
        }
        if pressed(Modifiers::COMMAND, Key::A) {
            self.run("select", json!({ "query": "all" }));
        }
        if pressed(Modifiers::SHIFT, Key::Z) {
            self.run("zoom-extents", json!({}));
        }
        if pressed(Modifiers::NONE, Key::Delete) && !self.session.engine.selection().is_empty() {
            self.run("delete", json!({ "targets": "selection" }));
        }
        if pressed(Modifiers::NONE, Key::Escape) {
            if self.session.engine.selection().is_empty() {
                viewport::exit_context(&mut self.session);
            } else {
                self.session
                    .engine
                    .set_selection(planyrith_core::document::Selection::new());
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    self.guarded(Pending::New);
                    ui.close();
                }
                if ui.button("Open…").clicked() {
                    self.guarded(Pending::Open(None));
                    ui.close();
                }
                ui.menu_button("Open Recent", |ui| {
                    let recent = self.settings.recent.clone();
                    if recent.is_empty() {
                        ui.weak("Nothing yet");
                    }
                    for path in recent {
                        if ui.button(path.display().to_string()).clicked() {
                            self.guarded(Pending::Open(Some(path)));
                            ui.close();
                        }
                    }
                });
                ui.separator();
                if ui.button("Save").clicked() {
                    self.save(None);
                    ui.close();
                }
                if ui.button("Save As…").clicked() {
                    self.dialog_purpose = Some(DialogPurpose::SaveAs);
                    self.dialog.save_file();
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    self.guarded(Pending::Quit);
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                let history = self.session.engine.history();
                let undo = history.undo_label().map(str::to_string);
                let redo = history.redo_label().map(str::to_string);
                let undo_text = undo.map_or_else(|| "Undo".into(), |l| format!("Undo {l}"));
                if ui
                    .add_enabled(history.can_undo(), egui::Button::new(undo_text))
                    .clicked()
                {
                    if let Err(e) = self.session.undo() {
                        self.error(e);
                    }
                    ui.close();
                }
                let redo_text = redo.map_or_else(|| "Redo".into(), |l| format!("Redo {l}"));
                let can_redo = self.session.engine.history().can_redo();
                if ui
                    .add_enabled(can_redo, egui::Button::new(redo_text))
                    .clicked()
                {
                    if let Err(e) = self.session.redo() {
                        self.error(e);
                    }
                    ui.close();
                }
                ui.separator();
                let has_selection = !self.session.engine.selection().is_empty();
                for (label, name, params, needs_selection) in [
                    ("Select All", "select", json!({ "query": "all" }), false),
                    ("Select None", "select", json!({ "query": "none" }), false),
                    ("Delete", "delete", json!({ "targets": "selection" }), true),
                    ("Hide", "hide", json!({ "targets": "selection" }), true),
                    ("Unhide All", "unhide", json!({ "targets": "all" }), false),
                    ("Lock", "lock", json!({ "targets": "selection" }), true),
                    ("Unlock", "unlock", json!({ "targets": "selection" }), true),
                    (
                        "Make Group",
                        "make-group",
                        json!({ "targets": "selection" }),
                        true,
                    ),
                    (
                        "Explode",
                        "explode",
                        json!({ "targets": "selection" }),
                        true,
                    ),
                ] {
                    if ui
                        .add_enabled(!needs_selection || has_selection, egui::Button::new(label))
                        .clicked()
                    {
                        if label == "Select None" {
                            self.session
                                .engine
                                .set_selection(planyrith_core::document::Selection::new());
                        } else {
                            self.run(name, params);
                        }
                        ui.close();
                    }
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("Make Component"))
                    .clicked()
                {
                    let n = self.session.doc().definitions.len() + 1;
                    self.run(
                        "make-component",
                        json!({ "targets": "selection", "name": format!("Component {n}") }),
                    );
                    ui.close();
                }
                let inside = !self.session.engine.context().path.is_root();
                if ui
                    .add_enabled(inside, egui::Button::new("Close Group/Component"))
                    .clicked()
                {
                    viewport::exit_context(&mut self.session);
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                let mode = self
                    .session
                    .doc()
                    .styles
                    .active_style()
                    .map(|s| s.faces.mode);
                ui.menu_button("Face Style", |ui| {
                    for (label, m, key) in [
                        ("Wireframe", FaceMode::Wireframe, "wireframe"),
                        ("Hidden Line", FaceMode::HiddenLine, "hidden-line"),
                        ("Shaded", FaceMode::Shaded, "shaded"),
                        (
                            "Shaded with Textures",
                            FaceMode::ShadedTextured,
                            "shaded-textured",
                        ),
                        ("Monochrome", FaceMode::Monochrome, "monochrome"),
                        ("X-Ray", FaceMode::XRay, "x-ray"),
                    ] {
                        if ui.radio(mode == Some(m), label).clicked() {
                            self.run("style-set", json!({ "faces": key }));
                            ui.close();
                        }
                    }
                });
                let style = self
                    .session
                    .doc()
                    .styles
                    .active_style()
                    .cloned()
                    .unwrap_or_default();
                let mut edges = style.edges.show;
                if ui.checkbox(&mut edges, "Edges").changed() {
                    self.run("style-set", json!({ "edges": edges }));
                }
                let mut back = style.edges.back_edges;
                if ui.checkbox(&mut back, "Back Edges").changed() {
                    self.run("style-set", json!({ "back_edges": back }));
                }
                let mut profiles = style.edges.profiles.is_some();
                if ui.checkbox(&mut profiles, "Profiles").changed() {
                    self.run(
                        "style-set",
                        json!({ "profiles": if profiles { 2 } else { 0 } }),
                    );
                }
                ui.checkbox(&mut self.viewport.show_hidden, "Hidden Geometry");
            });
            ui.menu_button("Camera", |ui| {
                ui.menu_button("Standard Views", |ui| {
                    for (label, view) in [
                        ("Top", "top"),
                        ("Bottom", "bottom"),
                        ("Front", "front"),
                        ("Back", "back"),
                        ("Left", "left"),
                        ("Right", "right"),
                        ("Iso", "iso"),
                    ] {
                        if ui.button(label).clicked() {
                            self.run("camera-standard", json!({ "view": view }));
                            ui.close();
                        }
                    }
                });
                ui.separator();
                for (label, projection) in [
                    ("Parallel Projection", "parallel"),
                    ("Perspective", "perspective"),
                    ("Two-Point Perspective", "two-point"),
                ] {
                    if ui.button(label).clicked() {
                        self.run("camera-set", json!({ "projection": projection }));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Zoom Extents\tShift+Z").clicked() {
                    self.run("zoom-extents", json!({}));
                    ui.close();
                }
                let has_selection = !self.session.engine.selection().is_empty();
                if ui
                    .add_enabled(has_selection, egui::Button::new("Zoom Selection"))
                    .clicked()
                {
                    self.run("zoom-extents", json!({ "targets": "selection" }));
                    ui.close();
                }
            });
            ui.menu_button("Window", |ui| {
                if ui.button("Reset Layout").clicked() {
                    self.tree = default_layout();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label(format!("Planyrith {}", env!("CARGO_PKG_VERSION")));
                ui.weak("Every tool is also a command: see `planyrith tool`.");
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Breadcrumb (docs/07 §4): click to leave levels.
            let doc = self.session.doc();
            let context = self.session.engine.context().path.canonical(doc);
            let mut target: Option<InstancePath> = None;
            if ui.link("Model").clicked() {
                target = Some(InstancePath::root());
            }
            let mut prefix = InstancePath::root();
            for step in &context.steps {
                prefix = prefix.child(*step);
                ui.label("›");
                let name = planyrith_core::document::EntityPath::new(
                    prefix.parent().unwrap_or_else(InstancePath::root),
                    *step,
                )
                .display_with(doc);
                let name = name.rsplit('/').next().unwrap_or(&name).to_string();
                if ui.link(name).clicked() {
                    target = Some(prefix.clone());
                }
            }
            if let Some(path) = target
                && let Err(e) = self.session.engine.set_context(EditContext { path })
            {
                self.error(e.to_string());
            }
            ui.separator();
            let selected = self.session.engine.selection().len();
            ui.label(if selected == 0 {
                "Select: click, double-click to open, middle-drag to orbit".to_string()
            } else {
                format!("{selected} selected")
            });
            if let Some(hover) = &self.viewport.hover {
                ui.separator();
                ui.weak(hover);
            }
            if let Some((text, is_error)) = &self.message {
                ui.separator();
                if *is_error {
                    ui.colored_label(ui.visuals().error_fg_color, text);
                } else {
                    ui.label(text);
                }
            }
        });
    }

    fn file_dialog(&mut self, ctx: &egui::Context) {
        self.dialog.update(ctx);
        if let Some(path) = self.dialog.take_picked() {
            match self.dialog_purpose.take() {
                Some(DialogPurpose::Open) => self.open(&path),
                Some(DialogPurpose::SaveAs) => {
                    let path = if path.extension().is_none() {
                        path.with_extension("plyr")
                    } else {
                        path
                    };
                    self.save(Some(path));
                }
                None => {}
            }
        }
    }

    fn discard_prompt(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending.clone() else {
            return;
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("unsaved changes")).show(ctx, |ui| {
            ui.heading("Unsaved changes");
            ui.label(format!("Save the changes to {}?", self.session.title()));
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(0);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
        });
        match choice {
            Some(0) => {
                self.save(None);
                if !self.session.dirty() {
                    self.pending = None;
                    self.perform(pending);
                }
            }
            Some(1) => {
                self.pending = None;
                self.perform(pending);
            }
            Some(2) => self.pending = None,
            _ => {}
        }
    }
}

/// Dock behavior: draws each pane with access to the app state.
struct Docks<'a> {
    session: &'a mut Session,
    viewport: &'a mut Viewport,
    panels: &'a mut PanelState,
    frame: &'a eframe::Frame,
    error: Option<String>,
}

impl egui_tiles::Behavior<Pane> for Docks<'_> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile: TileId, pane: &mut Pane) -> UiResponse {
        match pane {
            Pane::Viewport => self
                .viewport
                .ui(ui, self.frame.wgpu_render_state(), self.session),
            other => {
                egui::Frame::new()
                    .inner_margin(6.0)
                    .show(ui, |ui| match other {
                        Pane::EntityInfo => panels::entity_info(ui, self.session, &mut self.error),
                        Pane::Tags => panels::tags(ui, self.session, self.panels, &mut self.error),
                        Pane::Scenes => {
                            panels::scenes(ui, self.session, self.panels, &mut self.error)
                        }
                        Pane::Outliner => panels::outliner(ui, self.session, &mut self.error),
                        Pane::ToolOptions => panels::tool_options(ui),
                        Pane::Viewport => {}
                    });
            }
        }
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title().into()
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            all_panes_must_have_tabs: true,
            ..Default::default()
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        for warning in self.session.take_warnings() {
            self.info(warning);
        }
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                let mut docks = Docks {
                    session: &mut self.session,
                    viewport: &mut self.viewport,
                    panels: &mut self.panels,
                    frame,
                    error: None,
                };
                self.tree.ui(&mut docks, ui);
                if let Some(e) = docks.error.take() {
                    self.message = Some((e, true));
                }
            });
        if let Some(e) = self.viewport.error.take() {
            self.error(e);
        }
        self.file_dialog(&ctx);
        self.discard_prompt(&ctx);

        // Window title `<file name>[*] — Planyrith` (docs/07 §1).
        let title = format!(
            "{}{} — Planyrith",
            self.session.title(),
            if self.session.dirty() { "*" } else { "" }
        );
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.session.dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }
        if self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, LAYOUT_KEY, &self.tree);
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }
}
