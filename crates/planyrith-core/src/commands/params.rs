//! Implements the parameter types of docs/05-modeling-tools.md §1 and their CLI
//! text forms (docs/08-cli-specification.md §2, type table), plus the helpers
//! commands share to resolve their edit context, entity references and
//! selections.
//!
//! Frontends recognise semantic types by the schema name a field references
//! (`#/$defs/<Name>`): `Length`, `Angle`, `Query` and `EntityRef` carry their
//! own; `DVec3`/`DAffine3` fields point at the marker schemas below with
//! `#[schemars(with = "…")]`.

#![warn(unused_variables, dead_code)]

use std::borrow::Cow;

use glam::{DAffine3, DVec3};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

use crate::commands::{CommandError, CommonParams, Transaction};
use crate::document::{
    ContainerRef, Document, EditContext, EntityKey, EntityPath, EntityRef, InstancePath, Query,
    QueryEnv, QueryExpr, Selection,
};
use crate::ids::{EntityId, Pid};
use crate::math::{Plane, TransformParts};
use crate::units::{LengthUnit, parse_angle, parse_length};

/// Schema marker for `Point` fields (`DVec3`, millimeters): `[x, y, z]`.
pub struct PointSchema;

impl JsonSchema for PointSchema {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Point")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "array",
            "description": "A point `[x, y, z]` in millimeters (CLI: `x,y,z` with units).",
            "items": { "type": "number" },
            "minItems": 3,
            "maxItems": 3
        })
    }
}

/// Schema marker for `Vector` fields (`DVec3`): a direction or displacement.
pub struct VectorSchema;

impl JsonSchema for VectorSchema {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Vector")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "array",
            "description": "A vector `[dx, dy, dz]` in millimeters (CLI: `dx,dy,dz` with units, or `x|y|z|-x|-y|-z`).",
            "items": { "type": "number" },
            "minItems": 3,
            "maxItems": 3
        })
    }
}

/// Schema marker for `Transform` fields (`DAffine3`).
pub struct TransformSchema;

impl JsonSchema for TransformSchema {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Transform")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "array",
            "description": "An affine transform as 12 numbers, column-major 3×4 (CLI: `t=x,y,z;r=axis,angle;s=sx,sy,sz`).",
            "items": { "type": "number" },
            "minItems": 12,
            "maxItems": 12
        })
    }
}

/// The CLI parameter types of docs/08 §2 that need special parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliType {
    /// `Length`: number with optional unit.
    Length,
    /// `Angle`: number with `deg`/`rad`, default degrees.
    Angle,
    /// `Point`: `x,y,z` with units.
    Point,
    /// `Vector`: `dx,dy,dz` or an axis name.
    Vector,
    /// `Plane`: `xy|yz|xz[@offset]` or `origin;normal`.
    Plane,
    /// `Transform`: `t=…;r=…;s=…`.
    Transform,
    /// `Sel`: a query string.
    Query,
    /// `EntityRef`: `@pid`, `#path` or `{query}`.
    EntityRef,
}

impl CliType {
    /// The type a schema definition name denotes, if any.
    pub fn from_schema_name(name: &str) -> Option<Self> {
        Some(match name {
            "Length" => CliType::Length,
            "Angle" => CliType::Angle,
            "Point" => CliType::Point,
            "Vector" => CliType::Vector,
            "Plane" => CliType::Plane,
            "Transform" => CliType::Transform,
            "Query" => CliType::Query,
            "EntityRef" => CliType::EntityRef,
            _ => return None,
        })
    }
}

fn bad(param: &str, text: &str, why: impl std::fmt::Display) -> CommandError {
    CommandError::invalid_param(param, format!("`{text}`: {why}"))
}

/// Parses `x,y,z` lengths.
fn parse_triple(param: &str, text: &str, unit: LengthUnit) -> Result<DVec3, CommandError> {
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    let [x, y, z] = parts[..] else {
        return Err(bad(param, text, "expected three comma-separated values"));
    };
    let len = |s: &str| {
        parse_length(s, unit)
            .map(|l| l.as_mm())
            .map_err(|e| bad(param, text, e))
    };
    Ok(DVec3::new(len(x)?, len(y)?, len(z)?))
}

/// An axis name (`x`, `-y`, …) as a unit vector.
fn parse_axis(text: &str) -> Option<DVec3> {
    Some(match text.trim().to_ascii_lowercase().as_str() {
        "x" | "+x" => DVec3::X,
        "y" | "+y" => DVec3::Y,
        "z" | "+z" => DVec3::Z,
        "-x" => -DVec3::X,
        "-y" => -DVec3::Y,
        "-z" => -DVec3::Z,
        _ => return None,
    })
}

fn vec_json(v: DVec3) -> serde_json::Value {
    serde_json::json!([v.x, v.y, v.z])
}

/// Parses CLI text into the JSON value of a parameter of type `ty`
/// (docs/08 §2 table). `param` names the option in errors; `unit` is the unit
/// of bare lengths (`--units` or the document's display unit).
pub fn parse_cli_value(
    ty: CliType,
    param: &str,
    text: &str,
    unit: LengthUnit,
) -> Result<serde_json::Value, CommandError> {
    let text = text.trim();
    Ok(match ty {
        CliType::Length => serde_json::json!(
            parse_length(text, unit)
                .map_err(|e| bad(param, text, e))?
                .as_mm()
        ),
        CliType::Angle => serde_json::json!(
            parse_angle(text)
                .map_err(|e| bad(param, text, e))?
                .as_radians()
        ),
        CliType::Point => {
            if text.starts_with('@') {
                return Err(bad(
                    param,
                    text,
                    "vertices have no pid; give the point as x,y,z (docs/08 Decisions)",
                ));
            }
            vec_json(parse_triple(param, text, unit)?)
        }
        CliType::Vector => match parse_axis(text) {
            Some(axis) => vec_json(axis),
            None => vec_json(parse_triple(param, text, unit)?),
        },
        CliType::Plane => serde_json::to_value(parse_plane(param, text, unit)?)
            .map_err(|e| CommandError::Internal(e.to_string()))?,
        CliType::Transform => {
            let parts =
                TransformParts::parse_with_unit(text, unit).map_err(|e| bad(param, text, e))?;
            serde_json::to_value(parts.to_affine())
                .map_err(|e| CommandError::Internal(e.to_string()))?
        }
        CliType::Query => {
            Query::new(text).parse().map_err(|e| bad(param, text, e))?;
            serde_json::json!(text)
        }
        CliType::EntityRef => serde_json::to_value(parse_entity_ref(param, text)?)
            .map_err(|e| CommandError::Internal(e.to_string()))?,
    })
}

/// `xy|yz|xz[@offset]` (offset along the plane normal) or `origin;normal`.
fn parse_plane(param: &str, text: &str, unit: LengthUnit) -> Result<Plane, CommandError> {
    if let Some((origin, normal)) = text.split_once(';') {
        let origin = parse_triple(param, origin, unit)?;
        let normal = match parse_axis(normal) {
            Some(n) => n,
            None => parse_triple(param, normal, unit)?,
        };
        if normal.length_squared() == 0.0 {
            return Err(bad(param, text, "the normal must not be zero"));
        }
        return Ok(Plane::from_point_normal(origin, normal));
    }
    let (name, offset) = match text.split_once('@') {
        Some((n, o)) => (
            n,
            parse_length(o, unit)
                .map_err(|e| bad(param, text, e))?
                .as_mm(),
        ),
        None => (text, 0.0),
    };
    let normal = match name.trim().to_ascii_lowercase().as_str() {
        "xy" => DVec3::Z,
        "yz" => DVec3::X,
        "xz" => DVec3::Y,
        _ => return Err(bad(param, text, "expected xy, yz, xz or origin;normal")),
    };
    Ok(Plane::from_point_normal(normal * offset, normal))
}

/// `@pid`, `#path` (kept as a query atom) or `{query}`.
fn parse_entity_ref(param: &str, text: &str) -> Result<EntityRef, CommandError> {
    if let Some(digits) = text.strip_prefix('@') {
        let pid: u64 = digits
            .parse()
            .map_err(|_| bad(param, text, "expected @<pid>"))?;
        return Ok(EntityRef::Pid(Pid(pid)));
    }
    let source = match text.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
        Some(inner) => inner,
        None if text.starts_with('#') => text,
        None => return Err(bad(param, text, "expected @pid, #path or {query}")),
    };
    Query::new(source)
        .parse()
        .map_err(|e| bad(param, text, e))?;
    Ok(EntityRef::Query(Query::new(source)))
}

// ----- resolution helpers for commands ---------------------------------------------------

/// The container a command writes into and its container-to-world transform.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// The edit context of the command (queries evaluate inside it).
    pub context: EditContext,
    /// Container.
    pub container: ContainerRef,
    /// Container-to-world transform.
    pub world: DAffine3,
    /// World-to-container transform.
    pub local: DAffine3,
}

impl Target {
    /// A world point in container coordinates.
    pub fn to_local(&self, p: DVec3) -> DVec3 {
        self.local.transform_point3(p)
    }

    /// A world direction in container coordinates.
    pub fn dir_to_local(&self, v: DVec3) -> DVec3 {
        self.local.transform_vector3(v)
    }
}

/// Resolves the edit context of a command: `common.context` (the path of the
/// group or instance to edit) if given, else the engine's context. Groups on
/// the path are made private first (copy-on-edit, docs/04 §3.1), so call it
/// only from commands that edit the context. Points in params are world
/// coordinates; commands convert them with [`Target::to_local`] (docs/05
/// Decisions).
pub fn target_container(
    tx: &mut Transaction<'_>,
    common: &CommonParams,
) -> Result<Target, CommandError> {
    let context = command_context(tx, common);
    let doc = tx.document();
    let (container, world) = match &common.context {
        Some(path) => doc
            .resolve_container(&context.path)
            .map_err(|_| CommandError::NotFound(EntityRef::Path(path.clone())))?,
        None => doc.resolve_container(&context.path).map_err(|_| {
            CommandError::NotFound(EntityRef::Path(EntityPath::in_model(EntityKey::Pid(
                Pid::NONE,
            ))))
        })?,
    };
    if let Some(step) = doc.locked_step(&context.path) {
        return Err(CommandError::NotApplicable(format!(
            "{step} is locked; unlock it before editing inside it"
        )));
    }
    // The command is about to edit the context: groups on the way in are
    // made private first (copy-on-edit, docs/04 §3.1).
    crate::commands::common::make_path_private(tx, &context.path)?;
    if world.matrix3.determinant().abs() <= f64::EPSILON {
        return Err(CommandError::NotApplicable(
            "the edit context has a singular transform".into(),
        ));
    }
    Ok(Target {
        context,
        container,
        world,
        local: world.inverse(),
    })
}

/// The edit context a command runs in: `common.context` (the path of the
/// group or instance to edit) if given, else the engine's context.
pub fn command_context(tx: &Transaction<'_>, common: &CommonParams) -> EditContext {
    match &common.context {
        Some(path) => EditContext {
            path: path.instances.child(path.target),
        },
        None => tx.context().clone(),
    }
}

/// Evaluates the `Sel` query of parameter `param` inside edit context
/// `context` (docs/08 §5); errors name that parameter.
pub fn select(
    tx: &Transaction<'_>,
    context: &EditContext,
    param: &str,
    query: &Query,
) -> Result<Selection, CommandError> {
    let expr: QueryExpr = query
        .parse()
        .map_err(|e| CommandError::invalid_param(param, e.to_string()))?;
    let env = tx.query_env();
    let env = QueryEnv {
        selection: env.selection,
        last: env.last,
        context,
    };
    expr.eval(tx.document(), &env)
        .map_err(|e| CommandError::invalid_param(param, e.to_string()))
}

/// A resolved single entity occurrence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    /// Container.
    pub container: ContainerRef,
    /// Entity.
    pub id: EntityId,
    /// Container-to-world transform of the occurrence.
    pub world: DAffine3,
}

/// Resolves the `EntityRef` of parameter `param` to exactly one entity
/// (`E1101`/`E1102`); queries evaluate inside edit context `context`.
pub fn resolve_ref(
    tx: &Transaction<'_>,
    context: &EditContext,
    param: &str,
    r: &EntityRef,
) -> Result<Resolved, CommandError> {
    let doc = tx.document();
    match r {
        EntityRef::Pid(pid) => {
            let (container, id) = doc
                .locate_pid(*pid)
                .ok_or_else(|| CommandError::NotFound(r.clone()))?;
            let world = world_of(doc, container, context.path.clone());
            Ok(Resolved {
                container,
                id,
                world,
            })
        }
        EntityRef::Path(path) => {
            resolve_path(doc, path).ok_or_else(|| CommandError::NotFound(r.clone()))
        }
        EntityRef::Query(q) => {
            let selection = select(tx, context, param, q)?;
            if selection.len() != 1 {
                return Err(if selection.is_empty() {
                    CommandError::NotFound(r.clone())
                } else {
                    CommandError::Ambiguous {
                        query: q.clone(),
                        matched: selection.len(),
                    }
                });
            }
            let path = selection
                .single_item()
                .ok_or_else(|| CommandError::NotFound(r.clone()))?;
            resolve_path(doc, path).ok_or_else(|| CommandError::NotFound(r.clone()))
        }
    }
}

/// Resolves an occurrence path.
pub fn resolve_path(doc: &Document, path: &EntityPath) -> Option<Resolved> {
    let resolved = doc.resolve(path).ok()?;
    Some(Resolved {
        container: resolved.container,
        id: resolved.entity,
        world: resolved.world,
    })
}

/// World transform of `container` when reached through the edit context
/// (identity when the context does not open it).
fn world_of(doc: &Document, container: ContainerRef, context: InstancePath) -> DAffine3 {
    match doc.resolve_container(&context) {
        Ok((c, world)) if c == container => world,
        _ => DAffine3::IDENTITY,
    }
}

/// The pid of an entity, or `E1901` when it has none (every edge/face created
/// through a transaction has one).
pub fn pid_of(
    tx: &Transaction<'_>,
    container: ContainerRef,
    id: EntityId,
) -> Result<Pid, CommandError> {
    tx.document()
        .container(container)
        .and_then(|e| e.pid_of(id))
        .ok_or_else(|| CommandError::Internal(format!("{id} has no pid")))
}

/// Checks that a point is finite and within the model extent (`E1002`).
pub fn check_point(name: &str, p: DVec3) -> Result<(), CommandError> {
    if !p.is_finite() || p.abs().max_element() > crate::tolerance::Tolerances::MAX_EXTENT {
        return Err(CommandError::OutOfRange {
            name: name.into(),
            reason: "coordinates must be finite and within ±1e9 mm".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_values_parse_with_units() {
        let mm = LengthUnit::Millimeter;
        assert_eq!(
            parse_cli_value(CliType::Length, "d", "1.5m", mm).unwrap(),
            serde_json::json!(1500.0)
        );
        assert_eq!(
            parse_cli_value(CliType::Point, "p", "0,1m,20cm", mm).unwrap(),
            serde_json::json!([0.0, 1000.0, 200.0])
        );
        assert_eq!(
            parse_cli_value(CliType::Vector, "n", "-z", mm).unwrap(),
            serde_json::json!([-0.0, -0.0, -1.0])
        );
        let angle = parse_cli_value(CliType::Angle, "a", "90", mm).unwrap();
        assert!((angle.as_f64().unwrap() - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        let plane: Plane =
            serde_json::from_value(parse_cli_value(CliType::Plane, "p", "xy@1.2m", mm).unwrap())
                .unwrap();
        assert!((plane.signed_distance(DVec3::new(5.0, 5.0, 1200.0))).abs() < 1e-9);
        assert_eq!(
            parse_cli_value(CliType::EntityRef, "f", "@42", mm).unwrap(),
            serde_json::to_value(EntityRef::Pid(Pid(42))).unwrap()
        );
        assert!(parse_cli_value(CliType::Point, "p", "1,2", mm).is_err());
        assert!(parse_cli_value(CliType::EntityRef, "f", "face", mm).is_err());
        let t: DAffine3 = serde_json::from_value(
            parse_cli_value(CliType::Transform, "t", "t=1m,0,0", mm).unwrap(),
        )
        .unwrap();
        assert_eq!(t.translation, DVec3::new(1000.0, 0.0, 0.0));
    }

    #[test]
    fn schema_names_identify_cli_types() {
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct P {
            #[schemars(with = "PointSchema")]
            at: DVec3,
            len: crate::units::Length,
        }
        let schema = serde_json::to_value(schemars::schema_for!(P)).unwrap();
        let reference = schema["properties"]["at"]["$ref"].as_str().unwrap();
        assert_eq!(
            CliType::from_schema_name(reference.rsplit('/').next().unwrap()),
            Some(CliType::Point)
        );
        let reference = schema["properties"]["len"]["$ref"].as_str().unwrap();
        assert_eq!(
            CliType::from_schema_name(reference.rsplit('/').next().unwrap()),
            Some(CliType::Length)
        );
    }
}
