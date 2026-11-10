//! Implements docs/05-modeling-tools.md §5.18 (product data, milestone
//! 1.17): `properties-set`, `properties-list`, `revision-add`,
//! `revision-list` and `standard-part`.
//!
//! Properties are the attribute dictionary `properties` of a definition
//! (a group's or component's) or of the document; `part_number` is the key
//! the bill of materials reads. Revisions are the document attribute
//! `planyrith` / `revisions` (JSON), and the current one is also the
//! document property `revision`, so sheet labels and title blocks show it.

use std::collections::BTreeMap;

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{definition_uuid, targets_of};
use crate::commands::params::{PointSchema, target_container};
use crate::commands::transaction::AttrTarget;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ComponentDefinition, ComponentInstance, Document, Entities, EntityRecord, Query, Value,
};
use crate::ids::{DefinitionId, Pid};
use crate::parts::{PartKind, standard_part};
use crate::units::Length;

/// The dictionary of product properties.
pub const PROPERTIES: &str = "properties";
/// The key of the part number.
pub const PART_NUMBER: &str = "part_number";

fn meta(name: &str, label: &str, view_only: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        label: label.into(),
        category: CommandCategory::Organize,
        view_only,
        steps: Vec::new(),
    }
}

/// The definitions behind the groups and components `q` names.
fn definitions(
    tx: &Transaction<'_>,
    target: &crate::commands::params::Target,
    q: &Query,
) -> Result<Vec<(Pid, DefinitionId)>, CommandError> {
    let here = tx
        .document()
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
    let mut out = Vec::new();
    for id in targets_of(tx, target, "target", q)? {
        if let (Some(d), Some(pid)) = (here.definition_of(id), here.pid_of(id))
            && !out.iter().any(|(_, x)| *x == d)
        {
            out.push((pid, d));
        }
    }
    if out.is_empty() {
        return Err(CommandError::invalid_param(
            "target",
            "name groups or components",
        ));
    }
    Ok(out)
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Str(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// One set of properties.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PropertyRow {
    /// The group or component (none: the document).
    pub target: Option<Pid>,
    /// Its name (the document's title).
    pub name: String,
    /// The part number.
    pub part_number: Option<String>,
    /// The description.
    pub description: String,
    /// Every property, part number included.
    pub properties: BTreeMap<String, String>,
}

fn row_of(doc: &Document, target: Option<(Pid, DefinitionId)>) -> PropertyRow {
    let (attrs, name, description) = match target {
        None => (
            &doc.attrs,
            doc.info.title.clone(),
            doc.info.description.clone(),
        ),
        Some((_, d)) => match doc.definition(d) {
            Some(def) => (&def.attrs, def.name.clone(), def.description.clone()),
            None => (&doc.attrs, String::new(), String::new()),
        },
    };
    let properties: BTreeMap<String, String> = attrs
        .dictionary(PROPERTIES)
        .map(|d| {
            d.iter()
                .filter_map(|(k, v)| text(Some(v)).map(|t| (k.clone(), t)))
                .collect()
        })
        .unwrap_or_default();
    PropertyRow {
        target: target.map(|t| t.0),
        name,
        part_number: properties.get(PART_NUMBER).cloned(),
        description,
        properties,
    }
}

// ----- properties-set -------------------------------------------------------------------

/// `properties-set` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PropertiesSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Groups or components (their definitions); none: the document.
    #[serde(default)]
    pub target: Option<Query>,
    /// The part number (empty removes it).
    #[serde(default)]
    pub part_number: Option<String>,
    /// The description.
    #[serde(default)]
    pub description: Option<String>,
    /// Other properties by name (an empty value removes one).
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

/// What `properties-set` changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PropertiesOutput {
    /// The properties after the change (or as they are).
    pub rows: Vec<PropertyRow>,
}

fn check_key(k: &str) -> Result<(), CommandError> {
    if k.trim().is_empty() || k.chars().count() > 64 || k.contains(['.', '\n']) {
        return Err(CommandError::invalid_param(
            "properties",
            format!("`{k}`: a name is 1 to 64 characters without dots or line breaks"),
        ));
    }
    Ok(())
}

/// The `properties-set` command: part numbers, descriptions and custom
/// properties of definitions or of the document.
#[derive(Debug, Default, Clone, Copy)]
pub struct PropertiesSet;

impl Command for PropertiesSet {
    type Params = PropertiesSetParams;
    type Output = PropertiesOutput;

    fn name(&self) -> &'static str {
        "properties-set"
    }

    fn validate(&self, _: &Document, p: &PropertiesSetParams) -> Result<(), CommandError> {
        for (k, v) in &p.properties {
            check_key(k)?;
            if v.chars().count() > 4096 {
                return Err(CommandError::invalid_param(
                    "properties",
                    "a value is too long",
                ));
            }
        }
        if p.part_number.is_none() && p.description.is_none() && p.properties.is_empty() {
            return Err(CommandError::invalid_param(
                "properties",
                "give a part number, a description or properties",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: PropertiesSetParams,
    ) -> Result<PropertiesOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let targets: Vec<Option<(Pid, DefinitionId)>> = match &p.target {
            None => vec![None],
            Some(q) => definitions(tx, &target, q)?.into_iter().map(Some).collect(),
        };
        let mut values = p.properties.clone();
        if let Some(n) = &p.part_number {
            values.insert(PART_NUMBER.into(), n.clone());
        }
        for t in &targets {
            let at = match t {
                None => AttrTarget::Document,
                Some((_, d)) => AttrTarget::Definition(*d),
            };
            for (k, v) in &values {
                let value = (!v.is_empty()).then(|| Value::Str(v.clone()));
                tx.set_attribute(at, PROPERTIES, k, value)?;
            }
            if let Some(desc) = &p.description {
                match t {
                    None => {
                        let mut info = tx.document().info.clone();
                        info.description = desc.clone();
                        tx.set_info(info)?;
                    }
                    Some((_, d)) => {
                        let mut def = tx
                            .document()
                            .definition(*d)
                            .ok_or_else(|| CommandError::Internal("a definition vanished".into()))?
                            .clone();
                        def.description = desc.clone();
                        tx.update_definition(*d, def)?;
                    }
                }
            }
        }
        let doc = tx.document();
        Ok(PropertiesOutput {
            rows: targets.into_iter().map(|t| row_of(doc, t)).collect(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("properties-set", "Set Properties", false)
    }
}

// ----- properties-list ------------------------------------------------------------------

/// `properties-list` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PropertiesListParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Groups or components; none: the document and every component
    /// definition with properties.
    #[serde(default)]
    pub target: Option<Query>,
}

/// The `properties-list` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct PropertiesList;

impl Command for PropertiesList {
    type Params = PropertiesListParams;
    type Output = PropertiesOutput;

    fn name(&self) -> &'static str {
        "properties-list"
    }

    fn validate(&self, _: &Document, _: &PropertiesListParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: PropertiesListParams,
    ) -> Result<PropertiesOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let doc = tx.document();
        let rows = match &p.target {
            Some(q) => definitions(tx, &target, q)?
                .into_iter()
                .map(|t| row_of(doc, Some(t)))
                .collect(),
            None => {
                let mut rows = vec![row_of(doc, None)];
                for c in doc.containers() {
                    if let crate::document::ContainerRef::Definition(d) = c
                        && let Some(def) = doc.definition(d)
                        && !def.is_internal_group
                        && def
                            .attrs
                            .dictionary(PROPERTIES)
                            .is_some_and(|x| !x.is_empty())
                    {
                        let mut row = row_of(doc, Some((Pid::NONE, d)));
                        row.target = None;
                        rows.push(row);
                    }
                }
                rows
            }
        };
        Ok(PropertiesOutput { rows })
    }

    fn meta(&self) -> CommandMeta {
        meta("properties-list", "List Properties", true)
    }
}

// ----- revisions ------------------------------------------------------------------------

/// The key of the revision history in the `planyrith` dictionary.
pub const REVISIONS: &str = "revisions";

/// One recorded revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Revision {
    /// Its label (`A`, `B`, … `Z`, `AA`, …).
    pub label: String,
    /// What changed.
    pub description: String,
    /// Who made it.
    #[serde(default)]
    pub author: String,
    /// When, as written (the core reads no clock).
    #[serde(default)]
    pub date: String,
}

/// The revision label after `n` earlier ones.
pub fn revision_label(n: usize) -> String {
    let mut n = n + 1;
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// The document's revisions, oldest first.
pub fn revisions_of(doc: &Document) -> Vec<Revision> {
    match doc.attrs.get(crate::sketch::DICT, REVISIONS) {
        Some(Value::Str(json)) => serde_json::from_str(json).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `revision-add` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RevisionAddParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What changed.
    pub description: String,
    /// Who made it.
    #[serde(default)]
    pub author: String,
    /// When (`2026-11-10`), as it should be printed.
    #[serde(default)]
    pub date: String,
    /// The label (the next letter by default).
    #[serde(default)]
    pub label: Option<String>,
}

/// The revisions after a change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RevisionsOutput {
    /// Oldest first.
    pub revisions: Vec<Revision>,
    /// The current revision's label.
    pub current: Option<String>,
}

/// The `revision-add` command: records a revision of the document.
#[derive(Debug, Default, Clone, Copy)]
pub struct RevisionAdd;

impl Command for RevisionAdd {
    type Params = RevisionAddParams;
    type Output = RevisionsOutput;

    fn name(&self) -> &'static str {
        "revision-add"
    }

    fn validate(&self, _: &Document, p: &RevisionAddParams) -> Result<(), CommandError> {
        if p.description.trim().is_empty() || p.description.chars().count() > 1000 {
            return Err(CommandError::invalid_param(
                "description",
                "describe the revision in 1 to 1000 characters",
            ));
        }
        if p.label
            .as_ref()
            .is_some_and(|l| l.trim().is_empty() || l.chars().count() > 16)
        {
            return Err(CommandError::invalid_param("label", "1 to 16 characters"));
        }
        if p.author.chars().count() > 200 || p.date.chars().count() > 64 {
            return Err(CommandError::invalid_param("author", "too long"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: RevisionAddParams,
    ) -> Result<RevisionsOutput, CommandError> {
        let mut list = revisions_of(tx.document());
        let label = p.label.unwrap_or_else(|| revision_label(list.len()));
        if list.iter().any(|r| r.label == label) {
            return Err(CommandError::invalid_param(
                "label",
                format!("revision {label} exists"),
            ));
        }
        list.push(Revision {
            label: label.clone(),
            description: p.description,
            author: p.author,
            date: p.date,
        });
        let json =
            serde_json::to_string(&list).map_err(|e| CommandError::Internal(e.to_string()))?;
        tx.set_attribute(
            AttrTarget::Document,
            crate::sketch::DICT,
            REVISIONS,
            Some(Value::Str(json)),
        )?;
        tx.set_attribute(
            AttrTarget::Document,
            PROPERTIES,
            "revision",
            Some(Value::Str(label.clone())),
        )?;
        Ok(RevisionsOutput {
            revisions: list,
            current: Some(label),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("revision-add", "Add Revision", false)
    }
}

/// `revision-list` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RevisionListParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
}

/// The `revision-list` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct RevisionList;

impl Command for RevisionList {
    type Params = RevisionListParams;
    type Output = RevisionsOutput;

    fn name(&self) -> &'static str {
        "revision-list"
    }

    fn validate(&self, _: &Document, _: &RevisionListParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        _: RevisionListParams,
    ) -> Result<RevisionsOutput, CommandError> {
        let revisions = revisions_of(tx.document());
        let current = revisions.last().map(|r| r.label.clone());
        Ok(RevisionsOutput { revisions, current })
    }

    fn meta(&self) -> CommandMeta {
        meta("revision-list", "List Revisions", true)
    }
}

// ----- standard-part --------------------------------------------------------------------

/// `standard-part` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StandardPartParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What kind of part.
    pub kind: PartKind,
    /// Its size (`M8`; a pin's diameter `6`; a bearing's `6204`).
    pub size: String,
    /// The length of a screw (under the head) or pin.
    #[serde(default)]
    pub length: Option<Length>,
    /// Where its origin goes.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub position: Option<DVec3>,
    /// The direction of its axis (+Z).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub axis: Option<DVec3>,
}

/// What `standard-part` placed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StandardPartOutput {
    /// The component instance.
    pub instance: Option<Pid>,
    /// The component's name and part number.
    pub designation: String,
    /// Its description.
    pub description: String,
    /// Its volume, mm³.
    pub volume_mm3: f64,
    /// Whether an existing component of that designation was reused.
    pub reused: bool,
}

/// The `standard-part` command: places a component of a standard part,
/// made from its size table, reusing the component when the document has
/// one of that designation.
#[derive(Debug, Default, Clone, Copy)]
pub struct StandardPart;

impl Command for StandardPart {
    type Params = StandardPartParams;
    type Output = StandardPartOutput;

    fn name(&self) -> &'static str {
        "standard-part"
    }

    fn validate(&self, _: &Document, p: &StandardPartParams) -> Result<(), CommandError> {
        if !p.kind.sizes().contains(&p.size.as_str()) {
            return Err(CommandError::invalid_param(
                "size",
                format!("sizes: {}", p.kind.sizes().join(", ")),
            ));
        }
        if p.kind.has_length() && p.length.is_none() {
            return Err(CommandError::invalid_param("length", "give the length"));
        }
        if p.axis.is_some_and(|a| a.try_normalize().is_none()) {
            return Err(CommandError::invalid_param("axis", "must be non-zero"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StandardPartParams,
    ) -> Result<StandardPartOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let tol = *tx.tolerances();
        let part = standard_part(
            p.kind,
            &p.size,
            if p.kind.has_length() {
                p.length.map(|l| l.as_mm())
            } else {
                None
            },
            &tol,
        )?;
        let volume_mm3 = part.mesh.volume()?;
        let existing = tx.document().definition_by_name(&part.designation);
        let reused = existing.is_some();
        let definition = match existing {
            Some(d) => d,
            None => {
                let entities = Entities::from_mesh(part.mesh, || tx.allocate_pid());
                let uuid = definition_uuid(tx.document(), &part.designation, &entities);
                let mut def = ComponentDefinition::new(uuid, part.designation.clone());
                def.description = part.description.clone();
                def.entities = entities;
                let d = tx.insert_definition(def)?;
                for (k, v) in [
                    (PART_NUMBER, part.designation.as_str()),
                    ("standard", part.standard),
                ] {
                    tx.set_attribute(
                        AttrTarget::Definition(d),
                        PROPERTIES,
                        k,
                        Some(Value::Str(v.to_string())),
                    )?;
                }
                d
            }
        };
        let axis = p.axis.unwrap_or(DVec3::Z).normalize();
        let rotation = glam::DQuat::from_rotation_arc(DVec3::Z, axis);
        let world =
            DAffine3::from_rotation_translation(rotation, p.position.unwrap_or(DVec3::ZERO));
        let (_, pid) = tx.insert_entity(
            target.container,
            EntityRecord::Instance(ComponentInstance {
                definition,
                transform: target.local * world,
                name: None,
                material_override: None,
                glued_to: None,
                dynamic_attrs: Default::default(),
            }),
        )?;
        Ok(StandardPartOutput {
            instance: Some(pid),
            designation: part.designation,
            description: part.description,
            volume_mm3,
            reused,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("standard-part", "Standard Part", false)
    }
}

/// Registers the product data commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(PropertiesSet);
    registry.register(PropertiesList);
    registry.register(RevisionAdd);
    registry.register(RevisionList);
    registry.register(StandardPart);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::revision_label;
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn run(e: &mut Engine, name: &str, p: Value) -> Value {
        e.execute_erased(name, p).unwrap()
    }

    #[test]
    fn labels_run_past_z() {
        assert_eq!(revision_label(0), "A");
        assert_eq!(revision_label(25), "Z");
        assert_eq!(revision_label(26), "AA");
        assert_eq!(revision_label(27), "AB");
    }

    #[test]
    fn standard_parts_properties_and_revisions() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let a = run(
            &mut e,
            "standard-part",
            json!({"kind": "hex-bolt", "size": "M8", "length": 30}),
        );
        assert_eq!(a["designation"], "ISO 4017 M8x30");
        assert_eq!(a["reused"], false);
        let b = run(
            &mut e,
            "standard-part",
            json!({"kind": "hex-bolt", "size": "M8", "length": 30, "position": [50, 0, 0]}),
        );
        assert_eq!(b["reused"], true);
        run(
            &mut e,
            "standard-part",
            json!({"kind": "hex-nut", "size": "M8", "position": [0, 0, 10]}),
        );
        // The bill of materials counts them by part number.
        let bom = run(&mut e, "bill-of-materials", json!({}));
        let rows = bom["rows"].as_array().unwrap();
        let bolt = rows.iter().find(|r| r["name"] == "ISO 4017 M8x30").unwrap();
        assert_eq!(bolt["quantity"], 2);
        assert_eq!(bolt["part_number"], "ISO 4017 M8x30");
        // Custom properties on a component and on the document.
        let set = run(
            &mut e,
            "properties-set",
            json!({"target": "instance & definition:\"ISO 4032 M8\"", "part_number": "NUT-008",
                   "properties": {"vendor": "Acme"}}),
        );
        assert_eq!(set["rows"][0]["part_number"], "NUT-008");
        assert_eq!(set["rows"][0]["properties"]["vendor"], "Acme");
        run(
            &mut e,
            "properties-set",
            json!({"part_number": "ASM-001", "description": "Clamp"}),
        );
        let list = run(&mut e, "properties-list", json!({}));
        assert_eq!(list["rows"][0]["part_number"], "ASM-001");
        assert_eq!(list["rows"][0]["description"], "Clamp");
        let r = run(
            &mut e,
            "revision-add",
            json!({"description": "First issue", "date": "2026-11-10"}),
        );
        assert_eq!(r["current"], "A");
        let r = run(
            &mut e,
            "revision-add",
            json!({"description": "Longer bolts"}),
        );
        assert_eq!(r["current"], "B");
        let list = run(&mut e, "properties-list", json!({}));
        assert_eq!(list["rows"][0]["properties"]["revision"], "B");
        assert!(
            e.execute_erased("revision-add", json!({"description": "x", "label": "A"}))
                .is_err()
        );
        assert!(
            e.execute_erased("standard-part", json!({"kind": "washer", "size": "M7"}))
                .is_err()
        );
    }
}
