//! Implements docs/05-modeling-tools.md §10 `export { path, format, options }`
//! and docs/08-cli-specification.md §2 `planyrith export` for the 3D mesh
//! formats (docs/09 §5–§6): the visible model, or the occurrences a query
//! names, written through the engine's storage. It changes nothing in the
//! document, so it is not undoable.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::organize::libraries::storage;
use crate::commands::params::{command_context, select};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::selection::occurrence_name;
use crate::document::{
    ContainerRef, Document, EntityKey, EntityPath, InstancePath, Query, Selection,
};
use crate::ids::EntityId;
use crate::io::export::scene::collect;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, exporter};
use crate::io::import::sibling;
use crate::io::{IoError, NoProgress, StorageKey};
use std::sync::Arc;

use serde_json::json;

use crate::commands::{CommandRegistry, Engine};
use crate::units::{Length, LengthUnit};

/// `export` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportParams {
    /// SKP file version: 2017 (default) or 2026; requires the optional converter.
    #[serde(default)]
    pub skp_version: Option<u32>,
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// File to write.
    pub path: String,
    /// Format (from the extension).
    #[serde(default)]
    pub format: Option<ExportFormat>,
    /// Only the occurrences these entities belong to (the whole model).
    #[serde(default)]
    pub targets: Option<Query>,
    /// Include hidden entities and tags.
    #[serde(default)]
    pub include_hidden: bool,
    /// Unit of unitless formats (`mm`, `cm`, `m`, `in`, `ft`; mm).
    #[serde(default)]
    pub file_units: Option<String>,
    /// ASCII STL instead of binary.
    #[serde(default)]
    pub ascii: bool,
    /// Keep Z up in formats that are Y-up by default (OBJ, STL).
    #[serde(default)]
    pub z_up: bool,
    /// A 2D drawing of the view, hidden lines removed (DXF; SVG and PDF
    /// always are).
    #[serde(default)]
    pub two_d: bool,
    /// Draw this scene's view (its camera, tags, hidden entities…) instead
    /// of the current one.
    #[serde(default)]
    pub scene: Option<String>,
    /// Paper scale of a parallel view, `1:50` (else the view fits the
    /// paper width).
    #[serde(default)]
    pub scale: Option<String>,
    /// Paper width of a fitted view (297 mm).
    #[serde(default)]
    pub paper_width: Option<Length>,
    /// Write each top-level group or component (and the loose geometry)
    /// to its own file, named `<file>-<object>.<ext>` (STL, OBJ, glTF,
    /// GLB, 3MF; not with `targets`).
    #[serde(default)]
    pub per_object: bool,
    /// Lift what each file holds so its lowest point is at Z = 0, the
    /// build plate (STL, OBJ, glTF, GLB, 3MF).
    #[serde(default)]
    pub on_plate: bool,
}

/// `export` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportOutput {
    /// The format written.
    pub format: ExportFormat,
    /// What was written (summed over the files).
    #[serde(flatten)]
    pub report: ExportReport,
    /// The files written, when `per_object` split the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// The `export` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Export;

fn units(p: &ExportParams) -> Result<LengthUnit, CommandError> {
    p.file_units
        .as_deref()
        .map_or(Ok(LengthUnit::Millimeter), |u| {
            u.parse().map_err(|_| {
                CommandError::invalid_param("file_units", format!("unknown unit `{u}`"))
            })
        })
}

/// `1:50`, `1/50` or a plain factor → paper per model length.
fn paper_scale(p: &ExportParams) -> Result<Option<f64>, CommandError> {
    let Some(text) = p.scale.as_deref().map(str::trim) else {
        return Ok(None);
    };
    let bad = || CommandError::invalid_param("scale", format!("`{text}`: expected 1:50"));
    let value = match text.split_once([':', '/']) {
        Some((a, b)) => {
            let (a, b): (f64, f64) = (
                a.trim().parse().map_err(|_| bad())?,
                b.trim().parse().map_err(|_| bad())?,
            );
            a / b
        }
        None => text.parse::<f64>().map_err(|_| bad())?,
    };
    if value.is_finite() && value > 0.0 {
        Ok(Some(value))
    } else {
        Err(bad())
    }
}

fn format(p: &ExportParams) -> Result<ExportFormat, CommandError> {
    p.format
        .or_else(|| {
            p.path
                .rsplit(['/', '\\'])
                .next()
                .and_then(|f| f.rsplit_once('.'))
                .and_then(|(_, ext)| ExportFormat::from_extension(ext))
        })
        .ok_or_else(|| {
            CommandError::Io(IoError::UnsupportedFormat(format!(
                "cannot tell the format of `{}`; pass `format`",
                p.path
            )))
        })
}

/// `path` with `-<name>` before its extension.
fn object_path(path: &str, name: &str) -> String {
    let file_start = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let dot = path[file_start..].rfind('.').map(|i| file_start + i);
    let (stem, ext) = dot.map_or((path, ""), |i| (&path[..i], &path[i..]));
    format!("{stem}-{name}{ext}")
}

/// A name usable in a file name: letters, digits, `-` and `_`.
fn file_safe(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "object".into() } else { out }
}

/// One export per top-level group or instance with faces (it and what it
/// holds), and one for the loose faces of the model, each with its file.
fn per_object(doc: &Document, path: &str, include_hidden: bool) -> Vec<(String, ExportScope)> {
    let everything = collect(
        doc,
        &ExportScope {
            include_hidden,
            ..ExportScope::default()
        },
    );
    let mut tops: Vec<EntityKey> = Vec::new();
    let mut loose = false;
    for occ in &everything.occurrences {
        if everything
            .mesh(occ.container)
            .is_none_or(|m| m.faces.is_empty())
        {
            continue;
        }
        match occ.path.steps.first() {
            Some(step) if !tops.contains(step) => tops.push(*step),
            Some(_) => {}
            None => loose = true,
        }
    }
    let mut used: Vec<String> = Vec::new();
    let mut unique = |name: String| {
        let base = file_safe(&name);
        let name = std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base}-{n}")))
            .find(|n| !used.contains(n))
            .unwrap_or(base);
        used.push(name.clone());
        object_path(path, &name)
    };
    let mut jobs = Vec::new();
    if loose && let Some(model) = doc.container(ContainerRef::Model) {
        let mut selection = Selection::new();
        for &id in model.order() {
            if let (EntityId::Face(_), Some(pid)) = (id, model.pid_of(id)) {
                selection.insert(EntityPath::new(InstancePath::root(), EntityKey::Pid(pid)));
            }
        }
        jobs.push((
            unique("Model".into()),
            ExportScope {
                selection: Some(selection),
                scene: None,
                include_hidden,
            },
        ));
    }
    for key in tops {
        let name = occurrence_name(doc, &InstancePath::root().child(key));
        jobs.push((
            unique(name),
            ExportScope {
                selection: Some(Selection::single(EntityPath::new(
                    InstancePath::root(),
                    key,
                ))),
                scene: None,
                include_hidden,
            },
        ));
    }
    jobs
}

/// The shift that puts the lowest exported point at Z = 0.
fn plate_offset(doc: &Document, scope: &ExportScope) -> [f64; 3] {
    let scene = collect(doc, scope);
    let mut low = f64::INFINITY;
    for occ in &scene.occurrences {
        for face in scene.mesh(occ.container).iter().flat_map(|m| &m.faces) {
            for p in &face.positions {
                low = low.min(occ.world.transform_point3(*p).z);
            }
        }
    }
    if low.is_finite() {
        [0.0, 0.0, -low]
    } else {
        [0.0; 3]
    }
}

impl Command for Export {
    type Params = ExportParams;
    type Output = ExportOutput;

    fn name(&self) -> &'static str {
        "export"
    }

    fn validate(&self, _doc: &Document, p: &ExportParams) -> Result<(), CommandError> {
        if p.path.trim().is_empty() {
            return Err(CommandError::invalid_param("path", "must not be empty"));
        }
        units(p)?;
        format(p)?;
        if p.skp_version.is_some_and(|v| !matches!(v, 2017 | 2026)) {
            return Err(CommandError::invalid_param(
                "skp_version",
                "must be 2017 or 2026",
            ));
        }
        paper_scale(p)?;
        let mesh = matches!(
            format(p)?,
            ExportFormat::Stl
                | ExportFormat::Obj
                | ExportFormat::Gltf
                | ExportFormat::Glb
                | ExportFormat::ThreeMf
                | ExportFormat::Ply
                | ExportFormat::Usda
                | ExportFormat::Usdz
        );
        for (on, name) in [(p.per_object, "per_object"), (p.on_plate, "on_plate")] {
            if on && (!mesh || p.two_d) {
                return Err(CommandError::invalid_param(
                    name,
                    "only for STL, OBJ, glTF, GLB, 3MF, PLY and USD models",
                ));
            }
        }
        if p.per_object && p.targets.is_some() {
            return Err(CommandError::invalid_param(
                "per_object",
                "cannot be combined with targets",
            ));
        }
        if let Some(w) = p.paper_width
            && !(w.as_mm() > 0.0 && w.as_mm().is_finite())
        {
            return Err(CommandError::OutOfRange {
                name: "paper_width".into(),
                reason: "must be a positive length".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ExportParams,
    ) -> Result<ExportOutput, CommandError> {
        let format = format(&p)?;
        let exporter = exporter(format).ok_or_else(|| {
            CommandError::Io(IoError::UnsupportedFormat(format!(
                "exporting {format:?} files"
            )))
        })?;
        let selection = match &p.targets {
            Some(q) => Some(select(tx, &command_context(tx, &p.common), "targets", q)?),
            None => None,
        };
        let scope = ExportScope {
            selection,
            scene: p.scene.clone(),
            include_hidden: p.include_hidden,
        };
        let opts = ExportOptions {
            skp_version: p.skp_version,
            units: units(&p)?,
            ascii: p.ascii,
            z_up: p.z_up,
            destination: p.path.clone(),
            two_d: p.two_d,
            scale: paper_scale(&p)?,
            paper_width_mm: p.paper_width.map(Length::as_mm),
            ..ExportOptions::default()
        };
        // A scene applies to a copy, as `scene-activate` would.
        let scened;
        let doc = match &p.scene {
            Some(name) => {
                let mut copy = Engine::new(
                    tx.document().clone(),
                    Arc::new(CommandRegistry::with_builtins()),
                );
                copy.execute_erased("scene-activate", json!({ "scene": name }))
                    .map_err(|e| match e {
                        CommandError::InvalidParam { reason, .. } => {
                            CommandError::invalid_param("scene", reason)
                        }
                        other => other,
                    })?;
                scened = copy.document().clone();
                &scened
            }
            None => tx.document(),
        };
        let jobs = if p.per_object {
            per_object(doc, &p.path, p.include_hidden)
        } else {
            vec![(p.path.clone(), scope)]
        };
        let storage = storage(tx, &p.path)?;
        let mut total = ExportReport::default();
        let mut files = Vec::new();
        for (path, scope) in jobs {
            let mut opts = opts.clone();
            opts.destination.clone_from(&path);
            if p.on_plate {
                opts.offset_mm = plate_offset(doc, &scope);
            }
            let mut bytes = Vec::new();
            let report = exporter.export_with_storage(
                doc,
                &scope,
                &opts,
                &mut bytes,
                &NoProgress,
                storage,
            )?;
            storage.write(&StorageKey::new(&path), &bytes)?;
            for (name, contents) in &report.side_files {
                storage.write(&StorageKey::new(sibling(&path, name)), contents)?;
            }
            total.bytes_written += report.bytes_written;
            total.entities += report.entities;
            total.warnings.extend(report.warnings);
            if p.per_object {
                files.push(path);
            }
        }
        if format == ExportFormat::Skp {
            for warning in &total.warnings {
                tx.warn(warning.clone());
            }
        }
        Ok(ExportOutput {
            format,
            report: total,
            files,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "export".into(),
            category: CommandCategory::File,
            label: "Export".into(),
            view_only: true,
            steps: Vec::new(),
        }
    }
}
