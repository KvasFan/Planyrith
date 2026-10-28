//! Implements docs/10 §2–§4: Console, Extension Manager and property forms.

use crate::{platform, session::Session};
use egui::{Context, Ui};
use planyrith_core::commands::{CommandError, CommandRegistry, GroupToken};
use planyrith_core::io::{Storage, StorageKey};
use planyrith_core::scripting::plugins::{self, Plugin};
use serde_json::{Value, json};

/// Plugin storage root shared with the CLI on desktop, IndexedDB on web.
pub fn root() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        "/plugins".into()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::path::PathBuf;
        if let Ok(root) = std::env::var("PLANYRITH_PLUGIN_DIR") {
            return root;
        }
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from(".planyrith"));
        base.join("planyrith/plugins")
            .to_string_lossy()
            .replace('\\', "/")
    }
}
/// Registry used by GUI sessions and previews.
pub fn registry(storage: &dyn Storage) -> Result<CommandRegistry, CommandError> {
    plugins::registry(storage, &root())
}

/// User interface state; all document changes dispatch registered commands.
pub struct Extensions {
    pub console_open: bool,
    pub manager_open: bool,
    pub soften_open: bool,
    toolbars_open: bool,
    source: String,
    lines: Vec<String>,
    plugins: Option<Vec<Plugin>>,
    form: Option<(usize, usize, Value)>,
    panel: Option<(usize, usize, Value)>,
    callbacks: Vec<(String, Value)>,
    error: Option<String>,
    picker: Option<egui_file_dialog::FileDialog>,
    #[cfg(target_arch = "wasm32")]
    uploads: platform::web::Uploads,
    angle: f64,
    smooth: bool,
    coplanar: bool,
    soften_group: Option<GroupToken>,
    #[cfg(not(target_arch = "wasm32"))]
    attach_fields: (String, String, String),
}
impl Default for Extensions {
    fn default() -> Self {
        Self {
            console_open: false,
            manager_open: false,
            soften_open: false,
            toolbars_open: false,
            source: String::new(),
            lines: Vec::new(),
            plugins: None,
            form: None,
            panel: None,
            callbacks: Vec::new(),
            error: None,
            picker: None,
            #[cfg(target_arch = "wasm32")]
            uploads: Default::default(),
            angle: 20.0,
            smooth: true,
            coplanar: false,
            soften_group: None,
            #[cfg(not(target_arch = "wasm32"))]
            attach_fields: (String::new(), String::new(), "d1".into()),
        }
    }
}
impl Extensions {
    fn load(&mut self) {
        if self.plugins.is_some() {
            return;
        }
        let storage = platform::storage();
        match plugins::bundled().and_then(|bundled| {
            let mut all = std::collections::BTreeMap::new();
            for plugin in bundled
                .into_iter()
                .chain(plugins::installed(storage.as_ref(), &root())?)
            {
                all.insert(plugin.manifest.plugin.id.clone(), plugin);
            }
            Ok(all.into_values().collect())
        }) {
            Ok(plugins) => self.plugins = Some(plugins),
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    /// Extension menus are generated from installed manifests.
    pub fn menu(&mut self, ui: &mut Ui) {
        self.load();
        ui.checkbox(&mut self.toolbars_open, "Extension toolbars");
        if ui.button("Extension Manager…").clicked() {
            self.manager_open = true;
            ui.close();
        }
        if ui.button("Console").clicked() {
            self.console_open = true;
            ui.close();
        }
        ui.separator();
        if let Some(plugins) = &self.plugins {
            for (i, plugin) in plugins.iter().enumerate() {
                let disabled = platform::storage()
                    .exists(&StorageKey::new(format!(
                        "{}/{}.disabled",
                        root(),
                        plugin.manifest.plugin.id
                    )))
                    .unwrap_or(false);
                ui.add_enabled_ui(!disabled, |ui| {
                    ui.menu_button(&plugin.manifest.plugin.name, |ui| {
                        for (j, tool) in plugin.manifest.tools.iter().enumerate() {
                            if ui.button(&tool.label).clicked() {
                                self.form = Some((i, j, defaults(plugin, j)));
                                ui.close();
                            }
                        }
                        for menu in &plugin.manifest.menus {
                            let tool = plugin
                                .manifest
                                .tools
                                .iter()
                                .position(|t| t.on_execute == menu.on_click);
                            if ui
                                .button(menu.path.rsplit('/').next().unwrap_or(&menu.path))
                                .clicked()
                            {
                                if let Some(j) = tool {
                                    self.form = Some((i, j, defaults(plugin, j)));
                                } else if let Ok(source) = plugin.callback_source(&menu.on_click) {
                                    self.callbacks.push((source, json!({})));
                                }
                                ui.close();
                            }
                        }
                        for (j, panel) in plugin.manifest.panels.iter().enumerate() {
                            if ui.button(&panel.name).clicked() {
                                let values = plugin
                                    .form(&panel.form)
                                    .map(|form| form_defaults(&form))
                                    .unwrap_or(json!({}));
                                self.panel = Some((i, j, values));
                                ui.close();
                            }
                        }
                    })
                });
            }
        }
    }
    fn installed(&mut self, bytes: &[u8], session: &mut Session) {
        let storage = platform::storage();
        match plugins::install(storage.as_ref(), &root(), bytes)
            .map_err(|e| e.to_string())
            .and_then(|_| session.reload_extensions())
        {
            Ok(()) => {
                self.plugins = None;
                self.load();
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
    /// Renders the Console, manager, forms and Soften/Smooth window.
    pub fn ui(&mut self, ctx: &Context, session: &mut Session) {
        self.load();
        for (source, args) in self.callbacks.drain(..) {
            if let Err(e) = session.execute("script-eval", json!({"source":source,"args":args})) {
                self.error = Some(e);
            }
        }
        if self.toolbars_open
            && let Some(plugins) = &self.plugins
        {
            for (i, plugin) in plugins.iter().enumerate() {
                for toolbar in &plugin.manifest.toolbars {
                    if !session.engine.registry().names().any(|name| {
                        name.starts_with(&format!("ext:{}/", plugin.manifest.plugin.id))
                    }) {
                        continue;
                    }
                    egui::Window::new(&toolbar.name)
                        .id(egui::Id::new(("extension-toolbar", i, &toolbar.name)))
                        .default_width(220.0)
                        .show(ctx, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                for item in &toolbar.items {
                                    if let Some(name) = item.strip_prefix("tools:")
                                        && let Some(j) = plugin
                                            .manifest
                                            .tools
                                            .iter()
                                            .position(|t| t.name == name)
                                    {
                                        if ui.button(&plugin.manifest.tools[j].label).clicked() {
                                            self.form = Some((i, j, defaults(plugin, j)));
                                        }
                                    } else if let Some(path) = item.strip_prefix("menus:")
                                        && let Some(menu) =
                                            plugin.manifest.menus.iter().find(|m| m.path == path)
                                        && ui
                                            .button(path.rsplit('/').next().unwrap_or(path))
                                            .clicked()
                                        && let Ok(source) = plugin.callback_source(&menu.on_click)
                                    {
                                        self.callbacks.push((source, json!({})));
                                    }
                                }
                            });
                        });
                }
            }
        }
        if let Some((i, j, mut values)) = self.panel.take() {
            let mut open = true;
            if let Some(plugin) = self.plugins.as_ref().and_then(|plugins| plugins.get(i)) {
                let panel = &plugin.manifest.panels[j];
                if let Ok(form) = plugin.form(&panel.form) {
                    egui::Window::new(&panel.name)
                        .id(egui::Id::new("extension-panel"))
                        .open(&mut open)
                        .show(ctx, |ui| {
                            fields(ui, &form, &mut values, session);
                            if ui.button("Apply").clicked()
                                && let Ok(source) = plugin.callback_source(&panel.on_change)
                            {
                                self.callbacks.push((source, values.clone()));
                            }
                        });
                }
            }
            if open {
                self.panel = Some((i, j, values));
            }
        }
        let mut open = self.console_open;
        egui::Window::new("Console")
            .open(&mut open)
            .default_width(600.0)
            .show(ctx, |ui| {
                ui.weak("Lengths: mm. Angles: radians. Ctrl+Enter runs one undoable script.");
                ui.add(
                    egui::TextEdit::multiline(&mut self.source)
                        .code_editor()
                        .desired_rows(8)
                        .desired_width(f32::INFINITY),
                );
                let run = ui.button("Run").clicked()
                    || ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
                if run {
                    match session.execute("script-eval", json!({"source":self.source,"args":{}})) {
                        Ok(output) => {
                            if let Some(lines) = output["output"].as_array() {
                                self.lines.extend(
                                    lines.iter().filter_map(Value::as_str).map(str::to_string),
                                );
                            }
                            self.lines.push(output["value"].to_string());
                        }
                        Err(e) => self.lines.push(e),
                    }
                    if self.lines.len() > 500 {
                        self.lines.drain(..self.lines.len() - 500);
                    }
                }
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.lines {
                            ui.monospace(line);
                        }
                    });
            });
        self.console_open = open;
        let mut open = self.manager_open;
        egui::Window::new("Extension Manager")
            .open(&mut open)
            .default_width(500.0)
            .show(ctx, |ui| {
                if ui.button("Install .plyrx…").clicked() {
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        let mut picker = platform::file_dialog()
                            .add_file_filter_extensions("Planyrith extension", vec!["plyrx"]);
                        picker.pick_file();
                        self.picker = Some(picker);
                    }
                    #[cfg(target_arch = "wasm32")]
                    platform::web::upload(".plyrx", false, self.uploads.clone());
                }
                if let Some(plugins) = &self.plugins {
                    for plugin in plugins {
                        let info = &plugin.manifest.plugin;
                        ui.group(|ui| {
                            ui.strong(format!("{} {}", info.name, info.version));
                            ui.label(&info.description);
                            ui.small(format!("{} · {}", info.id, info.license));
                            let storage = platform::storage();
                            let key = StorageKey::new(format!("{}/{}.disabled", root(), info.id));
                            let mut enabled = !storage.exists(&key).unwrap_or(false);
                            if ui.checkbox(&mut enabled, "Enabled").changed() {
                                let result = if enabled {
                                    storage.delete(&key)
                                } else {
                                    storage.write(&key, b"disabled")
                                };
                                if let Err(e) = result
                                    .map_err(|e| e.to_string())
                                    .and_then(|_| session.reload_extensions())
                                {
                                    self.error = Some(e);
                                }
                            }
                            ui.small(format!(
                                "{} tools · {} menus · {} panels",
                                plugin.manifest.tools.len(),
                                plugin.manifest.menus.len(),
                                plugin.manifest.panels.len()
                            ));
                        });
                    }
                }
                if let Some(error) = &self.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                #[cfg(not(target_arch = "wasm32"))]
                ui.collapsing("Attach to local server", |ui| {
                    ui.label("Local TCP port or Unix socket");
                    ui.text_edit_singleline(&mut self.attach_fields.0);
                    ui.label("Token (TCP)");
                    ui.add(egui::TextEdit::singleline(&mut self.attach_fields.1).password(true));
                    ui.label("Document id (d1 for serve -f; empty creates a document)");
                    ui.text_edit_singleline(&mut self.attach_fields.2);
                    if session.dirty() {
                        ui.weak("Save the current document before attaching.");
                    }
                    if ui
                        .add_enabled(!session.dirty(), egui::Button::new("Attach"))
                        .clicked()
                    {
                        match Session::attach(
                            &self.attach_fields.0,
                            &self.attach_fields.1,
                            &self.attach_fields.2,
                        ) {
                            Ok(attached) => {
                                session.drop_autosave();
                                *session = attached;
                                self.error = None;
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                    if session.is_attached() {
                        ui.label("Connected. Commands, undo, redo and saving run on the server.");
                    }
                });
            });
        self.manager_open = open;
        let picked = self.picker.as_mut().and_then(|picker| {
            picker.update(ctx);
            picker.take_picked()
        });
        if let Some(path) = picked {
            match platform::storage().read(&StorageKey::new(path.to_string_lossy().to_string())) {
                Ok(bytes) => self.installed(&bytes, session),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let uploads = self.uploads.borrow_mut().drain(..).collect::<Vec<_>>();
            for (_, bytes) in uploads {
                self.installed(&bytes, session);
            }
        }
        if let Some((i, j, mut params)) = self.form.take() {
            let mut open = true;
            let mut error = None;
            if let Some(plugin) = self.plugins.as_ref().and_then(|plugins| plugins.get(i)) {
                let tool = &plugin.manifest.tools[j];
                egui::Window::new(&tool.label)
                    .id(egui::Id::new("extension-form"))
                    .open(&mut open)
                    .show(ctx, |ui| {
                        let form = tool.form.as_ref().and_then(|path| plugin.form(path).ok());
                        if let Some(form) = form {
                            fields(ui, &form, &mut params, session);
                        } else {
                            ui.label("Parameters (JSON)");
                            let mut text = params.to_string();
                            if ui.text_edit_multiline(&mut text).changed() {
                                match serde_json::from_str(&text) {
                                    Ok(value) => params = value,
                                    Err(e) => error = Some(e.to_string()),
                                }
                            }
                        }
                        for step in &tool.steps {
                            ui.weak(&step.prompt);
                        }
                        if ui.button("Apply").clicked() {
                            let name = format!("ext:{}/{}", plugin.manifest.plugin.id, tool.name);
                            if let Err(e) = session.execute(&name, params.clone()) {
                                error = Some(e);
                            }
                        }
                        if let Some(error) = self.error.as_ref().or(error.as_ref()) {
                            ui.colored_label(egui::Color32::LIGHT_RED, error);
                        }
                    });
            }
            if let Some(error) = error {
                self.error = Some(error);
            }
            if open {
                self.form = Some((i, j, params));
            }
        }
        let mut open = self.soften_open;
        egui::Window::new("Soften / Smooth").open(&mut open).show(ctx,|ui| {
            let response=ui.add(egui::Slider::new(&mut self.angle,0.0..=180.0).text("Angle (degrees)"));
            let changed=response.changed() | ui.checkbox(&mut self.smooth,"Smooth shading").changed() | ui.checkbox(&mut self.coplanar,"Soften coplanar edges").changed();
            ui.weak("Applies to selected edges and faces. Undo restores the previous flags.");
            if (changed || ui.button("Apply to selection").clicked()) && !session.engine.selection().is_empty() {
                if self.soften_group.is_none() {self.soften_group=Some(session.engine.begin_group("Soften / Smooth"));}
                if let Err(e)=session.execute("soften-smooth",json!({"targets":"selection","angle":self.angle.to_radians(),"smooth":self.smooth,"soften_coplanar":self.coplanar})) {self.error=Some(e);}
            }
            if !response.dragged() && let Some(group)=self.soften_group.take() {let _=session.engine.end_group(group);}
        });
        if !open && let Some(group) = self.soften_group.take() {
            let _ = session.engine.end_group(group);
        }
        self.soften_open = open;
    }
}

fn defaults(plugin: &Plugin, index: usize) -> Value {
    let tool = &plugin.manifest.tools[index];
    let mut params = json!({});
    if let Some(form) = tool.form.as_ref().and_then(|f| plugin.form(f).ok())
        && let Some(fields) = form["fields"].as_array()
    {
        for field in fields {
            if let Some(key) = field["key"].as_str() {
                params[key] = field["default"].clone();
            }
        }
    }
    params
}

fn form_defaults(form: &Value) -> Value {
    let mut params = json!({});
    if let Some(fields) = form["fields"].as_array() {
        for field in fields {
            if let Some(key) = field["key"].as_str() {
                params[key] = field["default"].clone();
            }
        }
    }
    params
}

fn fields(ui: &mut Ui, form: &Value, params: &mut Value, session: &Session) {
    let Some(fields) = form["fields"].as_array() else {
        return;
    };
    for field in fields {
        let Some(key) = field["key"].as_str() else {
            continue;
        };
        if params.get(key).is_none() {
            params[key] = field["default"].clone();
        }
        let label = field["label"].as_str().unwrap_or(key);
        ui.horizontal(|ui| match field["type"].as_str().unwrap_or("text") {
            "bool" => {
                let mut value = params[key].as_bool().unwrap_or(false);
                if ui.checkbox(&mut value, label).changed() {
                    params[key] = json!(value);
                }
            }
            "number" | "length" | "angle" | "int" => {
                ui.label(label);
                let number = |value: &Value| {
                    value.as_f64().or_else(|| {
                        value.as_str().and_then(|s| {
                            if field["type"] == "angle" {
                                planyrith_core::units::parse_angle(s)
                                    .ok()
                                    .map(|v| v.as_radians())
                            } else {
                                planyrith_core::units::parse_length(
                                    s,
                                    planyrith_core::units::LengthUnit::Millimeter,
                                )
                                .ok()
                                .map(|v| v.as_mm())
                            }
                        })
                    })
                };
                let mut n = number(&params[key]).unwrap_or(0.0);
                if params[key].is_string() {
                    params[key] = json!(n);
                }
                let mut widget = egui::DragValue::new(&mut n).range(
                    number(&field["min"]).unwrap_or(-1e9)..=number(&field["max"]).unwrap_or(1e9),
                );
                if field["type"] == "length" {
                    widget = widget.suffix(" mm");
                } else if field["type"] == "angle" {
                    widget = widget.suffix(" rad");
                }
                if ui.add(widget).changed() {
                    params[key] = if field["type"] == "int" {
                        json!(n.round() as i64)
                    } else {
                        json!(n)
                    };
                }
            }
            "choice" | "material" => {
                ui.label(label);
                let mut selected = params[key].as_str().unwrap_or("").to_string();
                let options = if field["type"] == "material" {
                    session
                        .doc()
                        .materials
                        .iter()
                        .map(|(_, m)| m.name.clone())
                        .collect::<Vec<_>>()
                } else {
                    field["options"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                egui::ComboBox::from_id_salt(key)
                    .selected_text(&selected)
                    .show_ui(ui, |ui| {
                        for option in options {
                            ui.selectable_value(&mut selected, option.clone(), option);
                        }
                    });
                params[key] = json!(selected);
            }
            "color" => {
                ui.label(label);
                let text = params[key]
                    .as_str()
                    .unwrap_or("#FFFFFFFF")
                    .trim_start_matches('#');
                let rgba = u32::from_str_radix(text, 16).unwrap_or(u32::MAX);
                let rgba = if text.len() == 6 {
                    (rgba << 8) | 255
                } else {
                    rgba
                };
                let [r, g, b, a] = rgba.to_be_bytes();
                let mut color = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
                if ui.color_edit_button_srgba(&mut color).changed() {
                    let [r, g, b, a] = color.to_array();
                    params[key] = json!(format!("#{r:02X}{g:02X}{b:02X}{a:02X}"));
                }
            }
            "entity-pick" => {
                ui.label(label);
                let mut text = params[key].as_str().unwrap_or("").to_string();
                if ui.text_edit_singleline(&mut text).changed() {
                    params[key] = json!(text);
                }
                if ui.button("Use selection").clicked()
                    && let Some(path) = session.engine.selection().iter().next()
                {
                    params[key] = json!(path.to_string());
                }
            }
            _ => {
                ui.label(label);
                let mut text = params[key].as_str().unwrap_or("").to_string();
                if ui.text_edit_multiline(&mut text).changed() {
                    params[key] = json!(text);
                }
            }
        });
    }
}
