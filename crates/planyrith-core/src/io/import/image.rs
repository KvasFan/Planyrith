//! Implements docs/09-file-formats.md §5 image import (PNG, JPEG, WebP): an
//! image entity lying on the ground at `at`, 1 m wide times `scale`
//! (`image-insert`'s defaults), or with `as: texture` a material carrying
//! the image as its texture.

use std::io::Read;

use glam::DVec3;

use crate::commands::Transaction;
use crate::commands::organize::images::{place_image, texture_material};
use crate::commands::params::Target;
use crate::io::import::build::io;
use crate::io::import::{
    Confidence, ImportAs, ImportFormat, ImportOptions, ImportReport, Importer, source_stem,
};
use crate::io::{IoError, Progress};

/// The image importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImageImporter;

impl Importer for ImageImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Image
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let magic = bytes.starts_with(b"\x89PNG")
            || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
            || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"));
        let lower = name.to_ascii_lowercase();
        if magic {
            Confidence::CERTAIN
        } else if [".png", ".jpg", ".jpeg", ".webp"]
            .iter()
            .any(|e| lower.ends_with(e))
        {
            Confidence::EXTENSION
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
            part: "image".into(),
            reason: e.to_string(),
        })?;
        progress.report(0.5, "placing the image");
        let stem = source_stem(&opts.source, "Image");
        let width = 1000.0 * opts.scale;
        let mut report = ImportReport::default();
        match &opts.import_as {
            ImportAs::Texture => {
                texture_material(tx, bytes, &stem, (Some(width), None)).map_err(io)?;
            }
            ImportAs::Group | ImportAs::Image => {
                let (pid, _) = place_image(
                    tx,
                    target,
                    bytes,
                    &stem,
                    opts.at,
                    (Some(width), None),
                    DVec3::Z,
                )
                .map_err(io)?;
                report.created.push(pid);
            }
            ImportAs::Component(_) => {
                return Err(IoError::UnsupportedFormat(
                    "an image imports as `image` or `texture`, not as a component".into(),
                ));
            }
        }
        Ok(report)
    }
}
