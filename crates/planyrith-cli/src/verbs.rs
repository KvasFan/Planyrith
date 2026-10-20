//! The session verbs of docs/08 §2.

use std::io::Read;
use std::path::Path;
use std::time::Instant;

use clap::{Args, Command, FromArgMatches};
use planyrith_core::commands::CommandRegistry;
use planyrith_core::diagnostics::{RepairFixes, repair, statistics, validate};
use planyrith_core::document::{
    ContainerRef, Document, EntityPath, EntityView, FrontOrBack, Query,
};
use planyrith_core::ids::EntityId;
use planyrith_core::io::export::json::{canonicalize, document_to_json};
use planyrith_core::io::native::{WriteOptions, write_document};
use planyrith_core::io::storage::{NativeFs, Storage};
use planyrith_core::units::LengthUnit;
use serde_json::{Map, Value, json};

use crate::args;
use crate::session::{Session, key};
use crate::shell;
use crate::{CliError, Global, Verb};

/// Runs a verb.
pub fn dispatch(global: Global, verb: Verb) -> Result<(), CliError> {
    match verb {
        Verb::New { template, title } => new(&global, &template, title),
        Verb::Info => info(&global),
        Verb::Query {
            query,
            fields,
            count,
            stats,
        } => query_verb(&global, &query, &fields, count, stats),
        Verb::Tool { name, args } => tool(&global, name, args),
        Verb::Export {
            output,
            format,
            canonical,
            pretty,
        } => export(&global, &output, format, canonical, pretty),
        Verb::Validate => validate_verb(&global),
        Verb::Repair { fix } => repair_verb(&global, &fix),
        Verb::Batch { script } => batch(&global, script.as_deref()),
    }
}

/// Prints a success envelope (docs/08 §7) or the human form.
fn emit(
    global: &Global,
    command: &str,
    output: Value,
    session: Option<&Session>,
    saved: Option<String>,
    started: Instant,
) {
    let warnings: Vec<Value> = session
        .map(|s| {
            s.warnings
                .borrow()
                .iter()
                .map(|w| json!({ "code": w.code.as_str(), "message": w.message }))
                .collect()
        })
        .unwrap_or_default();
    if global.json || global.jsonl {
        let document = session.map(|s| {
            json!({
                "path": saved.clone().or_else(|| s.path.as_ref().map(|p| p.display().to_string())),
                "saved": saved.is_some(),
                "generation": s.engine.generation(),
            })
        });
        let envelope = json!({
            "ok": true,
            "command": command,
            "output": output,
            "warnings": warnings,
            "document": document,
            "timing_ms": started.elapsed().as_secs_f64() * 1000.0,
        });
        println!("{envelope}");
        return;
    }
    for w in &warnings {
        eprintln!(
            "warning[{}]: {}",
            w["code"].as_str().unwrap_or(""),
            w["message"].as_str().unwrap_or("")
        );
    }
    match output {
        Value::Null => {}
        Value::String(s) => println!("{s}"),
        other => println!(
            "{}",
            serde_json::to_string_pretty(&other).unwrap_or_default()
        ),
    }
    if let Some(path) = saved
        && path != "-"
    {
        eprintln!("saved {path}");
    }
}

// ----- new ------------------------------------------------------------------------------

fn new(global: &Global, template: &str, title: Option<String>) -> Result<(), CliError> {
    let started = Instant::now();
    let path = global
        .out
        .clone()
        .or_else(|| global.file.clone())
        .ok_or_else(|| CliError::usage("give the new document's path with -f <path>"))?;
    if path.as_os_str() != "-" && path.exists() {
        return Err(CliError::usage(format!(
            "{} exists; `new` does not overwrite documents",
            path.display()
        )));
    }
    let mut doc = Document::new();
    let (unit, precision) = match template {
        "default" | "architectural-mm" | "blank" => (LengthUnit::Millimeter, 0),
        "woodworking-mm" => (LengthUnit::Millimeter, 1),
        "product-mm" => (LengthUnit::Millimeter, 2),
        "imperial-ft-in" => (LengthUnit::FeetInches, 3),
        other => return Err(CliError::usage(format!("unknown template `{other}`"))),
    };
    doc.info.display_units = match &global.units {
        Some(u) => u
            .parse()
            .map_err(|e| CliError::usage(format!("--units: {e}")))?,
        None => unit,
    };
    doc.info.precision = precision;
    if let Some(t) = title {
        doc.info.title = t;
    }
    let now = crate::session::now_rfc3339();
    let opts = WriteOptions {
        deterministic: global.deterministic,
        created: now.clone(),
        modified: now,
        include_history: false,
        ..WriteOptions::default()
    };
    let bytes =
        write_document(&doc, &[], &opts, &planyrith_core::io::NoProgress).map_err(CliError::io)?;
    let saved = if path.as_os_str() == "-" {
        use std::io::Write;
        std::io::stdout()
            .write_all(&bytes)
            .map_err(|e| CliError::usage(e.to_string()))?;
        "-".to_string()
    } else {
        NativeFs::new()
            .write(&key(&path), &bytes)
            .map_err(CliError::io)?;
        path.display().to_string()
    };
    emit(
        global,
        "new",
        json!({ "template": template }),
        None,
        Some(saved),
        started,
    );
    Ok(())
}

// ----- info -----------------------------------------------------------------------------

fn info(global: &Global) -> Result<(), CliError> {
    let started = Instant::now();
    let session = Session::open(global, false)?;
    let doc = session.engine.document();
    let stats = statistics(doc);
    let report = validate(doc);
    let bounds = doc.aabb();
    let output = json!({
        "title": doc.info.title,
        "units": doc.info.display_units,
        "format_version": doc.info.format_version,
        "file_size": session.file_size,
        "counts": stats,
        "bounds": if bounds.is_empty() { Value::Null } else {
            json!({ "min": [bounds.min.x, bounds.min.y, bounds.min.z], "max": [bounds.max.x, bounds.max.y, bounds.max.z] })
        },
        "problems": report.problems.len(),
    });
    emit(global, "info", output, Some(&session), None, started);
    Ok(())
}

// ----- tool -----------------------------------------------------------------------------

/// Parses a tool's options from `args` (`name` first is not included).
fn parse_tool_args(
    registry: &CommandRegistry,
    name: &str,
    raw: &[String],
    with_globals: bool,
) -> Result<ToolInvocation, CliError> {
    let command = registry.get(name).ok_or_else(|| {
        CliError::usage(format!(
            "unknown command `{name}` (`planyrith tool` lists the commands)"
        ))
    })?;
    let schema = serde_json::to_value(command.params_schema())
        .map_err(|e| CliError::usage(e.to_string()))?;
    let specs = args::specs(&schema);
    let meta = command.meta();
    let mut cmd = Command::new(format!("planyrith tool {name}"))
        .no_binary_name(true)
        .arg(
            clap::Arg::new("params_json")
                .long("params-json")
                .value_name("JSON")
                .help("The whole Params object as JSON (options override its fields)"),
        )
        .arg(
            clap::Arg::new("params_file")
                .long("params-file")
                .value_name("PATH")
                .help("Read the Params object from a JSON file"),
        )
        .arg(
            clap::Arg::new("schema")
                .long("schema")
                .action(clap::ArgAction::SetTrue)
                .help("Print the JSON schemas of params and output"),
        )
        .arg(
            clap::Arg::new("steps")
                .long("steps")
                .action(clap::ArgAction::SetTrue)
                .help("Print the interactive step sequence"),
        )
        .arg(
            clap::Arg::new("example")
                .long("example")
                .action(clap::ArgAction::SetTrue)
                .help("Print an example Params object"),
        );
    cmd = args::add_args(cmd.next_help_heading("Tool options"), &specs);
    if with_globals {
        cmd = Global::augment_args(cmd.next_help_heading("Global options"));
    }
    cmd = cmd.about(format!(
        "{} ({})",
        meta.label,
        format!("{:?}", meta.category).to_lowercase()
    ));
    let matches = match cmd.try_get_matches_from(raw) {
        Ok(m) => m,
        Err(e) => {
            let _ = e.print();
            return Err(if e.use_stderr() {
                CliError::usage(format!("invalid options for `{name}`"))
            } else {
                CliError {
                    exit: 0,
                    code: None,
                    message: String::new(),
                    param: None,
                    reported: true,
                }
            });
        }
    };
    let globals = if with_globals {
        Global::from_arg_matches(&matches).map_err(|e| CliError::usage(e.to_string()))?
    } else {
        Global::default()
    };
    let mut base = Map::new();
    if let Some(text) = matches.get_one::<String>("params_json") {
        base = parse_object(text)?;
    }
    if let Some(path) = matches.get_one::<String>("params_file") {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CliError::usage(format!("--params-file {path}: {e}")))?;
        base = parse_object(&text)?;
    }
    let info = if matches.get_flag("schema") {
        Some(json!({
            "params": schema,
            "output": serde_json::to_value(command.output_schema()).unwrap_or(Value::Null),
            "meta": meta,
        }))
    } else if matches.get_flag("steps") {
        Some(serde_json::to_value(&meta.steps).unwrap_or(Value::Null))
    } else if matches.get_flag("example") {
        Some(args::example(&specs))
    } else {
        None
    };
    Ok(ToolInvocation {
        name: name.to_string(),
        matches,
        specs,
        base,
        globals,
        info,
    })
}

struct ToolInvocation {
    name: String,
    matches: clap::ArgMatches,
    specs: Vec<args::ParamSpec>,
    base: Map<String, Value>,
    globals: Global,
    info: Option<Value>,
}

impl ToolInvocation {
    fn params(&self, session: &Session, global: &Global) -> Result<Value, CliError> {
        let unit = session.unit(global)?;
        let mut map = args::collect(&self.matches, &self.specs, unit, self.base.clone())?;
        if global.dry_run {
            map.insert("dry_run".into(), Value::Bool(true));
        }
        Ok(Value::Object(map))
    }
}

fn parse_object(text: &str) -> Result<Map<String, Value>, CliError> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(CliError::usage("params JSON must be an object")),
        Err(e) => Err(CliError::usage(format!("params JSON: {e}"))),
    }
}

fn tool(global: &Global, name: Option<String>, raw: Vec<String>) -> Result<(), CliError> {
    let started = Instant::now();
    let registry = CommandRegistry::with_builtins();
    let name = name.filter(|n| n != "--help" && n != "-h");
    let Some(name) = name else {
        if !global.json {
            println!("Usage: planyrith tool <NAME> [OPTIONS]   (planyrith tool <NAME> --help)\n");
        }
        let list = registry.list();
        if global.json {
            emit(
                global,
                "tool",
                serde_json::to_value(list).unwrap_or(Value::Null),
                None,
                None,
                started,
            );
        } else {
            for c in list {
                println!(
                    "{:<16} {:<10} {}",
                    c.name,
                    format!("{:?}", c.category).to_lowercase(),
                    c.label
                );
            }
        }
        return Ok(());
    };
    let invocation = match parse_tool_args(&registry, &name, &raw, true) {
        Ok(inv) => inv,
        Err(e) if e.exit == 0 => return Ok(()),
        Err(e) => return Err(e),
    };
    let global = global.merged(&invocation.globals);
    if let Some(info) = invocation.info.clone() {
        println!(
            "{}",
            serde_json::to_string_pretty(&info).unwrap_or_default()
        );
        return Ok(());
    }
    let run = || -> Result<(), CliError> {
        let mut session = Session::open(&global, true)?;
        let params = invocation.params(&session, &global)?;
        let output = session
            .engine
            .execute_erased(&invocation.name, params.clone())
            .map_err(CliError::from_command)?;
        let saved = if global.dry_run {
            None
        } else {
            session.journal(&invocation.name, &params, None, &global);
            session.save(&global)?
        };
        emit(
            &global,
            &invocation.name,
            output,
            Some(&session),
            saved,
            started,
        );
        Ok(())
    };
    // Options after the tool name may switch on JSON output: report errors
    // in that mode here.
    run().map_err(|mut e| {
        if global.json && !e.reported {
            println!("{}", e.to_json());
            e.reported = true;
        }
        e
    })
}

// ----- query ----------------------------------------------------------------------------

fn query_verb(
    global: &Global,
    query: &str,
    fields: &[String],
    count: bool,
    stats: bool,
) -> Result<(), CliError> {
    let started = Instant::now();
    let session = Session::open(global, false)?;
    let engine = &session.engine;
    let expr = Query::new(query)
        .parse()
        .map_err(|e| CliError::usage(format!("query: {e}")))?;
    let env = planyrith_core::document::QueryEnv {
        selection: engine.selection(),
        last: engine.last_created(),
        context: engine.context(),
    };
    let selection = expr
        .eval(engine.document(), &env)
        .map_err(|e| CliError::usage(format!("query: {e}")))?;
    if count {
        emit(
            global,
            "query",
            json!(selection.len()),
            Some(&session),
            None,
            started,
        );
        return Ok(());
    }
    let mut fields: Vec<String> = if fields.is_empty() {
        vec!["pid".into(), "kind".into(), "path".into()]
    } else {
        fields.to_vec()
    };
    if stats {
        for f in ["length", "area", "volume"] {
            if !fields.iter().any(|x| x == f) {
                fields.push(f.into());
            }
        }
    }
    let doc = engine.document();
    let mut rows = Vec::new();
    let (mut total_length, mut total_area, mut total_volume) = (0.0, 0.0, 0.0);
    for path in selection.iter() {
        let row = entity_row(doc, path, &fields)?;
        total_length += row.get("length").and_then(Value::as_f64).unwrap_or(0.0);
        total_area += row.get("area").and_then(Value::as_f64).unwrap_or(0.0);
        total_volume += row.get("volume").and_then(Value::as_f64).unwrap_or(0.0);
        rows.push(Value::Object(row));
    }
    let output = if stats {
        json!({ "count": rows.len(), "length": total_length, "area": total_area, "volume": total_volume })
    } else if global.json {
        Value::Array(rows)
    } else {
        let lines: Vec<String> = rows
            .iter()
            .map(|r| {
                fields
                    .iter()
                    .map(|f| match r.get(f) {
                        Some(Value::String(s)) => s.clone(),
                        Some(v) => v.to_string(),
                        None => "-".into(),
                    })
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect();
        Value::String(lines.join("\n"))
    };
    emit(global, "query", output, Some(&session), None, started);
    Ok(())
}

/// The requested fields of one entity occurrence.
fn entity_row(
    doc: &Document,
    path: &EntityPath,
    fields: &[String],
) -> Result<Map<String, Value>, CliError> {
    let resolved = doc
        .resolve(path)
        .map_err(|e| CliError::usage(format!("{path}: {e}")))?;
    let entities = doc
        .container(resolved.container)
        .ok_or_else(|| CliError::usage("container vanished"))?;
    let id = resolved.entity;
    let mesh = entities.mesh();
    let mut row = Map::new();
    for field in fields {
        let value = match field.as_str() {
            "pid" => json!(resolved.pid.0),
            "kind" => json!(id.kind().to_string()),
            "path" => json!(path.display_with(doc)),
            "tag" => json!(
                entities
                    .tag_of(id)
                    .and_then(|t| doc.tags.get(t))
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| "Untagged".into())
            ),
            "material" => match id {
                EntityId::Face(f) => mesh
                    .face(f)
                    .and_then(|face| {
                        doc.materials.get(
                            doc.materials
                                .resolve(face.sides.material_front, FrontOrBack::Front),
                        )
                    })
                    .map(|m| json!(m.name))
                    .unwrap_or(Value::Null),
                EntityId::Instance(i) => entities
                    .instance(i)
                    .and_then(|x| x.material_override)
                    .and_then(|m| doc.materials.get(m))
                    .map(|m| json!(m.name))
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            },
            "length" => match id {
                EntityId::Edge(e) => mesh
                    .edge_segment(e)
                    .map(|s| json!(s.transformed(&resolved.world).length()))
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            },
            "area" => match id {
                EntityId::Face(f) => mesh.face_area(f).map(|a| json!(a)).unwrap_or(Value::Null),
                _ => Value::Null,
            },
            "volume" | "is_solid" => {
                let definition = entities.definition_of(id).and_then(|d| doc.definition(d));
                let local = entities.transform_of(id);
                match (definition, local) {
                    (Some(def), Some(t)) => {
                        let solid = def.entities.mesh().is_solid();
                        if field == "is_solid" {
                            json!(solid)
                        } else {
                            def.entities
                                .mesh()
                                .volume()
                                .map(|v| json!((v * t.matrix3.determinant()).abs()))
                                .unwrap_or(Value::Null)
                        }
                    }
                    _ if field == "is_solid" && resolved.container == ContainerRef::Model => {
                        Value::Null
                    }
                    _ => Value::Null,
                }
            }
            "name" => match entities.view(id) {
                Some(EntityView::Group(g)) => json!(g.name),
                Some(EntityView::Instance(i)) => json!(i.name),
                _ => Value::Null,
            },
            "definition" => entities
                .definition_of(id)
                .and_then(|d| doc.definition(d))
                .map(|d| json!(d.name))
                .unwrap_or(Value::Null),
            "transform" => entities
                .transform_of(id)
                .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
                .unwrap_or(Value::Null),
            "hidden" => json!(entities.is_hidden(id)),
            "locked" => json!(entities.is_locked(id)),
            "bbox" => {
                let bb = match id {
                    EntityId::Edge(e) => mesh
                        .edge_segment(e)
                        .map(|s| s.transformed(&resolved.world).aabb())
                        .ok(),
                    EntityId::Face(f) => mesh
                        .face_aabb(f)
                        .map(|b| b.transformed(&resolved.world))
                        .ok(),
                    _ => entities
                        .definition_of(id)
                        .and_then(|d| doc.definition(d))
                        .zip(entities.transform_of(id))
                        .map(|(def, t)| {
                            def.entities
                                .local_aabb(&doc.definitions, false)
                                .transformed(&(resolved.world * t))
                        }),
                };
                bb.filter(|b| !b.is_empty())
                    .map(|b| json!({ "min": [b.min.x, b.min.y, b.min.z], "max": [b.max.x, b.max.y, b.max.z] }))
                    .unwrap_or(Value::Null)
            }
            other => {
                if let Some(spec) = other
                    .strip_prefix("attrs[")
                    .and_then(|r| r.strip_suffix(']'))
                {
                    let (dict, key) = spec.rsplit_once('.').unwrap_or((spec, ""));
                    entities
                        .attrs(id)
                        .and_then(|a| a.get(dict, key))
                        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                        .unwrap_or(Value::Null)
                } else {
                    return Err(CliError::usage(format!("unknown field `{other}`")));
                }
            }
        };
        row.insert(field.clone(), value);
    }
    Ok(row)
}

// ----- export ---------------------------------------------------------------------------

fn export(
    global: &Global,
    output: &Path,
    format: Option<String>,
    canonical: bool,
    pretty: bool,
) -> Result<(), CliError> {
    let started = Instant::now();
    let session = Session::open(global, false)?;
    let format = format
        .or_else(|| {
            output
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
        })
        .unwrap_or_default();
    if format != "json" {
        return Err(CliError::io(
            planyrith_core::io::IoError::UnsupportedFormat(format!(
                "export format `{format}` (milestone 0.1 exports json)"
            )),
        ));
    }
    let mut value = document_to_json(session.engine.document()).map_err(CliError::io)?;
    if canonical {
        canonicalize(&mut value);
    }
    let text = if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .map_err(|e| CliError::usage(e.to_string()))?;
    if output.as_os_str() == "-" {
        println!("{text}");
        return Ok(());
    }
    NativeFs::new()
        .write(&key(output), text.as_bytes())
        .map_err(CliError::io)?;
    emit(
        global,
        "export",
        json!({ "path": output.display().to_string(), "format": format }),
        Some(&session),
        None,
        started,
    );
    Ok(())
}

// ----- validate / repair -----------------------------------------------------------------

fn validate_verb(global: &Global) -> Result<(), CliError> {
    let started = Instant::now();
    let session = Session::open(global, false)?;
    let report = validate(session.engine.document());
    let errors = report.has_errors();
    emit(
        global,
        "validate",
        serde_json::to_value(&report).unwrap_or(Value::Null),
        Some(&session),
        None,
        started,
    );
    if errors {
        return Err(CliError {
            exit: 3,
            code: None,
            message: format!("{} problem(s) found", report.problems.len()),
            param: None,
            reported: false,
        });
    }
    Ok(())
}

fn repair_verb(global: &Global, fix: &str) -> Result<(), CliError> {
    let started = Instant::now();
    let fixes = if fix == "all" {
        RepairFixes::ALL
    } else {
        let mut f = RepairFixes::default();
        for part in fix.split(',').map(str::trim) {
            match part {
                "merge-vertices" => f.merge_vertices = true,
                "degenerate-faces" => f.degenerate_faces = true,
                "dangling-refs" => f.dangling_refs = true,
                other => return Err(CliError::usage(format!("unknown fix `{other}`"))),
            }
        }
        f
    };
    let session = Session::open(global, false)?;
    let mut doc = session.engine.document().clone();
    let report = repair(&mut doc, &fixes);
    let mut session = session;
    session.engine.replace_document(doc);
    let saved = session.save(global)?;
    let remaining = report.remaining.has_errors();
    emit(
        global,
        "repair",
        serde_json::to_value(&report).unwrap_or(Value::Null),
        Some(&session),
        saved,
        started,
    );
    if remaining {
        return Err(CliError {
            exit: 3,
            code: None,
            message: "problems remain after repair".into(),
            param: None,
            reported: false,
        });
    }
    Ok(())
}

// ----- batch -------------------------------------------------------------------------------

fn batch(global: &Global, script: Option<&Path>) -> Result<(), CliError> {
    let started = Instant::now();
    let text = match script {
        Some(p) if p.as_os_str() != "-" => std::fs::read_to_string(p)
            .map_err(|e| CliError::usage(format!("{}: {e}", p.display())))?,
        _ => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| CliError::usage(e.to_string()))?;
            s
        }
    };
    let mut session = Session::open(global, true)?;
    let registry = CommandRegistry::with_builtins();
    let token = session.engine.begin_group("Batch");
    let mut results = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let commands = shell::split_line(line)
            .map_err(|e| CliError::usage(format!("line {}: {e}", line_no + 1)))?;
        for words in commands {
            let (name, rest) = words
                .split_first()
                .ok_or_else(|| CliError::usage(format!("line {}: empty command", line_no + 1)))?;
            let inv = parse_tool_args(&registry, name, rest, false).map_err(|mut e| {
                e.message = format!("line {}: {}", line_no + 1, e.message);
                e
            })?;
            let params = inv.params(&session, global)?;
            let output = session
                .engine
                .execute_erased(name, params.clone())
                .map_err(|e| {
                    let mut err = CliError::from_command(e);
                    err.message = format!("line {} ({name}): {}", line_no + 1, err.message);
                    err
                })?;
            session.journal(name, &params, Some("Batch"), global);
            if global.jsonl {
                println!(
                    "{}",
                    json!({ "ok": true, "line": line_no + 1, "command": name, "output": output })
                );
            }
            results.push(json!({ "line": line_no + 1, "command": name, "output": output }));
        }
    }
    session
        .engine
        .end_group(token)
        .map_err(CliError::from_command)?;
    let saved = session.save(global)?;
    if global.jsonl {
        return Ok(());
    }
    let output = if global.json {
        Value::Array(results)
    } else {
        Value::String(format!("{} command(s) applied", results.len()))
    };
    emit(global, "batch", output, Some(&session), saved, started);
    Ok(())
}
