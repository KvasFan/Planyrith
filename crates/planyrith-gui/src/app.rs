//! The application shell (docs/07 §1, §7, §9): menu bar, docks, status bar,
//! keyboard shortcuts, file dialogs and the unsaved-changes guard. Menu items
//! are commands (docs/07 §7).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::Key;
use egui_file_dialog::FileDialog;
use egui_tiles::{Container, Tile, TileId, Tiles, Tree, UiResponse};
use planyrith_core::document::{EditContext, FaceMode, InstancePath};
use planyrith_core::inference::Lock;
use planyrith_core::math::Axis;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::panels::{self, PanelState};
use crate::session::Session;
use crate::tools::{self, Flow, Tool};
use crate::viewport::{self, Preview, Viewport};

/// A preview that took longer than this is only recomputed once the
/// pointer rests (docs/07 §3).
const PREVIEW_BUDGET: Duration = Duration::from_millis(16);
/// How long the pointer must rest before a slow preview is recomputed.
const PREVIEW_SETTLE_S: f64 = 0.15;

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
    /// Tool Help.
    ToolHelp,
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
            Pane::ToolHelp => "Tool Help",
        }
    }
}

/// The default layout: tool options | viewport | info, tags, tool help,
/// scenes, outliner.
fn default_layout() -> Tree<Pane> {
    let mut tiles = Tiles::default();
    let tools = tiles.insert_pane(Pane::ToolOptions);
    let view = tiles.insert_pane(Pane::Viewport);
    let info = tiles.insert_pane(Pane::EntityInfo);
    let tags = tiles.insert_pane(Pane::Tags);
    let scenes = tiles.insert_pane(Pane::Scenes);
    let outliner = tiles.insert_pane(Pane::Outliner);
    let help = tiles.insert_pane(Pane::ToolHelp);
    let upper = tiles.insert_tab_tile(vec![info, tags, help]);
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

const LAYOUT_KEY: &str = "planyrith.layout.v2";
const SETTINGS_KEY: &str = "planyrith.settings";

/// A tool's preview (docs/07 §3): recomputed when the tool's request or
/// the document changes; when computing took too long, only after the
/// pointer rests.
#[derive(Default)]
struct PreviewState {
    request: Option<(String, Value)>,
    generation: u64,
    shown: Option<Preview>,
    computed: bool,
    slow: bool,
    changed_at: f64,
    serial: u64,
    error: Option<String>,
}

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
    /// The tools and their shortcut keys (docs/05 §12).
    tools: Vec<(Box<dyn Tool>, Option<Key>)>,
    /// The active tool.
    active: usize,
    /// The value field (docs/07 §2).
    value: String,
    /// The last command a tool ran, and whether the value field may still
    /// replace it (until the next button press, docs/07 §2).
    last_run: Option<(String, Value)>,
    revalue_open: bool,
    preview: PreviewState,
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
        let mut app = App::blank(tree, settings);
        if let Some(path) = file {
            app.open(&path);
        }
        app
    }

    /// The app with an empty document.
    fn blank(tree: Tree<Pane>, settings: Settings) -> Self {
        App {
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
            tools: tools::catalogue(),
            active: 0,
            value: String::new(),
            last_run: None,
            revalue_open: false,
            preview: PreviewState::default(),
        }
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
                self.activate(0);
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
                self.activate(0);
            }
            Pending::Open(Some(path)) => self.open(&path),
            Pending::Open(None) => {
                self.dialog_purpose = Some(DialogPurpose::Open);
                self.dialog.pick_file();
            }
            Pending::Quit => self.quitting = true,
        }
    }

    /// Undo; a tool's step in progress refers to what was undone, so the
    /// tool starts over.
    fn undo(&mut self) {
        match self.session.undo() {
            Ok(()) => self.activate(self.active),
            Err(e) => self.error(e),
        }
    }

    fn redo(&mut self) {
        match self.session.redo() {
            Ok(()) => self.activate(self.active),
            Err(e) => self.error(e),
        }
    }

    fn tool(&self) -> &dyn Tool {
        self.tools[self.active].0.as_ref()
    }

    /// Makes a tool active, in its initial state.
    fn activate(&mut self, index: usize) {
        let mut fresh = tools::catalogue();
        if index < fresh.len() {
            self.tools[index].0 = fresh.swap_remove(index).0;
            self.active = index;
        }
        self.value.clear();
        self.last_run = None;
        self.revalue_open = false;
        self.viewport.after_run();
        self.clear_preview();
    }

    /// Activates the tool with this menu label.
    fn activate_label(&mut self, label: &str) {
        if let Some(i) = self.tools.iter().position(|(t, _)| t.label() == label) {
            self.activate(i);
        }
    }

    fn clear_preview(&mut self) {
        self.preview = PreviewState {
            serial: self.preview.serial,
            ..PreviewState::default()
        };
    }

    /// Runs what the tool asked for.
    fn apply_flows(&mut self, flows: Vec<Flow>) {
        for flow in flows {
            match flow {
                Flow::Continue => {}
                Flow::Run(commands) => {
                    for (name, params) in commands {
                        match self.session.execute(&name, params.clone()) {
                            Ok(_) => self.last_run = Some((name, params)),
                            Err(e) => {
                                self.error(e);
                                break;
                            }
                        }
                    }
                    self.revalue_open = true;
                    self.value.clear();
                    self.viewport.after_run();
                    self.clear_preview();
                }
                other => {
                    if let Err(e) = viewport::apply_view_flow(&mut self.session, &other) {
                        self.error(e);
                    }
                }
            }
        }
    }

    /// `Enter` in the value field (docs/07 §2): the active step takes the
    /// value, or, right after a tool completed, the value replaces the
    /// command it ran (undo, then run with the new value).
    fn apply_value(&mut self) {
        let text = std::mem::take(&mut self.value);
        let text = text.trim();
        let cx = self.viewport.cx(&self.session);
        if text.is_empty() {
            // Enter alone confirms the current inferred point (docs/05 §1).
            let tool = self.tools[self.active].0.as_mut();
            let flow = match self.viewport.pointer() {
                Some(p) if tool.step() > 0 => tool.click(p, &cx),
                _ => Flow::Continue,
            };
            self.apply_flows(vec![flow]);
            return;
        }
        let tool = self.tools[self.active].0.as_mut();
        let replaced = match (&self.last_run, self.revalue_open && tool.step() == 0) {
            (Some(last), true) => tool
                .revalue(last, text, &cx)
                .map(|params| (last.0.clone(), params)),
            _ => None,
        };
        let flow = match &replaced {
            Some(_) => None,
            None => Some(tool.value(text, &cx)),
        };
        if let Some((name, params)) = replaced {
            if let Err(e) = self.session.undo() {
                self.error(e);
                return;
            }
            match self.session.execute(&name, params.clone()) {
                Ok(_) => self.last_run = Some((name, params)),
                Err(e) => {
                    self.error(e);
                    let _ = self.session.redo();
                }
            }
            self.clear_preview();
            return;
        }
        match flow {
            Some(Ok(flow)) => self.apply_flows(vec![flow]),
            Some(Err(e)) => self.error(e),
            None => {}
        }
    }

    /// Keys for the active tool and the value field (docs/07 §2): number and
    /// unit keys go to the value field without focusing it; letters are tool
    /// shortcuts while it is empty.
    fn tool_keys(&mut self, ctx: &egui::Context) {
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        for event in events {
            match event {
                egui::Event::Text(text) => self.typed(&text),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if !modifiers.command && !modifiers.alt => self.key(key),
                _ => {}
            }
        }
    }

    fn typed(&mut self, text: &str) {
        let accepts = self.tool().command() != "select" || self.revalue_open;
        let starts_value = text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || "-+.,[<'\"*/(~".contains(c));
        if accepts && (!self.value.is_empty() || starts_value) {
            self.value.push_str(text);
            return;
        }
        if text == " " {
            self.activate(0);
            return;
        }
        let index = self.tools.iter().position(|(_, key)| {
            key.is_some_and(|k| k != Key::Space && k.name().eq_ignore_ascii_case(text))
        });
        if let Some(i) = index {
            self.activate(i);
        }
    }

    fn key(&mut self, key: Key) {
        match key {
            Key::Enter => self.apply_value(),
            Key::Backspace => {
                self.value.pop();
            }
            Key::Escape => self.escape(),
            Key::ArrowRight => self.arrow(Some(Axis::X)),
            Key::ArrowLeft => self.arrow(Some(Axis::Y)),
            Key::ArrowUp => self.arrow(Some(Axis::Z)),
            Key::ArrowDown => self.arrow(None),
            Key::Delete if self.value.is_empty() && !self.session.engine.selection().is_empty() => {
                self.run("delete", json!({ "targets": "selection" }));
            }
            _ => {}
        }
    }

    /// Arrow keys (docs/05 §2): the tool's plane, else an inference lock.
    fn arrow(&mut self, axis: Option<Axis>) {
        match axis {
            Some(axis) if self.tools[self.active].0.arrow(axis) => {}
            Some(axis) => self.viewport.lock(Lock::Axis(axis)),
            None => self.viewport.lock(Lock::ParallelPerpendicular),
        }
    }

    /// `Esc` (docs/05 §2, docs/07 §10): clears the value field, else cancels
    /// the tool's step, else clears the selection, else leaves the open
    /// group or component.
    fn escape(&mut self) {
        if !self.value.is_empty() {
            self.value.clear();
        } else if self.tools[self.active].0.escape() {
            self.viewport.after_run();
            self.clear_preview();
        } else if !self.session.engine.selection().is_empty() {
            self.session
                .engine
                .set_selection(planyrith_core::document::Selection::new());
        } else {
            viewport::exit_context(&mut self.session);
        }
    }

    /// Recomputes the tool's preview when needed (docs/07 §3).
    fn update_preview(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let request = {
            let cx = self.viewport.cx(&self.session);
            self.tools[self.active].0.preview(&cx)
        };
        let generation = self.session.engine.generation();
        let state = &mut self.preview;
        if request != state.request || generation != state.generation {
            state.request = request;
            state.generation = generation;
            state.changed_at = now;
            state.computed = false;
            if state.slow {
                state.shown = None;
            }
        }
        if state.computed {
            return;
        }
        let Some((name, params)) = state.request.clone() else {
            state.shown = None;
            state.error = None;
            state.computed = true;
            return;
        };
        if state.slow && now - state.changed_at < PREVIEW_SETTLE_S {
            ctx.request_repaint_after(Duration::from_secs_f64(PREVIEW_SETTLE_S));
            return;
        }
        let started = Instant::now();
        match self.session.preview(&name, params) {
            Ok((doc, touched)) => {
                state.serial += 1;
                state.shown = Some(Preview {
                    doc,
                    touched,
                    serial: state.serial,
                });
                state.error = None;
            }
            Err(e) => {
                state.shown = None;
                state.error = Some(e);
            }
        }
        state.slow = started.elapsed() > PREVIEW_BUDGET;
        state.computed = true;
        ctx.request_repaint();
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{KeyboardShortcut, Modifiers};
        let pressed = |m: Modifiers, k: Key| {
            ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)))
        };
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
            || pressed(Modifiers::COMMAND, Key::Y)
        {
            self.redo();
        }
        if pressed(Modifiers::COMMAND, Key::Z) {
            self.undo();
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
                    self.undo();
                    ui.close();
                }
                let redo_text = redo.map_or_else(|| "Redo".into(), |l| format!("Redo {l}"));
                let can_redo = self.session.engine.history().can_redo();
                if ui
                    .add_enabled(can_redo, egui::Button::new(redo_text))
                    .clicked()
                {
                    self.redo();
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
            ui.menu_button("Draw", |ui| {
                self.tool_button(ui, "Line");
                ui.menu_button("Arcs", |ui| {
                    for label in ["Arc", "2 Point Arc", "3 Point Arc", "Pie"] {
                        self.tool_button(ui, label);
                    }
                });
                ui.menu_button("Shapes", |ui| {
                    for label in ["Rectangle", "Circle", "Polygon"] {
                        self.tool_button(ui, label);
                    }
                });
            });
            ui.menu_button("Tools", |ui| {
                for label in [
                    "Select",
                    "Eraser",
                    "Paint",
                    "Move",
                    "Rotate",
                    "Scale",
                    "Push/Pull",
                    "Offset",
                ] {
                    self.tool_button(ui, label);
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

    /// A menu item that activates a tool, with its shortcut.
    fn tool_button(&mut self, ui: &mut egui::Ui, label: &str) {
        let Some(i) = self.tools.iter().position(|(t, _)| t.label() == label) else {
            return;
        };
        let mut button = egui::Button::new(label).selected(i == self.active);
        if let Some(key) = self.tools[i].1 {
            button = button.shortcut_text(key.name());
        }
        if ui.add(button).clicked() {
            self.activate_label(label);
            ui.close();
        }
    }

    /// The tool row (docs/07 §1 toolbars).
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let mut chosen = None;
            for (i, (tool, key)) in self.tools.iter().enumerate() {
                let response = ui.selectable_label(i == self.active, tool.label());
                let response = match key {
                    Some(k) => response.on_hover_text(format!("{} ({})", tool.label(), k.name())),
                    None => response.on_hover_text(tool.label()),
                };
                if response.clicked() {
                    chosen = Some(i);
                }
            }
            if let Some(i) = chosen {
                self.activate(i);
            }
        });
    }

    /// The active step's prompt and value field label (docs/07 §4).
    fn step_info(&self) -> (String, String) {
        let tool = self.tool();
        let (prompt, value_field) = self
            .session
            .engine
            .registry()
            .get(tool.command())
            .and_then(|c| c.meta().steps.get(tool.step()).cloned())
            .map(|s| (s.prompt, s.value_field))
            .unwrap_or_default();
        let label = if self.revalue_open && self.last_run.is_some() && tool.step() == 0 {
            "Value".to_string()
        } else {
            value_field
        };
        (prompt, label)
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
            let (prompt, value_label) = self.step_info();
            ui.label(prompt);
            let selected = self.session.engine.selection().len();
            if selected > 0 {
                ui.separator();
                ui.label(format!("{selected} selected"));
            }
            if let Some(inference) = self.viewport.current_inference()
                && !inference.label.is_empty()
                && self.tool().command() != "select"
            {
                ui.separator();
                ui.weak(&inference.label);
            } else if let Some(hover) = &self.viewport.hover {
                ui.separator();
                ui.weak(hover);
            }
            if let Some(e) = &self.preview.error {
                ui.separator();
                ui.weak(e);
            }
            if let Some((text, is_error)) = &self.message {
                ui.separator();
                if *is_error {
                    ui.colored_label(ui.visuals().error_fg_color, text);
                } else {
                    ui.label(text);
                }
            }
            // The value field (docs/07 §2): typed into without focus.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::Frame::new()
                    .stroke(ui.visuals().widgets.inactive.bg_stroke)
                    .inner_margin(egui::Margin::symmetric(4, 1))
                    .show(ui, |ui| {
                        ui.set_min_width(120.0);
                        ui.monospace(&self.value);
                    });
                if !value_label.is_empty() {
                    ui.label(value_label);
                }
            });
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
    render_state: Option<&'a egui_wgpu::RenderState>,
    tool: &'a mut dyn Tool,
    preview: Option<&'a Preview>,
    flows: Vec<Flow>,
    error: Option<String>,
}

impl egui_tiles::Behavior<Pane> for Docks<'_> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile: TileId, pane: &mut Pane) -> UiResponse {
        match pane {
            Pane::Viewport => {
                let flows =
                    self.viewport
                        .ui(ui, self.render_state, self.session, self.tool, self.preview);
                self.flows.extend(flows);
            }
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
                        Pane::ToolOptions => {
                            let cx = self.viewport.cx(self.session);
                            panels::tool_options(ui, self.tool, &cx);
                        }
                        Pane::ToolHelp => panels::tool_help(ui, self.session, self.tool),
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

impl App {
    /// One frame of the whole window (`render_state` is `None` headless).
    fn shell(&mut self, ui: &mut egui::Ui, render_state: Option<&egui_wgpu::RenderState>) {
        let ctx = ui.ctx().clone();
        // A button press ends the value field's hold on the last command.
        if ctx.input(|i| i.pointer.primary_pressed()) {
            self.revalue_open = false;
        }
        self.shortcuts(&ctx);
        self.tool_keys(&ctx);
        for warning in self.session.take_warnings() {
            self.info(warning);
        }
        self.update_preview(&ctx);
        egui::Panel::top("menu").show(ui, |ui| {
            self.menu_bar(ui);
            self.toolbar(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                let mut docks = Docks {
                    session: &mut self.session,
                    viewport: &mut self.viewport,
                    panels: &mut self.panels,
                    render_state,
                    tool: self.tools[self.active].0.as_mut(),
                    preview: self.preview.shown.as_ref(),
                    flows: Vec::new(),
                    error: None,
                };
                self.tree.ui(&mut docks, ui);
                let flows = std::mem::take(&mut docks.flows);
                if let Some(e) = docks.error.take() {
                    self.message = Some((e, true));
                }
                self.apply_flows(flows);
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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.shell(ui, frame.wgpu_render_state());
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, LAYOUT_KEY, &self.tree);
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use glam::DVec3;
    use planyrith_core::ids::EntityId;

    use super::*;

    /// The whole window driven by synthetic input, headless.
    struct Shell {
        ctx: egui::Context,
        time: f64,
        app: App,
    }

    impl Shell {
        fn new() -> Self {
            let mut shell = Shell {
                ctx: egui::Context::default(),
                time: 0.0,
                app: App::blank(default_layout(), Settings::default()),
            };
            shell.frame(Vec::new());
            shell
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let app = &mut self.app;
            let mut output = self.ctx.run_ui(input, |ui| app.shell(ui, None));
            output.textures_delta.clear();
        }

        /// Where a world point appears in the viewport.
        fn screen(&self, p: DVec3) -> egui::Pos2 {
            let cx = self.app.viewport.cx(&self.app.session);
            let pos = cx.to_screen(p).expect("in front of the camera");
            assert!(cx.rect.contains(pos), "{p} is off the viewport at {pos:?}");
            pos
        }

        fn click(&mut self, p: DVec3) {
            let pos = self.screen(p);
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
            self.time += 1.0;
        }

        fn hover(&mut self, p: DVec3) {
            let pos = self.screen(p);
            self.frame(vec![egui::Event::PointerMoved(pos)]);
        }

        /// Types text without focusing anything, then Enter if asked.
        fn type_text(&mut self, text: &str, enter: bool) {
            let mut events = vec![egui::Event::Text(text.to_string())];
            if enter {
                for pressed in [true, false] {
                    events.push(egui::Event::Key {
                        key: Key::Enter,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            }
            self.frame(events);
        }

        fn model_bounds(&self) -> planyrith_core::math::Aabb {
            self.app.session.doc().aabb()
        }
    }

    #[test]
    fn a_box_is_drawn_pushed_corrected_and_moved_with_the_tools() {
        let mut shell = Shell::new();
        shell
            .app
            .session
            .execute(
                "position-camera",
                json!({ "eye": [-4000, -9000, 9000], "target": [2000, 1500, 0] }),
            )
            .unwrap();
        shell.frame(Vec::new());

        // Rectangle: two clicks on the ground, then the exact size.
        shell.type_text("r", false);
        assert_eq!(shell.app.tool().command(), "rectangle");
        shell.click(DVec3::ZERO);
        shell.click(DVec3::new(3900.0, 2900.0, 0.0));
        assert_eq!(shell.app.session.doc().model.counts().faces, 1);
        shell.type_text("4000,3000", true);
        let b = shell.model_bounds();
        let size = b.max - b.min;
        assert!(
            size.distance(DVec3::new(4000.0, 3000.0, 0.0)) < 1e-6,
            "{b:?}"
        );
        assert!(
            b.min.length() < 1.0,
            "the first corner is where it was clicked"
        );
        let origin = b.min;

        // Push/Pull: pick the face, preview while moving, type the distance.
        shell.type_text("p", false);
        shell.click(origin + DVec3::new(2000.0, 1500.0, 0.0));
        shell.hover(origin + DVec3::new(2000.0, 1500.0, 800.0));
        shell.frame(Vec::new());
        let preview = shell.app.preview.shown.as_ref().expect("a preview");
        assert_eq!(preview.doc.model.counts().faces, 6);
        assert_eq!(
            shell.app.session.doc().model.counts().faces,
            1,
            "the preview leaves the document alone"
        );
        // Enter alone confirms the distance under the cursor...
        shell.type_text("", true);
        assert_eq!(shell.app.session.doc().model.counts().faces, 6);
        let height = shell.model_bounds().max.z;
        assert!(height > 100.0 && (height - 2500.0).abs() > 1.0, "{height}");
        // ...and a typed value replaces it.
        shell.type_text("2500", true);
        assert_eq!(shell.app.session.doc().model.counts().faces, 6);
        assert!((shell.model_bounds().max.z.abs() - 2500.0).abs() < 1e-6);
        // A new value right after replaces the distance (one undo step).
        shell.type_text("3000", true);
        assert!((shell.model_bounds().max.z.abs() - 3000.0).abs() < 1e-6);
        assert_eq!(shell.app.session.doc().model.counts().faces, 6);
        assert_eq!(
            shell.app.session.engine.history().undo_label(),
            Some("Push/Pull")
        );

        // Select everything, then Move it by a typed relative offset.
        shell.type_text(" ", false);
        assert_eq!(shell.app.tool().command(), "select");
        shell.app.run("select", json!({ "query": "all" }));
        shell.type_text("m", false);
        shell.click(DVec3::ZERO);
        shell.type_text("<1000,0,0>", true);
        let b = shell.model_bounds();
        assert!(
            (b.min.x - origin.x - 1000.0).abs() < 1e-6
                && (b.max.x - origin.x - 5000.0).abs() < 1e-6,
            "{b:?}"
        );
        assert!(
            shell.app.message.as_ref().is_none_or(|(_, e)| !e),
            "{:?}",
            shell.app.message
        );
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.typed(&c.to_string());
        }
        app.key(Key::Enter);
    }

    fn segment_lengths(app: &App) -> Vec<f64> {
        let doc = app.session.doc();
        let mut lengths: Vec<f64> = doc
            .model
            .order()
            .iter()
            .filter_map(|id| match id {
                EntityId::Edge(e) => doc.model.mesh().edge_segment(*e).ok(),
                _ => None,
            })
            .map(|s| s.length())
            .collect();
        lengths.sort_by(f64::total_cmp);
        lengths
    }

    #[test]
    fn the_value_field_drives_tools_and_replaces_the_last_value() {
        let mut app = App::blank(default_layout(), Settings::default());

        // A letter picks a tool while the value field is empty.
        app.typed("l");
        assert_eq!(app.tool().command(), "line");

        // Coordinates place points; each segment is a command.
        type_text(&mut app, "[0,0,0]");
        type_text(&mut app, "[1000,0,0]");
        assert_eq!(segment_lengths(&app), vec![1000.0]);
        assert_eq!(app.tool().step(), 1, "the chain continues");

        // Esc ends the chain; a value typed before the next click replaces
        // the last segment (docs/07 §2).
        app.key(Key::Escape);
        assert_eq!(app.tool().step(), 0);
        type_text(&mut app, "2500");
        let lengths = segment_lengths(&app);
        assert_eq!(lengths.len(), 1);
        assert!((lengths[0] - 2500.0).abs() < 1e-9, "{lengths:?}");
        assert_eq!(
            app.session.engine.history().undo_label(),
            Some("Line"),
            "one undo step"
        );

        // Letters typed after a number are part of the value (units).
        app.typed("3");
        app.typed("m");
        assert_eq!(app.value, "3m");
        assert_eq!(app.tool().command(), "line");
        app.key(Key::Escape);
        assert!(app.value.is_empty(), "Esc clears the value field first");

        // Space returns to Select; Select takes no typed values.
        app.typed(" ");
        assert_eq!(app.tool().command(), "select");
        app.revalue_open = false;
        app.typed("5");
        assert!(app.value.is_empty());
    }
}
