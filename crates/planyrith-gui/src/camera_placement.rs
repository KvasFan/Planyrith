//! Camera placement form for docs/05 §9 and docs/13 milestone 1.28.

use crate::i18n::t;
use planyrith_core::document::Camera;
use serde_json::{Value, json};

#[derive(Default)]
pub(crate) struct Placement {
    open: bool,
    eye: [f64; 3],
    destination: [f64; 3],
    direction: bool,
    height: f64,
    distance: f64,
}
impl Placement {
    pub(crate) fn show(&mut self, camera: &Camera) {
        self.open = true;
        self.eye = camera.eye.to_array();
        self.destination = (camera.target - camera.eye).normalize_or_zero().to_array();
        self.direction = true;
        self.height = 0.0;
        self.distance = camera.distance();
    }
    pub(crate) fn window(&mut self, ctx: &egui::Context) -> Option<Value> {
        if !self.open {
            return None;
        }
        let mut open = true;
        let mut result = None;
        egui::Window::new(t("Camera Placement"))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(t("Eye / base point (mm)"));
                vector(ui, &mut self.eye);
                if ui
                    .checkbox(&mut self.direction, t("Use a look direction"))
                    .changed()
                {
                    self.destination = if self.direction {
                        [0.0, 1.0, 0.0]
                    } else {
                        [0.0; 3]
                    };
                }
                ui.label(if self.direction {
                    t("Look direction")
                } else {
                    t("Target point (mm)")
                });
                vector(ui, &mut self.destination);
                ui.horizontal(|ui| {
                    ui.label(t("Eye height"));
                    ui.add(
                        egui::DragValue::new(&mut self.height)
                            .range(-1e9..=1e9)
                            .suffix(" mm"),
                    );
                });
                if self.direction {
                    ui.horizontal(|ui| {
                        ui.label(t("Target distance"));
                        ui.add(
                            egui::DragValue::new(&mut self.distance)
                                .range(0.001..=1e9)
                                .suffix(" mm"),
                        );
                    });
                }
                if ui.button(t("Apply camera placement")).clicked() {
                    let mut params = json!({"eye":self.eye,"eye_height":self.height});
                    if self.direction {
                        params["look_dir"] = json!(self.destination);
                        params["target_distance"] = json!(self.distance);
                    } else {
                        params["target"] = json!(self.destination);
                    }
                    result = Some(params);
                }
            });
        self.open = open;
        result
    }
}
fn vector(ui: &mut egui::Ui, value: &mut [f64; 3]) {
    ui.horizontal(|ui| {
        for v in value {
            ui.add(egui::DragValue::new(v).range(-1e9..=1e9).speed(1.0));
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    #[test]
    fn camera_form_dispatches_direction_and_height() {
        let mut placement = Placement::default();
        placement.show(&Camera::default());
        placement.eye = [10.0, 20.0, 0.0];
        placement.destination = [0.0, 1.0, 0.0];
        placement.height = 1600.0;
        placement.distance = 500.0;
        let mut h = Harness::new_ui_state(
            |ui, state: &mut (Placement, crate::session::Session)| {
                if let Some(params) = state.0.window(ui.ctx()) {
                    state.1.execute("position-camera", params).unwrap();
                }
            },
            (placement, crate::session::Session::new()),
        );
        h.run();
        h.get_by_label("Apply camera placement").click();
        h.run();
        let camera = h.state().1.doc().camera;
        assert!(camera.eye.distance(glam::DVec3::new(10.0, 20.0, 1600.0)) < 1e-6);
        assert!(
            camera
                .target
                .distance(glam::DVec3::new(10.0, 520.0, 1600.0))
                < 1e-6
        );
    }
}
