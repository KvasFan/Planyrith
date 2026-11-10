//! Implements docs/05-modeling-tools.md §8.12–§8.13 (sheet metal and
//! frames, milestone 1.14): `sheet-metal-base`, `sheet-metal-flange`,
//! `sheet-metal-set`, `sheet-metal-flat` and `frame-member`.
//!
//! A sheet-metal part is a group whose definition keeps its description
//! (outline, thickness, bend radius, K factor, flanges) as the attribute
//! `planyrith` / `sheet-metal`; its geometry is rebuilt from it after every
//! change (docs/03 §4.20). Frame members are groups with the attribute
//! `planyrith` / `member` (profile and length) that the cut list reads.

use glam::{DAffine3, DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{SideRows, definition_uuid, place_geometry, targets_of};
use crate::commands::params::{PointSchema, Target, VectorSchema, target_container};
use crate::commands::transaction::AttrTarget;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ComponentDefinition, ContainerRef, Document, Entities, EntityRecord, Group, Query, Value,
};
use crate::geom::HalfEdgeMesh;
use crate::geom::fabrication::{Flange, ProfileFrame, SheetMetal, prism, profile};
use crate::ids::{DefinitionId, EntityId, Pid};
use crate::units::{Angle, Length};

/// The key of a sheet-metal description in the `planyrith` dictionary.
pub const SHEET_METAL: &str = "sheet-metal";
/// The key of a frame member's profile and length.
pub const MEMBER: &str = "member";

/// A frame member's description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Member {
    /// The profile (`square-tube 40x40x3`).
    pub profile: String,
    /// The longest length, mm (to the far points of mitered ends).
    pub length_mm: f64,
    /// The angles of the ends' cuts from square, degrees.
    pub cuts_deg: [f64; 2],
}

fn meta(name: &str, label: &str, view_only: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        label: label.into(),
        category: CommandCategory::Solid,
        view_only,
        steps: Vec::new(),
    }
}

/// A new group named `name` holding `mesh`, in `target`'s container.
fn new_group(
    tx: &mut Transaction<'_>,
    target: &Target,
    name: &str,
    mesh: HalfEdgeMesh,
) -> Result<(Pid, DefinitionId), CommandError> {
    let entities = Entities::from_mesh(mesh, || tx.allocate_pid());
    let uuid = definition_uuid(tx.document(), name, &entities);
    let mut def = ComponentDefinition::new(uuid, name.to_string());
    def.is_internal_group = true;
    def.entities = entities;
    let definition = tx.insert_definition(def)?;
    let (_, pid) = tx.insert_entity(
        target.container,
        EntityRecord::Group(Group {
            definition,
            transform: DAffine3::IDENTITY,
            name: name.to_string(),
        }),
    )?;
    Ok((pid, definition))
}

fn store<T: Serialize>(
    tx: &mut Transaction<'_>,
    d: DefinitionId,
    key: &str,
    v: &T,
) -> Result<(), CommandError> {
    let json = serde_json::to_string(v).map_err(|e| CommandError::Internal(e.to_string()))?;
    tx.set_attribute(
        AttrTarget::Definition(d),
        crate::sketch::DICT,
        key,
        Some(Value::Str(json)),
    )
}

fn read<T: for<'a> Deserialize<'a>>(doc: &Document, d: DefinitionId, key: &str) -> Option<T> {
    match doc.definition(d)?.attrs.get(crate::sketch::DICT, key)? {
        Value::Str(s) => serde_json::from_str(s).ok(),
        _ => None,
    }
}

/// The sheet-metal part `q` names: its definition and description.
fn find_part(
    tx: &Transaction<'_>,
    target: &Target,
    q: &Query,
) -> Result<(DefinitionId, SheetMetal), CommandError> {
    let ids: Vec<EntityId> = targets_of(tx, target, "part", q)?
        .into_iter()
        .filter(|id| matches!(id, EntityId::Group(_) | EntityId::Instance(_)))
        .collect();
    let [id] = ids.as_slice() else {
        return Err(CommandError::invalid_param(
            "part",
            "must name exactly one group",
        ));
    };
    let d = tx
        .document()
        .container(target.container)
        .and_then(|c| c.definition_of(*id))
        .ok_or_else(|| CommandError::Internal("a group without a definition".into()))?;
    let sm = read(tx.document(), d, SHEET_METAL).ok_or_else(|| {
        CommandError::NotApplicable("that group is not a sheet-metal part".into())
    })?;
    Ok((d, sm))
}

/// Replaces the part's contents with the folded geometry of `sm`.
fn rebuild(
    tx: &mut Transaction<'_>,
    d: DefinitionId,
    sm: &SheetMetal,
) -> Result<f64, CommandError> {
    let folded = sm.folded(tx.tolerances())?;
    let volume = folded.volume()?;
    let container = ContainerRef::Definition(d);
    tx.with_mesh(container, |m, tol, dl| {
        let faces: Vec<_> = m.faces().map(|(f, _)| f).collect();
        for f in faces {
            if m.face(f).is_some() {
                m.erase_face(f, true, tol, dl)?;
            }
        }
        let edges: Vec<_> = m.edges().map(|(e, _)| e).collect();
        for e in edges {
            if m.edge(e).is_some() {
                m.erase_edge(e, tol, dl)?;
            }
        }
        Ok(())
    })?;
    let (indexed, _) = folded.to_indexed();
    place_geometry(
        tx,
        container,
        &indexed,
        vec![SideRows::default(); indexed.edges.len()],
        vec![SideRows::default(); indexed.faces.len()],
    )?;
    store(tx, d, SHEET_METAL, sm)?;
    Ok(volume)
}

/// What a sheet-metal command gave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetMetalOutput {
    /// The part.
    pub part: Option<Pid>,
    /// Its volume, mm³.
    pub volume_mm3: f64,
    /// Flanges.
    pub flanges: usize,
}

// ----- sheet-metal-base -----------------------------------------------------------------

/// `sheet-metal-base` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetMetalBaseParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The flat outline: one face of the edit context (its holes are not
    /// kept).
    pub profile: Query,
    /// Sheet thickness, toward the face's front.
    pub thickness: Length,
    /// Inside bend radius (the thickness by default).
    #[serde(default)]
    pub bend_radius: Option<Length>,
    /// Where the neutral layer lies, as a share of the thickness (0.44).
    #[serde(default = "k")]
    pub k_factor: f64,
    /// The part's name.
    #[serde(default = "sheet_name")]
    pub name: String,
}

fn k() -> f64 {
    0.44
}

fn sheet_name() -> String {
    "Sheet metal".into()
}

/// The `sheet-metal-base` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct SheetMetalBase;

impl Command for SheetMetalBase {
    type Params = SheetMetalBaseParams;
    type Output = SheetMetalOutput;

    fn name(&self) -> &'static str {
        "sheet-metal-base"
    }

    fn validate(&self, _: &Document, p: &SheetMetalBaseParams) -> Result<(), CommandError> {
        if !(p.thickness.as_mm() > 0.0 && p.thickness.as_mm().is_finite()) {
            return Err(CommandError::OutOfRange {
                name: "thickness".into(),
                reason: "must be positive".into(),
            });
        }
        if !(0.0..=1.0).contains(&p.k_factor) {
            return Err(CommandError::OutOfRange {
                name: "k_factor".into(),
                reason: "is between 0 and 1".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SheetMetalBaseParams,
    ) -> Result<SheetMetalOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let faces: Vec<_> = targets_of(tx, &target, "profile", &p.profile)?
            .into_iter()
            .filter_map(|id| match id {
                EntityId::Face(f) => Some(f),
                _ => None,
            })
            .collect();
        let [face] = faces.as_slice() else {
            return Err(CommandError::invalid_param(
                "profile",
                "must name exactly one face",
            ));
        };
        let (points, normal) = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            let f = mesh.face_ref(*face)?;
            (mesh.loop_positions(f.outer)?, f.plane.normal)
        };
        let origin = points[0];
        let u = (points[1] - points[0]).normalize();
        let v = normal.cross(u).normalize();
        let outline: Vec<[f64; 2]> = points
            .iter()
            .map(|q| [(*q - origin).dot(u), (*q - origin).dot(v)])
            .collect();
        let sm = SheetMetal {
            origin: origin.to_array(),
            u: u.to_array(),
            v: v.to_array(),
            outline,
            thickness_mm: p.thickness.as_mm(),
            bend_radius_mm: p.bend_radius.map_or(p.thickness.as_mm(), |r| r.as_mm()),
            k_factor: p.k_factor,
            flanges: Vec::new(),
        };
        let folded = sm.folded(tx.tolerances())?;
        let volume_mm3 = folded.volume()?;
        let face = *face;
        tx.with_mesh(target.container, |m, tol, d| {
            if m.face(face).is_some() {
                m.erase_face(face, true, tol, d)?;
            }
            Ok(())
        })?;
        let (pid, d) = new_group(tx, &target, &p.name, folded)?;
        store(tx, d, SHEET_METAL, &sm)?;
        Ok(SheetMetalOutput {
            part: Some(pid),
            volume_mm3,
            flanges: 0,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-metal-base", "Sheet Metal Base", false)
    }
}

// ----- sheet-metal-flange ---------------------------------------------------------------

fn right_angle() -> Angle {
    Angle::radians(std::f64::consts::FRAC_PI_2)
}

/// `sheet-metal-flange` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetMetalFlangeParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The sheet-metal part.
    pub part: Query,
    /// The outline edge, by number (0 is from the first corner).
    #[serde(default)]
    pub edge: Option<usize>,
    /// Or the outline edge nearest this point.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub near: Option<DVec3>,
    /// The bend angle (90°).
    #[serde(default = "right_angle")]
    pub angle: Angle,
    /// The flange's straight length beyond the bend.
    pub length: Length,
}

/// The `sheet-metal-flange` command (an edge flange; again on the same edge
/// replaces it).
#[derive(Debug, Default, Clone, Copy)]
pub struct SheetMetalFlange;

impl Command for SheetMetalFlange {
    type Params = SheetMetalFlangeParams;
    type Output = SheetMetalOutput;

    fn name(&self) -> &'static str {
        "sheet-metal-flange"
    }

    fn validate(&self, _: &Document, p: &SheetMetalFlangeParams) -> Result<(), CommandError> {
        if p.edge.is_none() == p.near.is_none() {
            return Err(CommandError::invalid_param("edge", "give edge or near"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SheetMetalFlangeParams,
    ) -> Result<SheetMetalOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let (d, mut sm) = find_part(tx, &target, &p.part)?;
        let n = sm.outline.len();
        let edge = match (p.edge, p.near) {
            (Some(e), _) => e,
            (None, Some(near)) => {
                let o = DVec3::from_array(sm.origin);
                let (u, v) = (DVec3::from_array(sm.u), DVec3::from_array(sm.v));
                let q = target.local.transform_point3(near);
                let at = |p: [f64; 2]| o + u * p[0] + v * p[1];
                (0..n)
                    .min_by(|&i, &j| {
                        let dist = |k: usize| {
                            crate::math::Segment::new(
                                at(sm.outline[k]),
                                at(sm.outline[(k + 1) % n]),
                            )
                            .distance_to_point(q)
                        };
                        dist(i).total_cmp(&dist(j)).then(i.cmp(&j))
                    })
                    .unwrap_or(0)
            }
            (None, None) => return Err(CommandError::invalid_param("edge", "give edge or near")),
        };
        sm.flanges.retain(|f| f.edge != edge);
        sm.flanges.push(Flange {
            edge,
            angle_rad: p.angle.as_radians(),
            length_mm: p.length.as_mm(),
        });
        sm.flanges.sort_by_key(|f| f.edge);
        let volume_mm3 = rebuild(tx, d, &sm)?;
        Ok(SheetMetalOutput {
            part: None,
            volume_mm3,
            flanges: sm.flanges.len(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-metal-flange", "Edge Flange", false)
    }
}

// ----- sheet-metal-set ------------------------------------------------------------------

/// `sheet-metal-set` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetMetalSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The sheet-metal part.
    pub part: Query,
    /// A new thickness.
    #[serde(default)]
    pub thickness: Option<Length>,
    /// A new inside bend radius.
    #[serde(default)]
    pub bend_radius: Option<Length>,
    /// A new K factor.
    #[serde(default)]
    pub k_factor: Option<f64>,
    /// Remove the flange on this outline edge.
    #[serde(default)]
    pub remove_flange: Option<usize>,
}

/// The `sheet-metal-set` command: new gauge, radius or K factor, or a
/// flange removed; the part is rebuilt.
#[derive(Debug, Default, Clone, Copy)]
pub struct SheetMetalSet;

impl Command for SheetMetalSet {
    type Params = SheetMetalSetParams;
    type Output = SheetMetalOutput;

    fn name(&self) -> &'static str {
        "sheet-metal-set"
    }

    fn validate(&self, _: &Document, p: &SheetMetalSetParams) -> Result<(), CommandError> {
        if p.thickness.is_none()
            && p.bend_radius.is_none()
            && p.k_factor.is_none()
            && p.remove_flange.is_none()
        {
            return Err(CommandError::invalid_param("thickness", "change something"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SheetMetalSetParams,
    ) -> Result<SheetMetalOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let (d, mut sm) = find_part(tx, &target, &p.part)?;
        if let Some(t) = p.thickness {
            sm.thickness_mm = t.as_mm();
        }
        if let Some(r) = p.bend_radius {
            sm.bend_radius_mm = r.as_mm();
        }
        if let Some(k) = p.k_factor {
            sm.k_factor = k;
        }
        if let Some(e) = p.remove_flange {
            let before = sm.flanges.len();
            sm.flanges.retain(|f| f.edge != e);
            if sm.flanges.len() == before {
                return Err(CommandError::invalid_param(
                    "remove_flange",
                    format!("no flange on edge {e}"),
                ));
            }
        }
        let volume_mm3 = rebuild(tx, d, &sm)?;
        Ok(SheetMetalOutput {
            part: None,
            volume_mm3,
            flanges: sm.flanges.len(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-metal-set", "Sheet Metal Settings", false)
    }
}

// ----- sheet-metal-flat -----------------------------------------------------------------

/// `sheet-metal-flat` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetMetalFlatParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The sheet-metal part.
    pub part: Query,
    /// Where to put the flat pattern, from the part's outline origin
    /// (beside the part by default).
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub offset: Option<DVec3>,
}

/// `sheet-metal-flat` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetMetalFlatOutput {
    /// The flat pattern's group.
    pub group: Pid,
    /// Its outline in the sheet's plane, mm.
    pub outline: Vec<[f64; 2]>,
    /// The bend zones' lines, mm.
    pub bend_lines: Vec<[[f64; 2]; 2]>,
    /// The blank's size, mm.
    pub size_mm: [f64; 2],
}

/// The `sheet-metal-flat` command: the unfolded blank as a group with its
/// bend lines drawn on top.
#[derive(Debug, Default, Clone, Copy)]
pub struct SheetMetalFlat;

impl Command for SheetMetalFlat {
    type Params = SheetMetalFlatParams;
    type Output = SheetMetalFlatOutput;

    fn name(&self) -> &'static str {
        "sheet-metal-flat"
    }

    fn validate(&self, _: &Document, _: &SheetMetalFlatParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SheetMetalFlatParams,
    ) -> Result<SheetMetalFlatOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let (_, sm) = find_part(tx, &target, &p.part)?;
        let (outline, bends) = sm.flat();
        let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for q in &outline {
            lo = lo.min(*q);
            hi = hi.max(*q);
        }
        let o = DVec3::from_array(sm.origin);
        let (u, v) = (DVec3::from_array(sm.u), DVec3::from_array(sm.v));
        let w = u.cross(v).normalize();
        let offset = p.offset.map_or_else(
            || u * ((hi.x - lo.x) + 50.0) - u * lo.x,
            |d| target.local.transform_vector3(d),
        );
        let origin = o + offset;
        let t = sm.thickness_mm;
        let blank = prism(
            ProfileFrame { origin, u, v },
            std::slice::from_ref(&outline),
            w,
            (origin, w),
            (origin + w * t, w),
            tx.tolerances(),
        )?;
        let (group, d) = new_group(tx, &target, "Flat pattern", blank)?;
        // Bend lines on the top face.
        let at = |q: DVec2| origin + u * q.x + v * q.y + w * t;
        for b in &bends {
            let (a, c) = (at(b[0]), at(b[1]));
            tx.with_mesh(ContainerRef::Definition(d), |m, tol, dl| {
                m.add_edge(a, c, tol, dl).map(|_| ())
            })?;
        }
        Ok(SheetMetalFlatOutput {
            group,
            outline: outline.iter().map(|q| q.to_array()).collect(),
            bend_lines: bends
                .iter()
                .map(|b| [b[0].to_array(), b[1].to_array()])
                .collect(),
            size_mm: [hi.x - lo.x, hi.y - lo.y],
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-metal-flat", "Flat Pattern", false)
    }
}

// ----- frame-member ---------------------------------------------------------------------

fn yes() -> bool {
    true
}

/// `frame-member` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameMemberParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The members' center lines: edges of the edit context.
    pub path: Query,
    /// The profile (`square-tube 40x40x3`, `IPE200`, …).
    pub profile: String,
    /// Turn the profile about its line.
    #[serde(default)]
    pub rotation: Angle,
    /// Miter where exactly two members meet (else square ends).
    #[serde(default = "yes")]
    pub miter: bool,
}

/// `frame-member` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameMemberOutput {
    /// The members' groups, in the order of the edges.
    pub members: Vec<Pid>,
    /// Their lengths, mm.
    pub lengths_mm: Vec<f64>,
}

/// The `frame-member` command: a structural member along each edge.
#[derive(Debug, Default, Clone, Copy)]
pub struct FrameMember;

impl Command for FrameMember {
    type Params = FrameMemberParams;
    type Output = FrameMemberOutput;

    fn name(&self) -> &'static str {
        "frame-member"
    }

    fn validate(&self, _: &Document, p: &FrameMemberParams) -> Result<(), CommandError> {
        profile(&p.profile)
            .map(|_| ())
            .map_err(|e| CommandError::invalid_param("profile", e.to_string()))
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: FrameMemberParams,
    ) -> Result<FrameMemberOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let (loops, canonical) = profile(&p.profile)?;
        let edges: Vec<_> = targets_of(tx, &target, "path", &p.path)?
            .into_iter()
            .filter_map(|id| match id {
                EntityId::Edge(e) => Some(e),
                _ => None,
            })
            .collect();
        if edges.is_empty() {
            return Err(CommandError::invalid_param(
                "path",
                "name at least one edge",
            ));
        }
        let segments: Vec<(DVec3, DVec3)> = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            edges
                .iter()
                .map(|e| mesh.edge_segment(*e).map(|s| (s.a, s.b)))
                .collect::<Result<_, _>>()?
        };
        let tol = *tx.tolerances();
        let same = |a: DVec3, b: DVec3| a.distance(b) <= tol.length;
        // How many members meet at a point.
        let meeting = |q: DVec3| {
            segments
                .iter()
                .filter(|(a, b)| same(*a, q) || same(*b, q))
                .count()
        };
        let mut out = FrameMemberOutput {
            members: Vec::new(),
            lengths_mm: Vec::new(),
        };
        for (i, &(a, b)) in segments.iter().enumerate() {
            let dir = (b - a).normalize();
            // The profile's frame: up is +Z unless the member is vertical.
            let helper = if dir.z.abs() > 0.99 {
                DVec3::X
            } else {
                DVec3::Z
            };
            let side = helper.cross(dir).normalize();
            let up = dir.cross(side).normalize();
            let (s, c) = (
                libm::sin(p.rotation.as_radians()),
                libm::cos(p.rotation.as_radians()),
            );
            let (pu, pv) = (side * c + up * s, up * c - side * s);
            // An end's plane: square, or halfway to the one other member
            // meeting there.
            let end_plane = |q: DVec3, outward: DVec3| -> (DVec3, DVec3) {
                if p.miter && meeting(q) == 2 {
                    for (j, &(c0, c1)) in segments.iter().enumerate() {
                        if j == i {
                            continue;
                        }
                        let other = if same(c0, q) {
                            (c1 - c0).normalize()
                        } else if same(c1, q) {
                            (c0 - c1).normalize()
                        } else {
                            continue;
                        };
                        // The bisector between this member's outward
                        // direction and the other's continuing one.
                        let n = (outward + other).normalize_or_zero();
                        if n.length_squared() > 0.5 && outward.dot(other).abs() < 1.0 - 1e-9 {
                            return (q, n);
                        }
                    }
                }
                (q, outward)
            };
            let start = end_plane(a, -dir);
            let end = end_plane(b, dir);
            let frame = ProfileFrame {
                origin: a,
                u: pu,
                v: pv,
            };
            let mesh = prism(frame, &loops, dir, (start.0, -start.1), end, &tol)?;
            // The longest length, between the ends' farthest corners.
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for (_, v) in mesh.vertices() {
                let t = (v.position - a).dot(dir);
                lo = lo.min(t);
                hi = hi.max(t);
            }
            let cut = |n: DVec3| libm::acos(n.dot(dir).abs().min(1.0)).to_degrees();
            let member = Member {
                profile: canonical.clone(),
                length_mm: hi - lo,
                cuts_deg: [cut(start.1), cut(end.1)],
            };
            let (pid, d) = new_group(tx, &target, &format!("Member {canonical}"), mesh)?;
            store(tx, d, MEMBER, &member)?;
            out.members.push(pid);
            out.lengths_mm.push(member.length_mm);
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("frame-member", "Frame Member", false)
    }
}

/// A frame member's description, when the definition is one.
pub fn member_of(doc: &Document, d: DefinitionId) -> Option<Member> {
    read(doc, d, MEMBER)
}

/// Registers the fabrication commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(SheetMetalBase);
    registry.register(SheetMetalFlange);
    registry.register(SheetMetalSet);
    registry.register(SheetMetalFlat);
    registry.register(FrameMember);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::json;
    use std::sync::Arc;

    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }

    #[test]
    fn a_tray_is_folded_changed_and_unfolded() {
        let mut e = engine();
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [200, 100, 0] }),
        )
        .unwrap();
        let out = e
            .execute_erased(
                "sheet-metal-base",
                json!({ "profile": "face", "thickness": 2 }),
            )
            .unwrap();
        assert!((out["volume_mm3"].as_f64().unwrap() - 40_000.0).abs() < 1e-6);
        // Flanges up on two long sides, by number and by a nearby point.
        e.execute_erased(
            "sheet-metal-flange",
            json!({ "part": "group", "edge": 0, "length": 30 }),
        )
        .unwrap();
        let out = e
            .execute_erased(
                "sheet-metal-flange",
                json!({ "part": "group", "near": [100, 101, 0], "length": 30 }),
            )
            .unwrap();
        assert_eq!(out["flanges"], 2);
        let check = e.execute_erased("solid-check", json!({})).unwrap();
        assert_eq!(check["printable"], true, "{check}");
        // A thicker gauge rebuilds it; undo restores.
        let before = out["volume_mm3"].as_f64().unwrap();
        let out = e
            .execute_erased(
                "sheet-metal-set",
                json!({ "part": "group", "thickness": 3 }),
            )
            .unwrap();
        assert!(out["volume_mm3"].as_f64().unwrap() > before * 1.4);
        e.undo().unwrap();
        // The flat pattern: 100 + 2 × (bend allowance + 30) across.
        let flat = e
            .execute_erased("sheet-metal-flat", json!({ "part": "group & name:Sheet*" }))
            .unwrap();
        let ba = std::f64::consts::FRAC_PI_2 * (2.0 + 0.44 * 2.0);
        let size = flat["size_mm"].as_array().unwrap();
        assert!(
            (size[1].as_f64().unwrap() - (100.0 + 2.0 * (ba + 30.0))).abs() < 1e-9,
            "{flat}"
        );
        assert_eq!(flat["bend_lines"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn a_mitered_frame_and_its_cut_list() {
        let mut e = engine();
        e.execute_erased(
            "line",
            json!({ "points": [[0, 0, 0], [1000, 0, 0], [1000, 600, 0], [0, 600, 0]], "close": true }),
        )
        .unwrap();
        e.execute_erased("erase", json!({ "targets": "face" }))
            .unwrap();
        let out = e
            .execute_erased(
                "frame-member",
                json!({ "path": "edge", "profile": "square-tube 40x40x3" }),
            )
            .unwrap();
        assert_eq!(out["members"].as_array().unwrap().len(), 4);
        // Mitered corners: each member reaches 20 mm past the corners.
        let lengths: Vec<f64> = out["lengths_mm"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert!(
            lengths.contains(&1040.0) || lengths.iter().any(|l| (l - 1040.0).abs() < 1e-6),
            "{lengths:?}"
        );
        assert!(
            lengths.iter().any(|l| (l - 640.0).abs() < 1e-6),
            "{lengths:?}"
        );
        let check = e.execute_erased("interference-check", json!({})).unwrap();
        assert_eq!(
            check["interferences"].as_array().unwrap().len(),
            0,
            "{check}"
        );
        let cut = e.execute_erased("cut-list", json!({})).unwrap();
        let rows = cut["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{cut}");
        assert_eq!(rows[0]["count"], 2);
        assert!(
            (rows[0]["length_mm"].as_f64().unwrap() - 1040.0).abs() < 1e-6,
            "{cut}"
        );
        assert!(rows[0]["name"].as_str().unwrap().contains("square-tube"));
        assert!(
            e.execute_erased("frame-member", json!({ "path": "edge", "profile": "tube" }))
                .is_err()
        );
    }
}
