//! Camera › Match Photo… (docs/05 §9.1, docs/07 §6): a picture over the
//! viewport with handles for two lines along the model's X axis, two along Y,
//! the origin and, optionally, the far end of a known length. Each change runs
//! `camera-match`, so the model is seen as the photo's camera saw it.

use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use glam::DVec2;
use planyrith_core::math::Axis;
use planyrith_core::units::parse_length;
use serde_json::json;

use crate::i18n::{t, t_args};
use crate::session::Session;

/// Handles: X line 1 (two ends), X line 2, Y line 1, Y line 2, the origin,
/// the known length's end.
const HANDLES: usize = 10;
const ORIGIN: usize = 8;
const LENGTH_END: usize = 9;
/// How near the pointer must be to grab a handle (points).
const GRAB: f32 = 10.0;

/// The window's and the overlay's state.
pub struct PhotoMatch {
    /// The window is shown (and the picture over the viewport).
    pub open: bool,
    /// A picture was asked for: the app opens its file dialog.
    pub pick: bool,
    texture: Option<egui::TextureHandle>,
    /// The picture's size (pixels).
    size: DVec2,
    /// Handle positions (picture pixels).
    handles: [DVec2; HANDLES],
    use_length: bool,
    length: String,
    axis: Axis,
    eye_height: String,
    opacity: f32,
    dragging: Option<usize>,
    changed: bool,
    /// The last result: field of view, or why there is none.
    result: Result<f64, String>,
}

impl Default for PhotoMatch {
    fn default() -> Self {
        PhotoMatch {
            open: false,
            pick: false,
            texture: None,
            size: DVec2::ONE,
            handles: [DVec2::ZERO; HANDLES],
            use_length: false,
            length: "1000mm".into(),
            axis: Axis::X,
            eye_height: "1600mm".into(),
            opacity: 0.5,
            dragging: None,
            changed: false,
            result: Err(String::new()),
        }
    }
}

/// Handles of a picture `size`: X lines to the right, Y lines to the left,
/// meeting at the origin below the middle.
fn default_handles(size: DVec2) -> [DVec2; HANDLES] {
    let p = |u: f64, v: f64| DVec2::new(u * size.x, v * size.y);
    [
        p(0.5, 0.75),
        p(0.85, 0.65),
        p(0.5, 0.3),
        p(0.85, 0.35),
        p(0.5, 0.75),
        p(0.15, 0.65),
        p(0.5, 0.3),
        p(0.15, 0.35),
        p(0.5, 0.75),
        p(0.85, 0.65),
    ]
}

impl PhotoMatch {
    /// Shows the window.
    pub fn show(&mut self) {
        self.open = true;
        if self.texture.is_none() {
            self.pick = true;
        }
    }

    /// Takes the picture `bytes` (PNG, JPEG…) named `name`.
    pub fn load(&mut self, ctx: &egui::Context, name: &str, bytes: &[u8]) -> Result<(), String> {
        let image = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
        let rgba = image.to_rgba8();
        let (w, h) = rgba.dimensions();
        let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        self.texture =
            Some(ctx.load_texture(format!("photo {name}"), color, egui::TextureOptions::LINEAR));
        self.size = DVec2::new(f64::from(w), f64::from(h));
        self.handles = default_handles(self.size);
        self.open = true;
        self.changed = true;
        Ok(())
    }

    /// The picture's place in viewport `rect`: as high as the viewport,
    /// centered (the camera's field of view is vertical).
    fn picture_rect(&self, rect: Rect) -> Rect {
        let width = rect.height() * (self.size.x / self.size.y) as f32;
        Rect::from_center_size(rect.center(), Vec2::new(width, rect.height()))
    }

    fn to_screen(&self, rect: Rect, p: DVec2) -> Pos2 {
        let r = self.picture_rect(rect);
        r.min
            + Vec2::new(
                (p.x / self.size.x) as f32 * r.width(),
                (p.y / self.size.y) as f32 * r.height(),
            )
    }

    fn to_picture(&self, rect: Rect, p: Pos2) -> DVec2 {
        let r = self.picture_rect(rect);
        let d = p - r.min;
        DVec2::new(
            f64::from(d.x / r.width()) * self.size.x,
            f64::from(d.y / r.height()) * self.size.y,
        )
    }

    fn active(&self) -> bool {
        self.open && self.texture.is_some()
    }

    fn handle_shown(&self, i: usize) -> bool {
        i != LENGTH_END || self.use_length
    }

    /// Moves handles with the pointer over viewport `rect`; true while the
    /// pointer is on a handle or drags one (the tool then gets no input).
    pub fn input(&mut self, ui: &egui::Ui, response: &egui::Response, rect: Rect) -> bool {
        if !self.active() {
            self.dragging = None;
            return false;
        }
        let (pressed, down, pos) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
            )
        });
        let near = pos.filter(|_| response.contains_pointer()).and_then(|pos| {
            (0..HANDLES)
                .filter(|&i| self.handle_shown(i))
                .map(|i| (i, self.to_screen(rect, self.handles[i]).distance(pos)))
                .filter(|(_, d)| *d <= GRAB)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        });
        if pressed && near.is_some() {
            self.dragging = near;
        }
        if !down {
            self.dragging = None;
        }
        if let (Some(i), Some(pos)) = (self.dragging, pos) {
            let p = self.to_picture(rect, pos);
            let p = p.clamp(DVec2::ZERO, self.size);
            // The origin and the lines' shared corner move together when
            // they sit on one point.
            let before = self.handles[i];
            for h in &mut self.handles {
                if h.distance(before) < 1e-9 {
                    *h = p;
                }
            }
            self.changed = true;
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if near.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        self.dragging.is_some() || near.is_some()
    }

    /// Draws the picture and the handles over viewport `rect`.
    pub fn paint(&self, painter: &egui::Painter, rect: Rect) {
        let Some(texture) = self.texture.as_ref().filter(|_| self.open) else {
            return;
        };
        let r = self.picture_rect(rect);
        painter.image(
            texture.id(),
            r,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::from_white_alpha((self.opacity * 255.0) as u8),
        );
        let red = Color32::from_rgb(220, 40, 40);
        let green = Color32::from_rgb(30, 160, 60);
        for (k, color) in [(0, red), (2, red), (4, green), (6, green)] {
            let a = self.to_screen(rect, self.handles[k]);
            let b = self.to_screen(rect, self.handles[k + 1]);
            // The line runs on across the picture, so it can be laid along
            // an edge; the segment between the handles is drawn bolder.
            let d = b - a;
            if d.length() > 1.0 {
                let far = d.normalized() * (r.width() + r.height());
                painter.line_segment(
                    [a - far, b + far],
                    Stroke::new(1.0, color.gamma_multiply(0.5)),
                );
            }
            painter.line_segment([a, b], Stroke::new(2.0, color));
        }
        for i in (0..HANDLES).filter(|&i| self.handle_shown(i)) {
            let at = self.to_screen(rect, self.handles[i]);
            let color = match i {
                0..=3 => red,
                4..=7 => green,
                ORIGIN => Color32::from_rgb(240, 200, 40),
                _ => Color32::from_rgb(40, 90, 220),
            };
            let radius = if i == ORIGIN { 7.0 } else { 5.0 };
            painter.circle(at, radius, Color32::WHITE, Stroke::new(2.0, color));
        }
    }

    /// The `camera-match` parameters for the handles.
    fn params(&self, unit: planyrith_core::units::LengthUnit) -> Result<serde_json::Value, String> {
        let h = &self.handles;
        let line = |i: usize| json!([h[i].x, h[i].y, h[i + 1].x, h[i + 1].y]);
        let mut params = json!({
            "image_size": [self.size.x, self.size.y],
            "x_line_1": line(0),
            "x_line_2": line(2),
            "y_line_1": line(4),
            "y_line_2": line(6),
            "origin": [h[ORIGIN].x, h[ORIGIN].y],
        });
        let parse = |text: &str| parse_length(text, unit).map_err(|e| e.to_string());
        if self.use_length {
            params["length"] = json!(parse(&self.length)?.as_mm());
            params["length_axis"] = json!(self.axis);
            params["length_end"] = json!([h[LENGTH_END].x, h[LENGTH_END].y]);
        } else {
            params["eye_height"] = json!(parse(&self.eye_height)?.as_mm());
        }
        Ok(params)
    }

    /// The window; runs `camera-match` after a change.
    pub fn window(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let unit = session.doc().info.display_units;
        let mut open = true;
        egui::Window::new(t("Match Photo"))
            .open(&mut open)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.label(t("Lay the red lines on two edges along the model's X axis, the green ones on two edges along Y and the yellow point on the origin."));
                ui.horizontal(|ui| {
                    if ui.button(t("Open Picture…")).clicked() {
                        self.pick = true;
                    }
                    if self.texture.is_some() && ui.button(t("Reset Lines")).clicked() {
                        self.handles = default_handles(self.size);
                        self.changed = true;
                    }
                });
                ui.add(egui::Slider::new(&mut self.opacity, 0.1..=1.0).text(t("Picture")));
                let before = (self.use_length, self.axis);
                ui.checkbox(&mut self.use_length, t("Known length (blue point)"));
                if self.use_length {
                    ui.horizontal(|ui| {
                        self.changed |= ui.text_edit_singleline(&mut self.length).changed();
                        egui::ComboBox::from_id_salt("photo axis")
                            .selected_text(format!("{:?}", self.axis))
                            .show_ui(ui, |ui| {
                                for axis in Axis::ALL {
                                    ui.selectable_value(&mut self.axis, axis, format!("{axis:?}"));
                                }
                            });
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label(t("Eye height"));
                        self.changed |= ui.text_edit_singleline(&mut self.eye_height).changed();
                    });
                }
                self.changed |= before != (self.use_length, self.axis);
                match &self.result {
                    Ok(fov) => {
                        ui.label(t_args(
                            "Field of view: { $degrees }°",
                            &[("degrees", format!("{fov:.1}").into())],
                        ));
                    }
                    Err(note) if !note.is_empty() => {
                        ui.colored_label(ui.visuals().error_fg_color, note);
                    }
                    Err(_) => {}
                }
                if ui.button(t("Done")).clicked() {
                    self.open = false;
                }
            });
        self.open &= open;
        if std::mem::take(&mut self.changed) && self.texture.is_some() {
            self.result = self.params(unit).and_then(|params| {
                session
                    .execute("camera-match", params)
                    .map(|out| out["fov_y_deg"].as_f64().unwrap_or(0.0))
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_starts_with_lines_that_give_a_camera() {
        let ctx = egui::Context::default();
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(640, 480, image::Rgba([200, 180, 160, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let mut photo = PhotoMatch::default();
        photo.load(&ctx, "room.png", &png).unwrap();
        assert!(photo.open && photo.changed);
        let params = photo
            .params(planyrith_core::units::LengthUnit::Millimeter)
            .unwrap();
        let params: planyrith_core::commands::view::CameraMatchParams =
            serde_json::from_value(params).unwrap();
        let found = planyrith_core::commands::view::picture_camera(&params).unwrap();
        assert!(
            found.fov_y_deg > 1.0 && found.fov_y_deg < 170.0,
            "{found:?}"
        );
        assert!((found.eye.z - 1600.0).abs() < 1e-6);

        // Screen and picture points map back and forth.
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(800.0, 400.0));
        let p = DVec2::new(123.0, 456.0);
        assert!(photo.to_picture(rect, photo.to_screen(rect, p)).distance(p) < 1e-3);
        assert!((photo.picture_rect(rect).height() - 400.0).abs() < 1e-3);
    }
}
