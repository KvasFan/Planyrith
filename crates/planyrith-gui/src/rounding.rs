//! Tools › Round Edges… (docs/05 §4.17–§4.18): `fillet` and `chamfer` on
//! the selected edges of the solid being edited; Tools › Thicken… (§4.19)
//! on the selected faces.

use serde_json::json;

use crate::i18n::t;
use crate::session::Session;

/// The window's state.
pub struct RoundEdges {
    /// The window is shown.
    pub open: bool,
    radius_mm: f64,
    segments: u32,
    distance_mm: f64,
    note: Option<String>,
}

impl Default for RoundEdges {
    fn default() -> Self {
        RoundEdges {
            open: false,
            radius_mm: 10.0,
            segments: 8,
            distance_mm: 10.0,
            note: None,
        }
    }
}

impl RoundEdges {
    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Round Edges"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(t("Select edges of a solid, inside its group, then:"));
                egui::Grid::new("round edges").num_columns(3).show(ui, |ui| {
                    ui.label(t("Radius"));
                    ui.add(
                        egui::DragValue::new(&mut self.radius_mm)
                            .range(0.1..=100_000.0)
                            .speed(0.5)
                            .suffix(" mm"),
                    );
                    if ui.button(t("Fillet")).clicked() {
                        self.run(
                            session,
                            "fillet",
                            json!({ "edges": "selection", "radius": self.radius_mm, "segments": self.segments }),
                        );
                    }
                    ui.end_row();
                    ui.label(t("Segments"));
                    ui.add(egui::DragValue::new(&mut self.segments).range(1..=64));
                    ui.end_row();
                    ui.label(t("Distance"));
                    ui.add(
                        egui::DragValue::new(&mut self.distance_mm)
                            .range(0.1..=100_000.0)
                            .speed(0.5)
                            .suffix(" mm"),
                    );
                    if ui.button(t("Chamfer")).clicked() {
                        self.run(
                            session,
                            "chamfer",
                            json!({ "edges": "selection", "distance": self.distance_mm }),
                        );
                    }
                    ui.end_row();
                });
                if let Some(note) = &self.note {
                    ui.label(note);
                }
            });
        self.open = open;
    }

    fn run(&mut self, session: &mut Session, name: &str, params: serde_json::Value) {
        self.note = Some(match session.execute(name, params) {
            Ok(out) => crate::i18n::t_args(
                "Edges rounded: { $count }.",
                &[(
                    "count",
                    out["rounded"].as_u64().unwrap_or(0).to_string().into(),
                )],
            ),
            Err(e) => e,
        });
    }
}

/// The Thicken window's state.
pub struct ThickenWindow {
    /// The window is shown.
    pub open: bool,
    thickness_mm: f64,
    centered: bool,
    note: Option<String>,
}

impl Default for ThickenWindow {
    fn default() -> Self {
        ThickenWindow {
            open: false,
            thickness_mm: 10.0,
            centered: false,
            note: None,
        }
    }
}

impl ThickenWindow {
    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Thicken"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(t("Select the faces of a surface, then:"));
                ui.horizontal(|ui| {
                    ui.label(t("Thickness"));
                    ui.add(
                        egui::DragValue::new(&mut self.thickness_mm)
                            .range(-100_000.0..=100_000.0)
                            .speed(0.5)
                            .suffix(" mm"),
                    );
                });
                ui.checkbox(&mut self.centered, t("Half on each side"));
                if ui.button(t("Thicken")).clicked() {
                    self.note = Some(
                        match session.execute(
                            "thicken",
                            json!({ "faces": "selection", "thickness": self.thickness_mm, "centered": self.centered }),
                        ) {
                            Ok(out) => crate::i18n::t_args(
                                "Solid made: { $volume } cm³.",
                                &[(
                                    "volume",
                                    format!(
                                        "{:.1}",
                                        out["volume_mm3"].as_f64().unwrap_or(0.0) / 1000.0
                                    )
                                    .into(),
                                )],
                            ),
                            Err(e) => e,
                        },
                    );
                }
                if let Some(note) = &self.note {
                    ui.label(note);
                }
            });
        self.open = open;
    }
}

/// Tools › Loft Selected Faces (docs/05 §4.20): the selected faces of the
/// edit context are the profiles, from the lowest to the highest (then by
/// y and x), which is the order of a loft built upward.
pub fn loft_selected(session: &mut Session) -> Result<String, String> {
    let doc = session.doc();
    let mut faces: Vec<(glam::DVec3, u64)> = Vec::new();
    for path in session.engine.selection().iter() {
        let Ok(r) = doc.resolve(path) else { continue };
        let planyrith_core::ids::EntityId::Face(f) = r.entity else {
            continue;
        };
        let Some(mesh) = doc.container(r.container).map(|c| c.mesh()) else {
            continue;
        };
        let Ok(vertices) = mesh.face_vertices(f) else {
            continue;
        };
        let center = vertices
            .iter()
            .filter_map(|&v| mesh.position(v).ok())
            .sum::<glam::DVec3>()
            / vertices.len().max(1) as f64;
        faces.push((r.world.transform_point3(center), r.pid.0));
    }
    if faces.len() < 2 {
        return Err(t("Select two or more faces to loft."));
    }
    faces.sort_by(|a, b| {
        (a.0.z, a.0.y, a.0.x)
            .partial_cmp(&(b.0.z, b.0.y, b.0.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let profiles: Vec<String> = faces.iter().map(|(_, pid)| format!("id={pid}")).collect();
    let out = session.execute("loft", json!({ "profiles": profiles }))?;
    Ok(crate::i18n::t_args(
        "Solid made: { $volume } cm³.",
        &[(
            "volume",
            format!("{:.1}", out["volume_mm3"].as_f64().unwrap_or(0.0) / 1000.0).into(),
        )],
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rounds_the_selected_edges() {
        let mut session = Session::new();
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 40 }),
            ),
            ("select", json!({ "query": "edge & length=100" })),
        ] {
            session.execute(name, params).unwrap();
        }
        let mut window = RoundEdges::default();
        window.run(
            &mut session,
            "fillet",
            json!({ "edges": "selection", "radius": 5.0, "segments": 2 }),
        );
        assert!(
            window.note.as_deref().unwrap().contains('4'),
            "{:?}",
            window.note
        );
        window.run(
            &mut session,
            "chamfer",
            json!({ "edges": "selection", "distance": 5.0 }),
        );
        assert!(window.note.is_some());
    }

    #[test]
    fn lofts_the_selected_faces_bottom_up() {
        let mut session = Session::new();
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [-50, -50, 0], "corner_b": [50, 50, 0] }),
            ),
            (
                "rectangle",
                json!({ "corner_a": [-20, -20, 300], "corner_b": [20, 20, 300] }),
            ),
            ("select", json!({ "query": "face" })),
        ] {
            session.execute(name, params).unwrap();
        }
        let note = loft_selected(&mut session).unwrap();
        assert!(note.contains("cm"), "{note}");
        session
            .execute("select", json!({ "query": "!all" }))
            .unwrap();
        assert!(loft_selected(&mut session).is_err());
    }
}
