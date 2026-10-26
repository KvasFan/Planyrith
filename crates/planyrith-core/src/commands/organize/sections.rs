//! Implements docs/05-modeling-tools.md §5.11 section planes:
//! `section-plane-add`, `section-plane-toggle`, `section-plane-reverse`,
//! `section-plane-delete`, `section-plane-align-view` (a view command) and
//! `section-create-group-from-slice`, plus the `section-display-set` view
//! command (docs/05 §9 `show-section-planes/cuts/fill`). A plane hides the
//! geometry of its container on its `+normal` side; at most one plane is
//! active per container (docs/04 §3.8).

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::params::{Target, command_context, resolve_ref, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ComponentDefinition, ContainerRef, Document, Entities, EntityRecord, EntityRef, EntityView,
    Group, Projection, SectionPlane,
};
use crate::geom::section::slice;
use crate::geom::{HalfEdgeMesh, NoDeltas};
use crate::ids::{EntityId, Pid};
use crate::math::Plane;
use crate::spatial::InstanceProvider;

fn yes() -> bool {
    true
}

fn meta(name: &str, label: &str, view_only: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: if view_only {
            CommandCategory::View
        } else {
            CommandCategory::Organize
        },
        label: label.into(),
        view_only,
        steps: Vec::new(),
    }
}

/// Deactivates every section plane of `container` except `keep`.
fn deactivate_others(
    tx: &mut Transaction<'_>,
    container: ContainerRef,
    keep: Option<EntityId>,
) -> Result<(), CommandError> {
    let active: Vec<(EntityId, SectionPlane)> = tx
        .document()
        .container(container)
        .map(|c| {
            c.order()
                .iter()
                .filter(|id| Some(**id) != keep)
                .filter_map(|&id| match c.view(id) {
                    Some(EntityView::SectionPlane(s)) if s.active => Some((id, s.clone())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    for (id, mut s) in active {
        s.active = false;
        tx.replace_entity(container, id, EntityRecord::SectionPlane(s))?;
    }
    Ok(())
}

/// The section plane a reference names.
fn section_plane(
    tx: &Transaction<'_>,
    common: &CommonParams,
    r: &EntityRef,
) -> Result<(ContainerRef, EntityId, SectionPlane, DAffine3), CommandError> {
    let context = command_context(tx, common);
    let resolved = resolve_ref(tx, &context, "section", r)?;
    match tx
        .document()
        .container(resolved.container)
        .and_then(|c| c.view(resolved.id))
    {
        Some(EntityView::SectionPlane(s)) => {
            Ok((resolved.container, resolved.id, s.clone(), resolved.world))
        }
        _ => Err(CommandError::invalid_param(
            "section",
            "is not a section plane",
        )),
    }
}

/// `section-plane-add` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SectionPlaneAddParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The cutting plane (world); geometry on its `+normal` side is hidden.
    pub plane: Plane,
    /// Name (`Section N`).
    #[serde(default)]
    pub name: Option<String>,
    /// Short symbol on the widget (`N`).
    #[serde(default)]
    pub symbol: Option<String>,
    /// Make it the active plane of its context (true).
    #[serde(default = "yes")]
    pub activate: bool,
    /// Fill the cut of solids (true).
    #[serde(default = "yes")]
    pub fill_cap: bool,
}

/// Output of the section plane commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SectionPlaneOutput {
    /// The section plane.
    pub section_plane: Pid,
    /// Its name.
    pub name: String,
    /// Whether it is active.
    pub active: bool,
}

/// The `section-plane-add` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct SectionPlaneAdd;

impl Command for SectionPlaneAdd {
    type Params = SectionPlaneAddParams;
    type Output = SectionPlaneOutput;

    fn name(&self) -> &'static str {
        "section-plane-add"
    }

    fn validate(&self, _doc: &Document, p: &SectionPlaneAddParams) -> Result<(), CommandError> {
        if !p.plane.normal.is_finite()
            || (p.plane.normal.length() - 1.0).abs() > 1e-6
            || !p.plane.d.is_finite()
        {
            return Err(CommandError::invalid_param("plane", "needs a unit normal"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SectionPlaneAddParams,
    ) -> Result<SectionPlaneOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let count = tx.document().container(target.container).map_or(0, |c| {
            c.order()
                .iter()
                .filter(|id| matches!(id, EntityId::SectionPlane(_)))
                .count()
        });
        let name = p.name.unwrap_or_else(|| format!("Section {}", count + 1));
        let plane = SectionPlane {
            plane: p.plane.transformed(&target.local),
            name: name.clone(),
            symbol: p.symbol.unwrap_or_else(|| (count + 1).to_string()),
            active: p.activate,
            fill_cap: p.fill_cap,
            cuts_hidden: false,
        };
        if p.activate {
            deactivate_others(tx, target.container, None)?;
        }
        let (_, pid) = tx.insert_entity(target.container, EntityRecord::SectionPlane(plane))?;
        Ok(SectionPlaneOutput {
            section_plane: pid,
            name,
            active: p.activate,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("section-plane-add", "Section Plane", false)
    }
}

/// Parameters naming one section plane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SectionPlaneRefParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The section plane.
    pub section: EntityRef,
    /// `section-plane-toggle`: the new state (default: the opposite).
    #[serde(default)]
    pub active: Option<bool>,
}

macro_rules! plane_command {
    ($ty:ident, $name:literal, $label:literal, $view:expr, |$tx:ident, $p:ident| $body:block) => {
        #[doc = concat!("The `", $name, "` command.")]
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $ty;

        impl Command for $ty {
            type Params = SectionPlaneRefParams;
            type Output = SectionPlaneOutput;

            fn name(&self) -> &'static str {
                $name
            }

            fn validate(
                &self,
                _doc: &Document,
                _p: &SectionPlaneRefParams,
            ) -> Result<(), CommandError> {
                Ok(())
            }

            fn apply(
                &self,
                $tx: &mut Transaction<'_>,
                $p: SectionPlaneRefParams,
            ) -> Result<SectionPlaneOutput, CommandError> {
                $body
            }

            fn meta(&self) -> CommandMeta {
                meta($name, $label, $view)
            }
        }
    };
}

plane_command!(
    SectionPlaneToggle,
    "section-plane-toggle",
    "Toggle Section Plane",
    false,
    |tx, p| {
        let (container, id, mut s, _) = section_plane(tx, &p.common, &p.section)?;
        s.active = p.active.unwrap_or(!s.active);
        if s.active {
            deactivate_others(tx, container, Some(id))?;
        }
        let pid = tx
            .document()
            .container(container)
            .and_then(|c| c.pid_of(id))
            .unwrap_or(Pid::NONE);
        let out = SectionPlaneOutput {
            section_plane: pid,
            name: s.name.clone(),
            active: s.active,
        };
        tx.replace_entity(container, id, EntityRecord::SectionPlane(s))?;
        Ok(out)
    }
);

plane_command!(
    SectionPlaneReverse,
    "section-plane-reverse",
    "Reverse Section Plane",
    false,
    |tx, p| {
        let (container, id, mut s, _) = section_plane(tx, &p.common, &p.section)?;
        s.plane = s.plane.flipped();
        let pid = tx
            .document()
            .container(container)
            .and_then(|c| c.pid_of(id))
            .unwrap_or(Pid::NONE);
        let out = SectionPlaneOutput {
            section_plane: pid,
            name: s.name.clone(),
            active: s.active,
        };
        tx.replace_entity(container, id, EntityRecord::SectionPlane(s))?;
        Ok(out)
    }
);

plane_command!(
    SectionPlaneDelete,
    "section-plane-delete",
    "Delete Section Plane",
    false,
    |tx, p| {
        let (container, id, s, _) = section_plane(tx, &p.common, &p.section)?;
        let pid = tx
            .document()
            .container(container)
            .and_then(|c| c.pid_of(id))
            .unwrap_or(Pid::NONE);
        tx.remove_entity(container, id)?;
        Ok(SectionPlaneOutput {
            section_plane: pid,
            name: s.name,
            active: false,
        })
    }
);

plane_command!(
    SectionPlaneAlignView,
    "section-plane-align-view",
    "Align View to Section",
    true,
    |tx, p| {
        let (container, id, s, world) = section_plane(tx, &p.common, &p.section)?;
        let plane = s.plane.transformed(&world);
        let mut camera = tx.document().camera;
        let distance = camera.distance().max(1.0);
        // Look at the kept side (against the normal), centered on where the
        // current target projects onto the plane.
        let target = camera.target - plane.normal * plane.signed_distance(camera.target);
        camera.target = target;
        camera.eye = target + plane.normal * distance;
        camera.up = if plane.normal.cross(DVec3::Z).length() > 1e-6 {
            DVec3::Z
        } else {
            DVec3::Y
        };
        if let Projection::Perspective { fov_y_deg } = camera.projection {
            let height = 2.0 * distance * libm::tan(fov_y_deg.to_radians() / 2.0);
            camera.projection = Projection::Parallel { height_mm: height };
        }
        tx.set_camera(camera);
        let pid = tx
            .document()
            .container(container)
            .and_then(|c| c.pid_of(id))
            .unwrap_or(Pid::NONE);
        Ok(SectionPlaneOutput {
            section_plane: pid,
            name: s.name,
            active: s.active,
        })
    }
);

/// `section-create-group-from-slice` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SliceParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The section plane to slice with…
    #[serde(default)]
    pub section: Option<EntityRef>,
    /// …or a plane (world).
    #[serde(default)]
    pub plane: Option<Plane>,
}

/// `section-create-group-from-slice` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SliceOutput {
    /// The group of cut lines (none when the plane cuts nothing).
    pub group: Option<Pid>,
    /// Its edges.
    pub edges: usize,
    /// Faces the closed cuts form.
    pub faces: usize,
}

/// The `section-create-group-from-slice` command: the lines where a plane
/// cuts the visible geometry in and below the edit context, as edges of a
/// new group (closed cuts become faces).
#[derive(Debug, Default, Clone, Copy)]
pub struct SectionSlice;

/// The world segments where `plane` (world) cuts what lies at or below
/// `target`'s container.
pub(crate) fn slice_below(doc: &Document, target: &Target, plane: &Plane) -> Vec<(DVec3, DVec3)> {
    let tol = doc.tolerances();
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let context = target.context.path.canonical(doc);
    for occ in InstanceProvider::occurrences(doc) {
        if occ.hidden || !occ.path.canonical(doc).steps.starts_with(&context.steps) {
            continue;
        }
        let Some(entities) = doc.container(occ.container) else {
            continue;
        };
        let Some(inv) =
            (occ.world.matrix3.determinant().abs() > 1e-12).then(|| occ.world.inverse())
        else {
            continue;
        };
        let local = plane.transformed(&inv);
        for (a, b) in slice(entities.mesh(), &local, tol) {
            let (a, b) = (occ.world.transform_point3(a), occ.world.transform_point3(b));
            let key = [a, b].map(|p| [p.x, p.y, p.z].map(|c| (c / tol.length).round() as i64));
            if seen.insert(key) {
                out.push((a, b));
            }
        }
    }
    out
}

impl Command for SectionSlice {
    type Params = SliceParams;
    type Output = SliceOutput;

    fn name(&self) -> &'static str {
        "section-create-group-from-slice"
    }

    fn validate(&self, _doc: &Document, p: &SliceParams) -> Result<(), CommandError> {
        match (&p.section, &p.plane) {
            (Some(_), None) => Ok(()),
            (None, Some(plane)) if (plane.normal.length() - 1.0).abs() <= 1e-6 => Ok(()),
            _ => Err(CommandError::invalid_param(
                "section",
                "give either `section` or a `plane` with a unit normal",
            )),
        }
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: SliceParams) -> Result<SliceOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let plane = match (&p.section, p.plane) {
            (Some(r), _) => {
                let (_, _, s, world) = section_plane(tx, &p.common, r)?;
                s.plane.transformed(&world)
            }
            (None, Some(plane)) => plane,
            _ => {
                return Err(CommandError::invalid_param("section", "missing"));
            }
        };
        let segments = slice_below(tx.document(), &target, &plane);
        if segments.is_empty() {
            return Ok(SliceOutput {
                group: None,
                edges: 0,
                faces: 0,
            });
        }
        let tol = *tx.tolerances();
        let mut mesh = HalfEdgeMesh::new();
        for (a, b) in &segments {
            let (a, b) = (target.to_local(*a), target.to_local(*b));
            if !tol.same_point(a, b) {
                let _ = mesh.add_edge(a, b, &tol, &mut NoDeltas);
            }
        }
        let (edges, faces) = (mesh.edge_count(), mesh.face_count());
        let name = format!(
            "Group {}",
            crate::commands::common::group_number(tx.document())
        );
        let entities = Entities::from_mesh(mesh, || tx.allocate_pid());
        let uuid = crate::commands::common::definition_uuid(tx.document(), &name, &entities);
        let mut def = ComponentDefinition::new(uuid, name);
        def.is_internal_group = true;
        def.entities = entities;
        let def = tx.insert_definition(def)?;
        let (_, pid) = tx.insert_entity(
            target.container,
            EntityRecord::Group(Group {
                definition: def,
                transform: DAffine3::IDENTITY,
                name: "Section slice".into(),
            }),
        )?;
        Ok(SliceOutput {
            group: Some(pid),
            edges,
            faces,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta(
            "section-create-group-from-slice",
            "Create Group from Slice",
            false,
        )
    }
}

/// `section-display-set` parameters (absent fields keep their value).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SectionDisplayParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Show the planes' widgets.
    #[serde(default)]
    pub planes: Option<bool>,
    /// Apply the cuts.
    #[serde(default)]
    pub cuts: Option<bool>,
    /// Fill the cuts of solids.
    #[serde(default)]
    pub fills: Option<bool>,
}

/// `section-display-set` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SectionDisplayOutput {
    /// Widgets shown.
    pub planes: bool,
    /// Cuts applied.
    pub cuts: bool,
    /// Fills drawn.
    pub fills: bool,
}

/// The `section-display-set` view command (docs/05 §9).
#[derive(Debug, Default, Clone, Copy)]
pub struct SectionDisplaySet;

impl Command for SectionDisplaySet {
    type Params = SectionDisplayParams;
    type Output = SectionDisplayOutput;

    fn name(&self) -> &'static str {
        "section-display-set"
    }

    fn validate(&self, _doc: &Document, _p: &SectionDisplayParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SectionDisplayParams,
    ) -> Result<SectionDisplayOutput, CommandError> {
        let mut rendering = tx.document().rendering.clone();
        let s = &mut rendering.section;
        s.planes_visible = p.planes.unwrap_or(s.planes_visible);
        s.cuts_visible = p.cuts.unwrap_or(s.cuts_visible);
        s.fills_visible = p.fills.unwrap_or(s.fills_visible);
        let out = SectionDisplayOutput {
            planes: s.planes_visible,
            cuts: s.cuts_visible,
            fills: s.fills_visible,
        };
        if rendering != tx.document().rendering {
            tx.set_rendering(rendering)?;
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("section-display-set", "Section Display", true)
    }
}
