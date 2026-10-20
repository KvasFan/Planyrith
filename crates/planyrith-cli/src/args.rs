//! Implements docs/08-cli-specification.md §2 `tool` options: every field of
//! a command's `Params` JSON schema becomes a `--kebab-case` option, typed by
//! the schema (semantic types are recognised by their `$defs` name, see
//! `planyrith_core::commands::params`).

use clap::{Arg, ArgAction, ArgMatches, Command};
use planyrith_core::commands::params::{CliType, parse_cli_value};
use planyrith_core::units::LengthUnit;
use serde_json::{Map, Value};

use crate::CliError;

/// Fields every `Params` flattens in (`CommonParams`); set from global options.
const COMMON_FIELDS: [&str; 2] = ["context", "dry_run"];

/// How one option's text becomes JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    /// A semantic type with its own text form (docs/08 §2 table).
    Cli(CliType),
    /// A plain number.
    Number,
    /// An integer (`Count`).
    Integer,
    /// A flag.
    Bool,
    /// Free text.
    Text,
    /// One of the listed lowercase variants.
    Enum(Vec<String>),
    /// Anything else: JSON text.
    Json,
}

/// One generated option.
#[derive(Debug, Clone)]
pub struct ParamSpec {
    /// Params field (snake_case).
    pub field: String,
    /// Option name (kebab-case, without `--`).
    pub flag: String,
    /// Conversion.
    pub kind: ParamKind,
    /// `Vec<T>`: repeat the option or `;`-separate values.
    pub repeated: bool,
    /// Required by the schema (and without default).
    pub required: bool,
    /// Help text.
    pub help: String,
}

/// The options of a params schema, in schema order.
pub fn specs(schema: &Value) -> Vec<ParamSpec> {
    let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (field, sub) in properties {
        if COMMON_FIELDS.contains(&field.as_str()) {
            continue;
        }
        let (kind, repeated) = classify(sub, &defs, 0);
        let description = sub
            .get("description")
            .or_else(|| referenced(sub, &defs).and_then(|d| d.get("description")))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut help = description;
        let hint = type_hint(&kind);
        if !hint.is_empty() {
            help = format!("{help} [{hint}]");
        }
        if let Some(default) = sub.get("default").filter(|d| !d.is_null()) {
            help = format!("{help} (default: {default})");
        }
        out.push(ParamSpec {
            flag: field.replace('_', "-"),
            field: field.clone(),
            kind,
            repeated,
            required: required.contains(&field.as_str()) && sub.get("default").is_none(),
            help: help.trim().to_string(),
        });
    }
    out
}

fn referenced<'a>(sub: &Value, defs: &'a Value) -> Option<&'a Value> {
    let name = sub.get("$ref")?.as_str()?.rsplit('/').next()?;
    defs.get(name)
}

fn type_hint(kind: &ParamKind) -> String {
    match kind {
        ParamKind::Cli(CliType::Length) => "Length: number with optional unit, e.g. 1.5m".into(),
        ParamKind::Cli(CliType::Angle) => "Angle: degrees, or with deg/rad".into(),
        ParamKind::Cli(CliType::Point) => "Point: x,y,z with units".into(),
        ParamKind::Cli(CliType::Vector) => "Vector: dx,dy,dz or x|y|z|-x|-y|-z".into(),
        ParamKind::Cli(CliType::Plane) => "Plane: xy|yz|xz[@offset] or origin;normal".into(),
        ParamKind::Cli(CliType::Transform) => "Transform: t=x,y,z;r=axis,angle;s=sx,sy,sz".into(),
        ParamKind::Cli(CliType::Query) => "Sel: selection query".into(),
        ParamKind::Cli(CliType::EntityRef) => "EntityRef: @pid, #path or {query}".into(),
        ParamKind::Number => "number".into(),
        ParamKind::Integer => "Count: integer".into(),
        ParamKind::Bool => String::new(),
        ParamKind::Text => "text".into(),
        ParamKind::Enum(values) => values.join("|"),
        ParamKind::Json => "JSON".into(),
    }
}

/// Classifies a property schema; the flag says whether it is a list.
fn classify(sub: &Value, defs: &Value, depth: u8) -> (ParamKind, bool) {
    if depth > 8 {
        return (ParamKind::Json, false);
    }
    if let Some(r) = sub.get("$ref").and_then(Value::as_str) {
        let name = r.rsplit('/').next().unwrap_or("");
        if let Some(t) = CliType::from_schema_name(name) {
            return (ParamKind::Cli(t), false);
        }
        return match defs.get(name) {
            Some(def) => classify(def, defs, depth + 1),
            None => (ParamKind::Json, false),
        };
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(variants) = sub.get(key).and_then(Value::as_array) {
            let non_null: Vec<&Value> = variants
                .iter()
                .filter(|v| v.get("type").and_then(Value::as_str) != Some("null"))
                .collect();
            if non_null.len() == 1 {
                return classify(non_null[0], defs, depth + 1);
            }
            let consts: Vec<String> = non_null
                .iter()
                .filter_map(|v| {
                    v.get("const")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            v.get("enum")
                                .and_then(Value::as_array)
                                .filter(|e| e.len() == 1)
                                .and_then(|e| e[0].as_str())
                        })
                        .map(str::to_string)
                })
                .collect();
            if !consts.is_empty() && consts.len() == non_null.len() {
                return (ParamKind::Enum(consts), false);
            }
            return (ParamKind::Json, false);
        }
    }
    if let Some(values) = sub.get("enum").and_then(Value::as_array) {
        let names: Vec<String> = values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if names.len() == values.len() {
            return (ParamKind::Enum(names), false);
        }
    }
    let ty = match sub.get("type") {
        Some(Value::String(t)) => t.as_str(),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("null"),
        _ => "",
    };
    match ty {
        "boolean" => (ParamKind::Bool, false),
        "integer" => (ParamKind::Integer, false),
        "number" => (ParamKind::Number, false),
        "string" => (ParamKind::Text, false),
        "array" => match sub.get("items") {
            Some(items) => (classify(items, defs, depth + 1).0, true),
            None => (ParamKind::Json, false),
        },
        _ => (ParamKind::Json, false),
    }
}

/// Adds one clap argument per spec.
pub fn add_args(mut cmd: Command, specs: &[ParamSpec]) -> Command {
    for spec in specs {
        if spec.kind == ParamKind::Bool {
            cmd = cmd
                .arg(
                    Arg::new(spec.field.clone())
                        .long(spec.flag.clone())
                        .action(ArgAction::SetTrue)
                        .help(spec.help.clone()),
                )
                .arg(
                    Arg::new(format!("no_{}", spec.field))
                        .long(format!("no-{}", spec.flag))
                        .action(ArgAction::SetTrue)
                        .conflicts_with(spec.field.clone())
                        .help(format!("Turns off --{}", spec.flag)),
                );
            continue;
        }
        let mut arg = Arg::new(spec.field.clone())
            .long(spec.flag.clone())
            .value_name(value_name(&spec.kind))
            .allow_hyphen_values(true)
            .help(if spec.required {
                format!("{} (required)", spec.help)
            } else {
                spec.help.clone()
            });
        if spec.repeated {
            arg = arg.action(ArgAction::Append);
        }
        if let ParamKind::Enum(values) = &spec.kind {
            arg = arg.value_parser(clap::builder::PossibleValuesParser::new(values.clone()));
        }
        cmd = cmd.arg(arg);
    }
    cmd
}

fn value_name(kind: &ParamKind) -> String {
    match kind {
        ParamKind::Cli(CliType::Length) => "LENGTH",
        ParamKind::Cli(CliType::Angle) => "ANGLE",
        ParamKind::Cli(CliType::Point) => "X,Y,Z",
        ParamKind::Cli(CliType::Vector) => "VECTOR",
        ParamKind::Cli(CliType::Plane) => "PLANE",
        ParamKind::Cli(CliType::Transform) => "TRANSFORM",
        ParamKind::Cli(CliType::Query) => "QUERY",
        ParamKind::Cli(CliType::EntityRef) => "REF",
        ParamKind::Number => "NUMBER",
        ParamKind::Integer => "COUNT",
        ParamKind::Bool => "",
        ParamKind::Text => "TEXT",
        ParamKind::Enum(_) => "VARIANT",
        ParamKind::Json => "JSON",
    }
    .to_string()
}

/// Converts one option value.
fn convert(spec: &ParamSpec, text: &str, unit: LengthUnit) -> Result<Value, CliError> {
    let bad = |why: String| CliError::usage(format!("--{}: {why}", spec.flag));
    Ok(match &spec.kind {
        ParamKind::Cli(t) => {
            parse_cli_value(*t, &spec.field, text, unit).map_err(CliError::from_command)?
        }
        ParamKind::Number => Value::from(
            text.trim()
                .parse::<f64>()
                .map_err(|_| bad(format!("`{text}` is not a number")))?,
        ),
        ParamKind::Integer => Value::from(
            text.trim()
                .parse::<i64>()
                .map_err(|_| bad(format!("`{text}` is not an integer")))?,
        ),
        ParamKind::Bool => Value::Bool(matches!(text, "true" | "1" | "yes")),
        ParamKind::Text | ParamKind::Enum(_) => Value::String(text.to_string()),
        ParamKind::Json => {
            serde_json::from_str(text).map_err(|e| bad(format!("invalid JSON: {e}")))?
        }
    })
}

/// Builds the params object: `base` (from `--params-json`/`--params-file`)
/// overridden by the given options; checks required fields.
pub fn collect(
    matches: &ArgMatches,
    specs: &[ParamSpec],
    unit: LengthUnit,
    mut base: Map<String, Value>,
) -> Result<Map<String, Value>, CliError> {
    for spec in specs {
        if spec.kind == ParamKind::Bool {
            if matches.get_flag(&spec.field) {
                base.insert(spec.field.clone(), Value::Bool(true));
            } else if matches.get_flag(&format!("no_{}", spec.field)) {
                base.insert(spec.field.clone(), Value::Bool(false));
            }
            continue;
        }
        let Some(values) = matches.get_many::<String>(&spec.field) else {
            continue;
        };
        let values: Vec<&String> = values.collect();
        let value = if spec.repeated {
            let mut items = Vec::new();
            for v in values {
                for piece in v.split(';').map(str::trim).filter(|p| !p.is_empty()) {
                    items.push(convert(spec, piece, unit)?);
                }
            }
            Value::Array(items)
        } else {
            let last = values.last().map(|s| s.as_str()).unwrap_or("");
            convert(spec, last, unit)?
        };
        base.insert(spec.field.clone(), value);
    }
    for spec in specs {
        if spec.required && !base.contains_key(&spec.field) {
            return Err(CliError::usage(format!(
                "missing required option --{}",
                spec.flag
            )));
        }
    }
    Ok(base)
}

/// A skeleton params object for `--example`.
pub fn example(specs: &[ParamSpec]) -> Value {
    let mut map = Map::new();
    for spec in specs.iter().filter(|s| s.required) {
        let v = match &spec.kind {
            ParamKind::Cli(CliType::Point) | ParamKind::Cli(CliType::Vector) => {
                serde_json::json!([0.0, 0.0, 0.0])
            }
            ParamKind::Cli(CliType::Length) | ParamKind::Number => serde_json::json!(1000.0),
            ParamKind::Cli(CliType::Angle) => serde_json::json!(0.0),
            ParamKind::Cli(CliType::EntityRef) => serde_json::json!({ "Pid": 1 }),
            ParamKind::Cli(CliType::Query) | ParamKind::Text => serde_json::json!("face"),
            ParamKind::Integer => serde_json::json!(1),
            ParamKind::Bool => serde_json::json!(false),
            ParamKind::Enum(values) => serde_json::json!(values.first()),
            _ => Value::Null,
        };
        let v = if spec.repeated {
            Value::Array(vec![v.clone(), v])
        } else {
            v
        };
        map.insert(spec.field.clone(), v);
    }
    Value::Object(map)
}
