//! Implements docs/09-file-formats.md §5 `.plyr`/`.plyrc` merge: another
//! document's model becomes a group (or component) of this one, with the
//! definitions, materials, images and tags it uses — copied the way
//! `component-insert` copies a component file (docs/09 Decisions: shared
//! UUIDs reuse definitions, materials and tags match by name). Scenes,
//! styles and the camera of the other document are not merged.

use std::io::Read;

use crate::commands::Transaction;
use crate::commands::organize::components::{Imports, import_definition};
use crate::commands::params::Target;
use crate::document::{ComponentDefinition, ContainerRef};
use crate::io::import::build::{io, place, top_name};
use crate::io::import::{
    Confidence, ImportAs, ImportFormat, ImportOptions, ImportReport, Importer, source_stem,
};
use crate::io::native::read_document;
use crate::io::{IoError, NoProgress, Progress};

/// The native-document importer (merge).
#[derive(Debug, Default, Clone, Copy)]
pub struct PlyrImporter;

impl Importer for PlyrImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Plyr
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".plyr") || lower.ends_with(".plyrc") {
            Confidence::EXTENSION
        } else if bytes.starts_with(b"PK") {
            Confidence(10)
        } else {
            Confidence::NONE
        }
    }

    fn import(
        &self,
        src: &mut dyn Read,
        opts: &ImportOptions,
        tx: &mut Transaction<'_>,
        target: &Target,
        progress: &dyn Progress,
    ) -> Result<ImportReport, IoError> {
        let mut bytes = Vec::new();
        src.read_to_end(&mut bytes).map_err(|e| IoError::Corrupt {
            part: "plyr".into(),
            reason: e.to_string(),
        })?;
        let read = read_document(&bytes, &NoProgress)?;
        let mut report = ImportReport {
            warnings: read.warnings,
            ..ImportReport::default()
        };
        let mut source = read.document;
        progress.report(0.4, "merging");
        // The other model, wrapped as a definition of the source document.
        let name = top_name(tx.document(), opts);
        let mut wrapper = ComponentDefinition::new(uuid::Uuid::nil(), name.clone());
        wrapper.is_internal_group = !matches!(opts.import_as, ImportAs::Component(_));
        wrapper.entities = std::mem::take(&mut source.model);
        let wrapped = source.add_definition(wrapper);
        let mut imports = Imports::default();
        let top = import_definition(tx, &source, wrapped, true, &mut imports).map_err(io)?;
        // Faces and edges of everything copied.
        for d in imports.copied() {
            if let Some(entities) = tx.document().container(ContainerRef::Definition(d)) {
                report.faces += entities.mesh().face_count();
                report.edges += entities.mesh().edge_count();
            }
        }
        let pid = place(tx, target, top, opts, &source_stem(&opts.source, "Model"))?;
        report.created.push(pid);
        Ok(report)
    }
}
