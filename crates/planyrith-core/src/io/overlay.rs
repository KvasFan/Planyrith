//! Implements docs/06-rendering-and-viewport.md §3.1 step 7 (overlays) for
//! every frontend: what guides, dimensions, texts and section plane widgets
//! look like, in world coordinates — lines the renderer draws, guide points
//! and labels (with their screen offsets) the frontend draws on top. One
//! description for the GUI, headless rendering and the 2D exports.

use glam::{DVec2, DVec3};

use crate::document::annotations::{anchor_position, dimension_view};
use crate::document::{ArrowStyle, Dimension, Document, EntityView, Guide};
use crate::io::render_mesh::DrawItem;

/// What an overlay element belongs to (its color in frontends).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlayKind {
    /// Guide lines and points.
    Guide,
    /// Dimensions.
    Dimension,
    /// A dimension whose anchor is gone (drawn red).
    Detached,
    /// Text leaders.
    Text,
    /// Section plane widgets (the active one stronger).
    SectionPlane {
        /// The plane cuts.
        active: bool,
    },
}

/// A line to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayLine {
    /// Start (world).
    pub a: DVec3,
    /// End (world); for an infinite line, `a + direction`.
    pub b: DVec3,
    /// Infinite through `a` and `b` (guide lines): frontends clip it to the
    /// view.
    pub infinite: bool,
    /// Dashed.
    pub dashed: bool,
    /// What it belongs to.
    pub kind: OverlayKind,
}

/// A text label.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayLabel {
    /// Explicit screen placement, world leader and font, when supplied.
    pub layout: Option<crate::document::TextLayout>,
    /// The point it belongs to (world).
    pub at: DVec3,
    /// Where the text sits relative to `at`, screen pixels (x right, y up);
    /// a leader joins them when non-zero.
    pub screen_offset: DVec2,
    /// The text (lines separated by `\n`).
    pub text: String,
    /// What it belongs to.
    pub kind: OverlayKind,
}

/// Everything to draw over the model.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overlay {
    /// Lines.
    pub lines: Vec<OverlayLine>,
    /// Guide points (drawn as small crosses).
    pub points: Vec<DVec3>,
    /// Labels.
    pub labels: Vec<OverlayLabel>,
}

/// The overlays of the visible occurrences `items` (`io::render_mesh::draw_list`).
pub fn overlay(doc: &Document, items: &[DrawItem]) -> Overlay {
    let mut out = Overlay::default();
    let show_planes = doc.rendering.section.planes_visible;
    for item in items.iter().filter(|i| !i.hidden) {
        let Some(entities) = doc.container(item.container) else {
            continue;
        };
        let w = |p: DVec3| item.world.transform_point3(p);
        for &id in entities.order() {
            if entities.is_hidden(id) {
                continue;
            }
            match entities.view(id) {
                Some(EntityView::Guide(Guide::Line {
                    origin,
                    dir,
                    finite,
                })) => {
                    let (a, b, infinite) = match finite {
                        Some((t0, t1)) => (*origin + *dir * *t0, *origin + *dir * *t1, false),
                        None => (*origin, *origin + *dir * 1000.0, true),
                    };
                    out.lines.push(OverlayLine {
                        a: w(a),
                        b: w(b),
                        infinite,
                        dashed: true,
                        kind: OverlayKind::Guide,
                    });
                }
                Some(EntityView::Guide(Guide::Point { pos })) => out.points.push(w(*pos)),
                Some(EntityView::Dimension(dim)) => {
                    dimension_lines(doc, item, dim, &mut out);
                }
                Some(EntityView::Text(t)) => {
                    let at = t
                        .anchor
                        .as_ref()
                        .and_then(|a| anchor_position(doc, item.container, a))
                        .map(w);
                    out.labels.push(OverlayLabel {
                        layout: t.layout.clone().map(|mut layout| {
                            layout.world_offset =
                                layout.world_offset.map(|v| item.world.transform_vector3(v));
                            layout
                        }),
                        at: at.unwrap_or(DVec3::ZERO),
                        screen_offset: if at.is_some() {
                            t.screen_offset
                        } else {
                            DVec2::ZERO
                        },
                        text: t.text.clone(),
                        kind: if at.is_some() || t.anchor.is_none() {
                            OverlayKind::Text
                        } else {
                            OverlayKind::Detached
                        },
                    });
                }
                Some(EntityView::SectionPlane(s)) if show_planes => {
                    let kind = OverlayKind::SectionPlane { active: s.active };
                    // A rectangle on the plane around the container's
                    // contents, 10 % larger.
                    let bounds = entities.local_aabb(&doc.definitions, false);
                    let frame = s.plane.frame();
                    let (lo, hi) = if bounds.is_empty() {
                        (DVec2::splat(-500.0), DVec2::splat(500.0))
                    } else {
                        let mut lo = DVec2::splat(f64::MAX);
                        let mut hi = DVec2::splat(f64::MIN);
                        for i in 0..8u8 {
                            let corner = DVec3::new(
                                if i & 1 == 0 {
                                    bounds.min.x
                                } else {
                                    bounds.max.x
                                },
                                if i & 2 == 0 {
                                    bounds.min.y
                                } else {
                                    bounds.max.y
                                },
                                if i & 4 == 0 {
                                    bounds.min.z
                                } else {
                                    bounds.max.z
                                },
                            );
                            let q = frame.to_2d(corner);
                            lo = lo.min(q);
                            hi = hi.max(q);
                        }
                        let margin = (hi - lo).max_element().max(100.0) * 0.05;
                        (lo - DVec2::splat(margin), hi + DVec2::splat(margin))
                    };
                    let corners = [(lo.x, lo.y), (hi.x, lo.y), (hi.x, hi.y), (lo.x, hi.y)]
                        .map(|(x, y)| frame.from_2d(DVec2::new(x, y)));
                    for k in 0..4 {
                        out.lines.push(OverlayLine {
                            a: w(corners[k]),
                            b: w(corners[(k + 1) % 4]),
                            infinite: false,
                            dashed: !s.active,
                            kind,
                        });
                    }
                    out.labels.push(OverlayLabel {
                        layout: None,
                        at: w(corners[2]),
                        screen_offset: DVec2::ZERO,
                        text: s.symbol.clone(),
                        kind,
                    });
                }
                _ => {}
            }
        }
    }
    out
}

/// Extension lines, the dimension line, its end marks and the label.
fn dimension_lines(doc: &Document, item: &DrawItem, dim: &Dimension, out: &mut Overlay) {
    let view = dimension_view(doc, item.container, dim);
    let kind = if view.detached {
        OverlayKind::Detached
    } else {
        OverlayKind::Dimension
    };
    let w = |p: DVec3| item.world.transform_point3(p);
    let mut line = |a: DVec3, b: DVec3| {
        if (b - a).length() > 1e-9 {
            out.lines.push(OverlayLine {
                a: w(a),
                b: w(b),
                infinite: false,
                dashed: false,
                kind,
            });
        }
    };
    let (style, radial) = match dim {
        Dimension::Linear { style, .. } => (style, false),
        Dimension::Radial { style, .. } => (style, true),
    };
    let [p, q] = view.line;
    if radial {
        line(p, q);
    } else {
        line(view.measured[0], p);
        line(view.measured[1], q);
        line(p, q);
        // End marks, scaled with the text height.
        let along = (q - p).normalize_or_zero();
        let across = (p - view.measured[0])
            .try_normalize()
            .unwrap_or_else(|| along.any_orthonormal_vector());
        let size = style.text_height.as_mm() * 0.4;
        for (end, sign) in [(p, 1.0), (q, -1.0)] {
            match style.arrow {
                ArrowStyle::None => {}
                ArrowStyle::Slash => {
                    let d = (along + across).normalize_or_zero() * size * 0.5;
                    line(end - d, end + d);
                }
                ArrowStyle::Dot => {
                    let d = across * size * 0.2;
                    line(end - d, end + d);
                }
                ArrowStyle::Closed | ArrowStyle::Open => {
                    let tip = end;
                    let back = end + along * sign * size;
                    line(tip, back + across * size * 0.3);
                    line(tip, back - across * size * 0.3);
                    if style.arrow == ArrowStyle::Closed {
                        line(back + across * size * 0.3, back - across * size * 0.3);
                    }
                }
            }
        }
    }
    out.labels.push(OverlayLabel {
        layout: style.font.clone().map(|font| crate::document::TextLayout {
            font: Some(font),
            ..Default::default()
        }),
        at: w(view.text_at),
        screen_offset: DVec2::ZERO,
        text: view.text,
        kind,
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::{EditContext, Selection};
    use crate::io::render_mesh::draw_list;

    #[test]
    fn dimensions_guides_texts_and_planes_draw() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        for (name, params) in [
            ("line", json!({ "points": [[0, 0, 0], [2000, 0, 0]] })),
            (
                "dimension",
                json!({ "start": [0, 0, 0], "end": [2000, 0, 0], "offset": [0, -500, 0] }),
            ),
            ("text", json!({ "at": [1000, 0, 0] })),
            (
                "tape-measure",
                json!({ "from": [500, 0, 0], "to": [500, 300, 0] }),
            ),
            (
                "tape-measure",
                json!({ "from": [0, 0, 0], "to": [100, 100, 0] }),
            ),
            (
                "section-plane-add",
                json!({ "plane": { "normal": [1, 0, 0], "d": -1000.0 } }),
            ),
        ] {
            e.execute_erased(name, params).unwrap();
        }
        let doc = e.document();
        let items = draw_list(doc, &Selection::new(), &EditContext::model(), false);
        let o = overlay(doc, &items);
        let dims = o
            .lines
            .iter()
            .filter(|l| l.kind == OverlayKind::Dimension)
            .count();
        assert_eq!(dims, 5, "two extensions, the line and two slashes");
        assert_eq!(o.points, vec![DVec3::new(100.0, 100.0, 0.0)]);
        assert!(
            o.lines
                .iter()
                .any(|l| l.infinite && l.kind == OverlayKind::Guide)
        );
        let texts: Vec<&str> = o.labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["2000.0mm", "2000.0mm", "1"]);
        assert_eq!(
            o.lines
                .iter()
                .filter(|l| matches!(l.kind, OverlayKind::SectionPlane { active: true }))
                .count(),
            4
        );
    }
}
