//! Implements docs/05-modeling-tools.md §5.8 for milestone 0.1 (colors only):
//! `material-create`, `material-edit`, `material-delete`, `material-apply`,
//! `material-purge`. Textures arrive with milestone 0.4.

use std::collections::{BTreeSet, VecDeque};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::targets_in;
use crate::commands::organize::tags::parse_color;
use crate::commands::params::target_container;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ContainerRef, Document, EntityRecord, EntityView, Material, Query};
use crate::geom::FaceSides;
use crate::ids::{EntityId, FaceId, MaterialId};

fn meta(name: &str, label: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Organize,
        label: label.into(),
        view_only: false,
        steps: Vec::new(),
    }
}

fn material_named(doc: &Document, name: &str) -> Result<MaterialId, CommandError> {
    doc.materials.by_name(name).ok_or_else(|| {
        CommandError::invalid_param("material", format!("no material named `{name}`"))
    })
}

fn check_opacity(o: f32) -> Result<(), CommandError> {
    if !(0.0..=1.0).contains(&o) {
        return Err(CommandError::OutOfRange {
            name: "opacity".into(),
            reason: "must be between 0 and 1".into(),
        });
    }
    Ok(())
}

/// Color alpha mirrors opacity (docs/04 §4).
fn with_opacity(mut m: Material, opacity: f32) -> Material {
    m.opacity = opacity;
    m.color.a = (opacity * 255.0).round() as u8;
    m
}

fn one() -> f32 {
    1.0
}

/// Output naming a material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialOutput {
    /// Material name.
    pub material: String,
}

// ----- create / edit / delete ---------------------------------------------------------------

/// `material-create` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialCreateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Unique name.
    pub name: String,
    /// Color `#RRGGBB`.
    pub color: String,
    /// Opacity 0–1 (1).
    #[serde(default = "one")]
    pub opacity: f32,
}

/// The `material-create` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterialCreate;

impl Command for MaterialCreate {
    type Params = MaterialCreateParams;
    type Output = MaterialOutput;

    fn name(&self) -> &'static str {
        "material-create"
    }

    fn validate(&self, doc: &Document, p: &MaterialCreateParams) -> Result<(), CommandError> {
        if p.name.trim().is_empty() || doc.materials.by_name(&p.name).is_some() {
            return Err(CommandError::invalid_param(
                "name",
                "must be a new, non-empty material name",
            ));
        }
        parse_color("color", &p.color)?;
        check_opacity(p.opacity)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MaterialCreateParams,
    ) -> Result<MaterialOutput, CommandError> {
        let color = parse_color("color", &p.color)?;
        tx.insert_material(with_opacity(
            Material::solid(p.name.clone(), color),
            p.opacity,
        ))?;
        Ok(MaterialOutput { material: p.name })
    }

    fn meta(&self) -> CommandMeta {
        meta("material-create", "Create Material")
    }
}

/// `material-edit` parameters (0.1: name, color, opacity).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialEditParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Material to edit.
    pub material: String,
    /// New name.
    #[serde(default)]
    pub name: Option<String>,
    /// New color `#RRGGBB`.
    #[serde(default)]
    pub color: Option<String>,
    /// New opacity 0–1.
    #[serde(default)]
    pub opacity: Option<f32>,
}

/// The `material-edit` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterialEdit;

impl Command for MaterialEdit {
    type Params = MaterialEditParams;
    type Output = MaterialOutput;

    fn name(&self) -> &'static str {
        "material-edit"
    }

    fn validate(&self, doc: &Document, p: &MaterialEditParams) -> Result<(), CommandError> {
        material_named(doc, &p.material)?;
        if let Some(c) = &p.color {
            parse_color("color", c)?;
        }
        if let Some(o) = p.opacity {
            check_opacity(o)?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MaterialEditParams,
    ) -> Result<MaterialOutput, CommandError> {
        let id = material_named(tx.document(), &p.material)?;
        let mut m = tx
            .document()
            .materials
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("material vanished".into()))?;
        if let Some(name) = &p.name {
            m.name = name.clone();
        }
        if let Some(c) = &p.color {
            let opacity = m.opacity;
            m.color = parse_color("color", c)?;
            m = with_opacity(m, opacity);
        }
        if let Some(o) = p.opacity {
            m = with_opacity(m, o);
        }
        let name = m.name.clone();
        tx.replace_material(id, m)?;
        Ok(MaterialOutput { material: name })
    }

    fn meta(&self) -> CommandMeta {
        meta("material-edit", "Edit Material")
    }
}

/// `material-delete` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialDeleteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Material to delete (faces using it fall back to the default).
    pub material: String,
}

/// The `material-delete` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterialDelete;

impl Command for MaterialDelete {
    type Params = MaterialDeleteParams;
    type Output = MaterialOutput;

    fn name(&self) -> &'static str {
        "material-delete"
    }

    fn validate(&self, doc: &Document, p: &MaterialDeleteParams) -> Result<(), CommandError> {
        let id = material_named(doc, &p.material)?;
        if doc
            .materials
            .get(id)
            .is_some_and(|m| m.is_default_for.is_some())
        {
            return Err(CommandError::NotApplicable(
                "the default materials cannot be deleted".into(),
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MaterialDeleteParams,
    ) -> Result<MaterialOutput, CommandError> {
        let id = material_named(tx.document(), &p.material)?;
        tx.remove_material(id)?;
        Ok(MaterialOutput {
            material: p.material,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("material-delete", "Delete Material")
    }
}

// ----- apply --------------------------------------------------------------------------------

/// Which face sides `material-apply` paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Front side.
    #[default]
    Front,
    /// Back side.
    Back,
    /// Both sides.
    Both,
}

/// How far painting spreads from the matched faces (docs/05 §5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Fill {
    /// The matched faces only.
    #[default]
    Single,
    /// Faces reachable across shared edges that carry the same material.
    Connected,
    /// Every face of the container with the same material.
    SameMaterial,
    /// Every face of the container.
    All,
}

/// `material-apply` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialApplyParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Faces, groups or instances to paint.
    pub targets: Query,
    /// Material name.
    pub material: String,
    /// Sides to paint.
    #[serde(default)]
    pub side: Side,
    /// Spread.
    #[serde(default)]
    pub fill: Fill,
}

/// `material-apply` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialApplyOutput {
    /// Faces painted.
    pub faces: usize,
    /// Groups/instances given a material override.
    pub occurrences: usize,
}

/// The `material-apply` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterialApply;

/// The material key a fill compares (the painted side, front for `both`).
fn side_material(sides: &FaceSides, side: Side) -> Option<MaterialId> {
    match side {
        Side::Back => sides.material_back,
        Side::Front | Side::Both => sides.material_front,
    }
}

fn spread(
    doc: &Document,
    container: ContainerRef,
    seeds: &[FaceId],
    side: Side,
    fill: Fill,
) -> Result<Vec<FaceId>, CommandError> {
    let Some(mesh) = doc.container(container).map(|e| e.mesh()) else {
        return Ok(Vec::new());
    };
    let material = |f: FaceId| mesh.face(f).map(|x| side_material(&x.sides, side));
    Ok(match fill {
        Fill::Single => seeds.to_vec(),
        Fill::All => mesh.faces().map(|(f, _)| f).collect(),
        Fill::SameMaterial => {
            let wanted: BTreeSet<Option<MaterialId>> =
                seeds.iter().filter_map(|&f| material(f)).collect();
            mesh.faces()
                .filter(|(_, x)| wanted.contains(&side_material(&x.sides, side)))
                .map(|(f, _)| f)
                .collect()
        }
        Fill::Connected => {
            let mut out: BTreeSet<FaceId> = seeds.iter().copied().collect();
            let mut queue: VecDeque<FaceId> = seeds.iter().copied().collect();
            while let Some(f) = queue.pop_front() {
                let m = material(f);
                for g in mesh.face_neighbors(f)? {
                    if material(g) == m && out.insert(g) {
                        queue.push_back(g);
                    }
                }
            }
            out.into_iter().collect()
        }
    })
}

impl Command for MaterialApply {
    type Params = MaterialApplyParams;
    type Output = MaterialApplyOutput;

    fn name(&self) -> &'static str {
        "material-apply"
    }

    fn validate(&self, doc: &Document, p: &MaterialApplyParams) -> Result<(), CommandError> {
        p.targets
            .parse()
            .map_err(|e| CommandError::invalid_param("targets", e.to_string()))?;
        material_named(doc, &p.material).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MaterialApplyParams,
    ) -> Result<MaterialApplyOutput, CommandError> {
        let material = material_named(tx.document(), &p.material)?;
        // The defaults are stored as "no material" so they follow the document.
        let value = tx
            .document()
            .materials
            .get(material)
            .and_then(|m| m.is_default_for)
            .map_or(Some(material), |_| None);
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?;
        let seeds: Vec<FaceId> = ids
            .iter()
            .filter_map(|id| match id {
                EntityId::Face(f) => Some(*f),
                _ => None,
            })
            .collect();
        let faces = spread(tx.document(), target.container, &seeds, p.side, p.fill)?;
        for &f in &faces {
            let Some(mut sides) = tx
                .document()
                .container(target.container)
                .and_then(|e| e.mesh().face(f))
                .map(|x| x.sides.clone())
            else {
                continue;
            };
            if matches!(p.side, Side::Front | Side::Both) {
                sides.material_front = value;
            }
            if matches!(p.side, Side::Back | Side::Both) {
                sides.material_back = value;
            }
            tx.set_face_sides(target.container, f, sides)?;
        }
        let mut occurrences = 0;
        for &id in &ids {
            match tx
                .document()
                .container(target.container)
                .and_then(|e| e.view(id))
            {
                Some(EntityView::Instance(i)) => {
                    let mut i = i.clone();
                    i.material_override = value;
                    tx.replace_entity(target.container, id, EntityRecord::Instance(i))?;
                    occurrences += 1;
                }
                Some(EntityView::Group(g)) => {
                    // Groups have no override: paint the default-material
                    // faces of their private definition instead.
                    let d = ContainerRef::Definition(g.definition);
                    let plain: Vec<(FaceId, FaceSides)> = tx
                        .document()
                        .container(d)
                        .map(|e| {
                            e.mesh()
                                .faces()
                                .filter(|(_, x)| x.sides.material_front.is_none())
                                .map(|(f, x)| (f, x.sides.clone()))
                                .collect()
                        })
                        .unwrap_or_default();
                    for (f, mut sides) in plain {
                        sides.material_front = value;
                        tx.set_face_sides(d, f, sides)?;
                    }
                    occurrences += 1;
                }
                _ => {}
            }
        }
        Ok(MaterialApplyOutput {
            faces: faces.len(),
            occurrences,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("material-apply", "Paint")
    }
}

// ----- purge --------------------------------------------------------------------------------

/// `material-purge` parameters.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct MaterialPurgeParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
}

/// `material-purge` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialPurgeOutput {
    /// Names of the removed materials.
    pub removed: Vec<String>,
}

/// The `material-purge` command: removes materials nothing uses.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterialPurge;

impl Command for MaterialPurge {
    type Params = MaterialPurgeParams;
    type Output = MaterialPurgeOutput;

    fn name(&self) -> &'static str {
        "material-purge"
    }

    fn validate(&self, _doc: &Document, _p: &MaterialPurgeParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        _p: MaterialPurgeParams,
    ) -> Result<MaterialPurgeOutput, CommandError> {
        let doc = tx.document();
        let mut used: BTreeSet<MaterialId> = BTreeSet::new();
        for c in doc.containers() {
            if let Some(e) = doc.container(c) {
                for (_, f) in e.mesh().faces() {
                    used.extend(f.sides.material_front);
                    used.extend(f.sides.material_back);
                }
                for (_, i) in e.instances() {
                    used.extend(i.material_override);
                }
                for (_, i) in e.images() {
                    used.insert(i.material);
                }
            }
        }
        let unused: Vec<(MaterialId, String)> = doc
            .materials
            .iter()
            .filter(|(id, m)| m.is_default_for.is_none() && !used.contains(id))
            .map(|(id, m)| (id, m.name.clone()))
            .collect();
        let mut removed = Vec::new();
        for (id, name) in unused {
            tx.remove_material(id)?;
            removed.push(name);
        }
        Ok(MaterialPurgeOutput { removed })
    }

    fn meta(&self) -> CommandMeta {
        meta("material-purge", "Purge Materials")
    }
}
