//! File › Work Together… (docs/08 §3.1, and §3.3 in the browser): join a
//! document on a `planyrith serve` server under a name, see who else works
//! on it and what they selected, and leave with a local copy.

use glam::DVec3;
use planyrith_core::document::Rgba8;

use crate::i18n::t;
use crate::link::Participant;
use crate::session::Session;

/// The window's state.
pub struct Together {
    /// The window is shown.
    pub open: bool,
    address: String,
    token: String,
    document: String,
    name: String,
    error: Option<String>,
}

impl Default for Together {
    fn default() -> Self {
        Together {
            open: false,
            address: String::new(),
            token: String::new(),
            document: "d1".into(),
            name: String::new(),
            error: None,
        }
    }
}

/// The twelve edges of each box the others selected, in their colors.
pub fn selection_lines(participants: &[Participant]) -> Vec<(Rgba8, [DVec3; 2])> {
    let mut out = Vec::new();
    for p in participants.iter().filter(|p| !p.you) {
        let color = Rgba8 {
            r: p.color[0],
            g: p.color[1],
            b: p.color[2],
            a: 255,
        };
        for item in &p.selection {
            let Some([lo, hi]) = item.bounds else {
                continue;
            };
            let (lo, hi) = (DVec3::from_array(lo), DVec3::from_array(hi));
            let corner = |i: usize| {
                DVec3::new(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                )
            };
            for (a, b) in [
                (0, 1),
                (2, 3),
                (4, 5),
                (6, 7),
                (0, 2),
                (1, 3),
                (4, 6),
                (5, 7),
                (0, 4),
                (1, 5),
                (2, 6),
                (3, 7),
            ] {
                out.push((color, [corner(a), corner(b)]));
            }
        }
    }
    out
}

/// A colored dot and a name.
pub fn chip(ui: &mut egui::Ui, p: &Participant) {
    let color = egui::Color32::from_rgb(p.color[0], p.color[1], p.color[2]);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
    let name = if p.you {
        format!("{} ({})", p.name, t("you"))
    } else {
        p.name.clone()
    };
    let paths: Vec<&str> = p
        .selection
        .iter()
        .take(8)
        .map(|s| s.path.as_str())
        .collect();
    ui.label(name).on_hover_text(format!(
        "{} {}\n{}",
        p.selection.len(),
        t("selected"),
        paths.join("\n")
    ));
}

impl Together {
    /// Opens the window filled in with `address` and `document` (a page
    /// opened with `?join=<document>` from a server, docs/08 §3.3).
    #[cfg(target_arch = "wasm32")]
    pub fn prefill(&mut self, address: &str, document: &str) {
        self.open = true;
        self.address = address.to_string();
        if !document.is_empty() {
            self.document = document.to_string();
        }
    }

    /// Draws the window; may replace `session` (joining or leaving).
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Work Together"))
            .open(&mut open)
            .default_width(360.0)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                if session.is_joining() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t("Joining…"));
                    });
                    if ui.button(t("Cancel")).clicked() {
                        *session = session.leave();
                    }
                    return;
                }
                if session.is_attached() {
                    ui.label(t("Working on the server's document with:"));
                    for p in &session.participants {
                        ui.push_id(p.client, |ui| ui.horizontal(|ui| chip(ui, p)));
                    }
                    ui.weak(t(
                        "Each person has a selection and an undo of their own; commands and saving run on the server.",
                    ));
                    if ui.button(t("Leave (keep a local copy)")).clicked() {
                        *session = session.leave();
                    }
                    return;
                }
                #[cfg(not(target_arch = "wasm32"))]
                ui.label(t(
                    "Join a document someone shares with: planyrith serve -f model.plyr --socket 0.0.0.0:7400 --token <secret>",
                ));
                #[cfg(target_arch = "wasm32")]
                ui.label(t(
                    "Join a document someone shares with: planyrith serve -f model.plyr --web 0.0.0.0:7400 --web-root <web build> --token <secret>",
                ));
                egui::Grid::new("together").num_columns(2).show(ui, |ui| {
                    ui.label(t("Server"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.address)
                            .hint_text("192.168.1.20:7400"),
                    );
                    ui.end_row();
                    ui.label(t("Token"));
                    ui.add(egui::TextEdit::singleline(&mut self.token).password(true));
                    ui.end_row();
                    ui.label(t("Document"));
                    ui.text_edit_singleline(&mut self.document);
                    ui.end_row();
                    ui.label(t("Your name"));
                    ui.text_edit_singleline(&mut self.name);
                    ui.end_row();
                });
                if session.dirty() {
                    ui.weak(t("Save the current document before joining."));
                }
                let ready = !session.dirty()
                    && !self.address.trim().is_empty()
                    && !self.name.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new(t("Join")))
                    .clicked()
                {
                    #[cfg(not(target_arch = "wasm32"))]
                    let joined = Session::join(
                        self.address.trim(),
                        &self.token,
                        self.document.trim(),
                        self.name.trim(),
                    );
                    // The browser does not wait: the session joins as
                    // answers arrive.
                    #[cfg(target_arch = "wasm32")]
                    let joined = crate::link::BrowserSocket::connect(self.address.trim()).map(
                        |socket| {
                            Session::join_over(
                                Box::new(socket),
                                &self.token,
                                self.document.trim(),
                                self.name.trim(),
                            )
                        },
                    );
                    match joined {
                        Ok(joined) => {
                            session.drop_autosave();
                            *session = joined;
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            });
        self.open = open;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::link::Selected;

    #[test]
    fn others_selections_become_colored_boxes() {
        let p = |you: bool| Participant {
            client: 1,
            name: "Anna".into(),
            color: [213, 94, 0],
            you,
            selection: vec![
                Selected {
                    path: "#Model/@5".into(),
                    bounds: Some([[0.0, 0.0, 0.0], [1000.0, 500.0, 300.0]]),
                },
                Selected {
                    path: "#Model/@6".into(),
                    bounds: None,
                },
            ],
        };
        let lines = selection_lines(&[p(false), p(true)]);
        assert_eq!(lines.len(), 12, "one box, only the other's");
        assert!(lines.iter().all(|(c, _)| c.r == 213));
        let far = lines
            .iter()
            .flat_map(|(_, s)| s.iter())
            .fold(DVec3::ZERO, |m, p| m.max(*p));
        assert_eq!(far, DVec3::new(1000.0, 500.0, 300.0));
    }
}
