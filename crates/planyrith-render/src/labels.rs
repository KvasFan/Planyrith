//! Labels of headless renders (docs/06 §3.1 step 7, §11): the overlay's
//! texts, leaders and guide points drawn on the read-back image on the CPU
//! with the core's bundled font. The GUI draws the same overlay with egui.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use glam::{DVec2, DVec3};
use image::RgbaImage;
use planyrith_core::document::{ArrowStyle, Camera};
use planyrith_core::io::overlay::{Overlay, OverlayKind};
use planyrith_core::math::Aabb;

/// Text size in pixels.
const TEXT_PX: f32 = 14.0;

/// Blends `color` with `alpha` (0–1) into the pixel at (`x`, `y`).
fn blend(image: &mut RgbaImage, x: i64, y: i64, color: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x >= i64::from(image.width()) || y >= i64::from(image.height()) {
        return;
    }
    let p = image.get_pixel_mut(x as u32, y as u32);
    let a = alpha.clamp(0.0, 1.0);
    for (channel, c) in p.0.iter_mut().zip(color) {
        *channel = (f32::from(*channel) * (1.0 - a) + f32::from(c) * a).round() as u8;
    }
    p.0[3] = p.0[3].max((a * 255.0) as u8);
}

/// A one-pixel line.
fn line(image: &mut RgbaImage, a: DVec2, b: DVec2, color: [u8; 3]) {
    let steps = (b - a).abs().max_element().ceil().max(1.0) as i64;
    for i in 0..=steps {
        let p = a + (b - a) * (i as f64 / steps as f64);
        blend(image, p.x.round() as i64, p.y.round() as i64, color, 1.0);
    }
}

/// Draws the overlay's labels and guide points on `image`, seen by
/// `camera` (its aspect replaced by the image's).
pub fn draw_labels(image: &mut RgbaImage, overlay: &Overlay, camera: &Camera) {
    let Ok(font) = FontRef::try_from_slice(planyrith_core::fonts::INTER) else {
        return;
    };
    let (w, h) = (f64::from(image.width()), f64::from(image.height()));
    let camera = Camera {
        aspect: w / h.max(1.0),
        ..*camera
    };
    let mut bounds = Aabb::EMPTY;
    for p in overlay
        .labels
        .iter()
        .map(|l| l.at)
        .chain(overlay.points.iter().copied())
    {
        bounds = bounds.union(&Aabb::new(p, p));
    }
    let depth = camera.fit_depth(&bounds);
    let to_px = |p: DVec3| {
        camera
            .project(p, depth)
            .map(|n| DVec2::new((n.x + 1.0) * 0.5 * w, (1.0 - n.y) * 0.5 * h))
    };
    for p in &overlay.points {
        if let Some(s) = to_px(*p) {
            let gray = [60, 60, 60];
            line(
                image,
                s - DVec2::new(4.0, 0.0),
                s + DVec2::new(4.0, 0.0),
                gray,
            );
            line(
                image,
                s - DVec2::new(0.0, 4.0),
                s + DVec2::new(0.0, 4.0),
                gray,
            );
        }
    }
    for label in &overlay.labels {
        let screen_only = label.layout.is_none()
            && label.kind == OverlayKind::Text
            && label.at == DVec3::ZERO
            && label.screen_offset == DVec2::ZERO;
        let anchor = if let Some(p) = label.layout.as_ref().and_then(|l| l.screen_position) {
            Some(DVec2::new(p.x * w, p.y * h))
        } else if screen_only {
            Some(DVec2::new(12.0, 12.0))
        } else {
            to_px(label.at)
        };
        let Some(anchor) = anchor else { continue };
        let at = label
            .layout
            .as_ref()
            .and_then(|l| l.world_offset)
            .and_then(|v| to_px(label.at + v))
            .unwrap_or(anchor + DVec2::new(label.screen_offset.x, -label.screen_offset.y));
        if at.distance(anchor) > 1.0 && label.layout.as_ref().is_none_or(|l| l.leader_visible) {
            line(image, anchor, at, [40, 40, 40]);
            if let Some(layout) = &label.layout {
                let dir = (at - anchor).normalize();
                let side = DVec2::new(-dir.y, dir.x);
                let color = [40, 40, 40];
                match layout.arrow {
                    ArrowStyle::None => {}
                    ArrowStyle::Dot => {
                        for y in -2..=2 {
                            for x in -2..=2 {
                                if x * x + y * y <= 4 {
                                    blend(
                                        image,
                                        anchor.x.round() as i64 + x,
                                        anchor.y.round() as i64 + y,
                                        color,
                                        1.0,
                                    );
                                }
                            }
                        }
                    }
                    ArrowStyle::Slash => {
                        let d = (dir + side) * 3.0;
                        line(image, anchor - d, anchor + d, color);
                    }
                    ArrowStyle::Open => {
                        line(image, anchor + dir * 8.0 + side * 3.0, anchor, color);
                        line(image, anchor, anchor + dir * 8.0 - side * 3.0, color);
                    }
                    ArrowStyle::Closed => {
                        for step in 0..=16 {
                            let s = f64::from(step) / 16.0;
                            let p = anchor + dir * (8.0 * s);
                            line(image, p - side * (3.0 * s), p + side * (3.0 * s), color);
                        }
                    }
                }
            }
        }
        let size = label
            .layout
            .as_ref()
            .and_then(|l| l.font.as_ref())
            .map_or(TEXT_PX, |f| {
                f.model_height_mm
                    .and_then(|height| to_px(label.at + camera.up.normalize_or_zero() * height))
                    .map_or((f.size_pt * 96.0 / 72.0) as f32, |p| {
                        p.distance(anchor) as f32
                    })
                    .clamp(1.0, 1000.0)
            });
        let scaled = font.as_scaled(PxScale::from(size));
        let (text_color, fill): ([u8; 3], [u8; 3]) = match label.kind {
            OverlayKind::Detached => ([200, 20, 20], [255, 255, 255]),
            OverlayKind::SectionPlane { .. } => ([255, 255, 255], [235, 120, 20]),
            _ => ([20, 20, 20], [255, 255, 255]),
        };
        let color = label.layout.as_ref().and_then(|l| l.color);
        let text_color = color.map_or(text_color, |c| [c.r, c.g, c.b]);
        let text_alpha = color.map_or(1.0, |c| f32::from(c.a) / 255.0);
        let lines: Vec<&str> = label.text.lines().collect();
        let line_height = f64::from(scaled.height() + scaled.line_gap());
        let widths: Vec<f64> = lines
            .iter()
            .map(|l| {
                l.chars()
                    .map(|c| f64::from(scaled.h_advance(font.glyph_id(c))))
                    .sum()
            })
            .collect();
        let width = widths.iter().copied().fold(0.0, f64::max);
        let height = line_height * lines.len().max(1) as f64;
        let top_left = at - DVec2::new(width / 2.0, height / 2.0);
        for y in (top_left.y - 2.0) as i64..=(top_left.y + height + 2.0) as i64 {
            for x in (top_left.x - 3.0) as i64..=(top_left.x + width + 3.0) as i64 {
                blend(image, x, y, fill, 0.85);
            }
        }
        for (row, text) in lines.iter().enumerate() {
            let mut x = top_left.x as f32;
            let baseline = top_left.y as f32 + row as f32 * line_height as f32 + scaled.ascent();
            for c in text.chars() {
                let id = font.glyph_id(c);
                let glyph = id.with_scale_and_position(PxScale::from(size), point(x, baseline));
                x += scaled.h_advance(id);
                if let Some(outlined) = font.outline_glyph(glyph) {
                    let b = outlined.px_bounds();
                    outlined.draw(|gx, gy, coverage| {
                        blend(
                            image,
                            i64::from(b.min.x as i32) + i64::from(gx),
                            i64::from(b.min.y as i32) + i64::from(gy),
                            text_color,
                            coverage * text_alpha,
                        );
                    });
                }
            }
        }
    }
}
