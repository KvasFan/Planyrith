//! The application shell (docs/07 §1, §7, §9): menu bar, docks, status bar,
//! keyboard shortcuts, file dialogs and the unsaved-changes guard. Menu items
//! are commands (docs/07 §7).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::Key;
use egui_file_dialog::FileDialog;
use egui_tiles::{Container, Tile, TileId, Tiles, Tree, UiResponse};
use glam::DVec3;
use planyrith_core::document::{EditContext, FaceMode, InstancePath};
use planyrith_core::inference::{Inference, InferenceKind, Lock};
use planyrith_core::math::Axis;
use planyrith_core::units::{ValueFieldInput, parse_value_field};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::icons::{self, Icon};
use crate::panels::{self, PanelState};
use crate::resources::{self, FileRequest, ResourceState, ToolRequest};
use crate::session::Session;
use crate::theme::{self, ThemeChoice};
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
    /// Materials.
    Materials,
    /// Components.
    Components,
    /// Styles.
    Styles,
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
            Pane::Materials => "Materials",
            Pane::Components => "Components",
            Pane::Styles => "Styles",
        }
    }
}

/// The default layout: tool options | viewport | info, materials,
/// components, styles, tags, tool help | outliner, scenes.
fn default_layout() -> Tree<Pane> {
    let mut tiles = Tiles::default();
    let tools = tiles.insert_pane(Pane::ToolOptions);
    let view = tiles.insert_pane(Pane::Viewport);
    let info = tiles.insert_pane(Pane::EntityInfo);
    let tags = tiles.insert_pane(Pane::Tags);
    let scenes = tiles.insert_pane(Pane::Scenes);
    let outliner = tiles.insert_pane(Pane::Outliner);
    let help = tiles.insert_pane(Pane::ToolHelp);
    let materials = tiles.insert_pane(Pane::Materials);
    let components = tiles.insert_pane(Pane::Components);
    let styles = tiles.insert_pane(Pane::Styles);
    let upper = tiles.insert_tab_tile(vec![info, materials, components, styles, tags, help]);
    let lower = tiles.insert_tab_tile(vec![outliner, scenes]);
    let right = tiles.insert_vertical_tile(vec![upper, lower]);
    if let Some(Tile::Container(Container::Linear(linear))) = tiles.get_mut(right) {
        linear.shares.set_share(upper, 0.65);
        linear.shares.set_share(lower, 0.35);
    }
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
#[derive(Debug, Clone, PartialEq, Eq)]
enum DialogPurpose {
    Open,
    SaveAs,
    /// A panel's file (texture, library, component).
    Resource(FileRequest),
}

/// Persisted per user (docs/07 §1: layouts are not stored in documents).
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    recent: Vec<PathBuf>,
    /// View › Axes.
    show_axes: bool,
    /// View › Theme.
    theme: ThemeChoice,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            recent: Vec::new(),
            show_axes: true,
            theme: ThemeChoice::System,
        }
    }
}

const LAYOUT_KEY: &str = "planyrith.layout.v3";
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
    /// `Tab` presses taken from egui's focus traversal this frame.
    tabs: usize,
    /// The scene animation: scenes to play, the next one, when.
    animation: Option<(Vec<String>, usize, f64)>,
    /// Materials, Components and Styles panels.
    resources: ResourceState,
    /// The texture placement window: the face and the fields.
    texture: Option<(planyrith_core::document::EntityPath, TextureFields)>,
}

/// The fields of the texture placement window (docs/05 §5.9).
#[derive(Debug, Clone, Copy, PartialEq)]
struct TextureFields {
    offset_u: f64,
    offset_v: f64,
    rotation: f64,
    scale_u: f64,
    scale_v: f64,
    both_sides: bool,
}

impl Default for TextureFields {
    fn default() -> Self {
        TextureFields {
            offset_u: 0.0,
            offset_v: 0.0,
            rotation: 0.0,
            scale_u: 1.0,
            scale_v: 1.0,
            both_sides: false,
        }
    }
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
        theme::apply(&cc.egui_ctx, app.settings.theme);
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
            tabs: 0,
            animation: None,
            resources: ResourceState::default(),
            texture: None,
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
                self.info(format!("Opened {}", file_name(path)));
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
            self.dialog = Self::document_dialog();
            self.dialog_purpose = Some(DialogPurpose::SaveAs);
            self.dialog.save_file();
            return;
        }
        match self.session.save(path.as_deref()) {
            Ok(path) => {
                self.remember(path.clone());
                self.info(format!("Saved {}", file_name(&path)));
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
                self.dialog = Self::document_dialog();
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

    /// Runs commands for the screenshot test, panicking on failure.
    #[cfg(test)]
    pub fn run_for_screens(&mut self, commands: &[(&str, Value)]) {
        for (name, params) in commands {
            if let Err(e) = self.session.execute(name, params.clone()) {
                panic!("{name}: {e}");
            }
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
                Flow::Texture(face) => self.texture = Some((face, TextureFields::default())),
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
        // A typed point (`x,y,z`, `[x,y,z]`, or `<dx,dy,dz>` from the step's
        // reference point) is a click there for every tool that picks points.
        if tool.takes_points()
            && let Ok(ValueFieldInput::Coordinate { absolute, xyz }) =
                parse_value_field(text, cx.unit)
        {
            let current = self.viewport.pointer();
            let (base, keep) = if absolute {
                (
                    DVec3::ZERO,
                    current.map_or(DVec3::ZERO, |p| p.inference.point),
                )
            } else {
                (
                    tool.setup(&cx).reference.unwrap_or(DVec3::ZERO),
                    DVec3::ZERO,
                )
            };
            let c = |i: usize| xyz[i].map_or(keep[i], |l| l.as_mm());
            let point = base + DVec3::new(c(0), c(1), c(2));
            let pointer = typed_pointer(point, current, cx.rect.center());
            let flow = tool.click(&pointer, &cx);
            self.apply_flows(vec![flow]);
            return;
        }
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
            Key::Backspace if !self.value.is_empty() => {
                self.value.pop();
            }
            Key::Backspace => {
                // Removes the last point of a chain and its segment.
                if self.tools[self.active].0.back()
                    && let Err(e) = self.session.undo()
                {
                    self.error(e);
                }
                self.last_run = None;
                self.clear_preview();
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
        if self.animation.take().is_some() {
            return;
        }
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

    /// The scene animation (docs/07 §6): each scene included in the
    /// animation in turn, its transition, then a one-second pause, looping
    /// until stopped.
    fn play_scenes(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        match self.panels.play.take() {
            Some(true) => {
                let doc = self.session.doc();
                let scenes: Vec<String> = doc
                    .scenes
                    .iter()
                    .filter(|(_, s)| s.include_in_animation)
                    .map(|(_, s)| s.name.clone())
                    .collect();
                self.animation = (!scenes.is_empty()).then_some((scenes, 0, now));
            }
            Some(false) => self.animation = None,
            None => {}
        }
        self.panels.playing = self.animation.is_some();
        let Some((scenes, next, at)) = &mut self.animation else {
            return;
        };
        if now < *at {
            ctx.request_repaint_after(Duration::from_secs_f64(*at - now));
            return;
        }
        let name = scenes[*next].clone();
        *next = (*next + 1) % scenes.len();
        let doc = self.session.doc();
        let seconds = doc
            .scenes
            .by_name(&name)
            .and_then(|id| doc.scenes.get(id))
            .map_or(0.0, |s| f64::from(s.transition_s));
        *at = now + seconds + 1.0;
        self.viewport.active_scene = Some(name.clone());
        if let Err(e) = self
            .session
            .execute("scene-activate", json!({ "scene": name }))
        {
            self.animation = None;
            self.error(e);
        }
        ctx.request_repaint();
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
            self.dialog = Self::document_dialog();
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
                    self.dialog = Self::document_dialog();
                    self.dialog_purpose = Some(DialogPurpose::SaveAs);
                    self.dialog.save_file();
                    ui.close();
                }
                ui.separator();
                if ui.button("Import…").clicked() {
                    self.resources.file = Some(FileRequest::Import);
                    ui.close();
                }
                ui.menu_button("Export", |ui| {
                    if ui.button("3D Model…").clicked() {
                        self.resources.file = Some(FileRequest::Export);
                        ui.close();
                    }
                    if ui.button("2D Graphic…").clicked() {
                        self.resources.file = Some(FileRequest::Export2d);
                        ui.close();
                    }
                    if ui.button("Component Report…").clicked() {
                        self.resources.file = Some(FileRequest::Report);
                        ui.close();
                    }
                });
                if ui.button("Insert Image…").clicked() {
                    self.resources.file = Some(FileRequest::Image);
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
                ui.checkbox(&mut self.settings.show_axes, "Axes");
                ui.separator();
                ui.menu_button("Theme", |ui| {
                    for choice in [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark] {
                        if ui
                            .radio(self.settings.theme == choice, choice.label())
                            .clicked()
                        {
                            self.settings.theme = choice;
                            theme::apply(ui.ctx(), choice);
                            ui.close();
                        }
                    }
                });
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

    /// The toolbar (docs/07 §1): file, history, tools, views and face
    /// styles as icon buttons with tooltips.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let group_gap = |ui: &mut egui::Ui| {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
            };
            if icons::button(ui, Icon::New, false, "New (Ctrl+N)").clicked() {
                self.guarded(Pending::New);
            }
            if icons::button(ui, Icon::Open, false, "Open… (Ctrl+O)").clicked() {
                self.guarded(Pending::Open(None));
            }
            if icons::button(ui, Icon::Save, false, "Save (Ctrl+S)").clicked() {
                self.save(None);
            }
            group_gap(ui);
            let history = self.session.engine.history();
            let undo = history.undo_label().map_or_else(
                || "Undo (Ctrl+Z)".to_string(),
                |l| format!("Undo {l} (Ctrl+Z)"),
            );
            let redo = history.redo_label().map_or_else(
                || "Redo (Ctrl+Y)".to_string(),
                |l| format!("Redo {l} (Ctrl+Y)"),
            );
            let (can_undo, can_redo) = (history.can_undo(), history.can_redo());
            if ui
                .add_enabled_ui(can_undo, |ui| icons::button(ui, Icon::Undo, false, &undo))
                .inner
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled_ui(can_redo, |ui| icons::button(ui, Icon::Redo, false, &redo))
                .inner
                .clicked()
            {
                self.redo();
            }
            for group in [
                &["select", "erase", "material-apply"][..],
                &[
                    "line",
                    "arc-bulge",
                    "arc-2point",
                    "arc-3point",
                    "pie",
                    "rectangle",
                    "circle",
                    "polygon",
                ],
                &["push-pull", "offset", "move", "rotate", "scale"],
            ] {
                group_gap(ui);
                for command in group {
                    let Some(i) = self.tools.iter().position(|(t, _)| t.command() == *command)
                    else {
                        continue;
                    };
                    let (tool, key) = &self.tools[i];
                    let Some(icon) = Icon::for_command(command) else {
                        continue;
                    };
                    let tip = match key {
                        Some(k) => format!("{} ({})", tool.label(), k.name()),
                        None => tool.label().to_string(),
                    };
                    if icons::button(ui, icon, i == self.active, &tip).clicked() {
                        self.activate(i);
                    }
                }
            }
            group_gap(ui);
            for (icon, view, tip) in [
                (Icon::ViewIso, "iso", "Iso view"),
                (Icon::ViewTop, "top", "Top view"),
                (Icon::ViewFront, "front", "Front view"),
                (Icon::ViewRight, "right", "Right view"),
            ] {
                if icons::button(ui, icon, false, tip).clicked() {
                    self.run("camera-standard", json!({ "view": view }));
                }
            }
            if icons::button(ui, Icon::ZoomExtents, false, "Zoom Extents (Shift+Z)").clicked() {
                self.run("zoom-extents", json!({}));
            }
            group_gap(ui);
            let mode = self
                .session
                .doc()
                .styles
                .active_style()
                .map(|s| s.faces.mode);
            for (icon, m, key, tip) in [
                (
                    Icon::Wireframe,
                    FaceMode::Wireframe,
                    "wireframe",
                    "Wireframe",
                ),
                (
                    Icon::HiddenLine,
                    FaceMode::HiddenLine,
                    "hidden-line",
                    "Hidden line",
                ),
                (Icon::Shaded, FaceMode::Shaded, "shaded", "Shaded"),
                (
                    Icon::Textured,
                    FaceMode::ShadedTextured,
                    "shaded-textured",
                    "Shaded with textures",
                ),
                (Icon::XRay, FaceMode::XRay, "x-ray", "X-ray"),
            ] {
                if icons::button(ui, icon, mode == Some(m), tip).clicked() {
                    self.run("style-set", json!({ "faces": key }));
                }
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
        let (prompt, value_label) = self.step_info();
        ui.horizontal(|ui| {
            // The value field (docs/07 §2), always at the right: typed into
            // without focus.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let visuals = ui.visuals().clone();
                let focused = !self.value.is_empty();
                egui::Frame::new()
                    .fill(visuals.extreme_bg_color)
                    .stroke(if focused {
                        visuals.selection.stroke
                    } else {
                        visuals.widgets.noninteractive.bg_stroke
                    })
                    .corner_radius(3)
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(ui, |ui| {
                        ui.set_min_width(150.0);
                        ui.monospace(&self.value);
                    });
                ui.label(if value_label.is_empty() {
                    egui::RichText::new("Value").weak()
                } else {
                    egui::RichText::new(&value_label)
                });
                ui.separator();
                // Everything else from the left.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    self.status_left(ui, &prompt);
                });
            });
        });
    }

    /// Breadcrumb, the step's prompt, the selection, what is under the
    /// cursor and the last message (docs/07 §4).
    fn status_left(&mut self, ui: &mut egui::Ui, prompt: &str) {
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
        if let Some(icon) = Icon::for_command(self.tool().command()) {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            icons::paint(ui.painter(), rect, icon, ui.visuals().strong_text_color());
        }
        ui.strong(prompt);
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
            let text = egui::RichText::new(text);
            let text = if *is_error {
                text.color(ui.visuals().error_fg_color)
            } else {
                text.weak()
            };
            ui.add(egui::Label::new(text).truncate());
        }
    }

    /// The texture placement window (docs/05 §5.9): offset, rotation and
    /// scale of the selected face's texture, applied as one command.
    fn texture_window(&mut self, ctx: &egui::Context) {
        let Some((face, fields)) = &mut self.texture else {
            return;
        };
        let mut open = true;
        let mut apply = None;
        egui::Window::new("Position Texture")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::Grid::new("texture fields")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Offset");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut fields.offset_u)
                                    .speed(5.0)
                                    .suffix(" mm"),
                            );
                            ui.add(
                                egui::DragValue::new(&mut fields.offset_v)
                                    .speed(5.0)
                                    .suffix(" mm"),
                            );
                        });
                        ui.end_row();
                        ui.label("Rotation");
                        ui.add(
                            egui::DragValue::new(&mut fields.rotation)
                                .speed(1.0)
                                .range(-360.0..=360.0)
                                .suffix("°"),
                        );
                        ui.end_row();
                        ui.label("Scale");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut fields.scale_u)
                                    .speed(0.01)
                                    .range(0.01..=100.0),
                            );
                            ui.add(
                                egui::DragValue::new(&mut fields.scale_v)
                                    .speed(0.01)
                                    .range(0.01..=100.0),
                            );
                        });
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut fields.both_sides, "Both sides");
                        ui.end_row();
                    });
                ui.horizontal(|ui| {
                    let side = if fields.both_sides { "both" } else { "front" };
                    if ui.button("Apply").clicked() {
                        apply = Some(json!({
                            "face": { "Path": face },
                            "side": side,
                            "offset_u": fields.offset_u,
                            "offset_v": fields.offset_v,
                            "rotation": fields.rotation,
                            "scale_u": fields.scale_u,
                            "scale_v": fields.scale_v,
                        }));
                    }
                    if ui.button("Reset").clicked() {
                        apply =
                            Some(json!({ "face": { "Path": face }, "side": side, "reset": true }));
                        *fields = TextureFields::default();
                    }
                });
            });
        if let Some(params) = apply {
            self.run("texture-position", params);
        }
        if !open {
            self.texture = None;
        }
    }

    /// Opens the file dialog for a panel's request.
    fn open_resource_dialog(&mut self, request: FileRequest) {
        let (title, kind, extensions, saves) = request.dialog();
        self.dialog = FileDialog::new()
            .title(title)
            .add_file_filter_extensions(kind, extensions.to_vec())
            .default_file_filter(kind);
        if saves {
            self.dialog.save_file();
        } else {
            self.dialog.pick_file();
        }
        self.dialog_purpose = Some(DialogPurpose::Resource(request));
    }

    /// The document dialog (open/save) again after a panel's dialog.
    fn document_dialog() -> FileDialog {
        FileDialog::new()
            .add_file_filter_extensions("Planyrith documents", vec!["plyr"])
            .default_file_filter("Planyrith documents")
    }

    /// Activates the tool a panel asked for.
    fn activate_request(&mut self, request: ToolRequest) {
        let (command, tool): (&str, Box<dyn Tool>) = match request {
            ToolRequest::Paint(m) => (
                "material-apply",
                Box::new(tools::PaintTool::with_material(m)),
            ),
            ToolRequest::Place(d) => ("component-insert", Box::new(tools::ComponentTool::new(d))),
        };
        if let Some(i) = self.tools.iter().position(|(t, _)| t.command() == command) {
            self.activate(i);
            self.tools[i].0 = tool;
        }
    }

    fn file_dialog(&mut self, ctx: &egui::Context) {
        self.dialog.update(ctx);
        if let Some(path) = self.dialog.take_picked() {
            match self.dialog_purpose.take() {
                Some(DialogPurpose::Resource(request)) => {
                    let (_, _, extensions, saves) = request.dialog();
                    let path = if saves && path.extension().is_none() {
                        path.with_extension(extensions.first().copied().unwrap_or("plyr"))
                    } else {
                        path
                    };
                    let (name, params) = request.command(&path.to_string_lossy());
                    match self.session.execute(name, params) {
                        Ok(_) => self.info(format!("{} {}", request.dialog().0, file_name(&path))),
                        Err(e) => self.error(e),
                    }
                }
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

/// The pointer for a point typed into the value field: no entity under it,
/// on the plane the real pointer is on when that plane holds the point.
fn typed_pointer(
    point: DVec3,
    current: Option<&tools::Pointer>,
    screen: egui::Pos2,
) -> tools::Pointer {
    let tol = planyrith_core::Tolerances::default();
    let plane = current
        .and_then(|p| p.inference.plane)
        .filter(|plane| plane.contains(point, &tol));
    tools::Pointer {
        inference: Inference {
            point,
            kind: InferenceKind::Free,
            on: None,
            locked: false,
            label: "Typed".into(),
            axis: None,
            from: None,
            direction: None,
            plane,
            via: None,
        },
        modifiers: egui::Modifiers::NONE,
        target: None,
        face: None,
        screen: current.map_or(screen, |p| p.screen),
    }
}

/// A path's file name for messages.
fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Dock behavior: draws each pane with access to the app state.
struct Docks<'a> {
    session: &'a mut Session,
    viewport: &'a mut Viewport,
    panels: &'a mut PanelState,
    resources: &'a mut ResourceState,
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
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
                egui::Frame::new()
                    .inner_margin(8.0)
                    .show(ui, |ui| match other {
                        Pane::EntityInfo => panels::entity_info(ui, self.session, &mut self.error),
                        Pane::Tags => panels::tags(ui, self.session, self.panels, &mut self.error),
                        Pane::Scenes => {
                            panels::scenes(ui, self.session, self.panels, &mut self.error)
                        }
                        Pane::Outliner => panels::outliner(ui, self.session, &mut self.error),
                        Pane::ToolOptions => {
                            let prompt = self
                                .session
                                .engine
                                .registry()
                                .get(self.tool.command())
                                .and_then(|c| c.meta().steps.get(self.tool.step()).cloned())
                                .map(|s| s.prompt)
                                .unwrap_or_default();
                            let cx = self.viewport.cx(self.session);
                            panels::tool_options(ui, self.tool, &cx, &prompt);
                        }
                        Pane::ToolHelp => panels::tool_help(ui, self.session, self.tool),
                        Pane::Materials => {
                            resources::materials(ui, self.session, self.resources, &mut self.error)
                        }
                        Pane::Components => {
                            resources::components(ui, self.session, self.resources, &mut self.error)
                        }
                        Pane::Styles => {
                            resources::styles(ui, self.session, self.resources, &mut self.error)
                        }
                        Pane::Viewport => {}
                    });
            }
        }
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title().into()
    }

    fn tab_bar_color(&self, visuals: &egui::Visuals) -> egui::Color32 {
        visuals.faint_bg_color
    }

    fn tab_bar_height(&self, _style: &egui::Style) -> f32 {
        26.0
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        2.0
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
        for _ in 0..std::mem::take(&mut self.tabs) {
            self.viewport.cycle();
        }
        for warning in self.session.take_warnings() {
            self.info(warning);
        }
        self.update_preview(&ctx);
        self.play_scenes(&ctx);
        self.viewport.show_axes = self.settings.show_axes;
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                let mut docks = Docks {
                    session: &mut self.session,
                    viewport: &mut self.viewport,
                    panels: &mut self.panels,
                    resources: &mut self.resources,
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
        if let Some(request) = self.resources.activate.take() {
            self.activate_request(request);
        }
        if let Some(request) = self.resources.file.take() {
            self.open_resource_dialog(request);
        }
        if let Some(e) = self.viewport.error.take() {
            self.error(e);
        }
        self.texture_window(&ctx);
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

    /// `Tab` cycles inference candidates while no text field has focus
    /// (docs/07 §5), so it is taken before egui's focus traversal sees it.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        raw_input.events.retain(|e| match e {
            egui::Event::Key {
                key: Key::Tab,
                pressed,
                modifiers,
                ..
            } if modifiers.is_none() => {
                if *pressed {
                    self.tabs += 1;
                }
                false
            }
            _ => true,
        });
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
    use planyrith_core::inference::InferenceKind;

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
            let mut input = input;
            eframe::App::raw_input_hook(&mut self.app, &self.ctx, &mut input);
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

        fn faces(&self) -> usize {
            self.app.session.doc().model.counts().faces
        }

        fn triple_click(&mut self, p: DVec3) {
            let pos = self.screen(p);
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            for _ in 0..3 {
                for pressed in [true, false] {
                    self.frame(vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }]);
                }
            }
            self.time += 1.0;
        }

        /// No error in the status bar.
        fn ok(&self, step: &str) {
            assert!(
                self.app.message.as_ref().is_none_or(|(_, e)| !e),
                "{step}: {:?}",
                self.app.message
            );
        }
    }

    #[test]
    fn typed_points_place_every_tool_step() {
        let mut app = App::blank(default_layout(), Settings::default());

        // Rectangle: the first corner typed as `x,y,z`, then the size.
        app.typed("r");
        type_text(&mut app, "1000,2000,0");
        assert_eq!(app.tool().step(), 1, "the corner is placed");
        type_text(&mut app, "500,300");
        let b = app.session.doc().aabb();
        assert!(
            b.min.distance(DVec3::new(1000.0, 2000.0, 0.0)) < 1e-9,
            "{b:?}"
        );
        assert!(
            b.max.distance(DVec3::new(1500.0, 2300.0, 0.0)) < 1e-9,
            "{b:?}"
        );

        // Circle: the center in brackets, then the radius.
        app.typed(" ");
        app.typed("c");
        type_text(&mut app, "[5000,0,0]");
        type_text(&mut app, "750");
        let b = app.session.doc().aabb();
        assert!((b.max.x - 5750.0).abs() < 1e-6, "{b:?}");

        // Line: an absolute start, then a point relative to it.
        app.typed(" ");
        app.typed("l");
        type_text(&mut app, "-1000,0,0");
        type_text(&mut app, "<0,0,2500>");
        let b = app.session.doc().aabb();
        assert!((b.max.z - 2500.0).abs() < 1e-9, "{b:?}");
        assert!((b.min.x + 1000.0).abs() < 1e-9, "{b:?}");
        assert!(
            app.message.as_ref().is_none_or(|(_, e)| !e),
            "{:?}",
            app.message
        );
    }

    #[test]
    fn tab_cycles_the_inference_candidates_until_the_pointer_moves() {
        let mut shell = Shell::new();
        shell
            .app
            .session
            .execute(
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [2000, 1500, 0] }),
            )
            .unwrap();
        shell
            .app
            .session
            .execute("camera-standard", json!({ "view": "top", "extents": true }))
            .unwrap();
        shell.type_text("l", false);
        let near_corner = shell.screen(DVec3::new(2000.0, 1500.0, 0.0)) + egui::vec2(-3.0, 2.0);
        shell.frame(vec![egui::Event::PointerMoved(near_corner)]);
        let kind = |shell: &Shell| shell.app.viewport.current_inference().map(|i| i.kind);
        assert_eq!(kind(&shell), Some(InferenceKind::Endpoint));
        let tab = |pressed| egui::Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        shell.frame(vec![tab(true), tab(false)]);
        shell.frame(Vec::new());
        let second = kind(&shell);
        assert!(
            second.is_some() && second != Some(InferenceKind::Endpoint),
            "{second:?}"
        );
        assert!(
            shell.ctx.memory(|m| m.focused().is_none()),
            "Tab did not move the keyboard focus"
        );
        shell.frame(vec![egui::Event::PointerMoved(
            near_corner + egui::vec2(1.5, 0.0),
        )]);
        assert_eq!(kind(&shell), Some(InferenceKind::Endpoint));
    }

    #[test]
    fn right_click_selects_what_is_under_the_cursor() {
        let mut shell = Shell::new();
        shell
            .app
            .session
            .execute(
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [2000, 1500, 0] }),
            )
            .unwrap();
        shell
            .app
            .session
            .execute("camera-standard", json!({ "view": "top", "extents": true }))
            .unwrap();
        shell.frame(Vec::new());
        let at = shell.screen(DVec3::new(1000.0, 700.0, 0.0));
        shell.frame(vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false] {
            shell.frame(vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        shell.frame(Vec::new());
        assert_eq!(shell.app.session.engine.selection().len(), 1);
    }

    #[test]
    fn scenes_animate_their_transitions_and_play_in_turn() {
        let mut shell = Shell::new();
        let session = &mut shell.app.session;
        session
            .execute(
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [2000, 1500, 0] }),
            )
            .unwrap();
        session
            .execute("camera-standard", json!({ "view": "top", "extents": true }))
            .unwrap();
        session
            .execute("scene-add", json!({ "name": "Plan", "transition_s": 2.0 }))
            .unwrap();
        session
            .execute("camera-standard", json!({ "view": "iso", "extents": true }))
            .unwrap();
        session
            .execute("scene-add", json!({ "name": "Iso" }))
            .unwrap();
        let iso = shell.app.session.doc().camera;
        shell.frame(Vec::new());

        // Activating "Plan" moves the document camera at once; the viewport
        // shows cameras in between for two seconds.
        shell
            .app
            .session
            .execute("scene-activate", json!({ "scene": "Plan" }))
            .unwrap();
        let plan = shell.app.session.doc().camera;
        shell.frame(Vec::new());
        shell.time += 1.0;
        shell.frame(Vec::new());
        let shown = shell.app.viewport.shown_camera().expect("a transition");
        let (to_plan, to_iso) = (shown.eye.distance(plan.eye), shown.eye.distance(iso.eye));
        assert!(to_plan > 1.0 && to_iso > 1.0, "{to_plan} {to_iso}");
        shell.time += 1.5;
        shell.frame(Vec::new());
        assert!(
            shell.app.viewport.shown_camera().is_none(),
            "the transition ended"
        );

        // Playing steps through the scenes included in the animation.
        shell.app.panels.play = Some(true);
        shell.frame(Vec::new());
        assert_eq!(shell.app.viewport.active_scene.as_deref(), Some("Plan"));
        shell.time += 3.5;
        shell.frame(Vec::new());
        assert_eq!(shell.app.viewport.active_scene.as_deref(), Some("Iso"));
        shell.app.key(Key::Escape);
        assert!(shell.app.animation.is_none());
    }

    #[test]
    fn panels_place_components_and_paint_with_materials() {
        let mut shell = Shell::new();
        let session = &mut shell.app.session;
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [500, 500, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 500.0 }),
            ),
            (
                "make-component",
                json!({ "targets": "all", "name": "Crate" }),
            ),
            (
                "rectangle",
                json!({ "corner_a": [2000, 0, 0], "corner_b": [3000, 1000, 0] }),
            ),
            (
                "material-create",
                json!({ "name": "Red", "color": "#CC2222" }),
            ),
            ("camera-standard", json!({ "view": "top", "extents": true })),
            ("zoom", json!({ "factor": 0.5 })),
        ] {
            session.execute(name, params).unwrap();
        }
        shell.frame(Vec::new());

        // Place two more crates by clicking.
        shell
            .app
            .activate_request(ToolRequest::Place("Crate".into()));
        assert_eq!(shell.app.tool().command(), "component-insert");
        shell.click(DVec3::new(-1500.0, 0.0, 0.0));
        shell.click(DVec3::new(-1500.0, 1500.0, 0.0));
        let doc = shell.app.session.doc();
        assert_eq!(doc.model.counts().instances, 3);
        assert_eq!(doc.definitions.len(), 1);

        // Paint the loose face red.
        shell.app.activate_request(ToolRequest::Paint("Red".into()));
        assert_eq!(shell.app.tool().command(), "material-apply");
        shell.click(DVec3::new(2500.0, 500.0, 0.0));
        let doc = shell.app.session.doc();
        let red = doc.materials.by_name("Red").unwrap();
        let painted = doc
            .model
            .mesh()
            .faces()
            .any(|(_, f)| f.sides.material_front == Some(red));
        assert!(painted, "{:?}", shell.app.message);
    }

    #[test]
    fn bundled_materials_load_and_paint_the_visible_side() {
        let mut shell = Shell::new();
        for b in crate::bundled::catalogue() {
            let path = crate::bundled::texture_path(&b).unwrap();
            shell
                .app
                .session
                .execute(
                    "material-create",
                    json!({ "name": b.name, "color": b.color, "texture": path.to_string_lossy(),
                            "texture_width": b.size_mm.0, "texture_height": b.size_mm.1 }),
                )
                .unwrap();
        }
        let doc = shell.app.session.doc();
        assert_eq!(doc.images.len(), crate::bundled::catalogue().len());

        // A vertical face whose front looks away from the camera: Paint
        // paints the side the eye sees.
        let session = &mut shell.app.session;
        session
            .execute(
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [2000, 0, 1500], "plane": { "normal": [0, 1, 0], "d": 0 } }),
            )
            .unwrap();
        session
            .execute(
                "position-camera",
                json!({ "eye": [1000, -6000, 750], "target": [1000, 0, 750] }),
            )
            .unwrap();
        shell.frame(Vec::new());
        shell
            .app
            .activate_request(ToolRequest::Paint("Brick".into()));
        shell.click(DVec3::new(1000.0, 0.0, 750.0));
        let doc = shell.app.session.doc();
        let brick = doc.materials.by_name("Brick").unwrap();
        let (_, face) = doc.model.mesh().faces().next().unwrap();
        let eye_side_is_front = face
            .plane
            .signed_distance(glam::DVec3::new(1000.0, -6000.0, 750.0))
            > 0.0;
        let painted = if eye_side_is_front {
            face.sides.material_front
        } else {
            face.sides.material_back
        };
        assert_eq!(painted, Some(brick), "{:?}", shell.app.message);
    }

    #[test]
    fn file_import_and_export_run_the_commands() {
        let mut shell = Shell::new();
        let dir = std::env::temp_dir().join(format!("planyrith-gui-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let glb = dir.join("box.glb");
        let session = &mut shell.app.session;
        session
            .execute(
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 600, 0] }),
            )
            .unwrap();
        session
            .execute(
                "push-pull",
                json!({ "face": { "Query": "face" }, "distance": 400.0 }),
            )
            .unwrap();
        let dirty = session.engine.generation();
        let (name, params) = FileRequest::Export.command(&glb.to_string_lossy());
        session.execute(name, params).unwrap();
        assert!(glb.exists());
        assert_eq!(
            session.engine.generation(),
            dirty,
            "exporting changes nothing"
        );
        let (name, params) = FileRequest::Import.command(&glb.to_string_lossy());
        let out = session.execute(name, params).unwrap();
        assert_eq!(out["faces"], 6, "{out}");
        shell.frame(Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// docs/qa/model-a-room.md, automated: the same ten steps with synthetic
    /// mouse and keyboard input (docs/13 0.3 acceptance).
    #[test]
    fn qa_model_a_room_in_ten_steps() {
        let mut shell = Shell::new();
        shell
            .app
            .session
            .execute(
                "position-camera",
                json!({ "eye": [-7000, -9000, 8000], "target": [3500, 2500, 800] }),
            )
            .unwrap();
        shell.frame(Vec::new());

        // 1. Rectangle: the outer outline of the walls, 6400 × 4400.
        shell.type_text("r", false);
        shell.click(DVec3::ZERO);
        shell.hover(DVec3::new(5000.0, 3000.0, 0.0));
        shell.type_text("6400,4400", true);
        shell.ok("1");
        let o = shell.model_bounds().min;
        let at = |x: f64, y: f64, z: f64| o + DVec3::new(x, y, z);
        assert_eq!(shell.faces(), 1);

        // 2. Offset the floor 200 inward.
        shell.type_text("f", false);
        shell.click(at(3200.0, 2200.0, 0.0));
        shell.hover(at(3200.0, 1000.0, 0.0));
        shell.type_text("200", true);
        shell.ok("2");
        assert_eq!(shell.faces(), 2, "the ring and the inner floor");

        // 3. Push/Pull the ring up 2700: the walls.
        shell.type_text("p", false);
        shell.click(at(3200.0, 100.0, 0.0));
        shell.hover(at(3200.0, 100.0, 1500.0));
        shell.type_text("2700", true);
        shell.ok("3");
        let b = shell.model_bounds();
        assert!((b.max.z - o.z - 2700.0).abs() < 1e-6, "{b:?}");

        // 4. Rectangle on the front wall's outer face: the door, 900 × 2100.
        shell.type_text("r", false);
        shell.click(at(1200.0, 0.0, 10.0));
        shell.hover(at(1800.0, 0.0, 1500.0));
        shell.type_text("900,2100", true);
        shell.ok("4");

        // 5. Push/Pull the door 200 into the wall: an opening.
        let faces = shell.faces();
        shell.type_text("p", false);
        shell.click(at(1650.0, 0.0, 1000.0));
        shell.hover(at(1650.0, 150.0, 1000.0));
        shell.type_text("200", true);
        shell.ok("5");
        assert!(shell.faces() > faces, "the opening's jambs");

        // 6.–7. A window on the left wall, 1200 × 1000 at 900, pushed through.
        shell.type_text("r", false);
        shell.click(at(0.0, 1600.0, 900.0));
        shell.hover(at(0.0, 2500.0, 1600.0));
        shell.type_text("1200,1000", true);
        shell.ok("6");
        shell.type_text("p", false);
        shell.click(at(0.0, 2200.0, 1400.0));
        shell.hover(at(150.0, 2200.0, 1400.0));
        shell.type_text("200", true);
        shell.ok("7");

        // 8.–9. A table block outside the room: 2200 × 800, 720 high.
        shell.type_text("r", false);
        shell.click(at(1000.0, -3000.0, 0.0));
        shell.hover(at(3000.0, -2000.0, 0.0));
        shell.type_text("2200,800", true);
        shell.ok("8");
        let table = shell.model_bounds();
        shell.type_text("p", false);
        shell.click(at(2100.0, -2600.0, 0.0));
        shell.hover(at(2100.0, -2600.0, 400.0));
        shell.type_text("720", true);
        shell.ok("9");

        // 10. Select the table (triple-click: connected geometry), Edit ›
        // Make Group, and Move it into the room by a typed offset.
        shell.type_text(" ", false);
        shell.triple_click(at(2100.0, -2600.0, 720.0));
        shell
            .app
            .run("make-group", json!({ "targets": "selection" }));
        shell.ok("10 group");
        shell.type_text("m", false);
        shell.click(at(1000.0, -3000.0, 720.0));
        shell.hover(at(1500.0, -2000.0, 720.0));
        shell.type_text("<0,4500,0>", true);
        shell.ok("10 move");

        // The door and the window are openings: rays through them reach the
        // inner faces of the opposite walls.
        let mut through = |from: DVec3, dir: DVec3| {
            let ray = planyrith_core::math::Ray::new(from, dir);
            let filter = planyrith_core::spatial::HitFilter {
                vertices: false,
                edges: false,
                ..Default::default()
            };
            shell.app.session.ray_cast(&ray, &filter)[0].point
        };
        let door = through(at(1650.0, -1000.0, 1000.0), DVec3::Y);
        assert!(
            (door.y - o.y - 4200.0).abs() < 1e-6,
            "through the door: {door}"
        );
        // The first corner snapped to the wall's bottom edge: no sill.
        let sill = through(at(1650.0, -1000.0, 5.0), DVec3::Y);
        assert!(sill.y > o.y + 1.0, "the door reaches the ground: {sill}");
        let window = through(at(-1000.0, 2200.0, 1400.0), DVec3::X);
        assert!(
            (window.x - o.x - 6200.0).abs() < 1e-6,
            "through the window: {window}"
        );

        let doc = shell.app.session.doc();
        assert_eq!(doc.model.counts().groups, 1);
        let group = doc
            .model
            .iter()
            .find(|id| matches!(id, EntityId::Group(_)))
            .unwrap();
        let path = planyrith_core::document::EntityPath::new(
            InstancePath::root(),
            planyrith_core::document::EntityKey::Id(group),
        );
        let bounds = doc.occurrence_aabb(&path).unwrap();
        assert!(
            bounds.min.y > o.y + 200.0 && bounds.max.y < o.y + 4200.0,
            "the table stands inside the room: {bounds:?} (outline from {o})"
        );
        assert!((bounds.max.z - o.z - 720.0).abs() < 1e-6);
        assert!(table.min.y < o.y, "it was drawn outside");
        assert!(
            planyrith_core::diagnostics::validate(doc).is_clean(),
            "{:?}",
            planyrith_core::diagnostics::validate(doc).problems
        );
        // `PLANYRITH_QA_OUT=room.plyr` keeps the result (for the expected
        // screenshot of docs/qa/model-a-room.md).
        if let Some(path) = std::env::var_os("PLANYRITH_QA_OUT") {
            shell
                .app
                .session
                .save(Some(std::path::Path::new(&path)))
                .unwrap();
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
        // ...and a typed value replaces it, in the same direction (up,
        // although the ground face's normal points down).
        shell.type_text("2500", true);
        assert_eq!(shell.app.session.doc().model.counts().faces, 6);
        let b = shell.model_bounds();
        assert!(
            b.min.z.abs() < 1e-6 && (b.max.z - 2500.0).abs() < 1e-6,
            "{b:?}"
        );
        // Again (still one undo step).
        shell.type_text("3000", true);
        assert!((shell.model_bounds().max.z - 3000.0).abs() < 1e-6);
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

        // Backspace removes the last point of a chain with its segment.
        type_text(&mut app, "[0,0,0]");
        type_text(&mut app, "[0,1000,0]");
        type_text(&mut app, "[0,1000,500]");
        assert_eq!(segment_lengths(&app), vec![500.0, 1000.0, 2500.0]);
        app.key(Key::Backspace);
        assert_eq!(segment_lengths(&app), vec![1000.0, 2500.0]);
        type_text(&mut app, "[500,1000,0]");
        assert_eq!(
            segment_lengths(&app),
            vec![500.0, 1000.0, 2500.0],
            "the chain goes on from the point before"
        );
        app.key(Key::Escape);

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
