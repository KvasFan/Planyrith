//! Implements docs/05-modeling-tools.md §7.2 `text`: screen text, alone or
//! with a leader to the point it is attached to. Attached to a vertex the
//! default text is its coordinates, to an edge (its midpoint) its length, to
//! a face its area (docs/05 §7.2).

use glam::{DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::params::{PointSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::annotations::{anchor_position, find_anchor, resolve_steps};
use crate::document::{AnchorKind, ContainerRef, Document, EntityRecord, Text};
use crate::geom::planar::{face_polygon, inside_or_on};
use crate::ids::{EntityId, Pid};
use crate::units::{Length, LengthUnit, format_length};

/// `text` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TextParams {
    /// The text (default: what the point it is attached to measures).
    #[serde(default)]
    pub text: Option<String>,
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The point the leader points at (world); none: plain screen text.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub at: Option<DVec3>,
    /// Text offset from the point, in screen pixels (x right, y up).
    #[serde(default)]
    #[schemars(with = "Option<[f64; 2]>")]
    pub screen_offset: Option<DVec2>,
}

/// `text` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TextOutput {
    /// The text entity.
    pub text: Pid,
    /// What it says.
    pub content: String,
}

/// The `text` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct TextCommand;

/// A length in the document's display units.
fn length(doc: &Document, mm: f64) -> String {
    format_length(Length::mm(mm), doc.info.display_units, doc.info.precision)
}

/// An area in the square of the display unit (feet for feet-and-inches,
/// inches for fractions).
fn area(doc: &Document, mm2: f64) -> String {
    let unit = match doc.info.display_units {
        LengthUnit::FeetInches => LengthUnit::Foot,
        LengthUnit::ArchitecturalFraction => LengthUnit::Inch,
        u => u,
    };
    let per = unit.millimeters();
    format!(
        "{:.*}{}²",
        usize::from(doc.info.precision),
        mm2 / (per * per),
        unit.symbol()
    )
}

/// The area of a face of `container` (not nested) under `p`.
fn face_area_at(doc: &Document, container: ContainerRef, p: DVec3) -> Option<f64> {
    let entities = doc.container(container)?;
    let mesh = entities.mesh();
    let tol = doc.tolerances();
    for &id in entities.order() {
        let EntityId::Face(f) = id else { continue };
        let face = mesh.face(f)?;
        if face.plane.signed_distance(p).abs() > tol.length * 10.0 {
            continue;
        }
        let frame = face.plane.frame();
        let Ok((outer, holes)) = face_polygon(mesh, f, &frame) else {
            continue;
        };
        if inside_or_on(frame.to_2d(p), &outer, &holes, tol) {
            return mesh.face_area(f).ok();
        }
    }
    None
}

impl Command for TextCommand {
    type Params = TextParams;
    type Output = TextOutput;

    fn name(&self) -> &'static str {
        "text"
    }

    fn validate(&self, _doc: &Document, p: &TextParams) -> Result<(), CommandError> {
        if let Some(at) = p.at {
            check_point("at", at)?;
        } else if p.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
            return Err(CommandError::invalid_param(
                "text",
                "screen text without `at` needs a text",
            ));
        }
        if let Some(o) = p.screen_offset
            && !o.is_finite()
        {
            return Err(CommandError::invalid_param(
                "screen_offset",
                "must be finite",
            ));
        }
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: TextParams) -> Result<TextOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let doc = tx.document();
        let anchor =
            p.at.map(|at| find_anchor(doc, target.container, target.to_local(at)));
        let content = match (&p.text, &anchor) {
            (Some(t), _) if !t.trim().is_empty() => t.clone(),
            (_, Some(a)) => match a.kind {
                AnchorKind::Start | AnchorKind::End => {
                    let world = anchor_position(doc, target.container, a)
                        .map(|q| target.world.transform_point3(q))
                        .unwrap_or_default();
                    format!(
                        "{}, {}, {}",
                        length(doc, world.x),
                        length(doc, world.y),
                        length(doc, world.z)
                    )
                }
                AnchorKind::Mid => {
                    let (inner, transform) =
                        resolve_steps(doc, target.container, &a.path.instances.steps)
                            .ok_or_else(|| CommandError::Internal("lost anchor".into()))?;
                    let seg = doc
                        .container(inner)
                        .and_then(|c| match c.key_to_id(a.path.target)? {
                            EntityId::Edge(e) => c.mesh().edge_segment(e).ok(),
                            _ => None,
                        })
                        .ok_or_else(|| CommandError::Internal("lost edge".into()))?;
                    let (a, b) = (
                        (target.world * transform).transform_point3(seg.a),
                        (target.world * transform).transform_point3(seg.b),
                    );
                    length(doc, (b - a).length())
                }
                AnchorKind::Point(q) => match face_area_at(doc, target.container, q) {
                    Some(mm2) => {
                        // Areas grow with the square of the context's scale.
                        let s = libm::pow(target.world.matrix3.determinant().abs(), 2.0 / 3.0);
                        area(doc, mm2 * s)
                    }
                    None => {
                        return Err(CommandError::invalid_param(
                            "text",
                            "the point is on no vertex, edge or face: give the text",
                        ));
                    }
                },
            },
            _ => {
                return Err(CommandError::invalid_param("text", "must not be empty"));
            }
        };
        let (_, pid) = tx.insert_entity(
            target.container,
            EntityRecord::Text(Text {
                anchor,
                text: content.clone(),
                screen_offset: p.screen_offset.unwrap_or(DVec2::new(40.0, 40.0)),
            }),
        )?;
        Ok(TextOutput { text: pid, content })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "text".into(),
            category: CommandCategory::Annotate,
            label: "Text".into(),
            view_only: false,
            steps: crate::commands::common::tool_steps(&[
                (
                    "Pick what the text points to, or empty space for screen text",
                    "",
                ),
                ("Place the text", ""),
            ]),
        }
    }
}
