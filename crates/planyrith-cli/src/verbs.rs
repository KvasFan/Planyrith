//! The session verbs of docs/08 §2.

use std::io::Read;
use std::path::Path;
use std::time::Instant;

use clap::{Args, Command, FromArgMatches};
use planyrith_core::commands::{CommandError, CommandRegistry};
use planyrith_core::diagnostics::{RepairFixes, repair, statistics, validate};
use planyrith_core::document::{
    Document, EntityPath, EntityView, FrontOrBack, Query, QueryEnv, measure_occurrence,
};
use planyrith_core::ids::EntityId;
use planyrith_core::io::export::json::{canonicalize, document_to_json};
use planyrith_core::io::storage::{NativeFs, Storage};
use planyrith_core::units::LengthUnit;
use serde_json::{Map, Value, json};

use crate::args;
use crate::out;
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

/// The warnings commands emitted in a session, as JSON objects.
fn warnings_of(session: Option<&Session>, from: usize) -> Vec<Value> {
    session
        .map(|s| {
            s.warnings
                .borrow()
                .iter()
                .skip(from)
                .map(|w| json!({ "code": w.code.as_str(), "message": w.message }))
                .collect()
        })
        .unwrap_or_default()
}

/// Prints the result (docs/08 §7): one JSON envelope in `--json`/`--jsonl`
/// mode, the human form otherwise. With `error` (problems that remain after
/// `validate`/`repair`), the envelope is `ok: false` and carries both the
/// output and the error, so JSON mode still prints exactly one document.
fn emit(
    global: &Global,
    command: &str,
    output: Value,
    session: Option<&Session>,
    saved: Option<String>,
    started: Instant,
    error: Option<&CliError>,
) -> Result<(), CliError> {
    let warnings = warnings_of(session, 0);
    // With `-o -` standard output carries the document itself: the report
    // goes to standard error so the document stays intact.
    let to_stderr = saved.as_deref() == Some("-");
    let print = |text: String| {
        if to_stderr {
            out::diagnostic(text);
            Ok(())
        } else {
            out::line(text)
        }
    };
    if global.json || global.jsonl {
        let document = session.map(|s| {
            json!({
                "path": saved.clone().or_else(|| s.path.as_ref().map(|p| p.display().to_string())),
                "saved": saved.is_some(),
                "generation": s.engine.generation(),
            })
        });
        let mut envelope = json!({
            "ok": error.is_none(),
            "command": command,
            "output": output,
            "warnings": warnings,
            "document": document,
            "timing_ms": started.elapsed().as_secs_f64() * 1000.0,
        });
        if let Some(e) = error {
            envelope["error"] = e.to_json()["error"].clone();
        }
        return print(envelope.to_string());
    }
    for w in &warnings {
        out::diagnostic(format!(
            "warning[{}]: {}",
            w["code"].as_str().unwrap_or(""),
            w["message"].as_str().unwrap_or("")
        ));
    }
    match output {
        Value::Null => {}
        Value::String(s) => print(s)?,
        other => print(serde_json::to_string_pretty(&other).unwrap_or_default())?,
    }
    if let Some(path) = saved
        && path != "-"
    {
        out::diagnostic(format!("saved {path}"));
    }
    Ok(())
}

/// Reads a text input file (batch script, params file); a missing or
/// unreadable file is an I/O error (`E1403`, exit 4) like a missing document.
fn read_input(path: &Path) -> Result<String, CliError> {
    std::fs::read_to_string(path).map_err(|e| {
        let name = path.display().to_string();
        CliError::io(if e.kind() == std::io::ErrorKind::NotFound {
            planyrith_core::io::IoError::NotFound(name)
        } else {
            planyrith_core::io::IoError::storage(name, e.to_string())
        })
    })
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
    // The usual session rules apply: `--no-save`/`--dry-run` write nothing.
    let session = Session::create(doc, Some(path));
    let saved = session.save(global)?;
    emit(
        global,
        "new",
        json!({ "template": template }),
        Some(&session),
        saved,
        started,
        None,
    )
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
    emit(global, "info", output, Some(&session), None, started, None)
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
        let text = read_input(Path::new(path))?;
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
    /// The params object. Under `--dry-run` commands still run, on the
    /// in-memory session only (nothing is saved or journaled), so a dry run
    /// reports the errors the real run would (docs/08 Decisions).
    fn params(&self, session: &Session, global: &Global) -> Result<Value, CliError> {
        let unit = session.unit(global)?;
        let map = args::collect(&self.matches, &self.specs, unit, self.base.clone())?;
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
        let list = registry.list();
        if global.json || global.jsonl {
            return emit(
                global,
                "tool",
                serde_json::to_value(list).unwrap_or(Value::Null),
                None,
                None,
                started,
                None,
            );
        }
        out::line("Usage: planyrith tool <NAME> [OPTIONS]   (planyrith tool <NAME> --help)\n")?;
        let width = list.iter().map(|c| c.name.len()).max().unwrap_or(0);
        for c in list {
            out::line(format!(
                "{:<width$} {:<10} {}",
                c.name,
                format!("{:?}", c.category).to_lowercase(),
                c.label
            ))?;
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
        return out::line(serde_json::to_string_pretty(&info).unwrap_or_default());
    }
    let run = || -> Result<(), CliError> {
        let mut session = Session::open(&global, true)?;
        let params = invocation.params(&session, &global)?;
        let output = session
            .engine
            .execute_erased(&invocation.name, params.clone())
            .map_err(CliError::from_command)?;
        let (output, saved) = if global.dry_run {
            (
                json!({ "dry_run": true, "valid": true, "output": output }),
                None,
            )
        } else {
            session.journal(&invocation.name, &params, None, &global);
            (output, session.save(&global)?)
        };
        emit(
            &global,
            &invocation.name,
            output,
            Some(&session),
            saved,
            started,
            None,
        )
    };
    // Options after the tool name may switch on JSON output: report errors
    // in that mode here.
    run().map_err(|mut e| {
        if (global.json || global.jsonl) && !e.reported {
            let _ = out::line(e.to_json());
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
        .map_err(|e| CliError::invalid("query", e.to_string()))?;
    let env = planyrith_core::document::QueryEnv {
        selection: engine.selection(),
        last: engine.last_created(),
        context: engine.context(),
    };
    let selection = expr
        .eval(engine.document(), &env)
        .map_err(|e| CliError::invalid("query", e.to_string()))?;
    if count {
        return emit(
            global,
            "query",
            json!(selection.len()),
            Some(&session),
            None,
            started,
            None,
        );
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
        let row = entity_row(doc, &env, path, &fields)?;
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
    emit(global, "query", output, Some(&session), None, started, None)
}

/// The requested fields of one entity occurrence. Lengths, areas, volumes
/// and bounding boxes are measured in the edit context's frame, exactly as
/// the query predicates measure them (docs/08 Decisions).
fn entity_row(
    doc: &Document,
    env: &QueryEnv<'_>,
    path: &EntityPath,
    fields: &[String],
) -> Result<Map<String, Value>, CliError> {
    let resolved = doc
        .resolve(path)
        .map_err(|e| CliError::invalid("query", format!("{path}: {e}")))?;
    let entities = doc.container(resolved.container).ok_or_else(|| {
        CliError::from_command(CommandError::Internal("container vanished".into()))
    })?;
    let id = resolved.entity;
    let mesh = entities.mesh();
    let measures = measure_occurrence(doc, env, path);
    let to_context = measures.map_or(resolved.world, |m| m.to_context);
    let number = |x: Option<f64>| x.map_or(Value::Null, |v| json!(v));
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
            "length" => number(measures.and_then(|m| m.length)),
            "area" => number(measures.and_then(|m| m.area)),
            "volume" => number(measures.and_then(|m| m.volume)),
            "is_solid" => entities
                .definition_of(id)
                .and_then(|d| doc.definition(d))
                .map(|def| json!(def.entities.mesh().is_solid()))
                .unwrap_or(Value::Null),
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
                        .map(|s| s.transformed(&to_context).aabb())
                        .ok(),
                    EntityId::Face(f) => mesh.face_aabb(f).map(|b| b.transformed(&to_context)).ok(),
                    _ => entities
                        .definition_of(id)
                        .and_then(|d| doc.definition(d))
                        .zip(entities.transform_of(id))
                        .map(|(def, t)| {
                            def.entities
                                .local_aabb(&doc.definitions, false)
                                .transformed(&(to_context * t))
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
                    return Err(CliError::invalid(
                        "fields",
                        format!("unknown field `{other}`"),
                    ));
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
        return out::line(text);
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
        None,
    )
}

// ----- validate / repair -----------------------------------------------------------------

fn validate_verb(global: &Global) -> Result<(), CliError> {
    let started = Instant::now();
    let session = Session::open(global, false)?;
    let report = validate(session.engine.document());
    let problems = report.has_errors().then(|| CliError {
        exit: 3,
        code: None,
        message: format!("{} problem(s) found", report.problems.len()),
        param: None,
        reported: global.json || global.jsonl,
    });
    emit(
        global,
        "validate",
        serde_json::to_value(&report).unwrap_or(Value::Null),
        Some(&session),
        None,
        started,
        problems.as_ref(),
    )?;
    problems.map_or(Ok(()), Err)
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
                other => return Err(CliError::invalid("fix", format!("unknown fix `{other}`"))),
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
    let remaining = report.remaining.has_errors().then(|| CliError {
        exit: 3,
        code: None,
        message: "problems remain after repair".into(),
        param: None,
        reported: global.json || global.jsonl,
    });
    emit(
        global,
        "repair",
        serde_json::to_value(&report).unwrap_or(Value::Null),
        Some(&session),
        saved,
        started,
        remaining.as_ref(),
    )?;
    remaining.map_or(Ok(()), Err)
}

// ----- batch -------------------------------------------------------------------------------

fn batch(global: &Global, script: Option<&Path>) -> Result<(), CliError> {
    let started = Instant::now();
    let text = match script {
        Some(p) if p.as_os_str() != "-" => read_input(p)?,
        _ => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map_err(|e| {
                CliError::io(planyrith_core::io::IoError::storage(
                    "standard input",
                    e.to_string(),
                ))
            })?;
            s
        }
    };
    let mut session = Session::open(global, true)?;
    let registry = CommandRegistry::with_builtins();
    let token = session.engine.begin_group("Batch");
    let mut results = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let line_no = line_no + 1;
        let commands = shell::split_line(line)
            .map_err(|e| CliError::usage(format!("line {line_no}: {e}")))
            .map_err(|e| stream_error(global, line_no, None, e))?;
        for words in commands {
            let (name, rest) = words
                .split_first()
                .ok_or_else(|| CliError::usage(format!("line {line_no}: empty command")))
                .map_err(|e| stream_error(global, line_no, None, e))?;
            let warned = session.warnings.borrow().len();
            let run = |session: &mut Session| -> Result<Value, CliError> {
                let inv = parse_tool_args(&registry, name, rest, false).map_err(|mut e| {
                    e.message = format!("line {line_no}: {}", e.message);
                    e
                })?;
                let params = inv.params(session, global)?;
                let output = session
                    .engine
                    .execute_erased(name, params.clone())
                    .map_err(|e| {
                        let mut err = CliError::from_command(e);
                        err.message = format!("line {line_no} ({name}): {}", err.message);
                        err
                    })?;
                if !global.dry_run {
                    session.journal(name, &params, Some("Batch"), global);
                }
                Ok(output)
            };
            let output = run(&mut session)
                .map_err(|e| stream_error(global, line_no, Some(name.as_str()), e))?;
            if global.jsonl {
                out::line(json!({
                    "ok": true,
                    "line": line_no,
                    "command": name,
                    "output": output,
                    "warnings": warnings_of(Some(&session), warned),
                }))?;
            }
            results.push(json!({ "line": line_no, "command": name, "output": output }));
        }
    }
    session
        .engine
        .end_group(token)
        .map_err(CliError::from_command)?;
    let saved = session.save(global)?;
    let output = if global.json || global.jsonl {
        let mut summary = json!({ "applied": results.len() });
        if global.json {
            summary = Value::Array(results);
        }
        if global.dry_run {
            summary = json!({ "dry_run": true, "valid": true, "output": summary });
        }
        summary
    } else if global.dry_run {
        Value::String(format!(
            "{} command(s) would apply (dry run: nothing saved)",
            results.len()
        ))
    } else {
        Value::String(format!("{} command(s) applied", results.len()))
    };
    emit(
        global,
        "batch",
        output,
        Some(&session),
        saved,
        started,
        None,
    )
}

/// In `--jsonl` mode a failing batch line is reported as one more stream
/// object, `{"ok": false, "line": N, "command": …, "error": {…}}`.
fn stream_error(global: &Global, line: usize, command: Option<&str>, mut e: CliError) -> CliError {
    if global.jsonl && !e.reported {
        let mut object = e.to_json();
        object["line"] = json!(line);
        object["command"] = json!(command);
        let _ = out::line(object);
        e.reported = true;
    }
    e
}
