//! Implements docs/09-file-formats.md §1 library files for
//! docs/05-modeling-tools.md §5.8: `material-library-save` writes chosen
//! materials (with their texture images) to a `.plyrm` library and
//! `material-library-load` adds a library's materials to the document. A
//! library is the native container holding a document with nothing but
//! those materials (docs/09 Decisions); files go through the engine's
//! storage (docs/01 §3.1 rule 2).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, Material};
use crate::io::native::{WriteOptions, read_document, write_document};
use crate::io::{IoError, NoProgress, Storage, StorageKey};

fn meta(name: &str, label: &str, view_only: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Organize,
        label: label.into(),
        view_only,
        steps: Vec::new(),
    }
}

/// The engine's storage, or an I/O error naming `path`.
pub(crate) fn storage<'a>(
    tx: &'a Transaction<'_>,
    path: &str,
) -> Result<&'a dyn Storage, CommandError> {
    tx.storage().ok_or_else(|| {
        CommandError::Io(IoError::Storage {
            key: path.to_string(),
            reason: "no storage is available to this command".into(),
        })
    })
}

/// Library files carry fixed timestamps: the core has no clock.
pub(crate) fn library_options() -> WriteOptions {
    WriteOptions {
        deterministic: true,
        include_history: false,
        ..WriteOptions::default()
    }
}

fn check_path(path: &str) -> Result<(), CommandError> {
    if path.trim().is_empty() {
        return Err(CommandError::invalid_param("path", "must not be empty"));
    }
    Ok(())
}

// ----- save ---------------------------------------------------------------------------------

/// `material-library-save` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialLibrarySaveParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Library file to write (`.plyrm`).
    pub path: String,
    /// Materials to include (all but the two defaults when empty).
    #[serde(default)]
    pub materials: Vec<String>,
}

/// Output of the library commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialLibraryOutput {
    /// Materials written or added, in library order.
    pub materials: Vec<String>,
    /// Materials not added because the document has one with that name.
    #[serde(default)]
    pub skipped: Vec<String>,
}

/// The `material-library-save` command (changes nothing in the document).
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterialLibrarySave;

impl Command for MaterialLibrarySave {
    type Params = MaterialLibrarySaveParams;
    type Output = MaterialLibraryOutput;

    fn name(&self) -> &'static str {
        "material-library-save"
    }

    fn validate(&self, doc: &Document, p: &MaterialLibrarySaveParams) -> Result<(), CommandError> {
        check_path(&p.path)?;
        for name in &p.materials {
            match doc
                .materials
                .by_name(name)
                .and_then(|id| doc.materials.get(id))
            {
                None => {
                    return Err(CommandError::invalid_param(
                        "materials",
                        format!("no material named `{name}`"),
                    ));
                }
                Some(m) if m.is_default_for.is_some() => {
                    return Err(CommandError::invalid_param(
                        "materials",
                        format!("`{name}` is a default material; libraries do not carry them"),
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MaterialLibrarySaveParams,
    ) -> Result<MaterialLibraryOutput, CommandError> {
        let doc = tx.document();
        let chosen: Vec<Material> = doc
            .materials
            .iter()
            .map(|(_, m)| m)
            .filter(|m| {
                m.is_default_for.is_none()
                    && (p.materials.is_empty() || p.materials.contains(&m.name))
            })
            .cloned()
            .collect();
        let mut library = Document::new();
        library.info.title = "Material library".into();
        let mut names = Vec::new();
        for m in chosen {
            if let Some(t) = &m.texture
                && let Some(data) = doc.images.get(t.image)
            {
                library.images.insert(data.clone());
            }
            names.push(m.name.clone());
            library
                .materials
                .insert(m)
                .map_err(|e| CommandError::Internal(e.to_string()))?;
        }
        let bytes = write_document(&library, &[], &library_options(), &NoProgress)?;
        storage(tx, &p.path)?.write(&StorageKey::new(&p.path), &bytes)?;
        Ok(MaterialLibraryOutput {
            materials: names,
            skipped: Vec::new(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("material-library-save", "Save Material Library", true)
    }
}

// ----- load ---------------------------------------------------------------------------------

/// `material-library-load` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialLibraryLoadParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Library file to read (`.plyrm`, or any `.plyr` document).
    pub path: String,
    /// Replace materials of the same name instead of skipping them (false).
    #[serde(default)]
    pub replace: bool,
}

/// The `material-library-load` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterialLibraryLoad;

impl Command for MaterialLibraryLoad {
    type Params = MaterialLibraryLoadParams;
    type Output = MaterialLibraryOutput;

    fn name(&self) -> &'static str {
        "material-library-load"
    }

    fn validate(&self, _doc: &Document, p: &MaterialLibraryLoadParams) -> Result<(), CommandError> {
        check_path(&p.path)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MaterialLibraryLoadParams,
    ) -> Result<MaterialLibraryOutput, CommandError> {
        let bytes = storage(tx, &p.path)?.read(&StorageKey::new(&p.path))?;
        let library = read_document(&bytes, &NoProgress)?.document;
        let mut out = MaterialLibraryOutput {
            materials: Vec::new(),
            skipped: Vec::new(),
        };
        // The container's own default materials are not part of the library.
        for (_, m) in library
            .materials
            .iter()
            .filter(|(_, m)| m.is_default_for.is_none())
        {
            let mut m = m.clone();
            if let Some(t) = &mut m.texture {
                let data = library.images.get(t.image).ok_or_else(|| {
                    CommandError::Io(IoError::Corrupt {
                        part: p.path.clone(),
                        reason: format!("the texture of `{}` is missing", m.name),
                    })
                })?;
                t.image = tx.insert_image(data.bytes.clone())?;
            }
            let existing = tx.document().materials.by_name(&m.name);
            let is_default = existing
                .and_then(|id| tx.document().materials.get(id))
                .is_some_and(|old| old.is_default_for.is_some());
            match existing {
                Some(id) if p.replace && !is_default => {
                    out.materials.push(m.name.clone());
                    tx.replace_material(id, m)?;
                }
                Some(_) => out.skipped.push(m.name.clone()),
                None => {
                    out.materials.push(m.name.clone());
                    tx.insert_material(m)?;
                }
            }
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("material-library-load", "Load Material Library", false)
    }
}
