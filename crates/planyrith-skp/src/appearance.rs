//! Imports styles, scene appearance and sun settings (docs/09 §5).
use super::Result;
use glam::DVec2;
use openskp::Model;
use planyrith_core::{commands::Transaction, document::*, ids::StyleId, units::Angle};
use std::collections::BTreeMap;

fn color(c: [u8; 4]) -> Rgba8 {
    Rgba8 {
        r: c[0],
        g: c[1],
        b: c[2],
        a: c[3],
    }
}
fn width(on: bool, n: u32) -> Option<u8> {
    on.then_some(n.clamp(1, 255) as u8)
}
fn style(name: &str, r: &openskp::RenderingOptions) -> Style {
    let mut s = Style {
        name: name.into(),
        ..Default::default()
    };
    s.faces.mode = if r.xray {
        FaceMode::XRay
    } else {
        match r.face_style {
            0 => FaceMode::Wireframe,
            1 => FaceMode::HiddenLine,
            5 => FaceMode::Monochrome,
            _ => {
                if r.textures {
                    FaceMode::ShadedTextured
                } else {
                    FaceMode::Shaded
                }
            }
        }
    };
    s.faces.front_default = color(r.front_color);
    s.faces.back_default = color(r.back_color);
    s.edges.show = r.edges;
    s.edges.profiles = width(r.profiles, r.profiles_width);
    s.edges.depth_cue = width(r.depth_cue, r.depth_cue_width);
    s.edges.extension = width(r.extension, r.extension_length);
    s.edges.endpoints = width(r.endpoints, r.endpoints_length);
    s.edges.jitter = r.jitter;
    s.edges.back_edges = r.back_edges;
    s.edges.color = match r.edge_color_mode {
        0 => EdgeColorMode::ByMaterial,
        2 => EdgeColorMode::ByAxis,
        _ => EdgeColorMode::All,
    };
    s.background.color = color(r.background);
    s.background.sky = r.sky.then_some(color(r.sky_color));
    s.background.ground = r.ground.then_some((
        color(r.ground_color),
        1.0 - r.ground_transparency.min(100) as f32 / 100.0,
    ));
    s.background.show_ground_from_below = r.ground_from_below;
    s.modeling.selection_color = color(r.selected_color);
    s.modeling.locked_color = color(r.locked_color);
    s.modeling.guide_color = color(r.guides_color);
    s.modeling.inactive_fade = r.fade_rest_of_model.clamp(0.0, 1.0) as f32;
    s.modeling.section_line_color = color(r.section_cut_color);
    s.modeling.section_line_width_px = r.section_cut_width.max(1) as f32;
    if let Some(c) = r.section_fill_color {
        s.modeling.section_fill = color(c);
    }
    if r.fog_start_m.is_finite()
        && r.fog_end_m.is_finite()
        && r.fog_start_m >= 0.0
        && r.fog_end_m > r.fog_start_m
    {
        s.fog = Some(Fog {
            start: r.fog_start_m * 1000.0,
            end: r.fog_end_m * 1000.0,
            use_background_color: r.fog_uses_background,
            color: color(r.fog_color),
        });
    }
    s
}
fn watermarks(
    tx: &mut Transaction<'_>,
    marks: &[openskp::Watermark],
    visible: bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<Watermark>> {
    let mut out = Vec::new();
    for mark in marks {
        if !mark.blend.is_finite() || !mark.scale.is_finite() || mark.scale < 0.0 {
            return Err("invalid watermark placement".into());
        }
        let Some(bytes) = &mark.image_bytes else {
            warnings.push(format!("Watermark {} has no embedded image", mark.name));
            continue;
        };
        let placement = if mark.stretched {
            WatermarkPlacement::Stretched
        } else if mark.tiled {
            WatermarkPlacement::Tiled
        } else if mark.position == 4 {
            WatermarkPlacement::Centered
        } else {
            let i = mark.position.min(8);
            WatermarkPlacement::Anchored((i % 3) as f32 / 2.0, (i / 3) as f32 / 2.0)
        };
        out.push(Watermark {
            image: tx.insert_image(super::images::asset(bytes)?)?,
            placement,
            opacity: if visible {
                mark.blend.clamp(0.0, 1.0) as f32
            } else {
                0.0
            },
            scale: mark.scale as f32,
            behind_model: mark.background,
        });
        if mark.mask {
            warnings.push(format!("Watermark {}: luminance mask is retained in the original source; native display uses its alpha channel",mark.name));
        }
    }
    Ok(out)
}

pub(super) fn shadows(s: &openskp::ShadowInfo) -> Result<ShadowSettings> {
    if !s.latitude.is_finite()
        || s.latitude.abs() > 90.0
        || !s.longitude.is_finite()
        || s.longitude.abs() > 180.0
        || !s.tz_offset_h.is_finite()
        || s.tz_offset_h.abs() > 24.0
        || s.north.iter().any(|v| !v.is_finite())
    {
        return Err("invalid geographic shadow settings".into());
    }
    let offset = s.tz_offset_h + if s.daylight_saving { 1.0 } else { 0.0 };
    let local = i64::from(s.time) + (offset * 3600.0).round() as i64;
    let days = local.div_euclid(86400);
    let sec = local.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let north = Angle::radians(DVec2::X.angle_to(DVec2::new(s.north[1], s.north[0])));
    Ok(ShadowSettings {
        enabled: s.displayed,
        sun: SunModel::Geo {
            date: Date {
                year: year as i32,
                month: month as u8,
                day: day as u8,
            },
            time: TimeOfDay {
                hour: (sec / 3600) as u8,
                minute: (sec % 3600 / 60) as u8,
            },
            lat: s.latitude,
            lon: s.longitude,
            utc_offset: offset,
            north_angle: north,
        },
        light: s.light.min(100) as u8,
        dark: s.dark.min(100) as u8,
        on_faces: s.on_faces,
        on_ground: s.on_ground,
        from_edges: s.from_edges,
    })
}
pub(super) fn import(
    tx: &mut Transaction<'_>,
    model: &Model,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<String, StyleId>> {
    let mut ids = Vec::new();
    let mut lookup = BTreeMap::new();
    for (i, src) in model.styles.iter().enumerate() {
        let mut name = if src.name.is_empty() {
            format!("Imported style {}", i + 1)
        } else {
            src.name.clone()
        };
        while tx.document().styles.by_name(&name).is_some() && !(i == 0 && name == "Default") {
            name.push_str(" (imported)");
        }
        let mut s = style(&name, &src.settings);
        s.watermarks = watermarks(tx, &src.watermarks, src.settings.watermarks, warnings)?;
        let id = if i == 0 {
            let id = tx.document().styles.active();
            tx.replace_style(id, s)?;
            id
        } else {
            tx.insert_style(s)?
        };
        ids.push(id);
        lookup.insert(src.guid.clone(), id);
        lookup.insert(src.name.clone(), id);
        if src.strokes.is_some() {
            warnings.push(format!("Style {}: custom stroke bitmaps are retained in the original source; native edges use its width and jitter settings",src.name));
        }
    }
    let active = model
        .active_style
        .and_then(|i| ids.get(i).copied())
        .unwrap_or(tx.document().styles.active());
    tx.set_active_style(active)?;
    if let Some(r) = &model.rendering {
        let name = tx
            .document()
            .styles
            .get(active)
            .map_or("Imported".into(), |s| s.name.clone());
        let mut s = style(&name, r);
        s.watermarks = watermarks(tx, &model.watermarks, r.watermarks, warnings)?;
        tx.replace_style(active, s)?;
        let mut rendering = tx.document().rendering.clone();
        rendering.fog_enabled = r.fog;
        rendering.section.planes_visible = r.section_display & 1 != 0;
        rendering.section.cuts_visible = r.section_display & 2 != 0;
        rendering.section.fills_visible = r.section_fill.unwrap_or(true);
        tx.set_rendering(rendering)?;
        tx.set_attribute(
            planyrith_core::commands::transaction::AttrTarget::Document,
            "org.planyrith.skp",
            "axes-visible",
            Some(Value::Bool(r.model_axes)),
        )?;
    }
    if let Some(s) = &model.shadows {
        let mut rendering = tx.document().rendering.clone();
        rendering.shadows = shadows(s)?;
        tx.set_rendering(rendering)?;
    }
    Ok(lookup)
}
pub(super) fn scene_style(
    tx: &mut Transaction<'_>,
    scene: &openskp::Scene,
    lookup: &BTreeMap<String, StyleId>,
) -> Result<Option<StyleId>> {
    if let Some(r) = &scene.rendering {
        let mut name = format!("Scene {}", scene.name);
        while tx.document().styles.by_name(&name).is_some() {
            name.push_str(" (imported)");
        }
        return Ok(Some(tx.insert_style(style(&name, r))?));
    }
    Ok(scene.style.as_ref().and_then(|s| lookup.get(s).copied()))
}
