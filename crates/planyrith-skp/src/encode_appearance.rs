//! Exports styles, scene appearance and geographic shadows (docs/09 §5).
use super::Result;
use openskp::Model;
use planyrith_core::{document::*, ids::StyleId};
use std::collections::BTreeMap;

fn rgba(c: Rgba8) -> [u8; 4] {
    [c.r, c.g, c.b, c.a]
}
pub(super) fn rendering(s: &Style) -> openskp::RenderingOptions {
    let faces = &s.faces;
    let edges = &s.edges;
    let background = &s.background;
    let modeling = &s.modeling;
    openskp::RenderingOptions {
        face_style: match faces.mode {
            FaceMode::Wireframe => 0,
            FaceMode::HiddenLine => 1,
            FaceMode::Monochrome => 5,
            _ => 2,
        },
        xray: faces.mode == FaceMode::XRay,
        textures: faces.mode == FaceMode::ShadedTextured,
        material_transparency: true,
        front_color: rgba(faces.front_default),
        back_color: rgba(faces.back_default),
        edges: edges.show,
        profiles: edges.profiles.is_some(),
        profiles_width: u32::from(edges.profiles.unwrap_or(2)),
        depth_cue: edges.depth_cue.is_some(),
        depth_cue_width: u32::from(edges.depth_cue.unwrap_or(2)),
        extension: edges.extension.is_some(),
        extension_length: u32::from(edges.extension.unwrap_or(2)),
        endpoints: edges.endpoints.is_some(),
        endpoints_length: u32::from(edges.endpoints.unwrap_or(2)),
        jitter: edges.jitter,
        back_edges: edges.back_edges,
        edge_color_mode: match edges.color {
            EdgeColorMode::ByMaterial => 0,
            EdgeColorMode::All => 1,
            EdgeColorMode::ByAxis => 2,
        },
        edge_color: [20, 20, 20, 255],
        background: rgba(background.color),
        sky: background.sky.is_some(),
        sky_color: rgba(background.sky.unwrap_or(Rgba8::WHITE)),
        ground: background.ground.is_some(),
        ground_color: rgba(background.ground.map_or(Rgba8::WHITE, |(c, _)| c)),
        ground_transparency: background.ground.map_or(0, |(_, a)| {
            ((1.0 - a.clamp(0.0, 1.0)) * 100.0).round() as u32
        }),
        ground_from_below: background.show_ground_from_below,
        selected_color: rgba(modeling.selection_color),
        locked_color: rgba(modeling.locked_color),
        guides_color: rgba(modeling.guide_color),
        fade_rest_of_model: f64::from(modeling.inactive_fade),
        section_cut_color: rgba(modeling.section_line_color),
        section_cut_width: modeling
            .section_line_width_px
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u32,
        section_fill_color: Some(rgba(modeling.section_fill)),
        fog: s.fog.is_some(),
        fog_color: rgba(s.fog.map_or(background.color, |f| f.color)),
        fog_uses_background: s.fog.is_none_or(|f| f.use_background_color),
        fog_start_m: s.fog.map_or(-1.0, |f| f.start / 1000.0),
        fog_end_m: s.fog.map_or(-1.0, |f| f.end / 1000.0),
        watermarks: !s.watermarks.is_empty(),
        model_axes: true,
        section_display: 3,
        section_fill: Some(true),
        ..Default::default()
    }
}
fn watermarks(
    doc: &Document,
    marks: &[Watermark],
    warnings: &mut Vec<String>,
) -> Result<Vec<openskp::Watermark>> {
    marks.iter().enumerate().map(|(i,w)| {
        let bytes=doc.images.get(w.image).ok_or("watermark asset missing")?;
        let mut image=bytes.bytes.clone();
        if bytes.format==ImageFormat::WebP {let decoded=image::load_from_memory(&image)?;let mut png=std::io::Cursor::new(Vec::new());decoded.write_to(&mut png,image::ImageFormat::Png)?;image=png.into_inner();}
        let position=match w.placement {WatermarkPlacement::Anchored(x,y)=>{
            if (x*2.0-(x*2.0).round()).abs()>1e-6 || (y*2.0-(y*2.0).round()).abs()>1e-6 {warnings.push("SKP watermark positions use a 3 by 3 grid; an intermediate position was rounded".into());}
            (y.clamp(0.0,1.0)*2.0).round() as u32*3+(x.clamp(0.0,1.0)*2.0).round() as u32},_=>4};
        Ok(openskp::Watermark {name:format!("Watermark {}",i+1),source_file:String::new(),background:w.behind_model,tiled:matches!(w.placement,WatermarkPlacement::Tiled),stretched:matches!(w.placement,WatermarkPlacement::Stretched),keep_aspect_ratio:true,position,mask:false,blend:f64::from(w.opacity),scale:f64::from(w.scale),image_bytes:Some(image),time:0})
    }).collect()
}
pub(super) fn shadows(
    s: &ShadowSettings,
    warnings: &mut Vec<String>,
) -> Result<Option<openskp::ShadowInfo>> {
    let SunModel::Geo {
        date,
        time,
        lat,
        lon,
        utc_offset,
        north_angle,
    } = s.sun
    else {
        if s.enabled {
            warnings.push("A manual sun direction cannot be represented by SKP's geographic sun settings; keep a native copy".into());
        }
        return Ok(Some(openskp::ShadowInfo {
            displayed: s.enabled,
            on_faces: s.on_faces,
            on_ground: s.on_ground,
            from_edges: s.from_edges,
            light: u32::from(s.light),
            dark: u32::from(s.dark),
            ..Default::default()
        }));
    };
    // Proleptic Gregorian civil date -> days since 1970, inverse of import.
    let year = i64::from(date.year) - i64::from(date.month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let m = i64::from(date.month);
    let day = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(date.day) - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + day - 719468;
    let seconds = days * 86400 + i64::from(time.hour) * 3600 + i64::from(time.minute) * 60
        - (utc_offset * 3600.0).round() as i64;
    let epoch = u32::try_from(seconds)
        .map_err(|_| "SKP geographic shadow time is outside its unsigned 32-bit epoch range")?;
    let direction = glam::DVec2::from_angle(north_angle.as_radians());
    Ok(Some(openskp::ShadowInfo {
        time: epoch,
        latitude: lat,
        longitude: lon,
        tz_offset_h: utc_offset,
        north: [direction.y, direction.x, 0.0],
        displayed: s.enabled,
        on_faces: s.on_faces,
        on_ground: s.on_ground,
        from_edges: s.from_edges,
        light: u32::from(s.light),
        dark: u32::from(s.dark),
        use_sun_for_shading: true,
        ..Default::default()
    }))
}
pub(super) fn append(
    doc: &Document,
    model: &mut Model,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<StyleId, String>> {
    let mut styles = BTreeMap::new();
    for (i, (id, s)) in doc.styles.iter().enumerate() {
        let guid =
            blake3::hash(format!("style:{i}:{}", s.name).as_bytes()).to_hex()[..32].to_owned();
        model.styles.push(openskp::Style {
            name: s.name.clone(),
            description: String::new(),
            guid: guid.clone(),
            settings: rendering(s),
            watermarks: watermarks(doc, &s.watermarks, warnings)?,
            strokes: None,
            items: Vec::new(),
        });
        if id == doc.styles.active() {
            model.active_style = Some(i);
        }
        styles.insert(id, guid);
    }
    if let Some(style) = doc.styles.active_style() {
        let mut r = rendering(style);
        r.fog = doc.rendering.fog_enabled;
        r.section_display = u32::from(doc.rendering.section.planes_visible)
            | (u32::from(doc.rendering.section.cuts_visible) << 1);
        r.section_fill = Some(doc.rendering.section.fills_visible);
        r.model_axes = doc
            .attrs
            .get("org.planyrith.skp", "axes-visible")
            .and_then(|v| {
                if let Value::Bool(v) = v {
                    Some(*v)
                } else {
                    None
                }
            })
            .unwrap_or(true);
        model.rendering = Some(r);
        model.watermarks = watermarks(doc, &style.watermarks, warnings)?;
    }
    model.shadows = shadows(&doc.rendering.shadows, warnings)?;
    model.geo_located = doc.info.geo.is_some();
    for (name, data) in doc
        .attrs
        .0
        .get("org.planyrith.skp.classifications")
        .into_iter()
        .flat_map(|entries| entries.iter())
    {
        if let Value::Bytes(data) = data {
            model.classifications.push(openskp::Classification {
                name: name
                    .split_once(':')
                    .map_or(name.as_str(), |(_, name)| name)
                    .into(),
                source_path: String::new(),
                data: data.clone(),
            });
        }
    }
    Ok(styles)
}
