//! Exports editable text and associative dimensions (docs/09 §5).
use super::{Result, encode};
use glam::DVec3;
use openskp::Model;
use planyrith_core::{
    document::annotations::{anchor_position, dimension_view, resolve_steps},
    document::*,
    ids::{EntityId, TagId},
};
use std::collections::BTreeMap;

pub(super) fn arrow(arrow: ArrowStyle) -> u32 {
    match arrow {
        ArrowStyle::None => 0,
        ArrowStyle::Slash => 1,
        ArrowStyle::Dot => 2,
        ArrowStyle::Closed => 3,
        ArrowStyle::Open => 4,
    }
}
fn font(model: &mut Model, font: TextFont, warnings: &mut Vec<String>) -> Result<usize> {
    if !font.size_pt.is_finite()
        || font.size_pt <= 0.0
        || font.size_pt > f64::from(u32::MAX)
        || font
            .model_height_mm
            .is_some_and(|h| !h.is_finite() || h <= 0.0)
    {
        return Err("invalid annotation font size".into());
    }
    if (font.size_pt - font.size_pt.round()).abs() > 1e-9 {
        warnings.push("SKP font sizes use whole points; a fractional size was rounded".into());
    }
    let font = openskp::Font {
        family: font.family,
        bold: font.bold,
        italic: font.italic,
        size_pt: font.size_pt.round().max(1.0) as u32,
        model_height: font.model_height_mm.is_some(),
        height_m: font
            .model_height_mm
            .map_or(openskp::Font::DEFAULT_HEIGHT_M, |h| h / 1000.0),
    };
    Ok(
        if let Some(i) = model.fonts.iter().position(|f| f == &font) {
            i
        } else {
            let i = model.fonts.len();
            model.fonts.push(font);
            i
        },
    )
}
fn path(
    doc: &Document,
    from: ContainerRef,
    path: &EntityPath,
) -> Result<(ContainerRef, u32, Vec<u32>)> {
    let mut c = from;
    let mut steps = Vec::new();
    for step in &path.instances.steps {
        let entities = doc.container(c).ok_or("anchor container missing")?;
        let id = entities
            .key_to_id(*step)
            .ok_or("detached annotation path")?;
        steps.push(encode::pid(entities.pid_of(id))?);
        c = ContainerRef::Definition(
            entities
                .definition_of(id)
                .ok_or("annotation path is not an instance")?,
        );
    }
    let entities = doc.container(c).ok_or("anchor container missing")?;
    let id = entities
        .key_to_id(path.target)
        .ok_or("detached annotation target")?;
    if !matches!(id, EntityId::Edge(_)) {
        return Err("annotation target is not an edge".into());
    }
    Ok((c, encode::pid(entities.pid_of(id))?, steps))
}
fn anchor(
    doc: &Document,
    c: ContainerRef,
    a: &Anchor,
    warnings: &mut Vec<String>,
) -> Result<openskp::Anchor> {
    if let AnchorKind::Point(p) = a.kind
        && a.position.is_none()
    {
        let mut current = c;
        let mut steps = Vec::new();
        for step in &a.path.instances.steps {
            let entities = doc.container(current).ok_or("anchor owner missing")?;
            let id = entities.key_to_id(*step).ok_or("detached point path")?;
            steps.push(encode::pid(entities.pid_of(id))?);
            current = ContainerRef::Definition(
                entities
                    .definition_of(id)
                    .ok_or("point path not an instance")?,
            );
        }
        return Ok(openskp::Anchor {
            kind: 1,
            point_m: (p / 1000.0).to_array(),
            path: steps,
            ..Default::default()
        });
    }
    let (_, entity, steps) = path(doc, c, &a.path)?;
    let mut out = openskp::Anchor {
        entity: Some(entity),
        path: steps,
        ..Default::default()
    };
    match &a.position {
        Some(AnchorPosition::CurveCenter) => out.kind = 10,
        Some(AnchorPosition::Intersection(other)) => {
            let (_, other, steps) = path(doc, c, other)?;
            out.kind = 9;
            out.second = Some(other);
            out.second_path = steps;
        }
        Some(AnchorPosition::EdgeParameter(t)) => {
            out.kind = 5;
            out.point_m = [*t, 0.0, 0.0];
        }
        Some(AnchorPosition::CurveParameter(_)) => {
            warnings.push("A text attached at an analytic curve angle exports at a fixed point; SKP edge attachments follow its segmented geometry".into());
            return Ok(openskp::Anchor {
                kind: 1,
                point_m: (anchor_position(doc, c, a).ok_or("detached curve angle")? / 1000.0)
                    .to_array(),
                ..Default::default()
            });
        }
        None => match a.kind {
            AnchorKind::Mid => out.kind = 4,
            AnchorKind::Start => {
                out.kind = 5;
            }
            AnchorKind::End => {
                out.kind = 5;
                out.point_m = [1.0, 0.0, 0.0];
            }
            AnchorKind::Point(_) => {}
        },
    }
    Ok(out)
}
fn color_material(model: &mut Model, color: Option<Rgba8>) -> Result<Option<u16>> {
    let Some(color) = color else { return Ok(None) };
    let rgba = [color.r, color.g, color.b, color.a];
    if let Some((i,_))=model.materials.iter().enumerate().find(|(_,m)|matches!(m,openskp::Material::Solid{rgba:c,opacity,..} if *c==rgba && (*opacity-1.0).abs()<1e-9)) {return Ok(model.material_links.iter().find(|(_,n)|*n==i).map(|(slot,_)|*slot));}
    let i = model.materials.len();
    let slot = u16::try_from(i + 1)?;
    model.materials.push(openskp::Material::Solid {
        name: format!(
            "Annotation #{:02x}{:02x}{:02x}{:02x}",
            color.r, color.g, color.b, color.a
        ),
        rgba,
        opacity: 1.0,
        colorize_type: 0,
    });
    model.material_links.push((slot, i));
    Ok(Some(slot))
}
pub(super) fn append(
    doc: &Document,
    model: &mut Model,
    tags: &BTreeMap<TagId, u16>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    for (ri, c) in doc
        .definition_order
        .iter()
        .map(|d| ContainerRef::Definition(*d))
        .chain(std::iter::once(ContainerRef::Model))
        .enumerate()
    {
        let entities = doc.container(c).ok_or("annotation container missing")?;
        let reference = model
            .geometry
            .get(ri)
            .ok_or("annotation geometry run missing")?
            .def_index;
        for id in entities.order().iter().copied() {
            let layer = encode::layer(entities, id, tags);
            let pid = encode::pid(entities.pid_of(id))?;
            match entities.view(id) {
                Some(EntityView::Text(t)) => {
                    let layout = t.layout.clone().unwrap_or_else(|| TextLayout {
                        leader_visible: t.anchor.is_some(),
                        arrow: ArrowStyle::Dot,
                        ..Default::default()
                    });
                    let font = font(
                        model,
                        layout.font.clone().unwrap_or_else(|| TextFont {
                            size_pt: 9.0,
                            ..Default::default()
                        }),
                        warnings,
                    )?;
                    let material = color_material(model, layout.color)?;
                    let attached = t
                        .anchor
                        .as_ref()
                        .map(|a| anchor(doc, c, a, warnings))
                        .transpose()?
                        .unwrap_or_default();
                    let world = layout.world_offset;
                    let leader = if t.anchor.is_none() {
                        openskp::Leader::None
                    } else if world.is_some() {
                        openskp::Leader::Pushpin
                    } else {
                        openskp::Leader::ViewBased
                    };
                    let text = openskp::Text {
                        pid,
                        def_index: reference,
                        content: t.text.clone(),
                        screen_position: layout
                            .screen_position
                            .unwrap_or(glam::DVec2::splat(0.02))
                            .to_array(),
                        anchor: attached,
                        leader,
                        leader_offset: world
                            .map_or([t.screen_offset.x, t.screen_offset.y, 0.0], |v| {
                                (v / 1000.0).to_array()
                            }),
                        arrow: arrow(layout.arrow),
                        font: Some(font),
                        material,
                        layer,
                        hidden: entities.is_hidden(id),
                        leader_hidden: !layout.leader_visible,
                        ..Default::default()
                    };
                    if let Some(a) = entities.attrs(id) {
                        encode::attributes(
                            model,
                            openskp::Owner::Text(model.texts.len()),
                            a,
                            warnings,
                        );
                    }
                    model.texts.push(text);
                    model.geometry[ri].topology.texts += 1;
                }
                Some(EntityView::Dimension(dim)) => {
                    let view = dimension_view(doc, c, dim);
                    if view.detached {
                        return Err("cannot export a detached dimension to SKP".into());
                    }
                    let (style, text_override) = match dim {
                        Dimension::Linear {
                            style,
                            text_override,
                            ..
                        }
                        | Dimension::Radial {
                            style,
                            text_override,
                            ..
                        } => (style, text_override),
                    };
                    let font = font(
                        model,
                        style.font.clone().unwrap_or_else(|| TextFont {
                            model_height_mm: Some(style.text_height.as_mm()),
                            ..Default::default()
                        }),
                        warnings,
                    )?;
                    let material = color_material(model, style.color)?;
                    match dim {
                        Dimension::Linear {
                            start,
                            end,
                            offset,
                            direction,
                            ..
                        } => {
                            let delta = view.measured[1] - view.measured[0];
                            let x = direction.unwrap_or(delta).normalize_or_zero();
                            let perpendicular = *offset - x * offset.dot(x);
                            let y = if let Some(y) = perpendicular.try_normalize() {
                                y
                            } else {
                                planyrith_core::math::Plane::from_point_normal(DVec3::ZERO, x)
                                    .frame()
                                    .u
                            };
                            if (offset.dot(x)).abs() > doc.tolerances().length {
                                warnings.push("A dimension's offset along its measuring direction cannot be represented in SKP".into());
                            }
                            let d = openskp::Dimension {
                                pid,
                                def_index: reference,
                                text_override: text_override.clone().unwrap_or_default(),
                                start: anchor(doc, c, start, warnings)?,
                                end: anchor(doc, c, end, warnings)?,
                                normal: x.cross(y).normalize_or_zero().to_array(),
                                x_axis: x.to_array(),
                                offset_m: perpendicular.length() / 1000.0,
                                text_position: 0,
                                aligned: true,
                                font: Some(font),
                                arrow: arrow(style.arrow),
                                layer,
                                material,
                                hidden: entities.is_hidden(id),
                                casts_shadows: true,
                                receives_shadows: true,
                                variant: 1,
                                undecoded: 0.0,
                            };
                            if let Some(a) = entities.attrs(id) {
                                encode::attributes(
                                    model,
                                    openskp::Owner::Dimension(model.dimensions.len()),
                                    a,
                                    warnings,
                                );
                            }
                            model.dimensions.push(d);
                            model.geometry[ri].topology.dimensions += 1;
                        }
                        Dimension::Radial {
                            curve,
                            kind,
                            leader,
                            ..
                        } => {
                            let (inner, world) = resolve_steps(doc, c, &curve.path.instances.steps)
                                .ok_or("detached radial path")?;
                            if inner != c {
                                return Err("radial dimensions across nested occurrences cannot be represented in SKP".into());
                            }
                            let (_, edge, _) = path(doc, c, &curve.path)?;
                            let offset = view.measured[1] - view.measured[0];
                            let n = doc
                                .container(inner)
                                .and_then(|e| {
                                    e.key_to_id(curve.path.target).and_then(|id| {
                                        if let EntityId::Edge(id) = id {
                                            e.mesh()
                                                .edge(id)
                                                .and_then(|e| e.curve)
                                                .and_then(|id| e.mesh().curve(id))
                                        } else {
                                            None
                                        }
                                    })
                                })
                                .and_then(|c| match c {
                                    planyrith_core::geom::CurveDescriptor::Circle {
                                        normal,
                                        ..
                                    }
                                    | planyrith_core::geom::CurveDescriptor::Arc {
                                        normal, ..
                                    } => Some(world.transform_vector3(*normal)),
                                    _ => None,
                                })
                                .ok_or("radial dimension curve missing")?;
                            let frame =
                                planyrith_core::math::Plane::from_point_normal(view.measured[0], n)
                                    .frame();
                            let angle = glam::DVec2::X.angle_to(glam::DVec2::new(
                                offset.dot(frame.u),
                                offset.dot(frame.v),
                            ));
                            let outward = offset.normalize_or_zero();
                            if (leader - outward * leader.dot(outward)).length()
                                > doc.tolerances().length
                            {
                                warnings.push("A radial dimension's sideways leader component cannot be represented in SKP".into());
                            }
                            let d = openskp::RadialDimension {
                                pid,
                                def_index: reference,
                                edge: Some(edge),
                                diameter: *kind == RadialKind::Diameter,
                                leader: [angle, -leader.dot(outward) / 25.4],
                                text_override: text_override.clone().unwrap_or_default(),
                                font: Some(font),
                                arrow: arrow(style.arrow),
                                material,
                                layer,
                                hidden: entities.is_hidden(id),
                                ..Default::default()
                            };
                            if let Some(a) = entities.attrs(id) {
                                encode::attributes(
                                    model,
                                    openskp::Owner::RadialDimension(model.radial_dimensions.len()),
                                    a,
                                    warnings,
                                );
                            }
                            model.radial_dimensions.push(d);
                            model.geometry[ri].topology.radial_dimensions += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}
