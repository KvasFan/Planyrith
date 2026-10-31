//! Imports text, typography and associative dimensions (docs/09 §5).
use super::{Result, decode};
use glam::{DVec2, DVec3};
use openskp::Model;
use planyrith_core::{
    commands::Transaction,
    document::*,
    ids::{DefinitionId, EntityId, MaterialId, Pid, TagId},
    units::Length,
};
use std::collections::BTreeMap;

fn owner(reference: Option<usize>, defs: &BTreeMap<usize, DefinitionId>) -> Result<ContainerRef> {
    Ok(match reference {
        None => ContainerRef::Model,
        Some(r) => ContainerRef::Definition(*defs.get(&r).ok_or("annotation definition missing")?),
    })
}
fn path_to(doc: &Document, from: ContainerRef, pid: Pid) -> Option<Vec<EntityKey>> {
    let mut queue = vec![(from, Vec::new())];
    let mut cursor = 0;
    while cursor < queue.len() && cursor < 4096 {
        let (container, path) = queue[cursor].clone();
        cursor += 1;
        let entities = doc.container(container)?;
        if entities.id_of(pid).is_some() {
            return Some(path);
        }
        if path.len() >= 32 {
            continue;
        }
        for id in entities.order() {
            if let (Some(def), Some(p)) = (entities.definition_of(*id), entities.pid_of(*id)) {
                let mut path = path.clone();
                path.push(EntityKey::Pid(p));
                queue.push((ContainerRef::Definition(def), path));
            }
        }
    }
    None
}
fn anchor(
    doc: &Document,
    model: &Model,
    container: ContainerRef,
    reference: Option<usize>,
    a: &openskp::Anchor,
    pids: &BTreeMap<u32, Vec<Pid>>,
) -> Result<Anchor> {
    if a.point_m.iter().any(|v| !v.is_finite() || v.abs() > 1e6) {
        return Err("invalid annotation anchor coordinate or parameter".into());
    }
    if a.kind == 9 {
        let mut first = a.clone();
        first.kind = 4;
        let mut second = first.clone();
        second.entity = Some(a.second.ok_or("intersection anchor has no second edge")?);
        second.path = a.second_path.clone();
        let mut native = anchor(doc, model, container, reference, &first, pids)?;
        let other = anchor(doc, model, container, reference, &second, pids)?;
        native.position = Some(AnchorPosition::Intersection(other.path));
        return Ok(native);
    }
    let mut current = container;
    let mut source = reference;
    let mut steps = Vec::new();
    for instance in &a.path {
        let run = model
            .geometry
            .iter()
            .find(|r| r.def_index == source)
            .ok_or("anchor source container missing")?;
        let placed = run
            .placed
            .iter()
            .find(|p| p.pid == *instance)
            .ok_or("anchor source instance missing")?;
        let pid = pids
            .get(instance)
            .and_then(|ps| {
                ps.iter()
                    .find(|p| doc.locate_pid(**p).is_some_and(|(c, _)| c == current))
            })
            .copied()
            .ok_or("anchor native instance missing")?;
        let (_, id) = doc.locate_pid(pid).ok_or("anchor instance missing")?;
        let def = doc
            .container(current)
            .and_then(|c| c.definition_of(id))
            .ok_or("anchor path is not an instance")?;
        steps.push(EntityKey::Pid(pid));
        current = ContainerRef::Definition(def);
        source = Some(placed.defref as usize);
    }
    if matches!(a.kind, 4 | 5 | 10)
        && let Some(pid) = a
            .entity
            .and_then(|id| pids.get(&id))
            .and_then(|ps| {
                ps.iter().find(|p| {
                    doc.locate_pid(**p)
                        .is_some_and(|(_, id)| matches!(id, EntityId::Edge(_)))
                })
            })
            .copied()
        && let Some(inner) = path_to(doc, current, pid)
    {
        steps.extend(inner);
        let kind = if a.kind == 4 || (a.point_m[0] - 0.5).abs() < 1e-9 {
            AnchorKind::Mid
        } else if a.point_m[0].abs() < 1e-9 {
            AnchorKind::Start
        } else {
            AnchorKind::End
        };
        return Ok(Anchor {
            position: Some(if a.kind == 10 {
                AnchorPosition::CurveCenter
            } else {
                AnchorPosition::EdgeParameter(if a.kind == 4 { 0.5 } else { a.point_m[0] })
            }),
            path: EntityPath::new(InstancePath { steps }, EntityKey::Pid(pid)),
            kind,
        });
    }
    let run = model.geometry.iter().find(|r| r.def_index == source);
    let point = match a.kind {
        2 => {
            let r = run.ok_or("anchor mesh missing")?;
            let i = r
                .mesh
                .vertex_pids
                .iter()
                .position(|p| Some(*p) == a.entity)
                .ok_or("anchor vertex missing")?;
            DVec3::from_array(
                *r.mesh
                    .vertices
                    .get(i)
                    .ok_or("anchor vertex index invalid")?,
            ) * 1000.0
        }
        4 | 5 | 10 => {
            let r = run.ok_or("anchor mesh missing")?;
            let e = r
                .mesh
                .edges
                .iter()
                .find(|e| Some(e.pid) == a.entity)
                .ok_or("anchor edge missing")?;
            if a.kind == 10 {
                let arc = e
                    .curve
                    .and_then(|i| r.curves.get(i as usize))
                    .and_then(|c| c.arc.as_ref())
                    .ok_or("anchor arc missing")?;
                DVec3::from_array(arc.center_m) * 1000.0
            } else {
                let x = DVec3::from_array(
                    *r.mesh
                        .vertices
                        .get(e.v0 as usize)
                        .ok_or("anchor vertex missing")?,
                );
                let y = DVec3::from_array(
                    *r.mesh
                        .vertices
                        .get(e.v1 as usize)
                        .ok_or("anchor vertex missing")?,
                );
                x.lerp(y, if a.kind == 4 { 0.5 } else { a.point_m[0] }) * 1000.0
            }
        }
        0 | 1 => DVec3::from_array(a.point_m) * 1000.0,
        _ => return Err(format!("unsupported annotation anchor kind {}", a.kind).into()),
    };
    if !point.is_finite() {
        return Err("non-finite annotation anchor".into());
    }
    let mut anchor = planyrith_core::document::annotations::find_anchor(doc, current, point);
    steps.extend(anchor.path.instances.steps);
    anchor.path.instances.steps = steps;
    Ok(anchor)
}
fn font(model: &Model, index: Option<usize>) -> Option<TextFont> {
    index.and_then(|i| model.fonts.get(i)).map(|f| TextFont {
        family: f.family.clone(),
        bold: f.bold,
        italic: f.italic,
        size_pt: f64::from(f.size_pt.max(1)),
        model_height_mm: (f.model_height && f.height_m > 0.0 && f.height_m.is_finite())
            .then_some(f.height_m * 1000.0),
    })
}
fn arrow(n: u32) -> ArrowStyle {
    match n {
        1 => ArrowStyle::Slash,
        2 => ArrowStyle::Dot,
        3 => ArrowStyle::Closed,
        4 => ArrowStyle::Open,
        _ => ArrowStyle::None,
    }
}
fn dim_style(model: &Model, index: Option<usize>, mark: u32, color: Option<Rgba8>) -> DimStyle {
    let font = font(model, index);
    let height = font
        .as_ref()
        .and_then(|f| f.model_height_mm)
        .unwrap_or(100.0);
    DimStyle {
        color,
        font,
        text_height: Length::mm(height),
        arrow: arrow(mark),
        ..Default::default()
    }
}
fn attributes(
    tx: &mut Transaction<'_>,
    model: &Model,
    c: ContainerRef,
    id: EntityId,
    owner: openskp::Owner,
) -> Result<()> {
    decode::entity_attributes(tx, c, id, decode::attributes(model, owner))
}
fn native_meta(
    tx: &mut Transaction<'_>,
    c: ContainerRef,
    id: EntityId,
    pid: Pid,
    source: (u32, bool, Option<TagId>),
    pids: &mut BTreeMap<u32, Vec<Pid>>,
) -> Result<()> {
    decode::metadata(tx, c, id, source.1, source.2)?;
    if source.0 != 0 {
        pids.entry(source.0).or_default().push(pid);
    }
    Ok(())
}

pub(super) fn import(
    tx: &mut Transaction<'_>,
    model: &Model,
    defs: &BTreeMap<usize, DefinitionId>,
    tags: &[TagId],
    materials: &[MaterialId],
    pids: &mut BTreeMap<u32, Vec<Pid>>,
) -> Result<()> {
    for (i, t) in model.texts.iter().enumerate() {
        if t.screen_position
            .iter()
            .chain(t.leader_offset.iter())
            .any(|v| !v.is_finite() || v.abs() > 1e6)
        {
            return Err("invalid text placement".into());
        }
        let c = owner(t.def_index, defs)?;
        let attached = if t.anchor.kind == 0 {
            None
        } else {
            Some(anchor(
                tx.document(),
                model,
                c,
                t.def_index,
                &t.anchor,
                pids,
            )?)
        };
        let color = decode::material(model, materials, t.material)
            .and_then(|m| tx.document().materials.get(m))
            .map(|m| m.color)
            .or_else(|| {
                model.text_defaults.as_ref().map(|d| {
                    let c = if attached.is_some() {
                        d.leader_text_color
                    } else {
                        d.screen_text_color
                    };
                    Rgba8 {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                        a: c[3],
                    }
                })
            });
        let pushpin = t.leader == openskp::Leader::Pushpin;
        let layout = TextLayout {
            screen_position: attached
                .is_none()
                .then_some(DVec2::from_array(t.screen_position)),
            world_offset: (pushpin && attached.is_some())
                .then_some(DVec3::from_array(t.leader_offset) * 1000.0),
            leader_visible: t.leader != openskp::Leader::None && !t.leader_hidden,
            font: font(
                model,
                t.font.or_else(|| {
                    model.text_defaults.as_ref().and_then(|d| {
                        if attached.is_some() {
                            d.font
                        } else {
                            d.screen_font
                        }
                    })
                }),
            ),
            arrow: arrow(t.arrow),
            color,
        };
        let record = Text {
            anchor: attached,
            text: t.content.clone(),
            screen_offset: if pushpin {
                DVec2::ZERO
            } else {
                DVec2::new(t.leader_offset[0], t.leader_offset[1])
            },
            layout: Some(layout),
        };
        let (id, pid) = tx.insert_entity(c, EntityRecord::Text(record))?;
        native_meta(
            tx,
            c,
            id,
            pid,
            (t.pid, t.hidden, decode::tag(model, tags, t.layer)),
            pids,
        )?;
        attributes(tx, model, c, id, openskp::Owner::Text(i))?;
    }
    for (i, d) in model.dimensions.iter().enumerate() {
        let c = owner(d.def_index, defs)?;
        let start = anchor(tx.document(), model, c, d.def_index, &d.start, pids)?;
        let end = anchor(tx.document(), model, c, d.def_index, &d.end, pids)?;
        let x = glam::DVec3::from_array(d.x_axis).normalize_or_zero();
        let n = glam::DVec3::from_array(d.normal).normalize_or_zero();
        let (direction, offset) = if d.variant == 2 {
            (n.cross(x), -x)
        } else {
            (x, n.cross(x))
        };
        if direction.length_squared() < 0.5
            || offset.length_squared() < 0.5
            || !d.offset_m.is_finite()
        {
            return Err("invalid dimension frame".into());
        }
        let record = Dimension::Linear {
            start,
            end,
            offset: offset * d.offset_m * 1000.0,
            direction: Some(direction),
            style: dim_style(
                model,
                d.font,
                d.arrow,
                decode::material(model, materials, d.material)
                    .and_then(|id| tx.document().materials.get(id))
                    .map(|m| m.color),
            ),
            text_override: (!d.text_override.is_empty()).then_some(d.text_override.clone()),
        };
        let (id, pid) = tx.insert_entity(c, EntityRecord::Dimension(record))?;
        native_meta(
            tx,
            c,
            id,
            pid,
            (d.pid, d.hidden, decode::tag(model, tags, d.layer)),
            pids,
        )?;
        attributes(tx, model, c, id, openskp::Owner::Dimension(i))?;
    }
    for (i, d) in model.radial_dimensions.iter().enumerate() {
        if d.leader.iter().any(|v| !v.is_finite() || v.abs() > 1e9) {
            return Err("invalid radial dimension leader".into());
        }
        let c = owner(d.def_index, defs)?;
        let source = d.edge.ok_or("radial dimension has no edge")?;
        let pid = pids
            .get(&source)
            .and_then(|ps| ps.first())
            .copied()
            .ok_or("radial dimension edge missing")?;
        let steps = path_to(tx.document(), c, pid)
            .ok_or("radial dimension edge is outside its container")?;
        let mut curve = Anchor {
            position: None,
            path: EntityPath::new(InstancePath { steps }, EntityKey::Pid(pid)),
            kind: AnchorKind::Mid,
        };
        let run = model
            .geometry
            .iter()
            .find(|r| r.def_index == d.def_index)
            .ok_or("radial dimension mesh missing")?;
        let arc = run
            .mesh
            .edges
            .iter()
            .find(|e| e.pid == source)
            .and_then(|e| e.curve)
            .and_then(|i| run.curves.get(i as usize))
            .and_then(|c| c.arc.as_ref())
            .ok_or("radial dimension has no analytic curve")?;
        let frame = planyrith_core::math::Plane::from_point_normal(
            DVec3::from_array(arc.center_m) * 1000.0,
            DVec3::from_array(arc.normal),
        )
        .frame();
        let x = DVec3::from_array(arc.x_axis_m).normalize_or_zero();
        let y = DVec3::from_array(arc.y_axis_m).normalize_or_zero();
        let cs = DVec2::from_angle(d.leader[0]);
        let dir = x * cs.x + y * cs.y;
        curve.position = Some(AnchorPosition::CurveParameter(
            DVec2::X.angle_to(DVec2::new(dir.dot(frame.u), dir.dot(frame.v))),
        ));
        let leader = dir * (-d.leader[1] * 25.4);
        let record = Dimension::Radial {
            curve,
            kind: if d.diameter {
                RadialKind::Diameter
            } else {
                RadialKind::Radius
            },
            leader,
            style: dim_style(
                model,
                d.font,
                d.arrow,
                decode::material(model, materials, d.material)
                    .and_then(|id| tx.document().materials.get(id))
                    .map(|m| m.color),
            ),
            text_override: (!d.text_override.is_empty()).then_some(d.text_override.clone()),
        };
        let (id, pid) = tx.insert_entity(c, EntityRecord::Dimension(record))?;
        native_meta(
            tx,
            c,
            id,
            pid,
            (d.pid, d.hidden, decode::tag(model, tags, d.layer)),
            pids,
        )?;
        attributes(tx, model, c, id, openskp::Owner::RadialDimension(i))?;
    }
    Ok(())
}
