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

/// One side of a mate picked in the GUI: the assembly's context, and the
/// `<occurrence>:<element>` reference.
#[derive(Debug, Clone, PartialEq)]
struct Side {
    assembly: planyrith_core::document::InstancePath,
    reference: String,
}

/// The side the selection names: a face or edge selected inside an
/// occurrence one level below the edit context, or selected in an
/// occurrence open for editing.
fn side_of_selection(session: &Session) -> Option<Side> {
    use planyrith_core::document::{EntityKey, InstancePath};
    let doc = session.doc();
    let context = session.engine.context().path.clone();
    let pid_in = |container: &InstancePath, key: EntityKey| -> Option<u64> {
        let (c, _) = doc.resolve_container(container).ok()?;
        let entities = doc.container(c)?;
        let id = entities.key_to_id(key)?;
        entities.pid_of(id).map(|p| p.0)
    };
    for path in session.engine.selection().iter() {
        let steps = &path.instances.steps;
        // Picked from the assembly, one level down.
        if steps.len() == context.steps.len() + 1
            && steps[..context.steps.len()] == context.steps[..]
        {
            let occ = pid_in(&context, steps[steps.len() - 1])?;
            let element = pid_in(&path.instances, path.target)?;
            return Some(Side {
                assembly: context.clone(),
                reference: format!("{occ}:{element}"),
            });
        }
        // Picked inside the open occurrence.
        if *steps == context.steps && !context.steps.is_empty() {
            let parent = InstancePath {
                steps: context.steps[..context.steps.len() - 1].to_vec(),
            };
            let occ = pid_in(&parent, context.steps[context.steps.len() - 1])?;
            let element = pid_in(&context, path.target)?;
            return Some(Side {
                assembly: parent,
                reference: format!("{occ}:{element}"),
            });
        }
    }
    None
}

/// Tools › Assembly… (docs/05 §5.17): mates between components, driven
/// values, the exploded view and the bill of materials.
pub struct AssemblyWindow {
    /// The window is shown.
    pub open: bool,
    a: Option<Side>,
    b: Option<Side>,
    length_mm: f64,
    angle_deg: f64,
    factor: f64,
    list: Option<Value>,
    bom: Option<Value>,
    seen: u64,
    note: Option<String>,
}

impl Default for AssemblyWindow {
    fn default() -> Self {
        AssemblyWindow {
            open: false,
            a: None,
            b: None,
            length_mm: 0.0,
            angle_deg: 90.0,
            factor: 1.0,
            list: None,
            bom: None,
            seen: u64::MAX,
            note: None,
        }
    }
}

impl AssemblyWindow {
    fn refresh(&mut self, session: &mut Session) {
        self.seen = session.engine.generation();
        self.list = session.execute("mate-list", json!({})).ok();
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

    fn add(&mut self, session: &mut Session, kind: &str) {
        let (Some(a), Some(b)) = (self.a.clone(), self.b.clone()) else {
            self.note = Some(t("Pick both sides first."));
            return;
        };
        if a.assembly != b.assembly {
            self.note = Some(t("Both sides must be in the same assembly."));
            return;
        }
        let _ = session
            .engine
            .set_context(planyrith_core::document::EditContext {
                path: a.assembly.clone(),
            });
        let mut params = json!({ "kind": kind, "a": a.reference, "b": b.reference });
        match kind {
            "distance" => params["length"] = json!(self.length_mm),
            "angle" => params["angle"] = json!(self.angle_deg.to_radians()),
            "aligned" => {
                params["kind"] = json!("coincident");
                params["aligned"] = json!(true);
            }
            _ => {}
        }
        self.run(session, "mate-add", params);
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            self.list = None;
            return;
        }
        if self.list.is_none() || self.seen != session.engine.generation() {
            self.refresh(session);
        }
        let mut open = true;
        egui::Window::new(t("Assembly"))
            .open(&mut open)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.label(t("Select a face or edge of a component (open it to reach one), then set it as a side:"));
                ui.horizontal(|ui| {
                    if ui.button(t("Side A")).clicked() {
                        self.a = side_of_selection(session);
                    }
                    ui.label(self.a.as_ref().map_or("—".to_string(), |s| s.reference.clone()));
                    if ui.button(t("Side B")).clicked() {
                        self.b = side_of_selection(session);
                    }
                    ui.label(self.b.as_ref().map_or("—".to_string(), |s| s.reference.clone()));
                });
                ui.horizontal_wrapped(|ui| {
                    for (label, kind) in [
                        (t("Coincident"), "coincident"),
                        (t("Aligned"), "aligned"),
                        (t("Parallel"), "parallel"),
                        (t("Perpendicular"), "perpendicular"),
                        (t("Concentric"), "concentric"),
                    ] {
                        if ui.button(label).clicked() {
                            self.add(session, kind);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.length_mm).suffix(" mm"));
                    if ui.button(t("Distance")).clicked() {
                        self.add(session, "distance");
                    }
                    ui.add(egui::DragValue::new(&mut self.angle_deg).range(0.0..=180.0).suffix("°"));
                    if ui.button(t("Angle")).clicked() {
                        self.add(session, "angle");
                    }
                    if ui.button(t("Fix")).clicked() {
                        if let Some(a) = self.a.clone() {
                            let occ = a.reference.split(':').next().unwrap_or_default().to_string();
                            let _ = session.engine.set_context(
                                planyrith_core::document::EditContext { path: a.assembly },
                            );
                            self.run(session, "mate-add", json!({ "kind": "fixed", "a": occ }));
                        } else {
                            self.note = Some(t("Pick both sides first."));
                        }
                    }
                });
                ui.separator();
                if let Some(list) = self.list.clone() {
                    let state = match list["state"].as_str().unwrap_or_default() {
                        "fully" => t("Fully constrained"),
                        "under" => t_args(
                            "Under-constrained: { $count } degrees of freedom",
                            &[("count", list["degrees_of_freedom"].as_u64().unwrap_or(0).to_string().into())],
                        ),
                        "conflict" => t("The mates conflict"),
                        _ => t("No mates yet"),
                    };
                    ui.strong(state);
                    let mut action: Option<(&str, Value)> = None;
                    egui::Grid::new("mates").striped(true).num_columns(4).show(ui, |ui| {
                        for m in list["mates"].as_array().into_iter().flatten() {
                            let id = m["id"].as_u64().unwrap_or(0);
                            ui.label(id.to_string());
                            ui.label(format!(
                                "{} {} {}",
                                m["kind"].as_str().unwrap_or_default(),
                                m["a"].as_str().unwrap_or_default(),
                                m["b"].as_str().unwrap_or_default()
                            ));
                            if let Some(mut l) = m["length_mm"].as_f64() {
                                // Dragging drives the mate.
                                if ui.add(egui::DragValue::new(&mut l).suffix(" mm")).changed() {
                                    action = Some(("mate-edit", json!({ "id": id, "length": l })));
                                }
                            } else if let Some(a) = m["angle_rad"].as_f64() {
                                let mut d = a.to_degrees();
                                if ui.add(egui::DragValue::new(&mut d).range(0.0..=180.0).suffix("°")).changed() {
                                    action = Some(("mate-edit", json!({ "id": id, "angle": d.to_radians() })));
                                }
                            } else {
                                ui.label("");
                            }
                            if ui.small_button("✕").on_hover_text(t("Delete")).clicked() {
                                action = Some(("mate-delete", json!({ "ids": [id] })));
                            }
                            ui.end_row();
                        }
                    });
                    if ui.button(t("Solve Again")).clicked() {
                        action = Some(("mate-solve", json!({})));
                    }
                    if let Some((name, params)) = action {
                        self.run(session, name, params);
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(t("Spread"));
                    ui.add(egui::DragValue::new(&mut self.factor).range(0.0..=10.0).speed(0.05));
                    if ui.button(t("Explode")).clicked() {
                        self.run(session, "explode-view", json!({ "factor": self.factor }));
                    }
                    if ui.button(t("Collapse")).clicked() {
                        self.run(session, "explode-view", json!({ "collapse": true }));
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button(t("Bill of Materials")).clicked() {
                        self.bom = session.execute("bill-of-materials", json!({ "nested": true })).ok();
                    }
                    if let Some(bom) = &self.bom
                        && ui.button(t("Copy as CSV")).clicked()
                    {
                        let mut csv = String::from("item,name,quantity,part_number,description\n");
                        for r in bom["rows"].as_array().into_iter().flatten() {
                            let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
                            csv.push_str(&format!(
                                "{},{},{},{},{}\n",
                                r["item"],
                                q(r["name"].as_str().unwrap_or_default()),
                                r["quantity"],
                                q(r["part_number"].as_str().unwrap_or_default()),
                                q(r["description"].as_str().unwrap_or_default())
                            ));
                        }
                        ui.ctx().copy_text(csv);
                    }
                });
                if let Some(bom) = &self.bom {
                    egui::Grid::new("bom").striped(true).num_columns(3).show(ui, |ui| {
                        for r in bom["rows"].as_array().into_iter().flatten() {
                            ui.label(r["item"].to_string());
                            ui.label(r["name"].as_str().unwrap_or_default());
                            ui.label(format!("× {}", r["quantity"]));
                            ui.end_row();
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

/// Tools › Fabrication… (docs/05 §8.12–§8.13): sheet-metal parts and frame
/// members from the selection.
pub struct FabricationWindow {
    /// The window is shown.
    pub open: bool,
    frames: bool,
    thickness_mm: f64,
    radius_mm: f64,
    k_factor: f64,
    edge: usize,
    angle_deg: f64,
    length_mm: f64,
    profile: String,
    miter: bool,
    note: Option<String>,
}

impl Default for FabricationWindow {
    fn default() -> Self {
        FabricationWindow {
            open: false,
            frames: false,
            thickness_mm: 2.0,
            radius_mm: 2.0,
            k_factor: 0.44,
            edge: 0,
            angle_deg: 90.0,
            length_mm: 30.0,
            profile: "square-tube 40x40x3".into(),
            miter: true,
            note: None,
        }
    }
}

/// Profiles offered by the frame page.
const PROFILES: [&str; 8] = [
    "square-tube 40x40x3",
    "rect-tube 60x40x3",
    "round-tube 42.4x3.2",
    "angle 50x50x5",
    "channel 100x50x6",
    "flat 40x5",
    "IPE100",
    "IPE200",
];

impl FabricationWindow {
    fn run(&mut self, session: &mut Session, name: &str, params: Value) {
        self.note = Some(match session.execute(name, params) {
            Ok(out) => {
                if let Some(v) = out["volume_mm3"].as_f64() {
                    t_args(
                        "Solid made: { $volume } cm³.",
                        &[("volume", format!("{:.1}", v / 1000.0).into())],
                    )
                } else if let Some(size) = out["size_mm"].as_array() {
                    t_args(
                        "Blank { $w } × { $h } mm.",
                        &[
                            (
                                "w",
                                format!("{:.1}", size[0].as_f64().unwrap_or(0.0)).into(),
                            ),
                            (
                                "h",
                                format!("{:.1}", size[1].as_f64().unwrap_or(0.0)).into(),
                            ),
                        ],
                    )
                } else if let Some(n) = out["members"].as_array() {
                    t_args(
                        "{ $count } pieces made.",
                        &[("count", n.len().to_string().into())],
                    )
                } else {
                    t("Done")
                }
            }
            Err(e) => e,
        });
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Fabrication"))
            .open(&mut open)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.frames, false, t("Sheet metal"));
                    ui.selectable_value(&mut self.frames, true, t("Frames"));
                });
                ui.separator();
                if self.frames {
                    ui.label(t("Select the center lines (edges), then:"));
                    egui::ComboBox::from_id_salt("frame profile")
                        .selected_text(self.profile.clone())
                        .show_ui(ui, |ui| {
                            for p in PROFILES {
                                ui.selectable_value(&mut self.profile, p.to_string(), p);
                            }
                        });
                    ui.text_edit_singleline(&mut self.profile);
                    ui.checkbox(&mut self.miter, t("Miter corners"));
                    if ui.button(t("Make Members")).clicked() {
                        self.run(
                            session,
                            "frame-member",
                            json!({ "path": "selection & edge", "profile": self.profile, "miter": self.miter }),
                        );
                    }
                } else {
                    egui::Grid::new("sheet metal").num_columns(2).show(ui, |ui| {
                        ui.label(t("Thickness"));
                        ui.add(egui::DragValue::new(&mut self.thickness_mm).range(0.1..=100.0).suffix(" mm"));
                        ui.end_row();
                        ui.label(t("Bend radius"));
                        ui.add(egui::DragValue::new(&mut self.radius_mm).range(0.0..=100.0).suffix(" mm"));
                        ui.end_row();
                        ui.label(t("K factor"));
                        ui.add(egui::DragValue::new(&mut self.k_factor).range(0.0..=1.0).speed(0.01));
                        ui.end_row();
                    });
                    if ui.button(t("Sheet from Selected Face")).clicked() {
                        self.run(
                            session,
                            "sheet-metal-base",
                            json!({ "profile": "selection & face", "thickness": self.thickness_mm,
                                    "bend_radius": self.radius_mm, "k_factor": self.k_factor }),
                        );
                    }
                    ui.separator();
                    ui.label(t("Select the sheet-metal part, then:"));
                    egui::Grid::new("flange").num_columns(2).show(ui, |ui| {
                        ui.label(t("Outline edge"));
                        ui.add(egui::DragValue::new(&mut self.edge).range(0..=999));
                        ui.end_row();
                        ui.label(t("Angle"));
                        ui.add(egui::DragValue::new(&mut self.angle_deg).range(1.0..=180.0).suffix("°"));
                        ui.end_row();
                        ui.label(t("Length"));
                        ui.add(egui::DragValue::new(&mut self.length_mm).range(0.1..=10_000.0).suffix(" mm"));
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        if ui.button(t("Add Flange")).clicked() {
                            self.run(
                                session,
                                "sheet-metal-flange",
                                json!({ "part": "selection", "edge": self.edge,
                                        "angle": self.angle_deg.to_radians(), "length": self.length_mm }),
                            );
                        }
                        if ui.button(t("Apply Gauge")).clicked() {
                            self.run(
                                session,
                                "sheet-metal-set",
                                json!({ "part": "selection", "thickness": self.thickness_mm,
                                        "bend_radius": self.radius_mm, "k_factor": self.k_factor }),
                            );
                        }
                        if ui.button(t("Flat Pattern")).clicked() {
                            self.run(session, "sheet-metal-flat", json!({ "part": "selection" }));
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

/// Three drag fields for a point or direction, mm.
fn xyz(ui: &mut egui::Ui, v: &mut [f64; 3]) {
    ui.horizontal(|ui| {
        for c in v.iter_mut() {
            ui.add(egui::DragValue::new(c).speed(1.0));
        }
    });
}

/// A command's result as a short note.
fn surface_note(out: &Value) -> String {
    if let Some(n) = out["trimmed"].as_u64() {
        return t_args(
            "{ $count } faces trimmed.",
            &[("count", n.to_string().into())],
        );
    }
    let mut s = t_args(
        "{ $count } faces, { $area } m².",
        &[
            (
                "count",
                out["faces"].as_u64().unwrap_or(0).to_string().into(),
            ),
            (
                "area",
                format!("{:.3}", out["area_mm2"].as_f64().unwrap_or(0.0) / 1.0e6).into(),
            ),
        ],
    );
    if out["closed"].as_bool() == Some(true) {
        s.push(' ');
        s.push_str(&t("The surfaces close a solid."));
    }
    s
}

/// Tools › Surfaces… (docs/05 §8.14): filling, extending, trimming and
/// knitting surfaces from the selection.
pub struct SurfacesWindow {
    /// The window is shown.
    pub open: bool,
    divisions: u32,
    method: usize,
    distance_mm: f64,
    point: [f64; 3],
    normal: [f64; 3],
    note: Option<String>,
}

impl Default for SurfacesWindow {
    fn default() -> Self {
        SurfacesWindow {
            open: false,
            divisions: 8,
            method: 0,
            distance_mm: 100.0,
            point: [0.0; 3],
            normal: [1.0, 0.0, 0.0],
            note: None,
        }
    }
}

impl SurfacesWindow {
    fn run(&mut self, session: &mut Session, name: &str, params: Value) {
        self.note = Some(match session.execute(name, params) {
            Ok(out) => surface_note(&out),
            Err(e) => e,
        });
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Surfaces"))
            .open(&mut open)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.label(t("Select the boundary edges (one closed chain), then:"));
                ui.horizontal(|ui| {
                    ui.label(t("Divisions"));
                    ui.add(egui::DragValue::new(&mut self.divisions).range(1..=200));
                    let methods = [t("Automatic"), t("Four sides"), t("Radial")];
                    egui::ComboBox::from_id_salt("fill method")
                        .selected_text(methods[self.method].clone())
                        .show_ui(ui, |ui| {
                            for (i, m) in methods.iter().enumerate() {
                                ui.selectable_value(&mut self.method, i, m.clone());
                            }
                        });
                });
                if ui.button(t("Fill Surface")).clicked() {
                    let method = ["auto", "coons", "radial"][self.method];
                    self.run(
                        session,
                        "surface-fill",
                        json!({ "boundary": "selection & edge", "divisions": self.divisions, "method": method }),
                    );
                }
                ui.separator();
                ui.label(t("Select open edges of a surface, then:"));
                ui.horizontal(|ui| {
                    ui.label(t("Distance"));
                    ui.add(egui::DragValue::new(&mut self.distance_mm).range(0.1..=100_000.0).suffix(" mm"));
                    if ui.button(t("Extend Surface")).clicked() {
                        self.run(
                            session,
                            "surface-extend",
                            json!({ "edges": "selection & edge", "distance": self.distance_mm }),
                        );
                    }
                });
                ui.separator();
                ui.label(t("Select the surface, then cut it with a plane:"));
                egui::Grid::new("trim plane").num_columns(2).show(ui, |ui| {
                    ui.label(t("Point"));
                    xyz(ui, &mut self.point);
                    ui.end_row();
                    ui.label(t("Removed side"));
                    xyz(ui, &mut self.normal);
                    ui.end_row();
                });
                if ui.button(t("Trim Surface")).clicked() {
                    self.run(
                        session,
                        "surface-trim",
                        json!({ "surface": "selection", "point": self.point, "normal": self.normal }),
                    );
                }
                ui.separator();
                ui.label(t("Select two or more surface groups, then:"));
                if ui.button(t("Knit Surfaces")).clicked() {
                    self.run(session, "surface-knit", json!({ "surfaces": "selection & group" }));
                }
                if let Some(n) = &self.note {
                    ui.label(n);
                }
            });
        self.open = open;
    }
}

/// The analyses of Tools › Analysis….
#[derive(Clone, Copy, PartialEq, Eq)]
enum Analysis {
    Curvature,
    Zebra,
    Draft,
    Thickness,
    Comb,
}

/// Tools › Analysis… (docs/05 §8.15): curvature, zebra stripes, draft,
/// wall thickness and curvature combs of the selection, shown as colored
/// copies.
pub struct AnalysisWindow {
    /// The window is shown.
    pub open: bool,
    kind: Analysis,
    direction: [f64; 3],
    stripes: u32,
    angle_deg: f64,
    minimum_mm: f64,
    rows: Vec<(String, u64, f64)>,
    note: Option<String>,
}

impl Default for AnalysisWindow {
    fn default() -> Self {
        AnalysisWindow {
            open: false,
            kind: Analysis::Draft,
            direction: [0.0, 0.0, 1.0],
            stripes: 12,
            angle_deg: 1.0,
            minimum_mm: 2.0,
            rows: Vec::new(),
            note: None,
        }
    }
}

impl AnalysisWindow {
    fn command(&self) -> (&'static str, Value) {
        match self.kind {
            Analysis::Curvature => ("analysis-curvature", json!({ "faces": "selection" })),
            Analysis::Zebra => (
                "analysis-zebra",
                json!({ "faces": "selection", "direction": self.direction, "stripes": self.stripes }),
            ),
            Analysis::Draft => (
                "analysis-draft",
                json!({ "faces": "selection", "pull": self.direction, "angle": self.angle_deg.to_radians() }),
            ),
            Analysis::Thickness => (
                "analysis-thickness",
                json!({ "faces": "selection", "minimum": self.minimum_mm }),
            ),
            Analysis::Comb => ("analysis-comb", json!({ "edges": "selection & edge" })),
        }
    }

    fn run(&mut self, session: &mut Session, name: &str, params: Value) {
        self.rows.clear();
        self.note = Some(match session.execute(name, params) {
            Ok(out) => {
                if let Some(classes) = out["classes"].as_array() {
                    self.rows = classes
                        .iter()
                        .map(|c| {
                            (
                                c["name"].as_str().unwrap_or_default().to_string(),
                                c["faces"].as_u64().unwrap_or(0),
                                c["area_mm2"].as_f64().unwrap_or(0.0),
                            )
                        })
                        .collect();
                }
                let (lo, hi) = if out.get("teeth").is_some() {
                    (&out["min_radius_mm"], &out["max_radius_mm"])
                } else {
                    (&out["min"], &out["max"])
                };
                match (lo.as_f64(), hi.as_f64()) {
                    (Some(a), Some(b)) => t_args(
                        "From { $min } to { $max }.",
                        &[
                            ("min", format!("{a:.2}").into()),
                            ("max", format!("{b:.2}").into()),
                        ],
                    ),
                    _ if out.get("removed").is_some() => t_args(
                        "{ $count } analyses removed.",
                        &[(
                            "count",
                            out["removed"].as_u64().unwrap_or(0).to_string().into(),
                        )],
                    ),
                    _ => t("Done"),
                }
            }
            Err(e) => e,
        });
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Analysis"))
            .open(&mut open)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.kind, Analysis::Curvature, t("Curvature"));
                    ui.selectable_value(&mut self.kind, Analysis::Zebra, t("Zebra"));
                    ui.selectable_value(&mut self.kind, Analysis::Draft, t("Draft"));
                    ui.selectable_value(&mut self.kind, Analysis::Thickness, t("Thickness"));
                    ui.selectable_value(&mut self.kind, Analysis::Comb, t("Comb"));
                });
                ui.separator();
                match self.kind {
                    Analysis::Comb => {
                        ui.label(t("Select a chain of edges, then:"));
                    }
                    _ => {
                        ui.label(t("Select faces, groups or components, then:"));
                    }
                }
                egui::Grid::new("analysis")
                    .num_columns(2)
                    .show(ui, |ui| match self.kind {
                        Analysis::Zebra => {
                            ui.label(t("Direction"));
                            xyz(ui, &mut self.direction);
                            ui.end_row();
                            ui.label(t("Stripes"));
                            ui.add(egui::DragValue::new(&mut self.stripes).range(2..=360));
                            ui.end_row();
                        }
                        Analysis::Draft => {
                            ui.label(t("Pull direction"));
                            xyz(ui, &mut self.direction);
                            ui.end_row();
                            ui.label(t("Least draft"));
                            ui.add(
                                egui::DragValue::new(&mut self.angle_deg)
                                    .range(0.0..=89.0)
                                    .suffix("°"),
                            );
                            ui.end_row();
                        }
                        Analysis::Thickness => {
                            ui.label(t("Least thickness"));
                            ui.add(
                                egui::DragValue::new(&mut self.minimum_mm)
                                    .range(0.01..=10_000.0)
                                    .suffix(" mm"),
                            );
                            ui.end_row();
                        }
                        _ => {}
                    });
                ui.horizontal(|ui| {
                    if ui.button(t("Analyze")).clicked() {
                        let (name, params) = self.command();
                        self.run(session, name, params);
                    }
                    if ui.button(t("Clear Analysis")).clicked() {
                        self.run(session, "analysis-clear", json!({}));
                    }
                });
                if !self.rows.is_empty() {
                    egui::Grid::new("analysis classes")
                        .striped(true)
                        .show(ui, |ui| {
                            for (name, faces, area) in &self.rows {
                                ui.label(name);
                                ui.label(faces.to_string());
                                ui.label(format!("{:.4} m²", area / 1.0e6));
                                ui.end_row();
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

/// The selected entities of one kind as a query of their pids
/// (`@12|@15`), each in its own container.
fn selected_query(session: &Session, faces: bool) -> Option<String> {
    use planyrith_core::ids::EntityId;
    let doc = session.doc();
    let mut out: Vec<String> = Vec::new();
    for path in session.engine.selection().iter() {
        let Ok((c, _)) = doc.resolve_container(&path.instances) else {
            continue;
        };
        let Some(entities) = doc.container(c) else {
            continue;
        };
        let Some(id) = entities.key_to_id(path.target) else {
            continue;
        };
        let wanted = match id {
            EntityId::Face(_) => faces,
            EntityId::Group(_) | EntityId::Instance(_) => !faces,
            _ => false,
        };
        if wanted && let Some(pid) = entities.pid_of(id) {
            out.push(format!("@{}", pid.0));
        }
    }
    (!out.is_empty()).then(|| out.join("|"))
}

/// The pages of Tools › Simulation….
#[derive(Clone, Copy, PartialEq, Eq)]
enum SimPage {
    Stress,
    Frequency,
    Heat,
    Motion,
}

/// Tools › Simulation… (docs/05 §8.16–§8.17): stress, natural frequencies,
/// heat conduction and motion studies.
pub struct SimulationWindow {
    /// The window is shown.
    pub open: bool,
    page: SimPage,
    body: String,
    material: usize,
    mesh_mm: f64,
    fixed: String,
    load_faces: String,
    pressure: bool,
    force: [f64; 3],
    pressure_mpa: f64,
    gravity: bool,
    scale: f64,
    modes: u32,
    show_mode: u32,
    hot: String,
    hot_power: bool,
    hot_value: f64,
    cold: String,
    cold_c: f64,
    mate: u32,
    to_value: f64,
    steps: u32,
    interference: bool,
    lines: Vec<String>,
    note: Option<String>,
}

impl Default for SimulationWindow {
    fn default() -> Self {
        SimulationWindow {
            open: false,
            page: SimPage::Stress,
            body: "selection".into(),
            material: 0,
            mesh_mm: 0.0,
            fixed: String::new(),
            load_faces: String::new(),
            pressure: false,
            force: [0.0, 0.0, -1000.0],
            pressure_mpa: 1.0,
            gravity: false,
            scale: 0.0,
            modes: 4,
            show_mode: 1,
            hot: String::new(),
            hot_power: true,
            hot_value: 10.0,
            cold: String::new(),
            cold_c: 20.0,
            mate: 1,
            to_value: 50.0,
            steps: 10,
            interference: true,
            lines: Vec::new(),
            note: None,
        }
    }
}

impl SimulationWindow {
    fn base(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("body".into(), json!(self.body));
        m.insert(
            "material".into(),
            json!(planyrith_core::sim::MATERIALS[self.material]),
        );
        if self.mesh_mm > 0.0 {
            m.insert("mesh_size".into(), json!(self.mesh_mm));
        }
        m
    }

    /// The command and parameters of the open page.
    fn command(&self) -> (&'static str, Value) {
        let mut m = self.base();
        match self.page {
            SimPage::Stress => {
                m.insert("fixed".into(), json!(self.fixed));
                let mut loads = Vec::new();
                if !self.load_faces.trim().is_empty() {
                    loads.push(if self.pressure {
                        json!({ "faces": self.load_faces, "pressure": self.pressure_mpa })
                    } else {
                        json!({ "faces": self.load_faces, "force": self.force })
                    });
                }
                m.insert("loads".into(), json!(loads));
                m.insert("gravity".into(), json!(self.gravity));
                m.insert("deformation_scale".into(), json!(self.scale));
                ("sim-static", Value::Object(m))
            }
            SimPage::Frequency => {
                m.insert("fixed".into(), json!(self.fixed));
                m.insert("modes".into(), json!(self.modes));
                m.insert("show_mode".into(), json!(self.show_mode.min(self.modes)));
                ("sim-frequency", Value::Object(m))
            }
            SimPage::Heat => {
                let hot = if self.hot_power {
                    json!({ "faces": self.hot, "power": self.hot_value })
                } else {
                    json!({ "faces": self.hot, "temperature": self.hot_value })
                };
                m.insert(
                    "conditions".into(),
                    json!([{ "faces": self.cold, "temperature": self.cold_c }, hot]),
                );
                ("sim-thermal", Value::Object(m))
            }
            SimPage::Motion => (
                "motion-study",
                json!({ "mate": self.mate, "to_length": self.to_value, "steps": self.steps,
                        "interference": self.interference }),
            ),
        }
    }

    fn run(&mut self, session: &mut Session) {
        let (name, params) = self.command();
        let (name, params) = if name == "motion-study" && self.to_value_is_angle(session) {
            let mut p = params;
            if let Some(o) = p.as_object_mut() {
                o.remove("to_length");
                o.insert("to_angle".into(), json!(self.to_value.to_radians()));
            }
            (name, p)
        } else {
            (name, params)
        };
        self.lines.clear();
        self.note = Some(match session.execute(name, params) {
            Ok(out) => self.summary(&out),
            Err(e) => e,
        });
    }

    /// Whether the chosen mate is an angle mate.
    fn to_value_is_angle(&self, session: &mut Session) -> bool {
        session
            .execute("mate-list", json!({}))
            .ok()
            .and_then(|l| {
                l["mates"].as_array().and_then(|m| {
                    m.iter()
                        .find(|x| x["id"].as_u64() == Some(u64::from(self.mate)))
                        .map(|x| x["kind"] == "angle")
                })
            })
            .unwrap_or(false)
    }

    fn summary(&mut self, out: &Value) -> String {
        let f = |v: &Value| v.as_f64().unwrap_or(0.0);
        if let Some(freqs) = out["frequencies_hz"].as_array() {
            self.lines = freqs
                .iter()
                .enumerate()
                .map(|(i, v)| format!("{}: {:.1} Hz", i + 1, f(v)))
                .collect();
            return t_args(
                "{ $count } natural frequencies.",
                &[("count", freqs.len().to_string().into())],
            );
        }
        if let Some(frames) = out["frames"].as_array() {
            self.lines = frames
                .iter()
                .map(|fr| {
                    let hit = fr["collisions"].as_array().is_some_and(|c| !c.is_empty());
                    format!("{:.1}{}", f(&fr["value"]), if hit { " ✗" } else { "" })
                })
                .collect();
            return match out["first_collision"].as_u64() {
                Some(step) => t_args(
                    "Parts collide from step { $step }.",
                    &[("step", step.to_string().into())],
                ),
                None => t("No collisions."),
            };
        }
        if out.get("max_c").is_some() {
            return t_args(
                "From { $min } to { $max } °C.",
                &[
                    ("min", format!("{:.2}", f(&out["min_c"])).into()),
                    ("max", format!("{:.2}", f(&out["max_c"])).into()),
                ],
            );
        }
        let mut s = t_args(
            "Largest stress { $stress } MPa, largest displacement { $disp } mm.",
            &[
                (
                    "stress",
                    format!("{:.2}", f(&out["max_von_mises_mpa"])).into(),
                ),
                (
                    "disp",
                    format!("{:.4}", f(&out["max_displacement_mm"])).into(),
                ),
            ],
        );
        if let Some(sf) = out["safety_factor"].as_f64() {
            s.push(' ');
            s.push_str(&t_args(
                "Safety factor { $factor }.",
                &[("factor", format!("{sf:.2}").into())],
            ));
        }
        s
    }

    fn faces_row(ui: &mut egui::Ui, session: &Session, label: String, text: &mut String) {
        ui.label(label);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(text).desired_width(150.0));
            if ui.button(t("Selected Faces")).clicked()
                && let Some(q) = selected_query(session, true)
            {
                *text = q;
            }
        });
        ui.end_row();
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Simulation"))
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.page, SimPage::Stress, t("Stress"));
                    ui.selectable_value(&mut self.page, SimPage::Frequency, t("Frequencies"));
                    ui.selectable_value(&mut self.page, SimPage::Heat, t("Heat"));
                    ui.selectable_value(&mut self.page, SimPage::Motion, t("Motion"));
                });
                ui.separator();
                if self.page == SimPage::Motion {
                    egui::Grid::new("motion").num_columns(2).show(ui, |ui| {
                        ui.label(t("Mate number"));
                        ui.add(egui::DragValue::new(&mut self.mate).range(1..=9999));
                        ui.end_row();
                        ui.label(t("Drive to"));
                        ui.add(egui::DragValue::new(&mut self.to_value).speed(1.0));
                        ui.end_row();
                        ui.label(t("Steps"));
                        ui.add(egui::DragValue::new(&mut self.steps).range(1..=200));
                        ui.end_row();
                    });
                    ui.checkbox(&mut self.interference, t("Check collisions"));
                } else {
                    ui.label(t(
                        "Name the body and its faces (pick them inside the body, then come back out).",
                    ));
                    egui::Grid::new("sim").num_columns(2).show(ui, |ui| {
                        ui.label(t("Body"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.body).desired_width(150.0));
                            if ui.button(t("Selected Group")).clicked()
                                && let Some(q) = selected_query(session, false)
                            {
                                self.body = q;
                            }
                        });
                        ui.end_row();
                        ui.label(t("Material"));
                        let materials = planyrith_core::sim::MATERIALS;
                        egui::ComboBox::from_id_salt("sim material")
                            .selected_text(materials[self.material])
                            .show_ui(ui, |ui| {
                                for (i, m) in materials.iter().enumerate() {
                                    ui.selectable_value(&mut self.material, i, *m);
                                }
                            });
                        ui.end_row();
                        ui.label(t("Mesh size"));
                        ui.add(egui::DragValue::new(&mut self.mesh_mm).range(0.0..=10_000.0).suffix(" mm"));
                        ui.end_row();
                        match self.page {
                            SimPage::Stress => {
                                Self::faces_row(ui, session, t("Fixed faces"), &mut self.fixed);
                                Self::faces_row(ui, session, t("Loaded faces"), &mut self.load_faces);
                                ui.checkbox(&mut self.pressure, t("Pressure"));
                                if self.pressure {
                                    ui.add(egui::DragValue::new(&mut self.pressure_mpa).speed(0.1).suffix(" MPa"));
                                } else {
                                    ui.horizontal(|ui| {
                                        for c in self.force.iter_mut() {
                                            ui.add(egui::DragValue::new(c).speed(10.0).suffix(" N"));
                                        }
                                    });
                                }
                                ui.end_row();
                                ui.checkbox(&mut self.gravity, t("Own weight"));
                                ui.end_row();
                                ui.label(t("Show deformed ×"));
                                ui.add(egui::DragValue::new(&mut self.scale).range(0.0..=1.0e6));
                                ui.end_row();
                            }
                            SimPage::Frequency => {
                                Self::faces_row(ui, session, t("Fixed faces"), &mut self.fixed);
                                ui.label(t("Frequencies"));
                                ui.add(egui::DragValue::new(&mut self.modes).range(1..=12));
                                ui.end_row();
                                ui.label(t("Show mode"));
                                ui.add(egui::DragValue::new(&mut self.show_mode).range(1..=12));
                                ui.end_row();
                            }
                            SimPage::Heat => {
                                Self::faces_row(ui, session, t("Cooled faces"), &mut self.cold);
                                ui.label(t("Held at"));
                                ui.add(egui::DragValue::new(&mut self.cold_c).speed(1.0).suffix(" °C"));
                                ui.end_row();
                                Self::faces_row(ui, session, t("Heated faces"), &mut self.hot);
                                ui.checkbox(&mut self.hot_power, t("Heat power"));
                                ui.add(
                                    egui::DragValue::new(&mut self.hot_value)
                                        .speed(1.0)
                                        .suffix(if self.hot_power { " W" } else { " °C" }),
                                );
                                ui.end_row();
                            }
                            SimPage::Motion => {}
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if ui.button(t("Run")).clicked() {
                        self.run(session);
                    }
                    if self.page != SimPage::Motion && ui.button(t("Clear Results")).clicked() {
                        self.lines.clear();
                        self.note = Some(match session.execute("analysis-clear", json!({})) {
                            Ok(_) => t("Done"),
                            Err(e) => e,
                        });
                    }
                });
                if let Some(n) = &self.note {
                    ui.label(n);
                }
                for line in &self.lines {
                    ui.label(line);
                }
            });
        self.open = open;
    }
}

/// Tools › Product Data… (docs/05 §5.18): part numbers and properties,
/// revisions and standard parts.
pub struct ProductWindow {
    /// The window is shown.
    pub open: bool,
    part_number: String,
    description: String,
    key: String,
    value: String,
    rows: Vec<(String, String)>,
    revision: String,
    author: String,
    date: String,
    revisions: Vec<String>,
    kind: usize,
    size: usize,
    length_mm: f64,
    note: Option<String>,
}

impl Default for ProductWindow {
    fn default() -> Self {
        ProductWindow {
            open: false,
            part_number: String::new(),
            description: String::new(),
            key: String::new(),
            value: String::new(),
            rows: Vec::new(),
            revision: String::new(),
            author: String::new(),
            date: String::new(),
            revisions: Vec::new(),
            kind: 0,
            size: 4,
            length_mm: 30.0,
            note: None,
        }
    }
}

/// The kinds offered, by command name.
const PART_KINDS: [&str; 6] = [
    "hex-bolt",
    "socket-screw",
    "hex-nut",
    "washer",
    "dowel-pin",
    "bearing",
];

/// A part kind's label.
fn part_label(i: usize) -> String {
    match i {
        0 => t("Hex bolt"),
        1 => t("Socket screw"),
        2 => t("Hex nut"),
        3 => t("Washer"),
        4 => t("Dowel pin"),
        _ => t("Ball bearing"),
    }
}

impl ProductWindow {
    fn target(session: &Session) -> Value {
        match selected_query(session, false) {
            Some(q) => json!(q),
            None => Value::Null,
        }
    }

    /// Reads the properties of the selection (or the document).
    fn load(&mut self, session: &mut Session) {
        let target = Self::target(session);
        match session.execute("properties-list", json!({ "target": target })) {
            Ok(out) => {
                let row = &out["rows"][0];
                self.part_number = row["part_number"].as_str().unwrap_or_default().to_string();
                self.description = row["description"].as_str().unwrap_or_default().to_string();
                self.rows = row["properties"]
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .filter(|(k, _)| k.as_str() != "part_number")
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                self.note = None;
            }
            Err(e) => self.note = Some(e),
        }
        if let Ok(out) = session.execute("revision-list", json!({})) {
            self.revisions = out["revisions"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|r| {
                            format!(
                                "{}  {}  {}",
                                r["label"].as_str().unwrap_or_default(),
                                r["date"].as_str().unwrap_or_default(),
                                r["description"].as_str().unwrap_or_default()
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
    }

    fn apply(&mut self, session: &mut Session) {
        let mut props = serde_json::Map::new();
        for (k, v) in &self.rows {
            props.insert(k.clone(), json!(v));
        }
        if !self.key.trim().is_empty() {
            props.insert(self.key.trim().to_string(), json!(self.value));
        }
        let params = json!({
            "target": Self::target(session),
            "part_number": self.part_number,
            "description": self.description,
            "properties": props,
        });
        self.note = Some(match session.execute("properties-set", params) {
            Ok(_) => {
                self.key.clear();
                self.value.clear();
                self.load(session);
                t("Done")
            }
            Err(e) => e,
        });
    }

    fn place(&mut self, session: &mut Session) {
        let kind = PART_KINDS[self.kind];
        let sizes = Self::sizes(self.kind);
        let size = sizes.get(self.size).copied().unwrap_or(sizes[0]);
        self.note = Some(
            match session.execute(
                "standard-part",
                json!({ "kind": kind, "size": size, "length": self.length_mm }),
            ) {
                Ok(out) => t_args(
                    "{ $part } placed.",
                    &[(
                        "part",
                        out["designation"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                            .into(),
                    )],
                ),
                Err(e) => e,
            },
        );
    }

    fn sizes(kind: usize) -> Vec<&'static str> {
        planyrith_core::parts::PartKind::ALL[kind].sizes()
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Product Data"))
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(t("Properties of the selected group or component (or of the model):"));
                egui::Grid::new("product").num_columns(2).show(ui, |ui| {
                    ui.label(t("Part number"));
                    ui.text_edit_singleline(&mut self.part_number);
                    ui.end_row();
                    ui.label(t("Description"));
                    ui.text_edit_singleline(&mut self.description);
                    ui.end_row();
                    for (k, v) in self.rows.iter_mut() {
                        ui.label(k.as_str());
                        ui.text_edit_singleline(v);
                        ui.end_row();
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.key).hint_text(t("New property")));
                    ui.text_edit_singleline(&mut self.value);
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    if ui.button(t("Read")).clicked() {
                        self.load(session);
                    }
                    if ui.button(t("Apply")).clicked() {
                        self.apply(session);
                    }
                });
                ui.separator();
                ui.label(t("Revisions"));
                for r in &self.revisions {
                    ui.label(r);
                }
                egui::Grid::new("revision").num_columns(2).show(ui, |ui| {
                    ui.label(t("What changed"));
                    ui.text_edit_singleline(&mut self.revision);
                    ui.end_row();
                    ui.label(t("By"));
                    ui.text_edit_singleline(&mut self.author);
                    ui.end_row();
                    ui.label(t("Date"));
                    ui.text_edit_singleline(&mut self.date);
                    ui.end_row();
                });
                if ui.button(t("Add Revision")).clicked() {
                    let params = json!({ "description": self.revision, "author": self.author, "date": self.date });
                    self.note = Some(match session.execute("revision-add", params) {
                        Ok(out) => {
                            self.revision.clear();
                            self.load(session);
                            t_args(
                                "Revision { $label }.",
                                &[("label", out["current"].as_str().unwrap_or_default().to_string().into())],
                            )
                        }
                        Err(e) => e,
                    });
                }
                ui.separator();
                ui.label(t("Standard parts"));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("part kind")
                        .selected_text(part_label(self.kind))
                        .show_ui(ui, |ui| {
                            for i in 0..PART_KINDS.len() {
                                if ui.selectable_value(&mut self.kind, i, part_label(i)).changed() {
                                    self.size = 0;
                                }
                            }
                        });
                    let sizes = Self::sizes(self.kind);
                    self.size = self.size.min(sizes.len() - 1);
                    egui::ComboBox::from_id_salt("part size")
                        .selected_text(sizes[self.size])
                        .show_ui(ui, |ui| {
                            for (i, s) in sizes.iter().enumerate() {
                                ui.selectable_value(&mut self.size, i, *s);
                            }
                        });
                    if planyrith_core::parts::PartKind::ALL[self.kind].has_length() {
                        ui.add(egui::DragValue::new(&mut self.length_mm).range(1.0..=1000.0).suffix(" mm"));
                    }
                });
                if ui.button(t("Place at Origin")).clicked() {
                    self.place(session);
                }
                if let Some(n) = &self.note {
                    ui.label(n);
                }
            });
        self.open = open;
    }
}

/// Tools › Molds… (docs/05 §8.18): a planar mold around the selection.
pub struct MoldWindow {
    /// The window is shown.
    pub open: bool,
    point: [f64; 3],
    normal: [f64; 3],
    margin_mm: f64,
    shrink_factor: f64,
    note: Option<String>,
}

impl Default for MoldWindow {
    fn default() -> Self {
        Self {
            open: false,
            point: [0.0; 3],
            normal: [0.0, 0.0, 1.0],
            margin_mm: 10.0,
            shrink_factor: 1.0,
            note: None,
        }
    }
}

impl MoldWindow {
    fn create(&mut self, session: &mut Session) {
        self.note = Some(note(
            session.execute(
                "mold-split",
                json!({
                    "body": "selection", "point": self.point, "normal": self.normal,
                    "margin": self.margin_mm, "shrink_factor": self.shrink_factor,
                }),
            ),
            |_| t("Two mold halves, parting line and surface created."),
        ));
    }

    /// Draws the window and dispatches the shared command.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Molds"))
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(t(
                    "Select one solid group or component. The division is planar.",
                ));
                ui.label(t("Parting plane point (mm)"));
                xyz(ui, &mut self.point);
                if ui.button(t("Use Selection Center")).clicked()
                    && let Some(bounds) = selection_bounds(session)
                {
                    self.point = bounds.center().to_array();
                }
                ui.label(t("Normal toward cavity"));
                xyz(ui, &mut self.normal);
                ui.horizontal(|ui| {
                    ui.label(t("Blank margin"));
                    ui.add(
                        egui::DragValue::new(&mut self.margin_mm)
                            .range(0.1..=10000.0)
                            .suffix(" mm"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(t("Shrink compensation factor"));
                    ui.add(
                        egui::DragValue::new(&mut self.shrink_factor)
                            .range(0.01..=10.0)
                            .speed(0.001),
                    );
                });
                ui.label(t(
                    "Check undercuts with Draft Analysis before choosing a division.",
                ));
                if ui.button(t("Create Mold")).clicked() {
                    self.create(session);
                }
                if let Some(n) = &self.note {
                    ui.label(n);
                }
            });
        self.open = open;
    }
}

/// Tools › Routing… (docs/05 §8.19): tubes on selected edges and placed fittings.
pub struct RoutingWindow {
    /// The window is shown.
    pub open: bool,
    size: usize,
    custom: bool,
    diameter_mm: f64,
    wall_mm: f64,
    bend_radius_mm: f64,
    reducer: bool,
    outlet_mm: f64,
    length_mm: f64,
    position: [f64; 3],
    axis: [f64; 3],
    note: Option<String>,
}

impl Default for RoutingWindow {
    fn default() -> Self {
        Self {
            open: false,
            size: 3,
            custom: false,
            diameter_mm: 20.0,
            wall_mm: 1.5,
            bend_radius_mm: 0.0,
            reducer: false,
            outlet_mm: 12.0,
            length_mm: 40.0,
            position: [0.0; 3],
            axis: [0.0, 0.0, 1.0],
            note: None,
        }
    }
}

impl RoutingWindow {
    fn section(&self) -> Value {
        let mut p = json!({"size": planyrith_core::geom::routing::SIZES[self.size]});
        if self.custom {
            p["outer_diameter"] = json!(self.diameter_mm);
            p["wall"] = json!(self.wall_mm);
        }
        p
    }

    fn create_route(&mut self, session: &mut Session) {
        let mut params = self.section();
        params["path"] = json!("selection");
        if self.bend_radius_mm > 0.0 {
            params["bend_radius"] = json!(self.bend_radius_mm);
        }
        self.note = Some(note(session.execute("route", params), Self::result));
    }

    fn create_fitting(&mut self, session: &mut Session) {
        let mut params = self.section();
        params["kind"] = json!(if self.reducer { "reducer" } else { "coupling" });
        params["length"] = json!(self.length_mm);
        params["position"] = json!(self.position);
        params["axis"] = json!(self.axis);
        if self.reducer {
            params["outlet_diameter"] = json!(self.outlet_mm);
        }
        self.note = Some(note(session.execute("route-fitting", params), Self::result));
    }

    fn result(out: &Value) -> String {
        t_args(
            "Centerline length: { $length } mm.",
            &[(
                "length",
                format!("{:.2}", out["length_mm"].as_f64().unwrap_or_default()).into(),
            )],
        )
    }

    /// Draws the window and dispatches the shared commands.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Routing"))
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(t("Outer diameter × wall thickness (mm)"));
                egui::ComboBox::from_id_salt("tube size")
                    .selected_text(planyrith_core::geom::routing::SIZES[self.size])
                    .show_ui(ui, |ui| {
                        for (i, size) in planyrith_core::geom::routing::SIZES.iter().enumerate() {
                            ui.selectable_value(&mut self.size, i, *size);
                        }
                    });
                ui.checkbox(&mut self.custom, t("Custom tube dimensions"));
                if self.custom {
                    ui.horizontal(|ui| {
                        ui.label(t("Outer diameter"));
                        ui.add(
                            egui::DragValue::new(&mut self.diameter_mm)
                                .range(0.1..=10000.0)
                                .suffix(" mm"),
                        );
                        ui.label(t("Wall thickness"));
                        ui.add(
                            egui::DragValue::new(&mut self.wall_mm)
                                .range(0.01..=5000.0)
                                .suffix(" mm"),
                        );
                    });
                }
                ui.separator();
                ui.label(t("Select an open chain of edges for the tube path."));
                ui.horizontal(|ui| {
                    ui.label(t("Bend radius (0 = automatic)"));
                    ui.add(
                        egui::DragValue::new(&mut self.bend_radius_mm)
                            .range(0.0..=100000.0)
                            .suffix(" mm"),
                    );
                });
                if ui.button(t("Create Tube")).clicked() {
                    self.create_route(session);
                }
                ui.separator();
                ui.label(t(
                    "Independent fitting: dimensions describe the fitting itself.",
                ));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.reducer, false, t("Coupling"));
                    ui.selectable_value(&mut self.reducer, true, t("Concentric reducer"));
                });
                if self.reducer {
                    ui.horizontal(|ui| {
                        ui.label(t("Outlet outer diameter"));
                        ui.add(
                            egui::DragValue::new(&mut self.outlet_mm)
                                .range(0.1..=10000.0)
                                .suffix(" mm"),
                        );
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(t("Length"));
                    ui.add(
                        egui::DragValue::new(&mut self.length_mm)
                            .range(0.1..=100000.0)
                            .suffix(" mm"),
                    );
                });
                ui.label(t("Position (mm)"));
                xyz(ui, &mut self.position);
                ui.label(t("Axis"));
                xyz(ui, &mut self.axis);
                if ui.button(t("Place Fitting")).clicked() {
                    self.create_fitting(session);
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

    /// The windows dispatch registry commands with the current selection.
    #[test]
    fn molds_and_routes_from_the_selection() {
        let mut s = Session::new();
        s.execute(
            "rectangle",
            json!({"corner_a": [0,0,0], "corner_b": [100,60,0]}),
        )
        .unwrap();
        s.execute(
            "push-pull",
            json!({"face": {"Query": "face & last"}, "distance": 40}),
        )
        .unwrap();
        s.execute(
            "make-group",
            json!({"targets": "face|edge", "name": "Part"}),
        )
        .unwrap();
        s.execute("select", json!({"query": "group"})).unwrap();
        let mut mold = MoldWindow {
            point: [50.0, 30.0, 20.0],
            ..Default::default()
        };
        mold.create(&mut s);
        assert_eq!(
            mold.note.as_deref(),
            Some("Two mold halves, parting line and surface created.")
        );
        assert_eq!(s.doc().model.groups().count(), 5);
        s.undo().unwrap();
        assert_eq!(s.doc().model.groups().count(), 1);
        s.execute("line", json!({"points": [[200,0,0],[300,0,0],[300,100,0]]}))
            .unwrap();
        s.execute("select", json!({"query": "edge"})).unwrap();
        let mut routing = RoutingWindow::default();
        routing.create_route(&mut s);
        assert!(
            routing
                .note
                .as_deref()
                .unwrap()
                .starts_with("Centerline length: 187.12"),
            "{:?}",
            routing.note
        );
        routing.reducer = true;
        routing.custom = true;
        routing.diameter_mm = 32.0;
        routing.wall_mm = 2.0;
        routing.outlet_mm = 20.0;
        routing.create_fitting(&mut s);
        assert_eq!(
            routing.note.as_deref(),
            Some("Centerline length: 40.00 mm.")
        );
        assert!(planyrith_core::diagnostics::validate(s.doc()).is_clean());
    }

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

    /// The fabrication window's commands: a sheet with a flange, its flat
    /// pattern, and members on edges.
    #[test]
    fn fabrication_from_the_selection() {
        let mut s = Session::new();
        s.execute(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [200, 100, 0] }),
        )
        .unwrap();
        s.execute("select", json!({ "query": "face" })).unwrap();
        let mut w = FabricationWindow::default();
        w.run(
            &mut s,
            "sheet-metal-base",
            json!({ "profile": "selection & face", "thickness": 2 }),
        );
        assert!(w.note.as_deref().unwrap().contains("40.0"), "{:?}", w.note);
        s.execute("select", json!({ "query": "group" })).unwrap();
        w.run(
            &mut s,
            "sheet-metal-flange",
            json!({ "part": "selection", "edge": 0, "length": 30 }),
        );
        w.run(&mut s, "sheet-metal-flat", json!({ "part": "selection" }));
        assert!(
            w.note.as_deref().unwrap().starts_with("Blank 200.0"),
            "{:?}",
            w.note
        );
        s.execute("line", json!({ "points": [[0, 500, 0], [1000, 500, 0]] }))
            .unwrap();
        s.execute("select", json!({ "query": "edge & last" }))
            .unwrap();
        w.run(
            &mut s,
            "frame-member",
            json!({ "path": "selection & edge", "profile": "IPE100" }),
        );
        assert_eq!(w.note.as_deref(), Some("1 pieces made."));
    }

    /// The surface and analysis windows' commands: a filled saddle, its
    /// zebra stripes, a block's draft, and clearing.
    #[test]
    fn surfaces_and_analysis_from_the_selection() {
        let mut s = Session::new();
        s.execute(
            "line",
            json!({ "points": [[0,0,0],[100,0,50],[100,100,0],[0,100,50]], "close": true }),
        )
        .unwrap();
        s.execute("select", json!({ "query": "edge" })).unwrap();
        let mut w = SurfacesWindow::default();
        w.run(
            &mut s,
            "surface-fill",
            json!({ "boundary": "selection & edge", "divisions": 6 }),
        );
        assert!(
            w.note.as_deref().unwrap().starts_with("72 faces"),
            "{:?}",
            w.note
        );
        s.execute("select", json!({ "query": "group" })).unwrap();
        let mut a = AnalysisWindow {
            kind: Analysis::Zebra,
            ..Default::default()
        };
        let (name, params) = a.command();
        a.run(&mut s, name, params);
        assert_eq!(a.rows.len(), 2);
        a.run(&mut s, "analysis-clear", json!({}));
        assert_eq!(a.note.as_deref(), Some("1 analyses removed."));
        w.run(
            &mut s,
            "surface-trim",
            json!({ "surface": "selection", "point": [50, 0, 0], "normal": [1, 0, 0] }),
        );
        assert!(
            w.note.as_deref().unwrap().ends_with("faces trimmed."),
            "{:?}",
            w.note
        );
    }

    /// The simulation window's commands: a held beam under a tip load, its
    /// frequencies and its temperatures, with faces from the selection.
    #[test]
    fn simulation_from_the_selection() {
        let mut s = Session::new();
        s.execute(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [200, 20, 0] }),
        )
        .unwrap();
        s.execute(
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": 20 }),
        )
        .unwrap();
        s.execute("select", json!({ "query": "face & plane:yz@0" }))
            .unwrap();
        let fixed = selected_query(&s, true).unwrap();
        assert!(fixed.starts_with('@'), "{fixed}");
        let mut w = SimulationWindow {
            body: "face".into(),
            mesh_mm: 20.0,
            fixed: fixed.clone(),
            load_faces: "face & plane:yz@200".into(),
            ..Default::default()
        };
        w.run(&mut s);
        assert!(
            w.note.as_deref().unwrap().starts_with("Largest stress"),
            "{:?}",
            w.note
        );
        w.page = SimPage::Frequency;
        w.modes = 2;
        w.run(&mut s);
        assert_eq!(w.lines.len(), 2, "{:?}", w.note);
        w.page = SimPage::Heat;
        w.cold = fixed;
        w.hot = "face & plane:yz@200".into();
        w.run(&mut s);
        assert!(
            w.note.as_deref().unwrap().starts_with("From 20.00"),
            "{:?}",
            w.note
        );
    }

    /// The product window's commands: a placed bolt, its part number and
    /// a revision of the model.
    #[test]
    fn product_data_from_the_selection() {
        let mut s = Session::new();
        let mut w = ProductWindow::default();
        w.place(&mut s);
        assert_eq!(w.note.as_deref(), Some("ISO 4017 M8x30 placed."));
        s.execute("select", json!({ "query": "instance" })).unwrap();
        w.load(&mut s);
        assert_eq!(w.part_number, "ISO 4017 M8x30");
        w.part_number = "B-830".into();
        w.key = "vendor".into();
        w.value = "Acme".into();
        w.apply(&mut s);
        assert_eq!(w.part_number, "B-830");
        assert_eq!(w.rows.iter().find(|r| r.0 == "vendor").unwrap().1, "Acme");
        w.revision = "First issue".into();
        let out = s
            .execute("revision-add", json!({ "description": w.revision }))
            .unwrap();
        assert_eq!(out["current"], "A");
    }

    /// A face selected inside an open component names the side
    /// `<component>:<face>` of the assembly around it.
    #[test]
    fn a_mate_side_comes_from_the_selection() {
        let mut s = Session::new();
        s.execute(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 100, 0] }),
        )
        .unwrap();
        s.execute(
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 100 }),
        )
        .unwrap();
        let out = s
            .execute(
                "make-component",
                json!({ "targets": "all", "name": "Cube" }),
            )
            .unwrap();
        let occ = out["instance"].as_u64().unwrap();
        assert!(side_of_selection(&s).is_none());
        let path = planyrith_core::document::InstancePath::root().child(
            planyrith_core::document::EntityKey::Pid(planyrith_core::ids::Pid(occ)),
        );
        s.engine
            .set_context(planyrith_core::document::EditContext { path })
            .unwrap();
        s.execute("select", json!({ "query": "face & plane:xy@100" }))
            .unwrap();
        let side = side_of_selection(&s).unwrap();
        assert!(side.reference.starts_with(&format!("{occ}:")), "{side:?}");
        assert!(side.assembly.steps.is_empty());
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
