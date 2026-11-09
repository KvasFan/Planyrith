//! Tools › Mirror…, Revolve…, Pattern… and Measure… (docs/05 §4.21–§4.23,
//! §8.3–§8.5, milestone 1.8): small windows that collect the parameters of
//! one command each and run it on the selection.

use std::collections::BTreeMap;

use glam::DVec3;
use planyrith_core::math::Aabb;
use serde_json::{Value, json};

use crate::i18n::{t, t_args};
use crate::session::Session;

/// The world bounds of the selection, if it has any.
fn selection_bounds(session: &Session) -> Option<Aabb> {
    let doc = session.doc();
    let mut bb = Aabb::EMPTY;
    for path in session.engine.selection().iter() {
        if let Some(b) = doc.occurrence_aabb(path) {
            bb = bb.union(&b);
        }
    }
    (!bb.is_empty()).then_some(bb)
}

fn note(result: Result<Value, String>, done: impl FnOnce(&Value) -> String) -> String {
    match result {
        Ok(out) => done(&out),
        Err(e) => e,
    }
}

fn copies_note(out: &Value) -> String {
    t_args(
        "{ $count } pieces made.",
        &[(
            "count",
            out["copies"]
                .as_array()
                .map_or(0, Vec::len)
                .to_string()
                .into(),
        )],
    )
}

/// An axis-plane choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Plane {
    /// Red and blue axes (normal green).
    #[default]
    RedBlue,
    /// Green and blue axes (normal red).
    GreenBlue,
    /// Red and green axes (normal blue).
    RedGreen,
}

impl Plane {
    fn normal(self) -> DVec3 {
        match self {
            Plane::RedBlue => DVec3::Y,
            Plane::GreenBlue => DVec3::X,
            Plane::RedGreen => DVec3::Z,
        }
    }

    fn label(self) -> String {
        match self {
            Plane::RedBlue => t("Red–blue plane"),
            Plane::GreenBlue => t("Green–blue plane"),
            Plane::RedGreen => t("Red–green plane"),
        }
    }
}

/// An axis choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Axis {
    /// Red.
    #[default]
    X,
    /// Green.
    Y,
    /// Blue.
    Z,
}

impl Axis {
    fn vector(self) -> DVec3 {
        match self {
            Axis::X => DVec3::X,
            Axis::Y => DVec3::Y,
            Axis::Z => DVec3::Z,
        }
    }

    fn label(self) -> String {
        match self {
            Axis::X => t("Red axis"),
            Axis::Y => t("Green axis"),
            Axis::Z => t("Blue axis"),
        }
    }

    fn combo(ui: &mut egui::Ui, id: &str, value: &mut Axis) {
        egui::ComboBox::from_id_salt(id)
            .selected_text(value.label())
            .show_ui(ui, |ui| {
                for a in [Axis::X, Axis::Y, Axis::Z] {
                    ui.selectable_value(value, a, a.label());
                }
            });
    }
}

/// Tools › Mirror… (docs/05 §4.21).
#[derive(Default)]
pub struct MirrorWindow {
    /// The window is shown.
    pub open: bool,
    plane: Plane,
    in_place: bool,
    note: Option<String>,
}

impl MirrorWindow {
    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Mirror"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(t("Mirror the selection across a plane through its center:"));
                egui::ComboBox::from_id_salt("mirror plane")
                    .selected_text(self.plane.label())
                    .show_ui(ui, |ui| {
                        for p in [Plane::RedBlue, Plane::GreenBlue, Plane::RedGreen] {
                            ui.selectable_value(&mut self.plane, p, p.label());
                        }
                    });
                ui.checkbox(&mut self.in_place, t("Mirror in place (no copy)"));
                if ui.button(t("Mirror")).clicked() {
                    self.note = Some(match selection_bounds(session) {
                        None => t("Select something first."),
                        Some(bb) => {
                            let c = bb.center();
                            // A copy goes beside the original: the plane
                            // touches the selection's far side.
                            let n = self.plane.normal();
                            let point = if self.in_place {
                                c
                            } else {
                                c + n * (bb.max - c).dot(n)
                            };
                            note(
                                session.execute(
                                    "mirror",
                                    json!({ "targets": "selection", "point": point.to_array(),
                                            "normal": n.to_array(), "copy": !self.in_place }),
                                ),
                                |_| t("Mirrored."),
                            )
                        }
                    });
                }
                if let Some(n) = &self.note {
                    ui.label(n);
                }
            });
        self.open = open;
    }
}

/// Tools › Revolve… (docs/05 §4.22).
pub struct RevolveWindow {
    /// The window is shown.
    pub open: bool,
    angle_deg: f64,
    segments: u32,
    keep_profile: bool,
    note: Option<String>,
}

impl Default for RevolveWindow {
    fn default() -> Self {
        RevolveWindow {
            open: false,
            angle_deg: 360.0,
            segments: 24,
            keep_profile: false,
            note: None,
        }
    }
}

impl RevolveWindow {
    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Revolve"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(t(
                    "Select a face and one edge in its plane as the axis, then:",
                ));
                egui::Grid::new("revolve").num_columns(2).show(ui, |ui| {
                    ui.label(t("Angle"));
                    ui.add(
                        egui::DragValue::new(&mut self.angle_deg)
                            .range(1.0..=360.0)
                            .suffix("°"),
                    );
                    ui.end_row();
                    ui.label(t("Segments per turn"));
                    ui.add(egui::DragValue::new(&mut self.segments).range(3..=720));
                    ui.end_row();
                });
                ui.checkbox(&mut self.keep_profile, t("Keep the profile face"));
                if ui.button(t("Revolve")).clicked() {
                    self.note = Some(note(
                        session.execute(
                            "revolve",
                            json!({ "profile": "selection & face", "axis_edge": "selection & edge",
                                    "angle": self.angle_deg.to_radians(), "segments": self.segments,
                                    "keep_profile": self.keep_profile }),
                        ),
                        |out| {
                            t_args(
                                "Solid made: { $volume } cm³.",
                                &[(
                                    "volume",
                                    format!(
                                        "{:.1}",
                                        out["volume_mm3"].as_f64().unwrap_or(0.0) / 1000.0
                                    )
                                    .into(),
                                )],
                            )
                        },
                    ));
                }
                if let Some(n) = &self.note {
                    ui.label(n);
                }
            });
        self.open = open;
    }
}

/// What the pattern window repeats along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Kind {
    #[default]
    Linear,
    Circular,
    Path,
}

/// Tools › Pattern… (docs/05 §4.23).
pub struct PatternWindow {
    /// The window is shown.
    pub open: bool,
    kind: Kind,
    count: u32,
    spacing_mm: f64,
    direction: Axis,
    count2: u32,
    spacing2_mm: f64,
    direction2: Axis,
    axis: Axis,
    center: [f64; 3],
    angle_deg: f64,
    align: bool,
    note: Option<String>,
}

impl Default for PatternWindow {
    fn default() -> Self {
        PatternWindow {
            open: false,
            kind: Kind::Linear,
            count: 4,
            spacing_mm: 500.0,
            direction: Axis::X,
            count2: 1,
            spacing2_mm: 500.0,
            direction2: Axis::Y,
            axis: Axis::Z,
            center: [0.0; 3],
            angle_deg: 360.0,
            align: true,
            note: None,
        }
    }
}

impl PatternWindow {
    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Pattern"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.kind, Kind::Linear, t("In rows"));
                    ui.selectable_value(&mut self.kind, Kind::Circular, t("Around an axis"));
                    ui.selectable_value(&mut self.kind, Kind::Path, t("Along edges"));
                });
                egui::Grid::new("pattern").num_columns(2).show(ui, |ui| {
                    ui.label(t("Count"));
                    ui.add(egui::DragValue::new(&mut self.count).range(2..=10_000));
                    ui.end_row();
                    match self.kind {
                        Kind::Linear => {
                            ui.label(t("Direction"));
                            Axis::combo(ui, "pattern direction", &mut self.direction);
                            ui.end_row();
                            ui.label(t("Spacing"));
                            ui.add(egui::DragValue::new(&mut self.spacing_mm).suffix(" mm"));
                            ui.end_row();
                            ui.label(t("Rows"));
                            ui.add(egui::DragValue::new(&mut self.count2).range(1..=10_000));
                            ui.end_row();
                            if self.count2 > 1 {
                                ui.label(t("Row direction"));
                                Axis::combo(ui, "pattern direction 2", &mut self.direction2);
                                ui.end_row();
                                ui.label(t("Row spacing"));
                                ui.add(egui::DragValue::new(&mut self.spacing2_mm).suffix(" mm"));
                                ui.end_row();
                            }
                        }
                        Kind::Circular => {
                            ui.label(t("Axis"));
                            Axis::combo(ui, "pattern axis", &mut self.axis);
                            ui.end_row();
                            ui.label(t("Through the point"));
                            ui.horizontal(|ui| {
                                for v in &mut self.center {
                                    ui.add(egui::DragValue::new(v).suffix(" mm"));
                                }
                            });
                            ui.end_row();
                            ui.label(t("Angle"));
                            ui.add(
                                egui::DragValue::new(&mut self.angle_deg)
                                    .range(1.0..=360.0)
                                    .suffix("°"),
                            );
                            ui.end_row();
                        }
                        Kind::Path => {
                            ui.label(t("Turn with the path"));
                            ui.checkbox(&mut self.align, "");
                            ui.end_row();
                        }
                    }
                });
                if self.kind == Kind::Path {
                    ui.weak(t("Select what to repeat and the edges of the path."));
                }
                if ui.button(t("Make Pattern")).clicked() {
                    let params = match self.kind {
                        Kind::Linear => json!({
                            "targets": "selection", "kind": "linear", "count": self.count,
                            "direction": self.direction.vector().to_array(),
                            "spacing": self.spacing_mm, "count2": self.count2,
                            "direction2": self.direction2.vector().to_array(),
                            "spacing2": self.spacing2_mm,
                        }),
                        Kind::Circular => json!({
                            "targets": "selection", "kind": "circular", "count": self.count,
                            "center": self.center, "axis": self.axis.vector().to_array(),
                            "angle": self.angle_deg.to_radians(),
                        }),
                        Kind::Path => json!({
                            "targets": "selection & !edge", "kind": "path", "count": self.count,
                            "path": "selection & edge", "align": self.align,
                        }),
                    };
                    self.note = Some(note(session.execute("pattern", params), copies_note));
                }
                if let Some(n) = &self.note {
                    ui.label(n);
                }
            });
        self.open = open;
    }
}

/// What the measuring window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Mass,
    Interference,
    CutList,
}

/// Tools › Measure… (docs/05 §8.3–§8.5): mass properties, interference and
/// the cut list of the selection (or of everything).
#[derive(Default)]
pub struct MeasureWindow {
    /// The window is shown.
    pub open: bool,
    page: Page,
    density: f64,
    result: Option<Result<Value, String>>,
}

fn mm(v: f64) -> String {
    format!("{v:.1}")
}

impl MeasureWindow {
    /// Opens the window on a page.
    pub fn show_mass(&mut self) {
        self.open = true;
        self.page = Page::Mass;
        self.result = None;
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Measure"))
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (page, label) in [
                        (Page::Mass, t("Mass Properties")),
                        (Page::Interference, t("Interference")),
                        (Page::CutList, t("Cut List")),
                    ] {
                        if ui.selectable_value(&mut self.page, page, label).changed() {
                            self.result = None;
                        }
                    }
                });
                if self.page == Page::Mass {
                    ui.horizontal(|ui| {
                        ui.label(t("Density where materials give none"));
                        ui.add(
                            egui::DragValue::new(&mut self.density)
                                .range(0.0..=100_000.0)
                                .suffix(" kg/m³"),
                        );
                    });
                }
                let scope = if session.engine.selection().is_empty() {
                    t("Everything visible")
                } else {
                    t("The selection")
                };
                ui.horizontal(|ui| {
                    ui.weak(scope);
                    if ui.button(t("Compute")).clicked() {
                        let mut params = json!({});
                        if !session.engine.selection().is_empty() {
                            params["targets"] = json!("selection");
                        }
                        let name = match self.page {
                            Page::Mass => {
                                if self.density > 0.0 {
                                    params["density"] = json!(self.density);
                                }
                                "mass-properties"
                            }
                            Page::Interference => "interference-check",
                            Page::CutList => "cut-list",
                        };
                        self.result = Some(session.execute(name, params));
                    }
                });
                ui.separator();
                match &self.result {
                    None => {}
                    Some(Err(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    Some(Ok(out)) => match self.page {
                        Page::Mass => mass_page(ui, out),
                        Page::Interference => interference_page(ui, out),
                        Page::CutList => cut_list_page(ui, out),
                    },
                }
            });
        self.open = open;
    }
}

fn mass_page(ui: &mut egui::Ui, out: &Value) {
    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            let rows: Vec<&Value> = out["objects"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(std::iter::once(&out["total"]))
                .collect();
            egui::Grid::new("mass")
                .striped(true)
                .num_columns(6)
                .show(ui, |ui| {
                    for h in [
                        "Name",
                        "Volume",
                        "Area",
                        "Mass",
                        "Center of mass",
                        "Principal moments",
                    ] {
                        ui.strong(t(h));
                    }
                    ui.end_row();
                    for r in rows {
                        ui.label(r["name"].as_str().unwrap_or_default());
                        ui.label(format!(
                            "{:.2} cm³",
                            r["volume_mm3"].as_f64().unwrap_or(0.0) / 1000.0
                        ));
                        ui.label(format!(
                            "{:.2} cm²",
                            r["area_mm2"].as_f64().unwrap_or(0.0) / 100.0
                        ));
                        ui.label(
                            r["mass_kg"]
                                .as_f64()
                                .map_or_else(|| "—".to_string(), |m| format!("{m:.3} kg")),
                        );
                        let c: Vec<String> = r["center_mm"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|v| mm(v.as_f64().unwrap_or(0.0)))
                            .collect();
                        ui.label(format!("{} mm", c.join(", ")));
                        let p: Vec<String> = r["principal_kg_mm2"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|v| format!("{:.4}", v.as_f64().unwrap_or(0.0) / 1e6))
                            .collect();
                        ui.label(if p.is_empty() {
                            "—".to_string()
                        } else {
                            format!("{} kg·m²", p.join(", "))
                        });
                        ui.end_row();
                    }
                });
            if let Some(skipped) = out["skipped"].as_array().filter(|s| !s.is_empty()) {
                let names: Vec<&str> = skipped.iter().filter_map(Value::as_str).collect();
                ui.weak(t_args(
                    "Not closed solids, left out: { $names }",
                    &[("names", names.join(", ").into())],
                ));
            }
        });
}

fn interference_page(ui: &mut egui::Ui, out: &Value) {
    let list = out["interferences"].as_array().cloned().unwrap_or_default();
    ui.label(t_args(
        "{ $checked } solids checked, { $count } overlaps.",
        &[
            (
                "checked",
                out["checked"].as_u64().unwrap_or(0).to_string().into(),
            ),
            ("count", list.len().to_string().into()),
        ],
    ));
    egui::Grid::new("interference")
        .striped(true)
        .num_columns(3)
        .show(ui, |ui| {
            for row in &list {
                ui.label(row["a_name"].as_str().unwrap_or_default());
                ui.label(row["b_name"].as_str().unwrap_or_default());
                ui.label(format!(
                    "{:.3} cm³",
                    row["volume_mm3"].as_f64().unwrap_or(0.0) / 1000.0
                ));
                ui.end_row();
            }
        });
}

/// The cut list as comma-separated values.
fn cut_list_csv(out: &Value) -> String {
    let mut csv = String::from("name,count,length_mm,width_mm,thickness_mm,material\n");
    for r in out["rows"].as_array().into_iter().flatten() {
        let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        csv.push_str(&format!(
            "{},{},{:.1},{:.1},{:.1},{}\n",
            quote(r["name"].as_str().unwrap_or_default()),
            r["count"].as_u64().unwrap_or(0),
            r["length_mm"].as_f64().unwrap_or(0.0),
            r["width_mm"].as_f64().unwrap_or(0.0),
            r["thickness_mm"].as_f64().unwrap_or(0.0),
            quote(r["material"].as_str().unwrap_or_default()),
        ));
    }
    csv
}

fn cut_list_page(ui: &mut egui::Ui, out: &Value) {
    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            egui::Grid::new("cut list")
                .striped(true)
                .num_columns(6)
                .show(ui, |ui| {
                    for h in ["Name", "Count", "Length", "Width", "Thickness", "Material"] {
                        ui.strong(t(h));
                    }
                    ui.end_row();
                    for r in out["rows"].as_array().into_iter().flatten() {
                        ui.label(r["name"].as_str().unwrap_or_default());
                        ui.label(r["count"].as_u64().unwrap_or(0).to_string());
                        for key in ["length_mm", "width_mm", "thickness_mm"] {
                            ui.label(format!("{} mm", mm(r[key].as_f64().unwrap_or(0.0))));
                        }
                        ui.label(r["material"].as_str().unwrap_or_default());
                        ui.end_row();
                    }
                });
        });
    if ui.button(t("Copy as CSV")).clicked() {
        ui.ctx().copy_text(cut_list_csv(out));
    }
}

/// The pids of the selected edges in the edit context, in selection order.
fn selected_edges(session: &Session) -> Vec<u64> {
    let doc = session.doc();
    let mut out = Vec::new();
    for path in session.engine.selection().iter() {
        let Ok(r) = doc.resolve(path) else { continue };
        if !matches!(r.entity, planyrith_core::ids::EntityId::Edge(_)) {
            continue;
        }
        if let Some(pid) = doc.container(r.container).and_then(|c| c.pid_of(r.entity)) {
            out.push(pid.0);
        }
    }
    out
}

/// The ends of two edges that are nearest each other: `[12.a, 15.b]`.
fn nearest_ends(session: &Session, a: u64, b: u64) -> Option<[String; 2]> {
    let doc = session.doc();
    let context = session.engine.context().path.clone();
    let (container, _) = doc.resolve_container(&context).ok()?;
    let entities = doc.container(container)?;
    let seg = |pid: u64| -> Option<planyrith_core::math::Segment> {
        match entities.id_of(planyrith_core::ids::Pid(pid))? {
            planyrith_core::ids::EntityId::Edge(e) => entities.mesh().edge_segment(e).ok(),
            _ => None,
        }
    };
    let (sa, sb) = (seg(a)?, seg(b)?);
    let mut best = None;
    for (ea, pa) in [("a", sa.a), ("b", sa.b)] {
        for (eb, pb) in [("a", sb.a), ("b", sb.b)] {
            let d = pa.distance(pb);
            if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, [format!("{a}.{ea}"), format!("{b}.{eb}")]));
            }
        }
    }
    best.map(|(_, e)| e)
}

/// Tools › Constraints… (docs/05 §5.15): the edit context's constraints
/// with their state, driving values to edit, and constraints to add to the
/// selected edges.
pub struct ConstraintsWindow {
    /// The window is shown.
    pub open: bool,
    length_mm: f64,
    angle_deg: f64,
    list: Option<Value>,
    note: Option<String>,
}

impl Default for ConstraintsWindow {
    fn default() -> Self {
        ConstraintsWindow {
            open: false,
            length_mm: 1000.0,
            angle_deg: 90.0,
            list: None,
            note: None,
        }
    }
}

impl ConstraintsWindow {
    fn refresh(&mut self, session: &mut Session) {
        self.list = session.execute("constraint-list", json!({})).ok();
    }

    fn run(&mut self, session: &mut Session, name: &str, params: Value) {
        self.note = Some(match session.execute(name, params) {
            Ok(out) => match out["degrees_of_freedom"].as_u64() {
                Some(0) => t("Fully constrained"),
                Some(n) => t_args(
                    "{ $count } degrees of freedom left.",
                    &[("count", n.to_string().into())],
                ),
                None => t("Done"),
            },
            Err(e) => e,
        });
        self.refresh(session);
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            self.list = None;
            return;
        }
        if self.list.is_none() {
            self.refresh(session);
        }
        let mut open = true;
        egui::Window::new(t("Constraints"))
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| {
                let edges = selected_edges(session);
                let q: Vec<String> = edges.iter().map(|p| format!("@{p}")).collect();
                ui.label(t_args(
                    "Selected edges: { $count }",
                    &[("count", edges.len().to_string().into())],
                ));
                ui.horizontal_wrapped(|ui| {
                    let one = !q.is_empty();
                    let two = q.len() >= 2;
                    for (label, kind, enabled) in [
                        ("Fix", "fixed", one),
                        ("Along red", "along-x", one),
                        ("Along green", "along-y", one),
                        ("Along blue", "along-z", one),
                        ("Parallel", "parallel", two),
                        ("Equal", "equal", two),
                    ] {
                        if ui.add_enabled(enabled, egui::Button::new(t(label))).clicked() {
                            self.run(session, "constraint-add", json!({ "kind": kind, "edges": q }));
                        }
                    }
                    let pair = q.len() == 2;
                    for (label, kind) in [("Perpendicular", "perpendicular"), ("Collinear", "collinear")] {
                        if ui.add_enabled(pair, egui::Button::new(t(label))).clicked() {
                            self.run(session, "constraint-add", json!({ "kind": kind, "edges": q }));
                        }
                    }
                    if ui
                        .add_enabled(pair, egui::Button::new(t("Join Ends")))
                        .clicked()
                        && let Some(points) = nearest_ends(session, edges[0], edges[1])
                    {
                        self.run(
                            session,
                            "constraint-add",
                            json!({ "kind": "coincident", "points": points }),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.length_mm).suffix(" mm"));
                    if ui
                        .add_enabled(edges.len() == 1, egui::Button::new(t("Length")))
                        .clicked()
                    {
                        self.run(
                            session,
                            "constraint-add",
                            json!({ "kind": "length", "edges": q, "length": self.length_mm }),
                        );
                    }
                    if ui
                        .add_enabled(edges.len() == 2, egui::Button::new(t("Distance between ends")))
                        .clicked()
                        && let Some(points) = nearest_ends(session, edges[0], edges[1])
                    {
                        self.run(
                            session,
                            "constraint-add",
                            json!({ "kind": "distance", "points": points, "length": self.length_mm }),
                        );
                    }
                    ui.add(
                        egui::DragValue::new(&mut self.angle_deg)
                            .range(0.0..=180.0)
                            .suffix("°"),
                    );
                    if ui
                        .add_enabled(edges.len() == 2, egui::Button::new(t("Angle")))
                        .clicked()
                    {
                        self.run(
                            session,
                            "constraint-add",
                            json!({ "kind": "angle", "edges": q,
                                    "angle": self.angle_deg.to_radians() }),
                        );
                    }
                });
                ui.separator();
                let Some(list) = self.list.clone() else {
                    return;
                };
                let state = match list["state"].as_str().unwrap_or_default() {
                    "fully" => t("Fully constrained"),
                    "under" => t_args(
                        "Under-constrained: { $count } degrees of freedom",
                        &[(
                            "count",
                            list["degrees_of_freedom"].as_u64().unwrap_or(0).to_string().into(),
                        )],
                    ),
                    "conflict" => t("The constraints conflict"),
                    _ => t("No constraints yet"),
                };
                ui.strong(state);
                let mut edit: Option<(u64, Value)> = None;
                let mut delete: Option<u64> = None;
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    egui::Grid::new("constraints").striped(true).num_columns(4).show(ui, |ui| {
                        for c in list["constraints"].as_array().into_iter().flatten() {
                            let id = c["id"].as_u64().unwrap_or(0);
                            ui.label(format!("{id}"));
                            let mut what = c["kind"].as_str().unwrap_or_default().to_string();
                            let names: Vec<String> = c["edges"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|e| format!("@{}", e.as_u64().unwrap_or(0)))
                                .chain(
                                    c["points"]
                                        .as_array()
                                        .into_iter()
                                        .flatten()
                                        .filter_map(|p| p.as_str().map(str::to_string)),
                                )
                                .collect();
                            what.push_str(&format!(" {}", names.join(" ")));
                            if c["missing"].as_bool() == Some(true) {
                                what.push_str(&format!(" ({})", t("missing")));
                            }
                            ui.label(what);
                            if let Some(mut l) = c["length_mm"].as_f64() {
                                if ui.add(egui::DragValue::new(&mut l).suffix(" mm")).changed() {
                                    edit = Some((id, json!({ "id": id, "length": l })));
                                }
                            } else if let Some(a) = c["angle_rad"].as_f64() {
                                let mut d = a.to_degrees();
                                if ui
                                    .add(egui::DragValue::new(&mut d).range(0.0..=180.0).suffix("°"))
                                    .changed()
                                {
                                    edit = Some((id, json!({ "id": id, "angle": d.to_radians() })));
                                }
                            } else {
                                ui.label("");
                            }
                            if ui.small_button("✕").on_hover_text(t("Delete")).clicked() {
                                delete = Some(id);
                            }
                            ui.end_row();
                        }
                    });
                });
                if let Some((_, params)) = edit {
                    self.run(session, "constraint-edit", params);
                }
                if let Some(id) = delete {
                    self.run(session, "constraint-delete", json!({ "ids": [id] }));
                }
                ui.horizontal(|ui| {
                    if ui.button(t("Solve Again")).clicked() {
                        self.run(session, "constraint-solve", json!({}));
                    }
                    if ui.button(t("Remove Missing")).clicked() {
                        self.run(session, "constraint-solve", json!({ "drop_missing": true }));
                    }
                });
                if let Some(n) = &self.note {
                    ui.label(n);
                }
            });
        self.open = open;
    }
}

/// Tools › Features… (docs/05 §5.16): the history of the part open for
/// editing — features to suppress, reorder, delete, edit and roll back to,
/// its parameters and configurations — and recording of the changes made
/// in it.
pub struct FeaturesWindow {
    /// The window is shown.
    pub open: bool,
    new_name: String,
    list: Option<Value>,
    editing: Option<(u64, String)>,
    new_param: (String, String),
    formulas: BTreeMap<String, String>,
    config_name: String,
    note: Option<String>,
    /// The document generation the list shows.
    seen: u64,
}

impl Default for FeaturesWindow {
    fn default() -> Self {
        FeaturesWindow {
            open: false,
            new_name: "Part".into(),
            list: None,
            editing: None,
            new_param: (String::new(), String::new()),
            formulas: BTreeMap::new(),
            config_name: String::new(),
            note: None,
            seen: u64::MAX,
        }
    }
}

impl FeaturesWindow {
    fn refresh(&mut self, session: &mut Session) {
        self.seen = session.engine.generation();
        self.list = session.execute("feature-list", json!({})).ok();
        self.formulas.clear();
        if let Some(d) = session.open_part()
            && let Some(dict) = session
                .doc()
                .definition(d)
                .and_then(|def| def.attrs.dictionary("parameters"))
        {
            for (k, v) in dict {
                if let planyrith_core::document::Value::Str(f) = v {
                    self.formulas.insert(k.clone(), f.clone());
                }
            }
        }
    }

    fn run(&mut self, session: &mut Session, name: &str, params: Value) {
        self.note = session.execute(name, params).err();
        self.refresh(session);
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            self.list = None;
            return;
        }
        let mut open = true;
        egui::Window::new(t("Features"))
            .open(&mut open)
            .default_width(480.0)
            .show(ctx, |ui| {
                let Some(d) = session.open_part() else {
                    self.list = None;
                    ui.label(t("No part is open for editing. Open one (double-click it), or make a new one:"));
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.new_name);
                        if ui.button(t("New Part")).clicked() {
                            match session.execute("part-create", json!({ "name": self.new_name })) {
                                Ok(out) => {
                                    let pid = planyrith_core::ids::Pid(
                                        out["part"].as_u64().unwrap_or_default(),
                                    );
                                    let path = session
                                        .engine
                                        .context()
                                        .path
                                        .child(planyrith_core::document::EntityKey::Pid(pid));
                                    let _ = session.engine.set_context(
                                        planyrith_core::document::EditContext { path },
                                    );
                                    session.record_features = true;
                                    self.note = Some(t(
                                        "The new part is open; what you draw in it is recorded as features.",
                                    ));
                                }
                                Err(e) => self.note = Some(e),
                            }
                        }
                    });
                    if let Some(n) = &self.note {
                        ui.label(n);
                    }
                    return;
                };
                if self.list.is_none() || self.seen != session.engine.generation() {
                    self.refresh(session);
                }
                let name = session
                    .doc()
                    .definition(d)
                    .map(|def| def.name.clone())
                    .unwrap_or_default();
                ui.heading(name.clone());
                ui.checkbox(
                    &mut session.record_features,
                    t("Record changes as features"),
                );
                let Some(list) = self.list.clone() else {
                    return;
                };
                let history = &list["history"];
                let rollback = history["rollback_before"].as_u64();
                let features: Vec<Value> = history["features"].as_array().cloned().unwrap_or_default();
                let mut action: Option<(&str, Value)> = None;
                egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                    egui::Grid::new("features").striped(true).num_columns(5).show(ui, |ui| {
                        for (i, f) in features.iter().enumerate() {
                            let id = f["id"].as_u64().unwrap_or(0);
                            if rollback == Some(id) {
                                ui.colored_label(ui.visuals().warn_fg_color, t("— rolled back here —"));
                                ui.end_row();
                            }
                            let mut on = !f["suppressed"].as_bool().unwrap_or(false);
                            if ui.checkbox(&mut on, "").on_hover_text(t("Unchecked: suppressed")).changed() {
                                action = Some(("feature-edit", json!({ "id": id, "suppressed": !on })));
                            }
                            let label = format!(
                                "{}. {} ({})",
                                i + 1,
                                f["label"].as_str().unwrap_or_default(),
                                f["command"].as_str().unwrap_or_default()
                            );
                            if ui.selectable_label(self.editing.as_ref().is_some_and(|(e, _)| *e == id), label).clicked() {
                                self.editing = Some((
                                    id,
                                    serde_json::to_string_pretty(&f["params"]).unwrap_or_default(),
                                ));
                            }
                            ui.horizontal(|ui| {
                                if ui.small_button("↑").clicked() && i > 0 {
                                    let before = features[i - 1]["id"].as_u64();
                                    action = Some(("feature-move", json!({ "id": id, "before": before })));
                                }
                                if ui.small_button("↓").clicked() && i + 1 < features.len() {
                                    let before = features.get(i + 2).and_then(|f| f["id"].as_u64());
                                    action = Some(("feature-move", json!({ "id": id, "before": before })));
                                }
                            });
                            if ui.small_button(t("Stop here")).on_hover_text(t("Roll back before this feature")).clicked() {
                                action = Some(("part-regenerate", json!({ "rollback_before": id })));
                            }
                            if ui.small_button("✕").on_hover_text(t("Delete")).clicked() {
                                action = Some(("feature-delete", json!({ "ids": [id] })));
                            }
                            ui.end_row();
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if rollback.is_some() && ui.button(t("Roll Forward")).clicked() {
                        action = Some(("part-regenerate", json!({ "roll_forward": true })));
                    }
                    if ui.button(t("Regenerate")).clicked() {
                        action = Some(("part-regenerate", json!({})));
                    }
                });
                if let Some((id, text)) = &mut self.editing {
                    ui.separator();
                    ui.label(t_args("Parameters of feature { $id }:", &[("id", id.to_string().into())]));
                    ui.add(egui::TextEdit::multiline(text).code_editor().desired_rows(5));
                    if ui.button(t("Apply")).clicked() {
                        match serde_json::from_str::<Value>(text) {
                            Ok(Value::Object(new)) => {
                                let mut changes = new.clone();
                                if let Some(old) = features
                                    .iter()
                                    .find(|f| f["id"].as_u64() == Some(*id))
                                    .and_then(|f| f["params"].as_object())
                                {
                                    for k in old.keys() {
                                        if !new.contains_key(k) {
                                            changes.insert(k.clone(), Value::Null);
                                        }
                                    }
                                }
                                action = Some(("feature-edit", json!({ "id": *id, "params": changes })));
                            }
                            _ => self.note = Some(t("The parameters must be a JSON object.")),
                        }
                    }
                }
                ui.separator();
                ui.strong(t("Parameters"));
                let values = list["parameters"].clone();
                let mut set: Option<(String, String)> = None;
                egui::Grid::new("part parameters").num_columns(3).show(ui, |ui| {
                    for (k, formula) in &mut self.formulas {
                        ui.label(k);
                        let r = ui.text_edit_singleline(formula);
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            set = Some((k.clone(), formula.clone()));
                        }
                        ui.weak(values[k].as_f64().map(|v| format!("= {v}")).unwrap_or_default());
                        ui.end_row();
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.new_param.0).hint_text(t("Name")));
                    ui.add(egui::TextEdit::singleline(&mut self.new_param.1).hint_text(t("formula")));
                    if ui.button(t("Add")).clicked() && !self.new_param.0.trim().is_empty() {
                        set = Some(std::mem::take(&mut self.new_param));
                    }
                    ui.end_row();
                });
                if let Some((param, formula)) = set {
                    self.note = session
                        .execute(
                            "parameter-set",
                            json!({ "component": name, "name": param.trim(), "formula": formula }),
                        )
                        .err();
                    if self.note.is_none() {
                        action = Some(("part-regenerate", json!({})));
                    }
                }
                ui.separator();
                ui.strong(t("Configurations"));
                ui.horizontal_wrapped(|ui| {
                    if let Some(configs) = history["configurations"].as_object() {
                        for config in configs.keys() {
                            let active = history["active"].as_str() == Some(config.as_str());
                            if ui.selectable_label(active, config).clicked() {
                                action = Some((
                                    "part-configuration",
                                    json!({ "name": config, "activate": true }),
                                ));
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.config_name).hint_text(t("Name")));
                    if ui.button(t("Save Parameters as Configuration")).clicked()
                        && !self.config_name.trim().is_empty()
                    {
                        action = Some((
                            "part-configuration",
                            json!({ "name": self.config_name.trim(), "values": self.formulas }),
                        ));
                    }
                });
                if let Some((name, params)) = action {
                    self.run(session, name, params);
                }
                if let Some(n) = &self.note {
                    ui.colored_label(ui.visuals().warn_fg_color, n);
                }
            });
        self.open = open;
    }
}

/// The pages of the solid features window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SolidPage {
    #[default]
    Hole,
    Shell,
    Draft,
    Split,
    Dome,
    Rib,
}

/// Tools › Solid Features… (docs/05 §8.6–§8.11): holes, shells, draft,
/// splitting, domes and ribs on the selected faces of a closed solid.
pub struct SolidFeaturesWindow {
    /// The window is shown.
    pub open: bool,
    page: SolidPage,
    kind: usize,
    size: usize,
    diameter_mm: f64,
    through: bool,
    depth_mm: f64,
    thickness_mm: f64,
    angle_deg: f64,
    plane: Plane,
    height_mm: f64,
    note: Option<String>,
}

impl Default for SolidFeaturesWindow {
    fn default() -> Self {
        SolidFeaturesWindow {
            open: false,
            page: SolidPage::Hole,
            kind: 0,
            size: 6,
            diameter_mm: 10.0,
            through: true,
            depth_mm: 10.0,
            thickness_mm: 3.0,
            angle_deg: 3.0,
            plane: Plane::GreenBlue,
            height_mm: 20.0,
            note: None,
        }
    }
}

const HOLE_KINDS: [&str; 5] = [
    "simple",
    "clearance",
    "tapped",
    "counterbore",
    "countersink",
];

fn hole_kind_label(i: usize) -> String {
    match i {
        0 => t("Plain"),
        1 => t("Clearance"),
        2 => t("Tapped"),
        3 => t("Counterbore"),
        _ => t("Countersink"),
    }
}

impl SolidFeaturesWindow {
    fn run(&mut self, session: &mut Session, name: &str, params: Value) {
        self.note = Some(match session.execute(name, params) {
            Ok(out) => match out["thread"].as_str() {
                Some(t) => t_args(
                    "Done; thread { $thread }.",
                    &[("thread", t.to_string().into())],
                ),
                None => t("Done"),
            },
            Err(e) => e,
        });
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Solid Features"))
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (page, label) in [
                        (SolidPage::Hole, "Hole"),
                        (SolidPage::Shell, "Shell"),
                        (SolidPage::Draft, "Draft"),
                        (SolidPage::Split, "Split"),
                        (SolidPage::Dome, "Dome"),
                        (SolidPage::Rib, "Rib"),
                    ] {
                        ui.selectable_value(&mut self.page, page, t(label));
                    }
                });
                ui.separator();
                let center = selection_bounds(session).map(|b| b.center());
                match self.page {
                    SolidPage::Hole => {
                        ui.label(t("Select the face to drill; the hole goes at the selection's center."));
                        egui::Grid::new("hole").num_columns(2).show(ui, |ui| {
                            ui.label(t("Kind"));
                            ui.horizontal_wrapped(|ui| {
                                for i in 0..HOLE_KINDS.len() {
                                    ui.selectable_value(&mut self.kind, i, hole_kind_label(i));
                                }
                            });
                            ui.end_row();
                            if self.kind == 0 {
                                ui.label(t("Diameter"));
                                ui.add(egui::DragValue::new(&mut self.diameter_mm).range(0.1..=10_000.0).suffix(" mm"));
                            } else {
                                ui.label(t("Size"));
                                egui::ComboBox::from_id_salt("hole size")
                                    .selected_text(planyrith_core::commands::solid_features::METRIC[self.size].0)
                                    .show_ui(ui, |ui| {
                                        for (i, (name, _)) in planyrith_core::commands::solid_features::METRIC.iter().enumerate() {
                                            ui.selectable_value(&mut self.size, i, *name);
                                        }
                                    });
                            }
                            ui.end_row();
                            ui.label(t("Through all"));
                            ui.checkbox(&mut self.through, "");
                            ui.end_row();
                            if !self.through {
                                ui.label(t("Depth"));
                                ui.add(egui::DragValue::new(&mut self.depth_mm).range(0.1..=10_000.0).suffix(" mm"));
                                ui.end_row();
                            }
                        });
                        if ui.button(t("Make Hole")).clicked() {
                            match center {
                                None => self.note = Some(t("Select something first.")),
                                Some(c) => {
                                    let mut params = json!({
                                        "face": "selection & face", "center": c.to_array(),
                                        "kind": HOLE_KINDS[self.kind],
                                    });
                                    if self.kind == 0 {
                                        params["diameter"] = json!(self.diameter_mm);
                                    } else {
                                        params["size"] = json!(planyrith_core::commands::solid_features::METRIC[self.size].0);
                                    }
                                    if !self.through {
                                        params["depth"] = json!(self.depth_mm);
                                    }
                                    self.run(session, "hole", params);
                                }
                            }
                        }
                    }
                    SolidPage::Shell => {
                        ui.label(t("Select the faces to open, then:"));
                        ui.horizontal(|ui| {
                            ui.label(t("Thickness"));
                            ui.add(egui::DragValue::new(&mut self.thickness_mm).range(0.01..=10_000.0).suffix(" mm"));
                        });
                        if ui.button(t("Shell")).clicked() {
                            self.run(session, "shell", json!({ "faces": "selection & face", "thickness": self.thickness_mm }));
                        }
                    }
                    SolidPage::Draft => {
                        ui.label(t("Select the side faces; they lean in going up the blue axis from the ground."));
                        ui.horizontal(|ui| {
                            ui.label(t("Angle"));
                            ui.add(egui::DragValue::new(&mut self.angle_deg).range(-60.0..=60.0).suffix("°"));
                        });
                        if ui.button(t("Draft")).clicked() {
                            self.run(session, "draft", json!({ "faces": "selection & face", "angle": self.angle_deg.to_radians() }));
                        }
                    }
                    SolidPage::Split => {
                        ui.label(t("Select a face of the solid; it is cut through the selection's center:"));
                        egui::ComboBox::from_id_salt("split plane")
                            .selected_text(self.plane.label())
                            .show_ui(ui, |ui| {
                                for p in [Plane::RedBlue, Plane::GreenBlue, Plane::RedGreen] {
                                    ui.selectable_value(&mut self.plane, p, p.label());
                                }
                            });
                        if ui.button(t("Split")).clicked() {
                            match center {
                                None => self.note = Some(t("Select something first.")),
                                Some(c) => self.run(
                                    session,
                                    "split-solid",
                                    json!({ "face": "selection & face", "point": c.to_array(),
                                            "normal": self.plane.normal().to_array() }),
                                ),
                            }
                        }
                    }
                    SolidPage::Dome => {
                        ui.label(t("Select a flat face, then:"));
                        ui.horizontal(|ui| {
                            ui.label(t("Height"));
                            ui.add(egui::DragValue::new(&mut self.height_mm).range(0.1..=10_000.0).suffix(" mm"));
                        });
                        if ui.button(t("Dome")).clicked() {
                            self.run(session, "dome", json!({ "face": "selection & face", "height": self.height_mm }));
                        }
                    }
                    SolidPage::Rib => {
                        ui.label(t("Select the rib's line and a face of the solid it joins; the rib rises along the blue axis."));
                        egui::Grid::new("rib").num_columns(2).show(ui, |ui| {
                            ui.label(t("Thickness"));
                            ui.add(egui::DragValue::new(&mut self.thickness_mm).range(0.01..=10_000.0).suffix(" mm"));
                            ui.end_row();
                            ui.label(t("Height"));
                            ui.add(egui::DragValue::new(&mut self.height_mm).range(0.01..=10_000.0).suffix(" mm"));
                            ui.end_row();
                        });
                        if ui.button(t("Rib")).clicked() {
                            self.run(
                                session,
                                "rib",
                                json!({ "edge": "selection & edge", "solid": "selection & face",
                                        "thickness": self.thickness_mm, "height": self.height_mm }),
                            );
                        }
                    }
                }
                if let Some(n) = &self.note {
                    ui.label(n);
                }
            });
        self.open = open;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_cut_list_copies_as_quoted_csv() {
        let out = json!({ "rows": [
            { "name": "Side \"L\"", "count": 2, "length_mm": 800.0, "width_mm": 400.0,
              "thickness_mm": 18.0, "material": "Oak" } ] });
        assert_eq!(
            cut_list_csv(&out),
            "name,count,length_mm,width_mm,thickness_mm,material\n\"Side \"\"L\"\"\",2,800.0,400.0,18.0,\"Oak\"\n"
        );
    }

    #[test]
    fn the_windows_run_their_commands_on_the_selection() {
        let mut s = Session::new();
        s.execute(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 100, 0] }),
        )
        .unwrap();
        s.execute(
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": 50 }),
        )
        .unwrap();
        s.execute("make-group", json!({ "targets": "all" }))
            .unwrap();
        s.execute("select", json!({ "query": "group" })).unwrap();
        let out = s
            .execute(
                "pattern",
                json!({ "targets": "selection", "kind": "linear", "count": 3,
                        "direction": [1, 0, 0], "spacing": 200 }),
            )
            .unwrap();
        assert_eq!(copies_note(&out), "2 pieces made.");
        let mass = s.execute("mass-properties", json!({})).unwrap();
        assert_eq!(mass["objects"].as_array().unwrap().len(), 3);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod gui_tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use serde_json::json;

    use crate::app::App;
    use crate::i18n::t;

    /// Tools › Measure… computes the mass properties of the model, and
    /// Tools › Pattern… repeats the selection, through the real menus.
    #[test]
    fn measure_and_pattern_through_the_menus() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_eframe(|cc| App::new(cc, None));
        harness.state_mut().run_for_screens(&[
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 100, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 100 }),
            ),
            ("make-group", json!({ "targets": "all", "name": "Cube" })),
            ("select", json!({ "query": "group" })),
        ]);
        harness.run_steps(2);
        harness.get_by_label(&t("Tools")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("Pattern…")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("Make Pattern")).click();
        harness.run_steps(2);
        assert!(harness.query_by_label_contains("3 pieces made").is_some());
        harness.get_by_label(&t("Tools")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("Measure…")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("Compute")).click();
        harness.run_steps(2);
        assert!(harness.query_by_label("Total").is_some());
        // The cube's row and the total.
        assert_eq!(harness.query_all_by_label("1000.00 cm³").count(), 2);
    }

    /// Tools › Features… makes a part; what is drawn in it is recorded, and
    /// suppressing the extrusion flattens it.
    #[test]
    fn features_are_recorded_while_drawing_in_a_part() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_eframe(|cc| App::new(cc, None));
        harness.run_steps(2);
        harness.get_by_label(&t("Tools")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("Features…")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("New Part")).click();
        harness.run_steps(2);
        harness.state_mut().run_for_screens(&[
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 100, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 40 }),
            ),
        ]);
        harness.run_steps(2);
        let volume = |h: &Harness<'_, App>| {
            let doc = h.state().session_for_tests().doc();
            doc.definitions
                .iter()
                .map(|(_, d)| d.entities.mesh().volume().unwrap_or(0.0))
                .sum::<f64>()
        };
        assert!((volume(&harness) - 400_000.0).abs() < 1e-6);
        // Both steps are features; unchecking the second suppresses it.
        assert!(harness.query_by_label_contains("2. Push/Pull").is_some());
        let boxes: Vec<_> = harness
            .query_all_by_role(egui::accesskit::Role::CheckBox)
            .collect();
        // The recording switch, then one per feature.
        assert_eq!(boxes.len(), 3);
        boxes[2].click();
        harness.run_steps(2);
        assert!(volume(&harness).abs() < 1e-6, "the extrusion is suppressed");
    }

    /// Tools › Solid Features… drills a tapped hole in the selected face.
    #[test]
    fn a_tapped_hole_through_the_window() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_eframe(|cc| App::new(cc, None));
        harness.state_mut().run_for_screens(&[
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 100, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 20 }),
            ),
            ("select", json!({ "query": "face & plane:xy@20" })),
        ]);
        harness.run_steps(2);
        harness.get_by_label(&t("Tools")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("Solid Features…")).click();
        harness.run_steps(2);
        harness.run_steps(4);
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, &t("Tapped"))
            .click();
        harness.run_steps(4);
        harness.get_by_label(&t("Make Hole")).click();
        harness.run_steps(2);
        let doc = harness.state().session_for_tests().doc().clone();
        let threaded = doc
            .model
            .iter()
            .filter(|id| {
                doc.model
                    .attrs(*id)
                    .and_then(|a| a.get("thread", "designation").cloned())
                    == Some(planyrith_core::document::Value::Str("M8x1.25".into()))
            })
            .count();
        assert!(threaded > 0, "the bore carries the thread");
        let volume = harness
            .state()
            .session_for_tests()
            .doc()
            .model
            .mesh()
            .volume()
            .unwrap();
        assert!(volume < 200_000.0);
    }

    /// Tools › Constraints… squares a skewed quad's bottom edge and gives
    /// it a length.
    #[test]
    fn constraints_through_the_window() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_eframe(|cc| App::new(cc, None));
        harness.state_mut().run_for_screens(&[
            (
                "line",
                json!({ "points": [[0, 0, 0], [1000, 20, 0], [990, 600, 0], [-10, 580, 0]],
                        "close": true }),
            ),
            ("select", json!({ "query": "@1" })),
        ]);
        harness.run_steps(2);
        harness.get_by_label(&t("Tools")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("Constraints…")).click();
        harness.run_steps(2);
        harness.get_by_label(&t("Along red")).click();
        harness.run_steps(2);
        assert!(
            harness
                .query_all_by_label_contains("degrees of freedom")
                .count()
                > 0
        );
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, &t("Length"))
            .click();
        harness.run_steps(2);
        let doc = harness.state().session_for_tests().doc().clone();
        let mesh = doc.model.mesh();
        let (e, _) = mesh
            .edges()
            .find(|(e, _)| {
                doc.model
                    .pid_of(planyrith_core::ids::EntityId::Edge(*e))
                    .map(|p| p.0)
                    == Some(1)
            })
            .unwrap();
        let s = mesh.edge_segment(e).unwrap();
        assert!((s.a.y - s.b.y).abs() < 1e-6, "along red");
        assert!((s.a.distance(s.b) - 1000.0).abs() < 1e-6, "the length");
    }
}
