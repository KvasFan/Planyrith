//! Implements docs/05-modeling-tools.md §5.4–§5.5 and the component
//! browser's insertion (docs/07 §6): `make-unique`, `component-insert`,
//! `component-save-as`, `component-reload` and `component-replace`.
//! Component files (`.plyrc`, docs/09 §1) are native containers whose model
//! holds one instance of the component at the origin; definitions move
//! between documents with their nested definitions, materials (matched by
//! name), images and tags (matched by name), and keep their UUID, by which
//! a document recognizes a component it already has (docs/09 Decisions).

use std::collections::BTreeMap;

use glam::{DAffine3, DMat3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{definition_uuid, targets_in};
use crate::commands::organize::libraries::{library_options, storage};
use crate::commands::params::PointSchema;
use crate::commands::params::{resolve_ref, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ComponentInstance, ContainerRef, Document, EntityPath, EntityRecord, EntityRef, EntityView,
    GlueTo, Query,
};
use crate::ids::{DefinitionId, EntityId, MaterialId, Pid, TagId};
use crate::io::native::{read_document, write_document};
use crate::io::{IoError, NoProgress, StorageKey};

fn meta(name: &str, label: &str, view_only: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Organize,
        label: label.into(),
        view_only,
        steps: Vec::new(),
    }
}

fn selection_query() -> Query {
    Query::new("selection")
}

/// A component definition by name or UUID (internal group definitions are
/// not components).
pub(crate) fn component_named(doc: &Document, text: &str) -> Result<DefinitionId, CommandError> {
    doc.definition_by_name(text)
        .or_else(|| {
            doc.definition_order.iter().copied().find(|d| {
                doc.definition(*d)
                    .is_some_and(|def| !def.is_internal_group && def.uuid.to_string() == text)
            })
        })
        .ok_or_else(|| {
            CommandError::invalid_param("definition", format!("no component named `{text}`"))
        })
}

/// `name`, or `name #2`, `name #3`… whichever no component has yet.
fn unique_name(doc: &Document, name: &str) -> String {
    if doc.definition_by_name(name).is_none() {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name} #{n}"))
        .find(|candidate| doc.definition_by_name(candidate).is_none())
        .unwrap_or_else(|| name.to_string())
}

/// Ids already copied from a source document into the transaction's.
#[derive(Debug, Default)]
pub(crate) struct Imports {
    definitions: BTreeMap<DefinitionId, DefinitionId>,
    materials: BTreeMap<MaterialId, MaterialId>,
    tags: BTreeMap<TagId, TagId>,
}

impl Imports {
    /// The definitions copied (or reused) so far, by target id.
    pub(crate) fn copied(&self) -> impl Iterator<Item = DefinitionId> + '_ {
        self.definitions.values().copied()
    }
}

/// Copies definition `d` of `source`, with the definitions it nests and the
/// materials, images and tags its contents use, into the transaction's
/// document. A definition whose UUID the document already has is reused
/// (unless `fresh`, which always copies `d` itself and gives the copy a new
/// UUID); materials and tags are matched by name.
pub(crate) fn import_definition(
    tx: &mut Transaction<'_>,
    source: &Document,
    d: DefinitionId,
    fresh: bool,
    imports: &mut Imports,
) -> Result<DefinitionId, CommandError> {
    if let Some(done) = imports.definitions.get(&d) {
        return Ok(*done);
    }
    let def = source
        .definition(d)
        .ok_or_else(|| CommandError::Internal(format!("{d} is not in the source")))?;
    if !fresh
        && let Some(existing) = tx.document().definition_order.iter().copied().find(|x| {
            tx.document()
                .definition(*x)
                .is_some_and(|o| o.uuid == def.uuid)
        })
    {
        imports.definitions.insert(d, existing);
        return Ok(existing);
    }
    let refs = def.entities.references();
    for inner in &refs.definitions {
        import_definition(tx, source, *inner, false, imports)?;
    }
    for m in &refs.materials {
        import_material(tx, source, *m, imports)?;
    }
    for t in &refs.tags {
        import_tag(tx, source, *t, imports)?;
    }
    let mut copy = def.clone();
    copy.entities
        .remap_refs(&imports.definitions, &imports.materials, &imports.tags);
    if !copy.is_internal_group {
        copy.name = unique_name(tx.document(), &copy.name);
    }
    if fresh {
        copy.uuid = definition_uuid(tx.document(), &copy.name, &copy.entities);
    }
    copy.usage_count = 0;
    if let Some(thumbnail) = copy.thumbnail
        && let Some(data) = source.images.get(thumbnail)
    {
        copy.thumbnail = Some(tx.insert_image(data.bytes.clone())?);
    }
    let pids: Vec<Pid> = (0..copy.entities.len())
        .map(|_| tx.allocate_pid())
        .collect();
    copy.entities.reassign_pids(pids);
    let id = tx.insert_definition(copy)?;
    imports.definitions.insert(d, id);
    Ok(id)
}

fn import_material(
    tx: &mut Transaction<'_>,
    source: &Document,
    m: MaterialId,
    imports: &mut Imports,
) -> Result<(), CommandError> {
    if imports.materials.contains_key(&m) {
        return Ok(());
    }
    let Some(material) = source.materials.get(m) else {
        return Ok(());
    };
    let target = match material.is_default_for {
        Some(side) => tx.document().materials.default_for(side),
        None => match tx.document().materials.by_name(&material.name) {
            Some(existing) => existing,
            None => {
                let mut copy = material.clone();
                if let Some(t) = &mut copy.texture {
                    let data = source.images.get(t.image).ok_or_else(|| {
                        CommandError::Io(IoError::Corrupt {
                            part: "images".into(),
                            reason: format!("the texture of `{}` is missing", copy.name),
                        })
                    })?;
                    t.image = tx.insert_image(data.bytes.clone())?;
                }
                tx.insert_material(copy)?
            }
        },
    };
    imports.materials.insert(m, target);
    Ok(())
}

fn import_tag(
    tx: &mut Transaction<'_>,
    source: &Document,
    t: TagId,
    imports: &mut Imports,
) -> Result<(), CommandError> {
    if imports.tags.contains_key(&t) {
        return Ok(());
    }
    let Some(tag) = source.tags.get(t) else {
        return Ok(());
    };
    let target = match tx.document().tags.by_name(&tag.name) {
        Some(existing) => existing,
        None => {
            let mut copy = tag.clone();
            copy.folder = None;
            tx.insert_tag(copy)?
        }
    };
    imports.tags.insert(t, target);
    Ok(())
}

/// The component a `.plyrc` file carries: the definition of the single
/// instance at the root of its model.
fn file_component(doc: &Document, path: &str) -> Result<DefinitionId, CommandError> {
    let mut instances = doc.model.instances();
    match (instances.next(), instances.next()) {
        (Some((_, i)), None) => Ok(i.definition),
        _ => Err(CommandError::Io(IoError::UnsupportedFormat(format!(
            "`{path}` is not a component file (one component instance at the root expected)"
        )))),
    }
}

fn read_component_file(
    tx: &Transaction<'_>,
    path: &str,
) -> Result<(Document, DefinitionId), CommandError> {
    let bytes = storage(tx, path)?.read(&StorageKey::new(path))?;
    let doc = read_document(&bytes, &NoProgress)?.document;
    let d = file_component(&doc, path)?;
    Ok((doc, d))
}

fn instances_of(doc: &Document, d: DefinitionId) -> Vec<(ContainerRef, EntityId)> {
    let mut out = Vec::new();
    for c in doc.containers() {
        if let Some(entities) = doc.container(c) {
            for (i, x) in entities.instances() {
                if x.definition == d {
                    out.push((c, EntityId::Instance(i)));
                }
            }
        }
    }
    out
}

/// Points an instance at another definition.
fn retarget(
    tx: &mut Transaction<'_>,
    container: ContainerRef,
    id: EntityId,
    definition: DefinitionId,
) -> Result<(), CommandError> {
    let Some(EntityView::Instance(x)) = tx.document().container(container).and_then(|e| e.view(id))
    else {
        return Ok(());
    };
    let record = EntityRecord::Instance(ComponentInstance {
        definition,
        ..x.clone()
    });
    tx.replace_entity(container, id, record)
}

// ----- make-unique --------------------------------------------------------------------------

/// `make-unique` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MakeUniqueParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Component instances to give their own definition (default: the
    /// selection).
    #[serde(default = "selection_query")]
    pub targets: Query,
}

/// `make-unique` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MakeUniqueOutput {
    /// New definitions (UUIDs), one per definition the instances shared.
    pub definitions: Vec<String>,
}

/// The `make-unique` command (docs/05 §5.4): the selected instances of each
/// definition move to one new copy of it; instances that are the only users
/// of their definition stay as they are.
#[derive(Debug, Default, Clone, Copy)]
pub struct MakeUnique;

impl Command for MakeUnique {
    type Params = MakeUniqueParams;
    type Output = MakeUniqueOutput;

    fn name(&self) -> &'static str {
        "make-unique"
    }

    fn validate(&self, _doc: &Document, _p: &MakeUniqueParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MakeUniqueParams,
    ) -> Result<MakeUniqueOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?;
        let doc = tx.document();
        let mut by_definition: BTreeMap<DefinitionId, Vec<EntityId>> = BTreeMap::new();
        for id in ids {
            if let EntityId::Instance(i) = id
                && let Some(x) = doc.container(target.container).and_then(|e| e.instance(i))
            {
                by_definition.entry(x.definition).or_default().push(id);
            }
        }
        if by_definition.is_empty() {
            return Err(CommandError::NotApplicable(
                "no component instance is selected".into(),
            ));
        }
        let mut out = Vec::new();
        for (d, chosen) in by_definition {
            if instances_of(tx.document(), d).len() == chosen.len() {
                continue;
            }
            let source = tx.document().clone();
            let mut imports = Imports::default();
            let copy = import_definition(tx, &source, d, true, &mut imports)?;
            for id in chosen {
                retarget(tx, target.container, id, copy)?;
            }
            if let Some(def) = tx.document().definition(copy) {
                out.push(def.uuid.to_string());
            }
        }
        Ok(MakeUniqueOutput { definitions: out })
    }

    fn meta(&self) -> CommandMeta {
        meta("make-unique", "Make Unique", false)
    }
}

// ----- component-save-as --------------------------------------------------------------------

/// `component-save-as` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentSaveAsParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The component (name or UUID).
    pub definition: String,
    /// File to write (`.plyrc`).
    pub path: String,
}

/// Output naming a component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentOutput {
    /// Component name.
    pub definition: String,
    /// Its UUID.
    pub uuid: String,
}

/// The `component-save-as` command (docs/05 §5.5; changes nothing).
#[derive(Debug, Default, Clone, Copy)]
pub struct ComponentSaveAs;

impl Command for ComponentSaveAs {
    type Params = ComponentSaveAsParams;
    type Output = ComponentOutput;

    fn name(&self) -> &'static str {
        "component-save-as"
    }

    fn validate(&self, doc: &Document, p: &ComponentSaveAsParams) -> Result<(), CommandError> {
        component_named(doc, &p.definition)?;
        if p.path.trim().is_empty() {
            return Err(CommandError::invalid_param("path", "must not be empty"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ComponentSaveAsParams,
    ) -> Result<ComponentOutput, CommandError> {
        let d = component_named(tx.document(), &p.definition)?;
        let source = tx.document();
        let def = source
            .definition(d)
            .ok_or_else(|| CommandError::Internal("definition vanished".into()))?;
        let out = ComponentOutput {
            definition: def.name.clone(),
            uuid: def.uuid.to_string(),
        };
        let mut file = Document::new();
        file.info.title = def.name.clone();
        file.info.display_units = source.info.display_units;
        {
            let mut ftx = Transaction::new(&mut file, "Save Component");
            let copy = import_definition(&mut ftx, source, d, false, &mut Imports::default())?;
            ftx.insert_entity(
                ContainerRef::Model,
                EntityRecord::Instance(ComponentInstance {
                    definition: copy,
                    transform: DAffine3::IDENTITY,
                    name: None,
                    material_override: None,
                    glued_to: None,
                    dynamic_attrs: Default::default(),
                }),
            )?;
            // The file is written below; its transaction needs no history.
            drop(ftx.commit());
        }
        let bytes = write_document(&file, &[], &library_options(), &NoProgress)?;
        storage(tx, &p.path)?.write(&StorageKey::new(&p.path), &bytes)?;
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("component-save-as", "Save Component As", true)
    }
}

// ----- component-insert ---------------------------------------------------------------------

/// `component-insert` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentInsertParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// A component of the model (name or UUID).
    #[serde(default)]
    pub definition: Option<String>,
    /// A component file (`.plyrc`) to insert from; a component it carries
    /// that the model already has (same UUID) is reused.
    #[serde(default)]
    pub path: Option<String>,
    /// Where the insertion point goes (world, mm).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub position: Option<DVec3>,
    /// A face to glue to (the component's red-green plane lies on it, its
    /// blue axis along the face normal); needs a gluing component.
    #[serde(default)]
    pub on_face: Option<EntityRef>,
    /// Rotation about the blue axis, degrees (0).
    #[serde(default)]
    pub rotation: f64,
}

/// `component-insert` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentInsertOutput {
    /// Component name.
    pub definition: String,
    /// The new instance.
    pub instance: Pid,
    /// `true` when it is glued to `on_face`.
    pub glued: bool,
}

/// The `component-insert` command (component browser, docs/07 §6).
#[derive(Debug, Default, Clone, Copy)]
pub struct ComponentInsert;

/// Whether a face with this world normal suits `glue`.
fn glues_on(glue: GlueTo, normal: DVec3) -> bool {
    let z = normal.normalize_or_zero().z.abs();
    match glue {
        GlueTo::None => false,
        GlueTo::Any => true,
        GlueTo::Horizontal => z > 1.0 - 1e-6,
        GlueTo::Vertical => z < 1e-6,
        GlueTo::Sloped => (1e-6..=1.0 - 1e-6).contains(&z),
    }
}

/// The world frame of a component lying on a plane with `normal`: blue
/// along the normal, red horizontal when the plane is not (else world red).
fn glue_frame(normal: DVec3) -> DMat3 {
    let z = normal.normalize_or_zero();
    let x = DVec3::Z.cross(z).try_normalize().unwrap_or(DVec3::X);
    let y = z.cross(x);
    DMat3::from_cols(x, y, z)
}

impl Command for ComponentInsert {
    type Params = ComponentInsertParams;
    type Output = ComponentInsertOutput;

    fn name(&self) -> &'static str {
        "component-insert"
    }

    fn validate(&self, doc: &Document, p: &ComponentInsertParams) -> Result<(), CommandError> {
        match (&p.definition, &p.path) {
            (Some(name), None) => component_named(doc, name).map(|_| ()),
            (None, Some(path)) if !path.trim().is_empty() => Ok(()),
            _ => Err(CommandError::invalid_param(
                "definition",
                "give either a component of the model or a component file `path`",
            )),
        }?;
        if let Some(p) = p.position {
            crate::commands::params::check_point("position", p)?;
        }
        if !p.rotation.is_finite() {
            return Err(CommandError::invalid_param("rotation", "must be finite"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ComponentInsertParams,
    ) -> Result<ComponentInsertOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let d = match (&p.definition, &p.path) {
            (Some(name), _) => component_named(tx.document(), name)?,
            (None, Some(path)) => {
                let (file, fd) = read_component_file(tx, path)?;
                import_definition(tx, &file, fd, false, &mut Imports::default())?
            }
            (None, None) => unreachable_params()?,
        };
        let def = tx
            .document()
            .definition(d)
            .cloned()
            .ok_or_else(|| CommandError::Internal("definition vanished".into()))?;
        let position = p.position.unwrap_or(DVec3::ZERO);
        let mut frame = DMat3::IDENTITY;
        let mut glued_to = None;
        if let Some(face) = &p.on_face {
            let resolved = resolve_ref(tx, &target.context, "on_face", face)?;
            let EntityId::Face(f) = resolved.id else {
                return Err(CommandError::NotApplicable(format!("{face} is not a face")));
            };
            let plane = tx
                .document()
                .container(resolved.container)
                .and_then(|c| c.mesh().face(f))
                .map(|x| x.plane.transformed(&resolved.world))
                .ok_or_else(|| CommandError::Internal("face vanished".into()))?;
            if !glues_on(def.behavior.glue_to, plane.normal) {
                return Err(CommandError::NotApplicable(format!(
                    "`{}` does not glue to this face (glue to: {:?})",
                    def.name, def.behavior.glue_to
                )));
            }
            frame = glue_frame(plane.normal);
            let doc = tx.document();
            let pid = doc
                .container(resolved.container)
                .and_then(|c| c.pid_of(EntityId::Face(f)))
                .ok_or_else(|| CommandError::Internal("face without pid".into()))?;
            glued_to = Some(EntityPath::new(
                target.context.path.clone(),
                crate::document::EntityKey::Pid(pid),
            ));
        }
        // libm, not the platform's sin/cos: output is bit-identical
        // everywhere (docs/02 Decisions).
        let (sin, cos) = (
            libm::sin(p.rotation.to_radians()),
            libm::cos(p.rotation.to_radians()),
        );
        let spin = DMat3::from_cols(
            DVec3::new(cos, sin, 0.0),
            DVec3::new(-sin, cos, 0.0),
            DVec3::Z,
        );
        let linear = frame * spin;
        // World placement: the insertion point lands on `position`.
        let world = DAffine3::from_mat3_translation(
            linear,
            position - linear * def.behavior.insertion_point,
        );
        let local = target.local * world;
        let (_, pid) = tx.insert_entity(
            target.container,
            EntityRecord::Instance(ComponentInstance {
                definition: d,
                transform: local,
                name: None,
                material_override: None,
                glued_to: glued_to.clone(),
                dynamic_attrs: Default::default(),
            }),
        )?;
        tx.set_selection(crate::document::Selection::single(EntityPath::new(
            target.context.path.clone(),
            crate::document::EntityKey::Pid(pid),
        )));
        Ok(ComponentInsertOutput {
            definition: def.name,
            instance: pid,
            glued: glued_to.is_some(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("component-insert", "Insert Component", false)
    }
}

fn unreachable_params() -> Result<DefinitionId, CommandError> {
    Err(CommandError::invalid_param(
        "definition",
        "give either a component of the model or a component file `path`",
    ))
}

// ----- component-reload ---------------------------------------------------------------------

/// `component-reload` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentReloadParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The component to update (name or UUID).
    pub definition: String,
    /// The component file to take its new contents from.
    pub path: String,
}

/// The `component-reload` command (docs/05 §5.5): the component's contents
/// and behavior come from the file; its name, UUID and instances stay.
#[derive(Debug, Default, Clone, Copy)]
pub struct ComponentReload;

impl Command for ComponentReload {
    type Params = ComponentReloadParams;
    type Output = ComponentOutput;

    fn name(&self) -> &'static str {
        "component-reload"
    }

    fn validate(&self, doc: &Document, p: &ComponentReloadParams) -> Result<(), CommandError> {
        component_named(doc, &p.definition).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ComponentReloadParams,
    ) -> Result<ComponentOutput, CommandError> {
        let old = component_named(tx.document(), &p.definition)?;
        let (file, fd) = read_component_file(tx, &p.path)?;
        let fresh = import_definition(tx, &file, fd, true, &mut Imports::default())?;
        for (c, id) in instances_of(tx.document(), old) {
            retarget(tx, c, id, fresh)?;
        }
        let previous = tx
            .document()
            .definition(old)
            .cloned()
            .ok_or_else(|| CommandError::Internal("definition vanished".into()))?;
        let removed = tx.remove_definition(old)?;
        // Groups inside the old contents leave their private definitions.
        let inner: Vec<DefinitionId> = removed
            .entities
            .groups()
            .map(|(_, g)| g.definition)
            .collect();
        for d in inner {
            crate::commands::common::purge_private_definition(tx, d)?;
        }
        // The reloaded definition takes the old one's identity.
        let mut meta_def = tx
            .document()
            .definition(fresh)
            .cloned()
            .ok_or_else(|| CommandError::Internal("definition vanished".into()))?;
        meta_def.uuid = previous.uuid;
        meta_def.name = previous.name.clone();
        tx.update_definition(fresh, meta_def)?;
        Ok(ComponentOutput {
            definition: previous.name,
            uuid: previous.uuid.to_string(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("component-reload", "Reload Component", false)
    }
}

// ----- component-replace --------------------------------------------------------------------

/// `component-replace` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentReplaceParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Instances to replace (default: the selection).
    #[serde(default = "selection_query")]
    pub instances: Query,
    /// The replacement: a component of the model (name or UUID) or a
    /// component file path.
    pub with: String,
}

/// `component-replace` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentReplaceOutput {
    /// The replacement's name.
    pub definition: String,
    /// Instances replaced.
    pub replaced: usize,
}

/// The `component-replace` command (docs/05 §5.5): the instances keep their
/// placement and show another component.
#[derive(Debug, Default, Clone, Copy)]
pub struct ComponentReplace;

impl Command for ComponentReplace {
    type Params = ComponentReplaceParams;
    type Output = ComponentReplaceOutput;

    fn name(&self) -> &'static str {
        "component-replace"
    }

    fn validate(&self, _doc: &Document, p: &ComponentReplaceParams) -> Result<(), CommandError> {
        if p.with.trim().is_empty() {
            return Err(CommandError::invalid_param("with", "must not be empty"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ComponentReplaceParams,
    ) -> Result<ComponentReplaceOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids: Vec<EntityId> = targets_in(tx, &target, &p.instances)?
            .into_iter()
            .filter(|id| matches!(id, EntityId::Instance(_)))
            .collect();
        if ids.is_empty() {
            return Err(CommandError::NotApplicable(
                "no component instance to replace".into(),
            ));
        }
        let d = match component_named(tx.document(), &p.with) {
            Ok(d) => d,
            Err(_) if tx.storage().is_some() => {
                let (file, fd) = read_component_file(tx, &p.with)?;
                import_definition(tx, &file, fd, false, &mut Imports::default())?
            }
            Err(e) => return Err(e),
        };
        for id in &ids {
            retarget(tx, target.container, *id, d)?;
        }
        let name = tx
            .document()
            .definition(d)
            .map(|x| x.name.clone())
            .unwrap_or_default();
        Ok(ComponentReplaceOutput {
            definition: name,
            replaced: ids.len(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("component-replace", "Replace Component", false)
    }
}
