//! `planyrith infer` (docs/08 §12): the inference engine of docs/03 §7 for
//! one ray, headless, for golden tests and scripts.

use std::time::Instant;

use glam::DVec3;
use planyrith_core::commands::params::{CliType, parse_cli_value};
use planyrith_core::document::EntityPath;
use planyrith_core::ids::EntityId;
use planyrith_core::inference::{
    Constraint, Inference, InferenceEngine, InferenceRequest, Lock, ViewMetrics,
};
use planyrith_core::math::{Axis, Plane, Ray};
use planyrith_core::spatial::DocumentIndex;
use planyrith_core::units::LengthUnit;
use serde_json::{Value, json};

use crate::session::Session;
use crate::{CliError, Global, out};

/// `infer` options.
#[derive(Debug, Clone, clap::Args)]
pub struct InferArgs {
    /// Ray origin (the eye).
    #[arg(long, value_name = "X,Y,Z", allow_hyphen_values = true)]
    pub from: String,
    /// Ray direction.
    #[arg(long, value_name = "DX,DY,DZ", allow_hyphen_values = true)]
    pub dir: String,
    /// Remember an entity (an edge gives parallel/perpendicular, any entity a
    /// "from point"); repeatable, at most three are kept.
    #[arg(long = "ref", value_name = "PATH")]
    pub refs: Vec<String>,
    /// Reference point: directions (axes, parallel…) start here.
    #[arg(long, value_name = "X,Y,Z", allow_hyphen_values = true)]
    pub reference: Option<String>,
    /// Last placed point (depth fallback when the ray misses the ground).
    #[arg(long, value_name = "X,Y,Z", allow_hyphen_values = true)]
    pub last: Option<String>,
    /// Constrain to the plane through the reference point (or the origin)
    /// with this normal.
    #[arg(long, value_name = "NX,NY,NZ", allow_hyphen_values = true)]
    pub plane: Option<String>,
    /// Lock to an axis through the reference point.
    #[arg(long, value_parser = ["x", "y", "z"])]
    pub lock: Option<String>,
    /// Snapping distance at the cursor.
    #[arg(long, default_value = "10mm")]
    pub tolerance: String,
    /// Print every candidate, best first.
    #[arg(long)]
    pub all: bool,
}

fn point(text: &str, unit: LengthUnit, param: &str) -> Result<DVec3, CliError> {
    let json =
        parse_cli_value(CliType::Point, param, text, unit).map_err(CliError::from_command)?;
    serde_json::from_value::<[f64; 3]>(json)
        .map(DVec3::from)
        .map_err(|e| CliError::invalid(param, e.to_string()))
}

fn vector(text: &str, param: &str) -> Result<DVec3, CliError> {
    let json = parse_cli_value(CliType::Vector, param, text, LengthUnit::Millimeter)
        .map_err(CliError::from_command)?;
    let v = serde_json::from_value::<[f64; 3]>(json)
        .map(DVec3::from)
        .map_err(|e| CliError::invalid(param, e.to_string()))?;
    v.try_normalize()
        .ok_or_else(|| CliError::invalid(param, "must not be zero"))
}

/// Runs the verb.
pub fn infer(global: &Global, args: &InferArgs) -> Result<(), CliError> {
    let started = Instant::now();
    let session = Session::open(global, false)?;
    let doc = session.engine.document();
    let unit = session.unit(global)?;
    let ray = Ray::new(point(&args.from, unit, "from")?, vector(&args.dir, "dir")?);
    let tolerance = parse_cli_value(CliType::Length, "tolerance", &args.tolerance, unit)
        .map_err(CliError::from_command)?
        .as_f64()
        .unwrap_or(10.0);
    let reference_point = args
        .reference
        .as_deref()
        .map(|t| point(t, unit, "reference"))
        .transpose()?;
    let last_point = args
        .last
        .as_deref()
        .map(|t| point(t, unit, "last"))
        .transpose()?;
    let constraint = match &args.plane {
        Some(text) => Constraint::Plane(Plane::from_point_normal(
            reference_point.unwrap_or(DVec3::ZERO),
            vector(text, "plane")?,
        )),
        None => Constraint::Free,
    };
    let req = InferenceRequest {
        ray,
        pixel_tolerance: InferenceRequest::DEFAULT_PIXEL_TOLERANCE,
        view: ViewMetrics::fixed(tolerance, InferenceRequest::DEFAULT_PIXEL_TOLERANCE),
        constraint,
        reference_point,
        last_point,
    };
    let mut engine = InferenceEngine::new();
    for text in &args.refs {
        let path = EntityPath::parse(text, doc)
            .map_err(|e| CliError::invalid("ref", format!("{text}: {e}")))?;
        let resolved = doc
            .resolve(&path)
            .map_err(|e| CliError::invalid("ref", format!("{text}: {e}")))?;
        let entities = doc
            .container(resolved.container)
            .ok_or_else(|| CliError::invalid("ref", format!("{text}: no container")))?;
        let (point, direction) = match resolved.entity {
            EntityId::Edge(e) => {
                let seg = entities
                    .mesh()
                    .edge_segment(e)
                    .map_err(|err| CliError::invalid("ref", err.to_string()))?
                    .transformed(&resolved.world);
                (seg.midpoint(), Some(seg.direction()))
            }
            EntityId::Face(f) => {
                let bounds = entities
                    .mesh()
                    .face_aabb(f)
                    .map_err(|err| CliError::invalid("ref", err.to_string()))?
                    .transformed(&resolved.world);
                (bounds.center(), None)
            }
            _ => (
                doc.occurrence_aabb(&path)
                    .map_or(DVec3::ZERO, |b| b.center()),
                None,
            ),
        };
        engine.add_reference(path, point, direction);
    }
    let index = DocumentIndex::build(doc);
    if let Some(axis) = &args.lock {
        let axis = match axis.as_str() {
            "x" => Axis::X,
            "y" => Axis::Y,
            _ => Axis::Z,
        };
        let current = engine.infer(doc, &index, &req);
        engine.engage_lock(Lock::Axis(axis), &current, &req);
    }
    let describe = |i: &Inference| -> Value {
        json!({
            "kind": i.kind,
            "label": i.label,
            "point": i.point.to_array(),
            "on": i.on.as_ref().map(|p| p.canonical(doc).display_with(doc)),
            "axis": i.axis.map(|a| match a { Axis::X => "x", Axis::Y => "y", Axis::Z => "z" }),
            "from": i.from.map(|p| p.to_array()),
            "locked": i.locked,
        })
    };
    let output = if args.all {
        Value::Array(
            engine
                .candidates(doc, &index, &req)
                .iter()
                .map(describe)
                .collect(),
        )
    } else {
        describe(&engine.infer(doc, &index, &req))
    };
    if global.json || global.jsonl {
        out::line(json!({
            "ok": true,
            "command": "infer",
            "output": output,
            "warnings": Value::Array(Vec::new()),
            "timing_ms": started.elapsed().as_secs_f64() * 1000.0,
        }))
    } else {
        let line = |v: &Value| {
            let p = &v["point"];
            format!(
                "{:<14} ({}, {}, {}){}",
                v["label"]
                    .as_str()
                    .filter(|l| !l.is_empty())
                    .unwrap_or("Free"),
                p[0],
                p[1],
                p[2],
                v["on"]
                    .as_str()
                    .map(|o| format!(" on {o}"))
                    .unwrap_or_default()
            )
        };
        match &output {
            Value::Array(list) => {
                for v in list {
                    out::line(line(v))?;
                }
                Ok(())
            }
            v => out::line(line(v)),
        }
    }
}
