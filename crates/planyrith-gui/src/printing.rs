//! Tools › Solids › Check for 3D Printing… (docs/05 §8.1–§8.2): the
//! objects `solid-check` reports, their problems, red marks on the open and
//! wire edges of the chosen one, and `solid-repair`.

use egui::RichText;
use glam::DVec3;
use planyrith_core::commands::solid_check::{SolidCheckObject, SolidCheckOutput};
use planyrith_core::geom::solid_check::diagnose;
use planyrith_core::units::{Length, format_length};
use serde_json::json;

use crate::i18n::{t, t_args};
use crate::session::Session;

/// What the cached marks were found for: the chosen row and the document
/// generation.
type MarksKey = (Option<usize>, u64);

/// The window's state.
#[derive(Default)]
pub struct PrintCheck {
    /// The window is shown.
    pub open: bool,
    /// The last check and the document generation it saw.
    result: Option<(SolidCheckOutput, u64)>,
    /// The row picked in the list.
    chosen: Option<usize>,
    /// A message from the last action.
    note: Option<String>,
    /// The marks for (chosen row, document generation).
    marks: Option<(MarksKey, Vec<[DVec3; 2]>)>,
    /// Export one file per object.
    per_object: bool,
    /// Export for printing was asked for: one file per object or not.
    pub export: Option<bool>,
}

impl PrintCheck {
    /// Opens the window and checks the model.
    pub fn show(&mut self, session: &mut Session) {
        self.open = true;
        self.chosen = None;
        self.note = None;
        self.check(session);
    }

    fn check(&mut self, session: &mut Session) {
        match session
            .execute("solid-check", json!({}))
            .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        {
            Ok(output) => {
                let SolidCheckOutput { objects, .. } = &output;
                if self.chosen.is_some_and(|i| i >= objects.len()) {
                    self.chosen = None;
                }
                // The first object that needs work is the one to look at.
                if self.chosen.is_none() {
                    self.chosen = objects.iter().position(|o| !o.printable);
                }
                self.result = Some((output, session.engine.generation()));
            }
            Err(e) => {
                self.result = None;
                self.note = Some(e);
            }
        }
    }

    /// The open and wire edges of the chosen object, in world coordinates
    /// (red marks in the viewport while the window is open).
    pub fn marks(&mut self, session: &Session) -> Vec<[DVec3; 2]> {
        if !self.open {
            return Vec::new();
        }
        let key = (self.chosen, session.engine.generation());
        if let Some((cached, marks)) = &self.marks
            && *cached == key
        {
            return marks.clone();
        }
        let marks = self.find_marks(session);
        self.marks = Some((key, marks.clone()));
        marks
    }

    fn find_marks(&self, session: &Session) -> Vec<[DVec3; 2]> {
        let Some(object) = self.chosen_object() else {
            return Vec::new();
        };
        let doc = session.doc();
        let Ok((container, world)) = doc.resolve_container(&object.path) else {
            return Vec::new();
        };
        let Some(mesh) = doc.container(container).map(|c| c.mesh()) else {
            return Vec::new();
        };
        let Ok(d) = diagnose(mesh, doc.tolerances()) else {
            return Vec::new();
        };
        d.open_edges
            .iter()
            .chain(&d.wire_edges)
            .filter_map(|&e| mesh.edge_segment(e).ok())
            .map(|s| [world.transform_point3(s.a), world.transform_point3(s.b)])
            .collect()
    }

    fn chosen_object(&self) -> Option<&SolidCheckObject> {
        let (output, _) = self.result.as_ref()?;
        output.objects.get(self.chosen?)
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Check for 3D Printing"))
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| self.contents(ui, session));
        self.open = open;
    }

    fn contents(&mut self, ui: &mut egui::Ui, session: &mut Session) {
        let unit = session.doc().info.display_units;
        let Some((output, generation)) = &self.result else {
            if let Some(note) = &self.note {
                ui.colored_label(ui.visuals().error_fg_color, note);
            }
            if ui.button(t("Check Again")).clicked() {
                self.check(session);
            }
            return;
        };
        let stale = *generation != session.engine.generation();
        let failing = output.objects.iter().filter(|o| !o.printable).count();
        let has_objects = !output.objects.is_empty();
        let summary = if output.objects.is_empty() {
            t("There is nothing to print: no visible object has faces.")
        } else if failing == 0 {
            t_args(
                "Every object is ready to print ({ $count }).",
                &[("count", output.objects.len().to_string().into())],
            )
        } else {
            t_args(
                "{ $failing } of { $count } objects are not ready to print.",
                &[
                    ("failing", failing.to_string().into()),
                    ("count", output.objects.len().to_string().into()),
                ],
            )
        };
        ui.label(RichText::new(summary).strong());
        if stale {
            ui.weak(t("The model has changed since the check."));
        }
        ui.add_space(4.0);
        let mut picked = None;
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                egui::Grid::new("print-check")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong(t("Object"));
                        ui.strong(t("Size"));
                        ui.strong(t("Status"));
                        ui.end_row();
                        for (i, object) in output.objects.iter().enumerate() {
                            let chosen = self.chosen == Some(i);
                            if ui.selectable_label(chosen, &object.name).clicked() {
                                picked = Some(i);
                            }
                            let [x, y, z] = object
                                .size_mm
                                .map(|v| format_length(Length::mm(v), unit, 1));
                            ui.label(format!("{x} × {y} × {z}"));
                            if object.printable {
                                ui.label(format!("✔ {}", t("Ready")));
                            } else {
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    format!("✖ {}", t("Needs repair")),
                                );
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(i) = picked {
            self.chosen = Some(i);
        }
        if let Some(object) = self.chosen_object() {
            ui.separator();
            ui.label(RichText::new(&object.name).strong());
            for line in problems(object) {
                ui.label(line);
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(t("Check Again")).clicked() {
                self.note = None;
                self.check(session);
            }
            let repair = ui.add_enabled(failing > 0, egui::Button::new(t("Repair")));
            if repair
                .on_hover_text(t(
                    "Closes holes, removes stray edges and turns inside-out parts right side out.",
                ))
                .clicked()
            {
                self.note = Some(match session.execute("solid-repair", json!({})) {
                    Ok(out) => repaired(&out),
                    Err(e) => e,
                });
                self.check(session);
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_objects, egui::Button::new(t("Export for Printing…")))
                .on_hover_text(t(
                    "Writes a 3MF or STL file with the objects resting on the build plate.",
                ))
                .clicked()
            {
                self.export = Some(self.per_object);
            }
            ui.checkbox(&mut self.per_object, t("One file per object"));
        });
        if let Some(note) = &self.note {
            ui.label(note);
        }
    }
}

/// What is wrong with `object`, one line each (its volume when nothing is).
fn problems(object: &SolidCheckObject) -> Vec<String> {
    let count = |n: usize| -> fluent_bundle::FluentValue<'static> { n.to_string().into() };
    if object.printable {
        let volume = object.volume_mm3.unwrap_or(0.0) / 1000.0;
        let mut lines = vec![t_args(
            "Closed and facing out. Volume: { $volume } cm³.",
            &[("volume", format!("{volume:.2}").into())],
        )];
        if object.shells > 1 {
            lines.push(t_args(
                "{ $count } separate parts.",
                &[("count", count(object.shells))],
            ));
        }
        if object.voids > 0 {
            lines.push(t_args(
                "Hollows inside: { $count }.",
                &[("count", count(object.voids))],
            ));
        }
        return lines;
    }
    let mut lines = Vec::new();
    if object.open_edges > 0 {
        lines.push(t_args(
            "Open edges (marked in red): { $edges }. Holes: { $holes }.",
            &[
                ("edges", count(object.open_edges)),
                ("holes", count(object.holes)),
            ],
        ));
    }
    if object.wire_edges > 0 {
        lines.push(t_args(
            "Stray edges that bound no face (marked in red): { $count }.",
            &[("count", count(object.wire_edges))],
        ));
    }
    if object.pinched_vertices > 0 {
        lines.push(t_args(
            "Points where parts touch only at a corner: { $count }.",
            &[("count", count(object.pinched_vertices))],
        ));
    }
    if object.inside_out_shells > 0 {
        lines.push(t_args(
            "Parts facing inward (reversed faces): { $count }.",
            &[("count", count(object.inside_out_shells))],
        ));
    }
    lines
}

/// The repair's outcome in a sentence or two.
fn repaired(out: &serde_json::Value) -> String {
    let n = |key: &str| -> fluent_bundle::FluentValue<'static> {
        out[key].as_u64().unwrap_or(0).to_string().into()
    };
    let mut text = t_args(
        "Holes closed: { $holes }. Stray edges removed: { $wires }. Parts turned right side out: { $shells }.",
        &[
            ("holes", n("holes_filled")),
            ("wires", n("wire_edges_removed")),
            ("shells", n("shells_reversed")),
        ],
    );
    if out["holes_left"].as_u64().unwrap_or(0) > 0 {
        text.push(' ');
        text.push_str(&t_args(
            "Holes left open: { $count } (a lone face, or a hole that folds over itself: close it by drawing).",
            &[("count", n("holes_left"))],
        ));
    }
    if out["locked"].as_u64().unwrap_or(0) > 0 {
        text.push(' ');
        text.push_str(&t_args(
            "Locked objects left alone: { $count }.",
            &[("count", n("locked"))],
        ));
    }
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn an_open_tray_is_marked_and_repaired() {
        let mut session = Session::new();
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 20 }),
            ),
            (
                "delete",
                json!({ "targets": "face & normal:z & plane:xy@20" }),
            ),
            ("make-group", json!({ "targets": "all", "name": "Tray" })),
        ] {
            session.execute(name, params).unwrap();
        }
        let mut check = PrintCheck::default();
        check.show(&mut session);
        assert_eq!(check.chosen, Some(0), "the object needing work is chosen");
        let marks = check.marks(&session);
        assert_eq!(marks.len(), 4, "the rim of the missing top");
        assert!(marks.iter().flatten().all(|p| (p.z - 20.0).abs() < 1e-9));
        let lines = problems(check.chosen_object().unwrap());
        assert_eq!(lines.len(), 1);

        let out = session.execute("solid-repair", json!({})).unwrap();
        assert!(repaired(&out).contains('1'));
        check.check(&mut session);
        let (output, _) = check.result.as_ref().unwrap();
        assert!(output.printable);
        assert!(check.marks(&session).is_empty());
        check.open = false;
        assert!(check.marks(&session).is_empty());
    }
}
