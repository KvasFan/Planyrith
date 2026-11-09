//! Tools › Mirror…, Revolve…, Pattern… and Measure… (docs/05 §4.21–§4.23,
//! §8.3–§8.5, milestone 1.8): small windows that collect the parameters of
//! one command each and run it on the selection.

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
}
