//! Implements docs/04-document-model.md §3.5–§3.6 for readers of dimensions
//! and texts: anchors resolve on read (so a dimension re-measures when the
//! geometry moves), a point picks the anchor it lies on, and a dimension
//! gives the geometry and text frontends draw. Everything is in the
//! coordinates of the container that holds the dimension or text.

use glam::{DAffine3, DVec3};

use crate::document::{
    Anchor, AnchorKind, ContainerRef, Dimension, Document, EntityKey, EntityPath, InstancePath,
    RadialKind,
};
use crate::geom::CurveDescriptor;
use crate::ids::{EntityId, Pid};
use crate::units::{Length, format_length};

/// Deepest nesting an anchor path or search follows.
const MAX_DEPTH: usize = 32;

/// Distance within which a point lies on a vertex or an edge's midpoint
/// when an anchor is picked (typed coordinates are rarely exact to the
/// merge tolerance).
pub const ANCHOR_SNAP_MM: f64 = 0.01;

/// The container `steps` reach from `from`, and its transform into `from`'s
/// coordinates.
pub fn resolve_steps(
    doc: &Document,
    from: ContainerRef,
    steps: &[EntityKey],
) -> Option<(ContainerRef, DAffine3)> {
    if steps.len() > MAX_DEPTH {
        return None;
    }
    let mut container = from;
    let mut transform = DAffine3::IDENTITY;
    for step in steps {
        let entities = doc.container(container)?;
        let id = entities.key_to_id(*step)?;
        transform *= entities.transform_of(id)?;
        container = ContainerRef::Definition(entities.definition_of(id)?);
    }
    Some((container, transform))
}

/// Where an anchor is, in the coordinates of `container` (the one holding
/// the dimension or text); `None` when it is detached (its edge is gone).
pub fn anchor_position(doc: &Document, container: ContainerRef, anchor: &Anchor) -> Option<DVec3> {
    let (inner, transform) = resolve_steps(doc, container, &anchor.path.instances.steps)?;
    let local = match anchor.kind {
        AnchorKind::Point(p) => p,
        kind => {
            let entities = doc.container(inner)?;
            let EntityId::Edge(e) = entities.key_to_id(anchor.path.target)? else {
                return None;
            };
            let mesh = entities.mesh();
            let (a, b) = mesh.edge_endpoints(e).ok()?;
            let (pa, pb) = (mesh.position(a).ok()?, mesh.position(b).ok()?);
            match kind {
                AnchorKind::Start => pa,
                AnchorKind::End => pb,
                _ => (pa + pb) / 2.0,
            }
        }
    };
    Some(transform.transform_point3(local))
}

/// A fixed-point anchor in `container`.
pub fn point_anchor(p: DVec3) -> Anchor {
    Anchor {
        path: EntityPath::new(InstancePath::root(), EntityKey::Pid(Pid::NONE)),
        kind: AnchorKind::Point(p),
    }
}

/// The anchor a point (in `container` coordinates) designates: a vertex
/// (as an end of its edge with the lowest pid) or an edge's midpoint of
/// `container` or of the groups and instances inside it (outer levels
/// first), else the point itself.
pub fn find_anchor(doc: &Document, container: ContainerRef, p: DVec3) -> Anchor {
    let mut levels: Vec<(ContainerRef, DAffine3, Vec<EntityKey>)> =
        vec![(container, DAffine3::IDENTITY, Vec::new())];
    let mut next = 0;
    while next < levels.len() && next < 4096 {
        let (c, transform, steps) = levels[next].clone();
        next += 1;
        let Some(entities) = doc.container(c) else {
            continue;
        };
        let local = transform.inverse().transform_point3(p);
        let mesh = entities.mesh();
        let mut best: Option<(Pid, AnchorKind)> = None;
        for &id in entities.order() {
            let EntityId::Edge(e) = id else { continue };
            let (Some(pid), Ok((a, b))) = (entities.pid_of(id), mesh.edge_endpoints(e)) else {
                continue;
            };
            let (Ok(pa), Ok(pb)) = (mesh.position(a), mesh.position(b)) else {
                continue;
            };
            let kind = if (pa - local).length() <= ANCHOR_SNAP_MM {
                AnchorKind::Start
            } else if (pb - local).length() <= ANCHOR_SNAP_MM {
                AnchorKind::End
            } else if ((pa + pb) / 2.0 - local).length() <= ANCHOR_SNAP_MM {
                AnchorKind::Mid
            } else {
                continue;
            };
            // Vertices beat midpoints; then the lowest pid.
            let better = match best {
                None => true,
                Some((old_pid, old_kind)) => {
                    let rank = |k: AnchorKind| u8::from(k == AnchorKind::Mid);
                    (rank(kind), pid) < (rank(old_kind), old_pid)
                }
            };
            if better {
                best = Some((pid, kind));
            }
        }
        if let Some((pid, kind)) = best {
            return Anchor {
                path: EntityPath::new(InstancePath { steps }, EntityKey::Pid(pid)),
                kind,
            };
        }
        if steps.len() >= MAX_DEPTH {
            continue;
        }
        for &id in entities.order() {
            if !matches!(id, EntityId::Group(_) | EntityId::Instance(_)) {
                continue;
            }
            let (Some(def), Some(t), Some(pid)) = (
                entities.definition_of(id),
                entities.transform_of(id),
                entities.pid_of(id),
            ) else {
                continue;
            };
            let mut inner = steps.clone();
            inner.push(EntityKey::Pid(pid));
            levels.push((ContainerRef::Definition(def), transform * t, inner));
        }
    }
    point_anchor(p)
}

/// What frontends draw for a dimension (docs/05 §7.1), in the coordinates
/// of its container.
#[derive(Debug, Clone, PartialEq)]
pub struct DimensionView {
    /// The measured points (for a radial dimension: center and the point
    /// on the curve the leader starts from).
    pub measured: [DVec3; 2],
    /// The dimension line (with the extension lines from `measured` to its
    /// ends for linear dimensions).
    pub line: [DVec3; 2],
    /// Where the text goes.
    pub text_at: DVec3,
    /// The measurement, mm.
    pub value: f64,
    /// The text shown.
    pub text: String,
    /// An anchor is gone: the dimension keeps its last shape.
    pub detached: bool,
}

/// The display text of a measured length: the style's units and precision
/// (else the document's), `text_override` with `<>` replaced by it.
pub fn dimension_text(
    doc: &Document,
    style: &crate::document::DimStyle,
    value: f64,
    prefix: &str,
    text_override: Option<&str>,
) -> String {
    let measured = format!(
        "{prefix}{}",
        format_length(
            Length::mm(value),
            style.units.unwrap_or(doc.info.display_units),
            style.precision.unwrap_or(doc.info.precision),
        )
    );
    match text_override {
        Some(t) => t.replace("<>", &measured),
        None => measured,
    }
}

/// The geometry and text of a dimension in `container`.
pub fn dimension_view(doc: &Document, container: ContainerRef, dim: &Dimension) -> DimensionView {
    match dim {
        Dimension::Linear {
            start,
            end,
            offset,
            direction,
            style,
            text_override,
        } => {
            let a = anchor_position(doc, container, start);
            let b = anchor_position(doc, container, end);
            let detached = a.is_none() || b.is_none();
            let (a, b) = (a.unwrap_or_default(), b.unwrap_or_default());
            let value = match direction.and_then(|d| d.try_normalize()) {
                Some(d) => (b - a).dot(d).abs(),
                None => (b - a).length(),
            };
            // A linear dimension's line runs along its direction, through
            // the offset start point.
            let line = match direction.and_then(|d| d.try_normalize()) {
                Some(d) => {
                    let s = a + *offset;
                    [s, s + d * (b - a).dot(d)]
                }
                None => [a + *offset, b + *offset],
            };
            DimensionView {
                measured: [a, b],
                line,
                text_at: (line[0] + line[1]) / 2.0,
                value,
                text: dimension_text(doc, style, value, "", text_override.as_deref()),
                detached,
            }
        }
        Dimension::Radial {
            curve,
            kind,
            leader,
            style,
            text_override,
        } => {
            let at = anchor_position(doc, container, curve);
            let circle = curve_of(doc, container, curve);
            let detached = at.is_none() || circle.is_none();
            let p = at.unwrap_or_default();
            let (center, radius) = circle.unwrap_or((p, 0.0));
            let value = match kind {
                RadialKind::Radius => radius,
                RadialKind::Diameter => radius * 2.0,
            };
            let prefix = match kind {
                RadialKind::Radius => "R",
                RadialKind::Diameter => "Ø",
            };
            DimensionView {
                measured: [center, p],
                line: [p, p + *leader],
                text_at: p + *leader,
                value,
                text: dimension_text(doc, style, value, prefix, text_override.as_deref()),
                detached,
            }
        }
    }
}

/// Center and radius (in `container` coordinates) of the arc or circle the
/// anchor's edge belongs to.
pub fn curve_of(doc: &Document, container: ContainerRef, anchor: &Anchor) -> Option<(DVec3, f64)> {
    let (inner, transform) = resolve_steps(doc, container, &anchor.path.instances.steps)?;
    let entities = doc.container(inner)?;
    let EntityId::Edge(e) = entities.key_to_id(anchor.path.target)? else {
        return None;
    };
    let mesh = entities.mesh();
    let curve = mesh.edge(e)?.curve?;
    let (center, radius) = match mesh.curve(curve)? {
        CurveDescriptor::Arc { center, radius, .. }
        | CurveDescriptor::Circle { center, radius, .. } => (*center, *radius),
        _ => return None,
    };
    // Radii scale with the occurrence (uniform scales; the mean otherwise).
    let scale = (transform.matrix3.x_axis.length()
        + transform.matrix3.y_axis.length()
        + transform.matrix3.z_axis.length())
        / 3.0;
    Some((transform.transform_point3(center), radius * scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_anchors_resolve_to_themselves() {
        let doc = Document::new();
        let p = DVec3::new(1.0, 2.0, 3.0);
        let a = find_anchor(&doc, ContainerRef::Model, p);
        assert_eq!(a.kind, AnchorKind::Point(p));
        assert_eq!(anchor_position(&doc, ContainerRef::Model, &a), Some(p));
    }
}
