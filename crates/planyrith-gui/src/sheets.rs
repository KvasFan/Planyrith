//! Implements docs/15 §4: a paper-space workspace. Every edit dispatches a
//! sheet command; projection, measurement, layout and export live in the core.

use crate::{platform, session::Session};
use egui::{Color32, Pos2, Sense, StrokeKind, Vec2};
#[cfg(target_arch = "wasm32")]
use planyrith_core::io::StorageKey;
use planyrith_core::sheets::layout::{self, Drawing, Graphic};
use planyrith_core::sheets::{self, Entry, ItemId, Page, Rect, RenderMode, SheetAnchor, SheetItem};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    View,
    Dimension,
    Text,
    Table,
    Image,
    Rectangle,
    Ellipse,
    Polyline,
    TitleBlock,
    Label,
}
impl Tool {
    fn label(self) -> &'static str {
        match self {
            Self::Select => "Select / Move",
            Self::View => "Model View",
            Self::Dimension => "Dimension",
            Self::Text => "Text",
            Self::Table => "Table",
            Self::Image => "Image",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Polyline => "Polyline",
            Self::TitleBlock => "Title Block",
            Self::Label => "Bound Label",
        }
    }
}
/// State of the Sheets workspace; persistent content belongs to Session's document.
pub struct Sheets {
    /// Whether the paper workspace is active.
    pub open: bool,
    page: String,
    rename: String,
    selected: Option<ItemId>,
    tool: Tool,
    first: Option<[f64; 2]>,
    points: Vec<[f64; 2]>,
    zoom: f32,
    pan: Vec2,
    grid: f64,
    snap: bool,
    scale: String,
    scene: String,
    render: RenderMode,
    text: String,
    image_path: String,
    paper: String,
    paper_key: Option<(String, sheets::Paper)>,
    margin: f64,
    settings_draft: Option<sheets::SheetSettings>,
    settings_key: Option<sheets::SheetSettings>,
    landscape: bool,
    output: String,
    dpi: u32,
    all_pages: bool,
    draft: Option<Entry>,
    draft_key: Option<(String, ItemId, u64)>,
    cache: Option<(String, u64, Drawing)>,
    textures: BTreeMap<String, egui::TextureHandle>,
    drag: Option<([f64; 2], ItemId)>,
    editing: Option<(String, ItemId)>,
    error: Option<String>,
    picker: Option<egui_file_dialog::FileDialog>,
    #[cfg(target_arch = "wasm32")]
    uploads: platform::web::Uploads,
}
impl Default for Sheets {
    fn default() -> Self {
        Self {
            open: false,
            page: String::new(),
            rename: String::new(),
            selected: None,
            tool: Tool::Select,
            first: None,
            points: Vec::new(),
            zoom: 1.0,
            pan: Vec2::ZERO,
            grid: 1.0,
            snap: true,
            scale: "1:50".into(),
            scene: String::new(),
            render: RenderMode::Vector,
            text: "Note".into(),
            image_path: String::new(),
            paper: "A4".into(),
            paper_key: None,
            margin: 10.0,
            settings_draft: None,
            settings_key: None,
            landscape: true,
            output: "sheets.pdf".into(),
            dpi: 300,
            all_pages: true,
            draft: None,
            draft_key: None,
            cache: None,
            textures: BTreeMap::new(),
            drag: None,
            editing: None,
            error: None,
            picker: None,
            #[cfg(target_arch = "wasm32")]
            uploads: Default::default(),
        }
    }
}
fn color(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}
fn frame_ui(ui: &mut egui::Ui, r: &mut Rect) {
    egui::Grid::new("sheet-frame-fields").show(ui, |ui| {
        for (label, value) in [
            ("X (mm)", &mut r.x),
            ("Y (mm)", &mut r.y),
            ("Width (mm)", &mut r.width),
            ("Height (mm)", &mut r.height),
        ] {
            ui.label(label);
            ui.add(egui::DragValue::new(value).speed(0.5));
            ui.end_row();
        }
    });
}
impl Sheets {
    fn execute(&mut self, session: &mut Session, name: &str, params: Value) -> Option<Value> {
        match session.execute(name, params) {
            Ok(out) => {
                self.error = None;
                self.cache = None;
                self.draft_key = None;
                Some(out)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }
    fn ensure_page(&mut self, session: &mut Session) -> bool {
        if session.doc().sheets.page(&self.page).is_some() {
            return true;
        }
        if let Some(page) = session.doc().sheets.pages.first() {
            self.page = page.name.clone();
            return true;
        }
        self.add_page(session);
        session.doc().sheets.page(&self.page).is_some()
    }
    fn add_page(&mut self, session: &mut Session) {
        let mut i = 1;
        let name = loop {
            let name = format!("A{:03}", 100 + i);
            if session.doc().sheets.page(&name).is_none() {
                break name;
            }
            i += 1;
        };
        if self
            .execute(
                session,
                "sheet-add-page",
                json!({"page":name,"paper":self.paper,"landscape":self.landscape,"margin":self.margin}),
            )
            .is_some()
        {
            self.rename = name.clone();
            self.page = name;
            self.selected = None;
        }
    }
    /// Adds a linked view of the current model and activates the paper workspace.
    pub fn send_to_sheets(&mut self, session: &mut Session) {
        if !self.ensure_page(session) {
            return;
        }
        let scene = if self.scene.is_empty() {
            None
        } else {
            Some(self.scene.clone())
        };
        let params = json!({"page":self.page,"camera":session.doc().camera,"scene":scene,"scale":self.scale,"render":self.render,"frame":Rect::default()});
        if let Some(out) = self.execute(session, "sheet-add-view", params) {
            self.selected = out["id"].as_str().map(ItemId::named);
            self.open = true;
        }
    }
    /// Cancels pending paper input after undo or redo.
    pub(crate) fn cancel_placement(&mut self) {
        self.first = None;
        self.points.clear();
        self.drag = None;
        self.tool = Tool::Select;
        self.cache = None;
        self.draft_key = None;
    }
    /// Workspace tabs and the model-view camera editing transition.
    pub fn tabs(&mut self, ui: &mut egui::Ui, session: &mut Session) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.open, false, "Model");
            ui.selectable_value(&mut self.open, true, "Sheets");
            if let Some((page, id)) = self.editing.clone() {
                ui.separator();
                ui.label("Editing sheet view camera");
                if ui.button("Use this view").clicked()
                    && let Some(mut entry) = session
                        .doc()
                        .sheets
                        .page(&page)
                        .and_then(|p| p.items.iter().find(|e| e.id == id))
                        .cloned()
                {
                    if let SheetItem::ModelView {
                        camera,
                        camera_override,
                        frozen,
                        ..
                    } = &mut entry.item
                    {
                        *camera = session.doc().camera;
                        *camera_override = true;
                        *frozen = None;
                    }
                    if self
                        .execute(
                            session,
                            "sheet-edit-item",
                            json!({"page":page,"id":id,"item":entry.item}),
                        )
                        .is_some()
                    {
                        self.editing = None;
                        self.open = true;
                    }
                }
                if ui.button("Cancel view edit").clicked() {
                    self.editing = None;
                    self.open = true;
                }
            }
        });
    }
    /// Draws the paper editor and collects parameters for core commands.
    pub fn ui(&mut self, ui: &mut egui::Ui, session: &mut Session) {
        ui.horizontal_wrapped(|ui| {
            let names = session
                .doc()
                .sheets
                .pages
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>();
            if !names.contains(&self.page)
                && let Some(name) = names.first()
            {
                self.page = name.clone();
                self.selected = None;
            }
            egui::ComboBox::from_id_salt("sheet-pages")
                .selected_text(if self.page.is_empty() {
                    "No pages"
                } else {
                    &self.page
                })
                .show_ui(ui, |ui| {
                    for name in names {
                        if ui
                            .selectable_value(&mut self.page, name.clone(), &name)
                            .changed()
                        {
                            self.selected = None;
                            self.cache = None;
                        }
                    }
                });
            if ui.button("Add Page").clicked() {
                self.add_page(session);
            }
            if ui
                .add_enabled(!self.page.is_empty(), egui::Button::new("Remove Page"))
                .clicked()
            {
                let page = self.page.clone();
                self.execute(session, "sheet-remove-page", json!({"page":page}));
                self.page.clear();
                self.selected = None;
            }
            ui.separator();
            egui::ComboBox::from_id_salt("sheet-tool")
                .selected_text(self.tool.label())
                .show_ui(ui, |ui| {
                    for tool in [
                        Tool::Select,
                        Tool::View,
                        Tool::Dimension,
                        Tool::Text,
                        Tool::Table,
                        Tool::Image,
                        Tool::Rectangle,
                        Tool::Ellipse,
                        Tool::Polyline,
                        Tool::TitleBlock,
                        Tool::Label,
                    ] {
                        if ui
                            .selectable_value(&mut self.tool, tool, tool.label())
                            .changed()
                        {
                            self.first = None;
                            self.points.clear();
                        }
                    }
                });
            if ui.button("Fit").clicked() {
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
            }
            ui.checkbox(&mut self.snap, "Snap");
            ui.add(
                egui::DragValue::new(&mut self.grid)
                    .range(0.1..=100.0)
                    .suffix(" mm grid"),
            );
        });
        if let Some(error) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        let page = session.doc().sheets.page(&self.page).cloned();
        let Some(page) = page else {
            ui.label("Add a page or use File › Send to Sheets to place the current model view.");
            return;
        };
        ui.separator();
        let available = ui.available_size();
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(220.0, available.y),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_max_width(220.0);
                    egui::ScrollArea::vertical()
                        .id_salt("sheet-properties-scroll")
                        .show(ui, |ui| self.properties(ui, session, &page));
                },
            );
            ui.separator();
            self.canvas(ui, session, &page);
        });
        if let Some(picker) = &mut self.picker {
            picker.update(ui.ctx());
            if let Some(path) = picker.take_picked() {
                self.image_path = path.to_string_lossy().into_owned();
            }
        }
        #[cfg(target_arch = "wasm32")]
        for (name, bytes) in self.uploads.borrow_mut().drain(..) {
            let path = format!(
                "/uploads/{}",
                planyrith_core::sheets::export::file_name(&name)
            );
            match platform::storage().write(&StorageKey::new(&path), &bytes) {
                Ok(()) => self.image_path = path,
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }
    fn properties(&mut self, ui: &mut egui::Ui, session: &mut Session, page: &Page) {
        let paper_key = (page.name.clone(), page.paper.clone());
        if self.paper_key.as_ref() != Some(&paper_key) {
            self.landscape = page.paper.width > page.paper.height;
            self.margin = page.paper.margin;
            self.paper = ["A0", "A1", "A2", "A3", "A4", "Letter", "Tabloid"]
                .into_iter()
                .find(|name| {
                    sheets::Paper::named(name, self.landscape).is_some_and(|p| {
                        (p.width - page.paper.width).abs() < 1e-6
                            && (p.height - page.paper.height).abs() < 1e-6
                    })
                })
                .map_or_else(
                    || format!("{}x{}", page.paper.width, page.paper.height),
                    str::to_string,
                );
            self.paper_key = Some(paper_key);
        }
        ui.strong("Page");
        if self.rename.is_empty() {
            self.rename = page.name.clone();
        }
        ui.text_edit_singleline(&mut self.rename);
        if ui.button("Rename Page").clicked() {
            let name = self.rename.clone();
            if self
                .execute(
                    session,
                    "sheet-rename-page",
                    json!({"page":page.name,"name":name}),
                )
                .is_some()
            {
                self.page = name;
            }
        }
        ui.horizontal(|ui| {
            let index = session
                .doc()
                .sheets
                .pages
                .iter()
                .position(|p| p.name == page.name)
                .unwrap_or(0);
            if ui
                .add_enabled(index > 0, egui::Button::new("Earlier"))
                .clicked()
            {
                self.execute(
                    session,
                    "sheet-move-page",
                    json!({"page":page.name,"index":index-1}),
                );
            }
            if ui
                .add_enabled(
                    index + 1 < session.doc().sheets.pages.len(),
                    egui::Button::new("Later"),
                )
                .clicked()
            {
                self.execute(
                    session,
                    "sheet-move-page",
                    json!({"page":page.name,"index":index+1}),
                );
            }
        });
        ui.strong("Paper");
        ui.horizontal(|ui| {
            ui.label("Size");
            egui::ComboBox::from_id_salt("sheet-paper")
                .selected_text(&self.paper)
                .show_ui(ui, |ui| {
                    for p in ["A0", "A1", "A2", "A3", "A4", "Letter", "Tabloid"] {
                        ui.selectable_value(&mut self.paper, p.into(), p);
                    }
                });
        });
        ui.text_edit_singleline(&mut self.paper)
            .on_hover_text("A0–A4, Letter, Tabloid or widthxheight in millimeters");
        ui.checkbox(&mut self.landscape, "Landscape");
        ui.add(
            egui::DragValue::new(&mut self.margin)
                .range(0.0..=1000.0)
                .suffix(" mm margin"),
        );
        if ui.button("Apply Paper").clicked() {
            self.execute(
                session,
                "sheet-set-paper",
                json!({"page":page.name,"paper":self.paper,"landscape":self.landscape,"margin":self.margin}),
            );
        }
        ui.separator();
        ui.strong("Place items");
        ui.label("Drag a frame or pick two corners. Dimensions: pick two endpoints. Polyline: double-click to finish.");
        ui.label("Scale");
        ui.text_edit_singleline(&mut self.scale);
        egui::ComboBox::from_id_salt("sheet-scene")
            .selected_text(if self.scene.is_empty() {
                "Current camera"
            } else {
                &self.scene
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.scene, String::new(), "Current camera");
                for (_, scene) in session.doc().scenes.iter() {
                    ui.selectable_value(&mut self.scene, scene.name.clone(), &scene.name);
                }
            });
        render_ui(ui, &mut self.render);
        ui.label("Text / label binding");
        ui.text_edit_multiline(&mut self.text);
        if self.tool == Tool::Image {
            ui.label("Image");
            ui.text_edit_singleline(&mut self.image_path);
            if ui.button("Choose Image…").clicked() {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let mut picker = platform::file_dialog();
                    picker.pick_file();
                    self.picker = Some(picker);
                }
                #[cfg(target_arch = "wasm32")]
                platform::web::upload(".png,.jpg,.jpeg,.webp", false, self.uploads.clone());
            }
        }
        ui.separator();
        ui.strong("Layers");
        for layer in &page.layers {
            let mut visible = layer.visible;
            let mut locked = layer.locked;
            ui.horizontal(|ui|{let changed=ui.checkbox(&mut visible,&layer.name).changed()|ui.checkbox(&mut locked,"Lock").changed();if changed{self.execute(session,"sheet-layer-set",json!({"page":page.name,"layer":layer.name,"visible":visible,"locked":locked}));}});
        }
        if ui.button("Add Notes Layer").clicked() {
            self.execute(
                session,
                "sheet-layer-set",
                json!({"page":page.name,"layer":"Notes"}),
            );
        }
        ui.separator();
        if let Some(id) = self.selected.clone()
            && let Some(entry) = page.items.iter().find(|e| e.id == id)
        {
            ui.strong(format!("Item {}", id.as_str()));
            let key = (page.name.clone(), id.clone(), session.engine.generation());
            if self.draft_key.as_ref() != Some(&key) {
                self.draft = Some(entry.clone());
                self.draft_key = Some(key);
            }
            if let Some(mut draft) = self.draft.take() {
                let locked = page
                    .layers
                    .iter()
                    .any(|l| l.name == entry.layer && l.locked);
                ui.add_enabled_ui(!locked, |ui| {
                    egui::ComboBox::from_id_salt("sheet-item-layer")
                        .selected_text(&draft.layer).show_ui(ui, |ui| {
                            for layer in &page.layers {
                                ui.selectable_value(&mut draft.layer, layer.name.clone(), &layer.name);
                            }
                        });
                    edit_item(ui, session, &mut draft.item, page);
                    if ui.button("Apply Properties").clicked() {
                        let params=json!({"page":page.name,"id":id,"layer":draft.layer,"item":draft.item});
                        self.execute(session,"sheet-edit-item",params);
                    }
                    if matches!(draft.item,SheetItem::ModelView{..}) {
                        if ui.button("Edit Camera in Model").clicked() {
                            self.begin_view_edit(session,page,&id,&draft.item);
                        }
                        ui.horizontal(|ui| {
                            let freeze=ui.button("Freeze").clicked();
                            let follow=ui.button("Follow Model").clicked();
                            if freeze||follow {
                                self.execute(session,"sheet-freeze-view",json!({"page":page.name,"id":id,"frozen":freeze,"dpi":self.dpi}));
                            }
                        });
                    }
                    if let Ok(bounds)=layout::item_bounds(session.doc(),page,&entry.item) {
                        ui.label("Align to printable area");
                        ui.horizontal(|ui| {
                            for(label,x)in [("Left",page.paper.margin),("Center",(page.paper.width-bounds.width)/2.0),("Right",page.paper.width-page.paper.margin-bounds.width)] {
                                if ui.button(label).clicked() {
                                    self.execute(session,"sheet-move-item",json!({"page":page.name,"id":id,"delta":[x-bounds.x,0]}));
                                }
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Send Back").clicked(){self.execute(session,"sheet-order-item",json!({"page":page.name,"id":id,"index":0}));}
                        if ui.button("Bring Front").clicked(){self.execute(session,"sheet-order-item",json!({"page":page.name,"id":id,"index":page.items.len()-1}));}
                    });
                    if ui.button("Delete Item").clicked() {
                        self.execute(session,"sheet-remove-item",json!({"page":page.name,"id":id}));
                        self.selected=None;
                    }
                });
                self.draft = Some(draft);
            }
        }
        ui.separator();
        self.settings(ui, session);
        ui.separator();
        ui.strong("Export");
        ui.text_edit_singleline(&mut self.output);
        ui.checkbox(&mut self.all_pages, "All pages");
        ui.add(
            egui::DragValue::new(&mut self.dpi)
                .range(36..=1200)
                .suffix(" dpi"),
        );
        if ui.button("Export PDF / SVG / DXF").clicked() {
            let name = self.output.clone();
            let params = json!({"path":name,"page":if self.all_pages{None}else{Some(&page.name)},"dpi":self.dpi});
            if let Some(result) = self.execute(session, "sheet-export", params) {
                #[cfg(target_arch = "wasm32")]
                if let Some(files) = result["files"].as_array() {
                    for file in files {
                        if let Some(name) = file.as_str() {
                            match platform::storage().read(&StorageKey::new(name)) {
                                Ok(bytes) => {
                                    if let Err(e) = platform::web::download(
                                        name.rsplit('/').next().unwrap_or(name),
                                        &bytes,
                                    ) {
                                        self.error = Some(e);
                                    }
                                }
                                Err(e) => self.error = Some(e.to_string()),
                            }
                        }
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                let _ = result;
            }
        }
    }
    fn settings(&mut self, ui: &mut egui::Ui, session: &mut Session) {
        let current = sheets::SheetSettings::from(&session.doc().sheets);
        if self.settings_key.as_ref() != Some(&current) {
            self.settings_draft = Some(current.clone());
            self.settings_key = Some(current);
        }
        if let Some(mut settings) = self.settings_draft.take() {
            ui.collapsing("Sheet pens and title template",|ui|{
                ui.add(egui::DragValue::new(&mut settings.styles.text_height).range(0.5..=100.0).suffix(" mm default text"));
                ui.collapsing("Edges",|ui|stroke_ui(ui,&mut settings.styles.edge));
                ui.collapsing("Profiles",|ui|stroke_ui(ui,&mut settings.styles.profile));
                for(id,tag)in session.doc().tags.iter(){if id==session.doc().tags.untagged(){continue;}
                    ui.push_id(&tag.name,|ui|{
                        let mut custom=settings.styles.pens.contains_key(&tag.name);
                        if ui.checkbox(&mut custom,format!("Pen: {}",tag.name)).changed(){if custom{settings.styles.pens.insert(tag.name.clone(),settings.styles.edge.clone());}else{settings.styles.pens.remove(&tag.name);}}
                        if let Some(pen)=settings.styles.pens.get_mut(&tag.name){stroke_ui(ui,pen);}
                    });
                }
                let mut template=settings.title_block.is_some();
                if ui.checkbox(&mut template,"Reusable title block").changed(){settings.title_block=template.then(||sheets::TitleBlockTemplate{fields:BTreeMap::from([("Project".into(),session.doc().info.title.clone()),("Author".into(),String::new()),("Revision".into(),String::new()),("Date".into(),String::new())])});}
                if let Some(template)=&mut settings.title_block{for(key,value)in &mut template.fields{ui.label(key);ui.text_edit_singleline(value);}}
                if ui.button("Apply Sheet Defaults").clicked(){let params=json!({"styles":settings.styles,"title_block":settings.title_block,"clear_title_block":settings.title_block.is_none()});self.execute(session,"sheet-settings",params);}
            });
            self.settings_draft = Some(settings);
        }
    }
    fn begin_view_edit(
        &mut self,
        session: &mut Session,
        page: &Page,
        id: &ItemId,
        item: &SheetItem,
    ) {
        match layout::view_document(session.doc(), item) {
            Ok((_, camera)) => {
                let height = if let planyrith_core::document::Projection::Parallel { height_mm } =
                    camera.projection
                {
                    height_mm
                } else {
                    1000.0
                };
                let params = json!({"eye":camera.eye,"target":camera.target,"up":camera.up,"height":height,"aspect":camera.aspect});
                if self.execute(session, "camera-set", params).is_some() {
                    self.editing = Some((page.name.clone(), id.clone()));
                    self.open = false;
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    fn drawing(&mut self, session: &Session, page: &Page) -> Option<&Drawing> {
        let generation = session.engine.generation();
        if self
            .cache
            .as_ref()
            .is_none_or(|(name, g, _)| name != &page.name || *g != generation)
        {
            match layout::layout_page(session.doc(), page, 72, None) {
                Ok(drawing) => {
                    self.cache = Some((page.name.clone(), generation, drawing));
                    self.textures.clear();
                }
                Err(e) => {
                    self.error = Some(e.to_string());
                    return None;
                }
            }
        }
        self.cache.as_ref().map(|(_, _, d)| d)
    }
    fn canvas(&mut self, ui: &mut egui::Ui, session: &mut Session, page: &Page) {
        let size = ui.available_size().max(egui::vec2(100.0, 100.0));
        let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = response.rect;
        painter.rect_filled(rect, 0.0, Color32::from_gray(75));
        let fit = ((size.x - 32.0) / page.paper.width as f32)
            .min((size.y - 32.0) / page.paper.height as f32)
            .max(0.01);
        let px = fit * self.zoom;
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                self.zoom = (self.zoom * (scroll * 0.003).exp()).clamp(0.15, 20.0);
                ui.ctx().request_repaint();
            }
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            self.pan += ui.input(|i| i.pointer.delta());
        }
        let origin = rect.center()
            - egui::vec2(
                page.paper.width as f32 * px / 2.0,
                page.paper.height as f32 * px / 2.0,
            )
            + self.pan;
        let screen = |p: [f64; 2]| {
            origin + egui::vec2(p[0] as f32 * px, (page.paper.height - p[1]) as f32 * px)
        };
        let paper = |p: Pos2| {
            [
                (p.x - origin.x) as f64 / f64::from(px),
                page.paper.height - (p.y - origin.y) as f64 / f64::from(px),
            ]
        };
        let paper_rect = egui::Rect::from_min_size(
            origin,
            egui::vec2(page.paper.width as f32 * px, page.paper.height as f32 * px),
        );
        painter.rect_filled(paper_rect, 0.0, Color32::WHITE);
        let page_painter = painter.with_clip_rect(rect.intersect(paper_rect));
        let inset = page.paper.margin;
        page_painter.rect_stroke(
            egui::Rect::from_two_pos(
                screen([inset, inset]),
                screen([page.paper.width - inset, page.paper.height - inset]),
            ),
            0.0,
            egui::Stroke::new(1.0, Color32::LIGHT_GRAY),
            StrokeKind::Inside,
        );
        if self.snap {
            let step = (f64::from(8.0 / px) / self.grid).ceil().max(1.0) * self.grid;
            let count_x = (page.paper.width / step) as usize;
            let count_y = (page.paper.height / step) as usize;
            for i in 0..=count_x.min(1000) {
                let x = i as f64 * step;
                page_painter.line_segment(
                    [screen([x, 0.0]), screen([x, page.paper.height])],
                    egui::Stroke::new(0.3, Color32::from_gray(240)),
                );
            }
            for i in 0..=count_y.min(1000) {
                let y = i as f64 * step;
                page_painter.line_segment(
                    [screen([0.0, y]), screen([page.paper.width, y])],
                    egui::Stroke::new(0.3, Color32::from_gray(240)),
                );
            }
        }
        let drawing = self.drawing(session, page).cloned();
        if let Some(drawing) = drawing {
            for group in drawing.groups {
                let clip = if group.clip.is_empty() {
                    rect.intersect(paper_rect)
                } else {
                    let mut bounds = egui::Rect::NOTHING;
                    for p in &group.clip {
                        bounds.extend_with(screen(*p));
                    }
                    rect.intersect(bounds)
                };
                let paint = page_painter.with_clip_rect(clip);
                for graphic in group.graphics {
                    match graphic {
                        Graphic::Path {
                            points,
                            closed,
                            stroke,
                            fill,
                            ..
                        } => {
                            let points = points.into_iter().map(screen).collect::<Vec<_>>();
                            let pen = egui::Stroke::new(
                                (stroke.width as f32 * px).max(0.5),
                                color(stroke.color),
                            );
                            if let Some(fill) = fill
                                && points.len() >= 3
                            {
                                paint.add(egui::Shape::convex_polygon(
                                    points.clone(),
                                    color(fill),
                                    egui::Stroke::NONE,
                                ));
                            }
                            if closed {
                                paint.add(egui::Shape::closed_line(points, pen));
                            } else {
                                paint.add(egui::Shape::line(points, pen));
                            }
                        }
                        Graphic::Text {
                            text,
                            at,
                            height,
                            color: c,
                        } => {
                            paint.text(
                                screen(at),
                                egui::Align2::LEFT_BOTTOM,
                                text,
                                egui::FontId::proportional(height as f32 * px),
                                color(c),
                            );
                        }
                        Graphic::Image { bytes, frame } => {
                            let key =
                                planyrith_core::document::ImageRef::from_bytes(&bytes).to_hex();
                            if !self.textures.contains_key(&key)
                                && let Ok(image) = image::load_from_memory(&bytes)
                            {
                                let rgba = image.to_rgba8();
                                let img = egui::ColorImage::from_rgba_unmultiplied(
                                    [image.width() as usize, image.height() as usize],
                                    rgba.as_raw(),
                                );
                                self.textures.insert(
                                    key.clone(),
                                    ui.ctx()
                                        .load_texture(&key, img, egui::TextureOptions::LINEAR),
                                );
                            }
                            if let Some(texture) = self.textures.get(&key) {
                                paint.image(
                                    texture.id(),
                                    egui::Rect::from_two_pos(
                                        screen([frame.x, frame.y]),
                                        screen([frame.x + frame.width, frame.y + frame.height]),
                                    ),
                                    egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                        }
                    }
                }
            }
        }
        if let Some(id) = &self.selected
            && let Some(entry) = page.items.iter().find(|e| e.id == *id)
            && let Ok(r) = layout::item_bounds(session.doc(), page, &entry.item)
        {
            page_painter.rect_stroke(
                egui::Rect::from_two_pos(
                    screen([r.x, r.y]),
                    screen([r.x + r.width, r.y + r.height]),
                ),
                0.0,
                egui::Stroke::new(1.5, Color32::from_rgb(40, 120, 240)),
                StrokeKind::Outside,
            );
        }
        if !ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.first = None;
            self.points.clear();
            self.drag = None;
            self.tool = Tool::Select;
        }
        if !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete))
            && let Some(id) = self.selected.clone()
        {
            self.execute(
                session,
                "sheet-remove-item",
                json!({"page":page.name,"id":id}),
            );
            self.selected = None;
        }
        let pointer = response.interact_pointer_pos().map(paper);
        if let Some(mut p) = pointer {
            if self.snap {
                p = p.map(|x| (x / self.grid).round() * self.grid);
            }
            let inside =
                p[0] >= 0.0 && p[1] >= 0.0 && p[0] <= page.paper.width && p[1] <= page.paper.height;
            let hit = |p: [f64; 2]| {
                page.items
                    .iter()
                    .rev()
                    .find(|e| {
                        page.layers.iter().any(|l| l.name == e.layer && l.visible)
                            && layout::item_bounds(session.doc(), page, &e.item).is_ok_and(|r| {
                                Rect {
                                    x: r.x - 1.0,
                                    y: r.y - 1.0,
                                    width: r.width + 2.0,
                                    height: r.height + 2.0,
                                }
                                .contains(p)
                            })
                    })
                    .map(|e| e.id.clone())
            };
            if inside && self.tool == Tool::Select {
                if response.clicked() {
                    self.selected = hit(p);
                }
                if response.drag_started_by(egui::PointerButton::Primary) {
                    let start = ui
                        .input(|i| i.pointer.press_origin())
                        .map(paper)
                        .unwrap_or(p);
                    if let Some(id) = hit(start) {
                        self.selected = Some(id.clone());
                        self.drag = Some((start, id));
                    }
                }
                if response.drag_stopped_by(egui::PointerButton::Primary)
                    && let Some((start, id)) = self.drag.take()
                {
                    let mut delta = [p[0] - start[0], p[1] - start[1]];
                    if self.snap {
                        delta = delta.map(|x| (x / self.grid).round() * self.grid);
                    }
                    self.execute(
                        session,
                        "sheet-move-item",
                        json!({"page":page.name,"id":id,"delta":delta}),
                    );
                }
            } else if inside {
                if response.drag_started_by(egui::PointerButton::Primary) {
                    self.first = ui.input(|i| i.pointer.press_origin()).map(paper);
                }
                if response.drag_stopped_by(egui::PointerButton::Primary)
                    && let Some(start) = self.first.take()
                {
                    self.place(session, page, start, p);
                } else if response.clicked() {
                    if self.tool == Tool::Polyline {
                        self.points.push(p);
                        if response.double_clicked() && self.points.len() >= 2 {
                            let points = std::mem::take(&mut self.points);
                            self.execute(
                                session,
                                "sheet-add-shape",
                                json!({"page":page.name,"points":points}),
                            );
                            self.tool = Tool::Select;
                        }
                    } else if let Some(start) = self.first.take() {
                        self.place(session, page, start, p);
                    } else {
                        self.first = Some(p);
                    }
                }
            }
            if let Some(start) = self.first {
                page_painter.line_segment(
                    [screen(start), screen(p)],
                    egui::Stroke::new(1.0, Color32::LIGHT_BLUE),
                );
            }
            if !self.points.is_empty() {
                let mut points = self.points.iter().copied().map(screen).collect::<Vec<_>>();
                points.push(screen(p));
                page_painter.add(egui::Shape::line(
                    points,
                    egui::Stroke::new(1.0, Color32::LIGHT_BLUE),
                ));
            }
        }
    }
    fn place(&mut self, session: &mut Session, page: &Page, a: [f64; 2], b: [f64; 2]) {
        let frame = Rect {
            x: a[0].min(b[0]),
            y: a[1].min(b[1]),
            width: (a[0] - b[0]).abs().max(10.0),
            height: (a[1] - b[1]).abs().max(10.0),
        };
        let mut params = json!({"page":page.name,"frame":frame});
        let command = match self.tool {
            Tool::View => {
                params["scale"] = json!(self.scale);
                params["scene"] = if self.scene.is_empty() {
                    Value::Null
                } else {
                    json!(self.scene)
                };
                params["render"] = json!(self.render);
                "sheet-add-view"
            }
            Tool::Text => {
                params["text"] = json!(self.text);
                "sheet-add-text"
            }
            Tool::Table => {
                params["rows"] = json!([["Name", "Count"], ["Item", "1"]]);
                "sheet-add-table"
            }
            Tool::Image => {
                params["path"] = json!(self.image_path);
                "sheet-add-image"
            }
            Tool::Rectangle | Tool::Ellipse => {
                params["points"] = json!([a, b]);
                params["shape"] = json!(if self.tool == Tool::Rectangle {
                    "rectangle"
                } else {
                    "ellipse"
                });
                "sheet-add-shape"
            }
            Tool::TitleBlock => {
                params["fields"] = json!({"Project":session.doc().info.title,"Revision":"A"});
                "sheet-add-title-block"
            }
            Tool::Label => {
                params["binding"] = json!(if self.text == "Note" {
                    "title"
                } else {
                    &self.text
                });
                "sheet-add-label"
            }
            Tool::Dimension => {
                let view=page.items.iter().rev().find(|e|matches!(&e.item,SheetItem::ModelView{frame,..} if frame.contains(a)&&frame.contains(b)));
                if let Some(view) = view {
                    match (
                        layout::pick_anchor(session.doc(), &view.item, a),
                        layout::pick_anchor(session.doc(), &view.item, b),
                    ) {
                        (Ok(start), Ok(end)) => {
                            params["view"] = json!(view.id);
                            params["start"] = json!(start);
                            params["end"] = json!(end);
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            self.error = Some(e.to_string());
                            return;
                        }
                    }
                } else {
                    params["start"] = json!(SheetAnchor::Paper { point: a });
                    params["end"] = json!(SheetAnchor::Paper { point: b });
                }
                "sheet-add-dimension"
            }
            _ => return,
        };
        if let Some(result) = self.execute(session, command, params) {
            self.selected = result["id"].as_str().map(ItemId::named);
            self.tool = Tool::Select;
        }
    }
}
fn stroke_ui(ui: &mut egui::Ui, stroke: &mut sheets::Stroke) {
    ui.add(
        egui::DragValue::new(&mut stroke.width)
            .range(0.01..=10.0)
            .speed(0.05)
            .suffix(" mm pen"),
    );
    ui.color_edit_button_srgba_unmultiplied(&mut stroke.color);
    let mut pattern = if stroke.dash.is_empty() {
        0
    } else if stroke.dash == [4.0, 2.0] {
        1
    } else if stroke.dash == [0.25, 1.5] {
        2
    } else {
        3
    };
    let names = ["Solid", "Dashed", "Dotted", "Dash-dot"];
    let mut changed = false;
    egui::ComboBox::from_id_salt("pen-pattern")
        .selected_text(names[pattern])
        .show_ui(ui, |ui| {
            for (i, name) in names.iter().enumerate() {
                changed |= ui.selectable_value(&mut pattern, i, *name).changed();
            }
        });
    if changed {
        stroke.dash = match pattern {
            0 => Vec::new(),
            1 => vec![4.0, 2.0],
            2 => vec![0.25, 1.5],
            _ => vec![4.0, 1.0, 0.25, 1.0],
        };
    }
}
fn render_ui(ui: &mut egui::Ui, mode: &mut RenderMode) {
    egui::ComboBox::from_id_salt("sheet-render")
        .selected_text(format!("{mode:?}"))
        .show_ui(ui, |ui| {
            for m in [RenderMode::Vector, RenderMode::Raster, RenderMode::Hybrid] {
                ui.selectable_value(mode, m, format!("{m:?}"));
            }
        });
}
fn edit_item(ui: &mut egui::Ui, session: &Session, item: &mut SheetItem, page: &Page) {
    match item {
        SheetItem::ModelView {
            frame,
            scale,
            scene,
            render,
            style,
            camera_override,
            clip,
            ..
        } => {
            frame_ui(ui, frame);
            ui.horizontal(|ui| {
                ui.label("Scale");
                ui.add(egui::DragValue::new(&mut scale.numerator).range(1..=100000));
                ui.label(":");
                ui.add(egui::DragValue::new(&mut scale.denominator).range(1..=100000));
            });
            let mut name = scene.clone().unwrap_or_default();
            egui::ComboBox::from_id_salt("sheet-view-link")
                .selected_text(if name.is_empty() {
                    "Camera snapshot"
                } else {
                    &name
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut name, String::new(), "Camera snapshot");
                    for (_, s) in session.doc().scenes.iter() {
                        ui.selectable_value(&mut name, s.name.clone(), &s.name);
                    }
                });
            *scene = (!name.is_empty()).then_some(name);
            ui.checkbox(camera_override, "Use custom camera, keep scene appearance");
            render_ui(ui, render);
            ui.collapsing("Crop boundary (paper mm)", |ui| {
                for (i, p) in clip.iter_mut().enumerate() {
                    ui.push_id(i, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut p[0]));
                            ui.add(egui::DragValue::new(&mut p[1]));
                        });
                    });
                }
                if ui.button("Start Rectangle Crop").clicked() {
                    *clip = layout::rectangle(*frame);
                }
                if ui.button("Clear Crop").clicked() {
                    clip.clear();
                }
            });
            let mut name = style.clone().unwrap_or_default();
            egui::ComboBox::from_id_salt("sheet-view-style")
                .selected_text(if name.is_empty() {
                    "Scene style"
                } else {
                    &name
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut name, String::new(), "Scene style");
                    for (_, s) in session.doc().styles.iter() {
                        ui.selectable_value(&mut name, s.name.clone(), &s.name);
                    }
                });
            *style = (!name.is_empty()).then_some(name);
        }
        SheetItem::Text {
            runs,
            frame,
            alignment,
            ..
        } => {
            frame_ui(ui, frame);
            for (i, run) in runs.iter_mut().enumerate() {
                ui.push_id(i, |ui| {
                    ui.text_edit_multiline(&mut run.text);
                    ui.add(
                        egui::DragValue::new(&mut run.height)
                            .range(0.5..=100.0)
                            .suffix(" mm text"),
                    );
                    ui.color_edit_button_srgba_unmultiplied(&mut run.color);
                });
            }
            if ui.button("Add Text Run").clicked() {
                runs.push(sheets::TextRun {
                    text: String::new(),
                    height: 3.5,
                    color: [0, 0, 0, 255],
                });
            }
            ui.horizontal(|ui| {
                for a in [
                    sheets::Alignment::Left,
                    sheets::Alignment::Center,
                    sheets::Alignment::Right,
                ] {
                    ui.selectable_value(alignment, a, format!("{a:?}"));
                }
            });
        }
        SheetItem::Table {
            frame,
            rows,
            query,
            columns,
        } => {
            frame_ui(ui, frame);
            let mut q = query.clone().unwrap_or_default();
            ui.label("Live query (empty = literal cells)");
            ui.text_edit_singleline(&mut q);
            *query = (!q.is_empty()).then_some(q);
            if query.is_some() {
                let mut fields = columns.join(",");
                ui.text_edit_singleline(&mut fields);
                *columns = fields
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            } else {
                for (i, row) in rows.iter_mut().enumerate() {
                    ui.push_id(i, |ui| {
                        for (j, cell) in row.iter_mut().enumerate() {
                            ui.push_id(j, |ui| {
                                ui.text_edit_singleline(cell);
                            });
                        }
                    });
                }
                if ui.button("Add Row").clicked() {
                    rows.push(vec![String::new(); rows.first().map_or(2, Vec::len)]);
                }
                if ui.button("Add Column").clicked() {
                    for row in rows {
                        row.push(String::new());
                    }
                }
            }
        }
        SheetItem::Image { frame, .. } => frame_ui(ui, frame),
        SheetItem::Shape {
            points,
            closed,
            stroke,
            fill,
            ..
        } => {
            for (i, p) in points.iter_mut().enumerate() {
                ui.push_id(i, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut p[0]).suffix(" x mm"));
                        ui.add(egui::DragValue::new(&mut p[1]).suffix(" y mm"));
                    });
                });
            }
            ui.checkbox(closed, "Closed");
            ui.add(
                egui::DragValue::new(&mut stroke.width)
                    .range(0.05..=10.0)
                    .suffix(" mm pen"),
            );
            ui.color_edit_button_srgba_unmultiplied(&mut stroke.color);
            let mut filled = fill.is_some();
            if ui.checkbox(&mut filled, "Fill").changed() {
                *fill = filled.then_some([210, 210, 210, 255]);
            }
            if let Some(c) = fill {
                ui.color_edit_button_srgba_unmultiplied(c);
            }
        }
        SheetItem::TitleBlock { frame, fields } => {
            frame_ui(ui, frame);
            for (k, v) in fields.iter_mut() {
                ui.label(k);
                ui.text_edit_singleline(v);
            }
            if ui.button("Add Date Field").clicked() {
                fields.entry("Date".into()).or_default();
            }
            if ui.button("Add Author Field").clicked() {
                fields.entry("Author".into()).or_default();
            }
        }
        SheetItem::Label {
            frame,
            binding,
            prefix,
        } => {
            frame_ui(ui, frame);
            ui.label("title, author, scene:<view>, attr.dictionary.key");
            ui.text_edit_singleline(binding);
            ui.text_edit_singleline(prefix);
        }
        SheetItem::Dimension {
            offset,
            text,
            view,
            dimension_kind,
            start,
            end,
            frozen,
        } => {
            ui.horizontal(|ui| {
                ui.label("Offset (mm)");
                ui.add(egui::DragValue::new(&mut offset[0]));
                ui.add(egui::DragValue::new(&mut offset[1]));
            });
            if frozen.is_some() {
                ui.weak("Frozen with its view. Choose Follow Model to update this measurement.");
                return;
            }
            let mut value = text.clone().unwrap_or_default();
            ui.label("Text (<> = measured value)");
            ui.text_edit_singleline(&mut value);
            *text = (!value.is_empty()).then_some(value);
            egui::ComboBox::from_id_salt("sheet-dimension-kind")
                .selected_text(format!("{dimension_kind:?}"))
                .show_ui(ui, |ui| {
                    for kind in [
                        sheets::DimensionKind::Aligned,
                        sheets::DimensionKind::Horizontal,
                        sheets::DimensionKind::Vertical,
                        sheets::DimensionKind::Radius,
                        sheets::DimensionKind::Diameter,
                    ] {
                        ui.selectable_value(dimension_kind, kind, format!("{kind:?}"));
                    }
                });
            egui::ComboBox::from_id_salt("sheet-dimension-view")
                .selected_text(view.as_ref().map_or("Paper", ItemId::as_str))
                .show_ui(ui, |ui| {
                    ui.selectable_value(view, None, "Paper");
                    for e in &page.items {
                        if matches!(e.item, SheetItem::ModelView { .. }) {
                            ui.selectable_value(view, Some(e.id.clone()), e.id.as_str());
                        }
                    }
                });
            for (label, anchor) in [("Start", start), ("End", end)] {
                ui.label(label);
                match anchor {
                    SheetAnchor::Paper { point } => {
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut point[0]).suffix(" mm"));
                            ui.add(egui::DragValue::new(&mut point[1]).suffix(" mm"));
                        });
                    }
                    SheetAnchor::Model { .. } => {
                        ui.weak("Linked to model geometry");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    #[test]
    fn send_to_sheets_dispatches_the_same_core_commands() {
        let mut session = Session::new();
        session
            .execute(
                "rectangle",
                json!({"corner_a":[0,0,0],"corner_b":[1000,1000,0]}),
            )
            .unwrap();
        let mut direct = planyrith_core::commands::Engine::new(
            session.doc().clone(),
            std::sync::Arc::new(planyrith_core::commands::CommandRegistry::with_builtins()),
        );
        direct
            .execute_erased(
                "sheet-add-page",
                json!({"page":"A101","paper":"A4","landscape":true}),
            )
            .unwrap();
        direct.execute_erased("sheet-add-view",json!({"page":"A101","camera":session.doc().camera,"scale":"1:50","render":"vector","frame":Rect::default()})).unwrap();
        let mut sheets = Sheets::default();
        sheets.send_to_sheets(&mut session);
        assert!(sheets.open);
        assert_eq!(session.doc().sheets, direct.document().sheets);
        session.undo().unwrap();
        assert!(session.doc().sheets.pages[0].items.is_empty());
        session.redo().unwrap();
        assert_eq!(session.doc().sheets, direct.document().sheets);
    }
    #[test]
    fn paper_canvas_accepts_two_clicks_and_undo() {
        let ctx = egui::Context::default();
        let mut session = Session::new();
        let mut sheets = Sheets::default();
        sheets.add_page(&mut session);
        sheets.open = true;
        sheets.tool = Tool::Rectangle;
        let mut time = 0.0;
        let mut frame = |sheets: &mut Sheets, session: &mut Session, events| {
            time += 1.0 / 60.0;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        Pos2::ZERO,
                        egui::vec2(1200.0, 800.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| sheets.ui(ui, session),
            );
            output.textures_delta.clear();
            output
        };
        let output = frame(&mut sheets, &mut session, Vec::new());
        let paper = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) if r.fill == Color32::WHITE => Some(r.rect),
                _ => None,
            })
            .max_by(|a, b| a.area().total_cmp(&b.area()))
            .unwrap();
        let project = |p: [f32; 2]| {
            paper.left_bottom()
                + egui::vec2(p[0] / 297.0 * paper.width(), -p[1] / 210.0 * paper.height())
        };
        for p in [project([30.0, 30.0]), project([70.0, 60.0])] {
            frame(
                &mut sheets,
                &mut session,
                vec![egui::Event::PointerMoved(p)],
            );
            for pressed in [true, false] {
                frame(
                    &mut sheets,
                    &mut session,
                    vec![egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            }
        }
        assert_eq!(
            session.doc().sheets.pages[0].items.len(),
            1,
            "{:?}",
            sheets.error
        );
        assert!(matches!(
            session.doc().sheets.pages[0].items[0].item,
            SheetItem::Shape { .. }
        ));
        session.undo().unwrap();
        assert!(session.doc().sheets.pages[0].items.is_empty());
    }
}
