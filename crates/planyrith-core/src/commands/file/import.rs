//! Implements docs/05-modeling-tools.md §10 `import { path, format?, options }`
//! and docs/08-cli-specification.md §2 `planyrith import`: reads a file
//! through the engine's storage and builds it into the edit context as a
//! group (default) or component with the format's importer (docs/09 §5–§6).

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::organize::libraries::storage;
use crate::commands::params::{PointSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::Document;
use crate::io::import::{ImportAs, ImportFormat, ImportOptions, ImportReport, importer};
use crate::io::{IoError, NoProgress, StorageKey};
use crate::units::{Angle, LengthUnit};

fn yes() -> bool {
    true
}

fn one() -> f64 {
    1.0
}

fn twenty_degrees() -> Angle {
    Angle::degrees(20.0)
}

/// `import` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImportParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// File to import.
    pub path: String,
    /// Format (from the extension, else from the contents).
    #[serde(default)]
    pub format: Option<ImportFormat>,
    /// `group` (default), `component:<name>` or `image`.
    #[serde(default, rename = "as")]
    pub import_as: Option<String>,
    /// Unit of the file (`mm`, `cm`, `m`, `in`, `ft`; the file's own, else
    /// mm).
    #[serde(default)]
    pub file_units: Option<String>,
    /// Extra scale factor (1).
    #[serde(default = "one")]
    pub scale: f64,
    /// Turn Y-up into Z-up; absent: the format decides (OBJ and STL are
    /// Y-up, glTF says so itself).
    #[serde(default)]
    pub swap_yz: Option<bool>,
    /// Merge coplanar adjacent triangles into faces (true).
    #[serde(default = "yes")]
    pub merge_coplanar: bool,
    /// Edges between faces turning less than this become soft (20°).
    #[serde(default = "twenty_degrees")]
    pub soften: Angle,
    /// Where the file's origin goes (world, mm; the origin).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub at: Option<DVec3>,
}

/// `import` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportOutput {
    /// The format read.
    pub format: ImportFormat,
    /// What was made.
    #[serde(flatten)]
    pub report: ImportReport,
}

/// The `import` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Import;

fn import_as(p: &ImportParams) -> Result<ImportAs, CommandError> {
    match p.import_as.as_deref().map(str::trim) {
        None | Some("group") => Ok(ImportAs::Group),
        Some("image") => Ok(ImportAs::Image),
        Some(other) => match other.strip_prefix("component:").map(str::trim) {
            Some(name) if !name.is_empty() => Ok(ImportAs::Component(name.to_string())),
            _ => Err(CommandError::invalid_param(
                "as",
                format!("`{other}`: expected group, component:<name> or image"),
            )),
        },
    }
}

fn units(p: &ImportParams) -> Result<Option<LengthUnit>, CommandError> {
    p.file_units
        .as_deref()
        .map(|u| {
            u.parse().map_err(|_| {
                CommandError::invalid_param("file_units", format!("unknown unit `{u}`"))
            })
        })
        .transpose()
}

fn extension(path: &str) -> Option<&str> {
    let file = path.rsplit(['/', '\\']).next()?;
    file.rsplit_once('.').map(|(_, ext)| ext)
}

impl Command for Import {
    type Params = ImportParams;
    type Output = ImportOutput;

    fn name(&self) -> &'static str {
        "import"
    }

    fn validate(&self, _doc: &Document, p: &ImportParams) -> Result<(), CommandError> {
        if p.path.trim().is_empty() {
            return Err(CommandError::invalid_param("path", "must not be empty"));
        }
        import_as(p)?;
        units(p)?;
        if !(p.scale > 0.0 && p.scale.is_finite()) {
            return Err(CommandError::OutOfRange {
                name: "scale".into(),
                reason: "must be a positive number".into(),
            });
        }
        let soften = p.soften.to_degrees();
        if !(0.0..=180.0).contains(&soften) {
            return Err(CommandError::OutOfRange {
                name: "soften".into(),
                reason: "must be between 0° and 180°".into(),
            });
        }
        if let Some(at) = p.at {
            check_point("at", at)?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ImportParams,
    ) -> Result<ImportOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let bytes = storage(tx, &p.path)?.read(&StorageKey::new(&p.path))?;
        let format = p
            .format
            .or_else(|| extension(&p.path).and_then(ImportFormat::from_extension))
            .ok_or_else(|| {
                CommandError::Io(IoError::UnsupportedFormat(format!(
                    "cannot tell the format of `{}`; pass `format`",
                    p.path
                )))
            })?;
        let importer = importer(format).ok_or_else(|| {
            CommandError::Io(IoError::UnsupportedFormat(format!(
                "importing {format:?} files"
            )))
        })?;
        let opts = ImportOptions {
            units: units(&p)?,
            scale: p.scale,
            swap_yz: p.swap_yz,
            merge_coplanar: p.merge_coplanar,
            soften_angle: p.soften,
            import_as: import_as(&p)?,
            at: p.at.unwrap_or(DVec3::ZERO),
            source: p.path.clone(),
        };
        let report = importer.import(&mut bytes.as_slice(), &opts, tx, &target, &NoProgress)?;
        Ok(ImportOutput { format, report })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "import".into(),
            category: CommandCategory::File,
            label: "Import".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}
