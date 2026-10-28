//! Implements docs/10-extensions-api.md §2: sandboxed Rhai evaluation, generated
//! registry bindings, read-only queries and atomic command replay.
//!
//! - one script function per registered command, generated from
//!   `commands::CommandRegistry` (kebab-case → snake_case, params as a map);
//! - `query`, `select`, `context`, `enter`, `exit_context`;
//! - unit helpers `mm`, `cm`, `m`, `inch`, `ft`, `deg`, `rad` (bare numbers are
//!   millimeters; angles default to radians);
//! - read-only document views, attribute access, `group(label, fn)` for one
//!   undo step, progress/cancel;
//! - operation/data limits and cancellation; deadlines supplied by the host.
//!
//! Owner key: `later`.

/// `true` once the embedded runtime is linked.
pub const RUNTIME_AVAILABLE: bool = true;

mod attributes;
mod command;
pub use attributes::{AttributeParams, AttributeSet};
pub mod plugins;
pub use command::{ScriptEval, ScriptParams};

mod tests;

use crate::commands::{
    CommandCategory, CommandError, CommandMeta, CommandRegistry, Engine, Transaction,
};
use crate::document::{EditContext, EntityPath, InstancePath, Query, QueryEnv};
use crate::io::CancelToken;
use rhai::{Array, Dynamic, EvalAltResult, FnPtr, ImmutableString, Map, NativeCallContext};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

type ScriptResult<T> = Result<T, Box<EvalAltResult>>;

/// Per-invocation interpreter limits; zero never means unlimited.
#[derive(Debug, Clone, Copy)]
pub struct ScriptLimits {
    /// Maximum Rhai operations (50 million by default).
    pub operations: u64,
    /// Maximum source, output and command data, in bytes (256 MiB default).
    pub data_bytes: usize,
}
impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            operations: 50_000_000,
            data_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Host cancellation/deadline hook. The core never reads a clock.
#[derive(Clone, Default)]
pub struct ScriptControl {
    /// Explicit cancellation token.
    pub cancel: CancelToken,
    /// Host checks its deadline; true interrupts evaluation.
    pub interrupted: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

/// A script's JSON return value and captured print/log output.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScriptOutput {
    /// Return value, JSON null for unit.
    pub value: Value,
    /// Captured lines, never mixed into JSON-RPC framing.
    pub output: Vec<String>,
    /// Number of dispatched commands.
    pub commands: usize,
}

pub(crate) enum Action {
    Command(String, Value),
    Context(EditContext),
}
pub(crate) struct Plan {
    pub(crate) actions: Vec<Action>,
    pub(crate) output: ScriptOutput,
}
struct Host {
    engine: Engine,
    actions: Vec<Action>,
    lines: Vec<String>,
    bytes: usize,
    limits: ScriptLimits,
}

fn exception(e: impl std::fmt::Display) -> Box<EvalAltResult> {
    e.to_string().into()
}
fn dynamic(value: &impl Serialize) -> ScriptResult<Dynamic> {
    rhai::serde::to_dynamic(value).map_err(exception)
}
fn value(input: &Dynamic) -> ScriptResult<Value> {
    rhai::serde::from_dynamic(input).map_err(exception)
}
fn number(n: Dynamic) -> ScriptResult<f64> {
    if let Some(i) = n.clone().try_cast::<i64>() {
        Ok(i as f64)
    } else {
        n.try_cast::<f64>()
            .filter(|v| v.is_finite())
            .ok_or_else(|| exception("expected a finite number"))
    }
}
fn script_error(e: Box<EvalAltResult>) -> CommandError {
    let pos = e.position();
    CommandError::Script {
        line: pos.line().unwrap_or(1) as u32,
        col: pos.position().unwrap_or(1) as u32,
        message: e.to_string(),
    }
}

impl Host {
    fn charge(&mut self, size: usize) -> ScriptResult<()> {
        self.bytes = self.bytes.saturating_add(size);
        if self.bytes > self.limits.data_bytes {
            return Err(exception("script data budget exceeded"));
        }
        Ok(())
    }
    fn command(&mut self, name: &str, params: Value) -> ScriptResult<Dynamic> {
        self.charge(name.len() + params.to_string().len())?;
        let result = self
            .engine
            .execute_erased(name, params.clone())
            .map_err(|e| {
                exception(json!({"code":e.code(),"message":e.to_string(),"param":e.param()}))
            })?;
        self.actions.push(Action::Command(name.into(), params));
        dynamic(&result)
    }
    fn context(&mut self, context: EditContext) -> ScriptResult<()> {
        self.engine
            .set_context(context.clone())
            .map_err(exception)?;
        self.actions.push(Action::Context(context));
        Ok(())
    }
}

fn query(engine: &Engine, source: &str) -> ScriptResult<Array> {
    let expr = Query::new(source).parse().map_err(exception)?;
    let env = QueryEnv {
        selection: engine.selection(),
        last: engine.last_created(),
        context: engine.context(),
    };
    let selected = expr.eval(engine.document(), &env).map_err(exception)?;
    let mut output = Vec::new();
    for path in selected.iter() {
        let r = engine.document().resolve(path).map_err(exception)?;
        let entities = engine
            .document()
            .container(r.container)
            .ok_or_else(|| exception("container missing"))?;
        let measures = crate::document::measure_occurrence(engine.document(), &env, path);
        let mut record = json!({"kind":r.entity.kind().to_string(),"pid":entities.pid_of(r.entity).map(|p|p.0),"path":path.to_string(),"attrs":entities.attrs(r.entity)});
        record["attrs"] = Value::Object(
            entities
                .attrs(r.entity)
                .map(|a| {
                    a.0.iter()
                        .map(|(dict, values)| {
                            (
                                dict.clone(),
                                Value::Object(
                                    values
                                        .iter()
                                        .map(|(key, value)| (key.clone(), attributes::json(value)))
                                        .collect(),
                                ),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        record["tag"] = json!(
            entities
                .tag_of(r.entity)
                .and_then(|tag| engine.document().tags.get(tag))
                .map(|tag| &tag.name)
        );
        record["transform"] = json!(entities.transform_of(r.entity));
        let vertices = match r.entity {
            crate::ids::EntityId::Face(f) => entities.mesh().face_vertices(f).unwrap_or_default(),
            crate::ids::EntityId::Edge(e) => entities
                .mesh()
                .edge_endpoints(e)
                .map(|(a, b)| vec![a, b])
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        record["vertices"] = json!(
            vertices
                .into_iter()
                .filter_map(|v| entities.mesh().position(v).ok())
                .map(|p| r.world.transform_point3(p).to_array())
                .collect::<Vec<_>>()
        );
        record["material"] = json!(if let crate::ids::EntityId::Face(f) = r.entity {
            entities
                .mesh()
                .face(f)
                .and_then(|f| f.sides.material_front)
                .and_then(|m| engine.document().materials.get(m))
                .map(|m| &m.name)
        } else {
            None
        });
        if let Some(measures) = measures {
            record["area"] = json!(measures.area);
            record["length"] = json!(measures.length);
            record["volume"] = json!(measures.volume);
            record["bbox"] = json!(engine.document().occurrence_aabb(path));
        }
        output.push(dynamic(&record)?);
    }
    Ok(output)
}

/// Evaluates against an isolated command engine, recording only registry calls.
/// A failure discards the plan; successful calls replay in one transaction.
pub(crate) fn plan(
    engine: Engine,
    source: &str,
    args: &Value,
    entry: Option<&str>,
    limits: ScriptLimits,
    control: ScriptControl,
) -> Result<Plan, CommandError> {
    if limits.operations == 0
        || limits.data_bytes == 0
        || source.len() > limits.data_bytes.min(4 * 1024 * 1024)
    {
        return Err(CommandError::Script {
            line: 1,
            col: 1,
            message: "invalid limits or script source exceeds budget".into(),
        });
    }
    let names = engine
        .registry()
        .names()
        .filter(|name| !name.contains(':') && *name != "script-eval")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let doc = json!({"info":engine.document().info,"materials":engine.document().materials,"tags":engine.document().tags,"scenes":engine.document().scenes,"definitions":engine.document().definition_order.iter().filter_map(|id|engine.document().definition(*id)).map(|d|json!({"name":d.name,"uuid":d.uuid,"description":d.description})).collect::<Vec<_>>()});
    let host = Rc::new(RefCell::new(Host {
        engine,
        actions: Vec::new(),
        lines: Vec::new(),
        bytes: source.len().saturating_add(args.to_string().len()),
        limits,
    }));
    let mut runtime = rhai::Engine::new();
    runtime.disable_symbol("eval");
    runtime.register_fn(
        "join",
        |items: &mut Array, separator: ImmutableString| -> String {
            items
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(&separator)
        },
    );
    runtime
        .set_max_operations(limits.operations)
        .set_max_call_levels(32)
        .set_max_expr_depths(64, 64)
        .set_max_variables(256)
        .set_max_functions(256)
        .set_max_string_size(limits.data_bytes.min(1024 * 1024))
        .set_max_array_size(65536)
        .set_max_map_size(4096);
    let stop = control.clone();
    let budget = host.clone();
    runtime.on_progress(move |_| {
        if stop.cancel.is_cancelled()
            || stop.interrupted.as_ref().is_some_and(|f| f())
            || budget
                .try_borrow()
                .is_ok_and(|h| h.bytes > h.limits.data_bytes)
        {
            Some("script cancelled or time limit exceeded".into())
        } else {
            None
        }
    });
    let h = host.clone();
    runtime.on_print(move |line| {
        if let Ok(mut h) = h.try_borrow_mut()
            && h.charge(line.len()).is_ok()
        {
            h.lines.push(line.into());
        }
    });
    for name in names {
        let function = name.replace('-', "_");
        let h = host.clone();
        runtime.register_fn(function, move |params: Map| -> ScriptResult<Dynamic> {
            h.borrow_mut()
                .command(&name, value(&Dynamic::from_map(params))?)
        });
    }
    let h = host.clone();
    runtime.register_fn(
        "command",
        move |name: ImmutableString, params: Map| -> ScriptResult<Dynamic> {
            if name == "script-eval" {
                return Err(exception("nested script evaluations are unavailable"));
            }
            h.borrow_mut()
                .command(&name, value(&Dynamic::from_map(params))?)
        },
    );
    for (name, factor) in [
        ("mm", 1.0),
        ("cm", 10.0),
        ("m", 1000.0),
        ("inch", 25.4),
        ("ft", 304.8),
        ("rad", 1.0),
        ("deg", std::f64::consts::PI / 180.0),
    ] {
        runtime.register_fn(name, move |n: Dynamic| -> ScriptResult<f64> {
            Ok(number(n)? * factor)
        });
    }
    runtime.register_fn("sel", |q: ImmutableString| -> ScriptResult<Dynamic> {
        dynamic(&json!({"Query":q.as_str()}))
    });
    runtime.register_fn(
        "pt",
        |x: Dynamic, y: Dynamic, z: Dynamic| -> ScriptResult<Array> {
            Ok(vec![
                number(x)?.into(),
                number(y)?.into(),
                number(z)?.into(),
            ])
        },
    );
    runtime.register_fn(
        "vec",
        |x: Dynamic, y: Dynamic, z: Dynamic| -> ScriptResult<Array> {
            Ok(vec![
                number(x)?.into(),
                number(y)?.into(),
                number(z)?.into(),
            ])
        },
    );
    runtime.register_fn(
        "plane",
        |origin: Array, normal: Array| -> ScriptResult<Dynamic> {
            let parse = |a: Array| -> ScriptResult<glam::DVec3> {
                if a.len() != 3 {
                    return Err(exception("plane vectors need three coordinates"));
                }
                Ok(glam::DVec3::new(
                    number(a[0].clone())?,
                    number(a[1].clone())?,
                    number(a[2].clone())?,
                ))
            };
            let origin = parse(origin)?;
            let normal = parse(normal)?;
            if normal.length_squared() < 1e-12 {
                return Err(exception("plane normal is zero"));
            }
            dynamic(&crate::math::Plane::from_point_normal(origin, normal))
        },
    );
    let h = host.clone();
    runtime.register_fn("query", move |q: ImmutableString| -> ScriptResult<Array> {
        query(&h.borrow().engine, &q)
    });
    let h = host.clone();
    runtime.register_fn("set_attr",move |entity:Map,name:ImmutableString,v:Dynamic|->ScriptResult<Dynamic> {
        let (dictionary,key)=name.rsplit_once('.').ok_or_else(||exception("attribute name needs dictionary.key"))?;
        let path=entity.get("path").and_then(|p|p.clone().try_cast::<ImmutableString>()).ok_or_else(||exception("expected an entity returned by query"))?;
        h.borrow_mut().command("attribute-set",json!({"targets":path.as_str(),"dictionary":dictionary,"key":key,"value":value(&v)?}))
    });
    let h = host.clone();
    runtime.register_fn(
        "get_attr",
        move |entity: Map, name: ImmutableString| -> ScriptResult<Dynamic> {
            let (dictionary, key) = name
                .rsplit_once('.')
                .ok_or_else(|| exception("attribute name needs dictionary.key"))?;
            let path = entity
                .get("path")
                .and_then(|p| p.clone().try_cast::<ImmutableString>())
                .ok_or_else(|| exception("expected an entity returned by query"))?;
            let h = h.borrow();
            let path = EntityPath::parse(&path, h.engine.document()).map_err(exception)?;
            let resolved = h.engine.document().resolve(&path).map_err(exception)?;
            let attribute = h
                .engine
                .document()
                .container(resolved.container)
                .and_then(|e| e.attrs(resolved.entity))
                .and_then(|a| a.get(dictionary, key));
            dynamic(&attribute.map(attributes::json))
        },
    );
    let h = host.clone();
    runtime.register_fn("bbox", move |q: ImmutableString| -> ScriptResult<Dynamic> {
        let h = h.borrow();
        let items = query(&h.engine, &q)?;
        let mut points = Vec::new();
        for item in items {
            let item = value(&item)?;
            for key in ["min", "max"] {
                if let Ok(p) = serde_json::from_value::<[f64; 3]>(item["bbox"][key].clone()) {
                    points.push(glam::DVec3::from_array(p));
                }
            }
        }
        dynamic(&crate::math::Aabb::from_points(points))
    });
    let h = host.clone();
    runtime.register_fn(
        "log",
        move |level: ImmutableString, message: ImmutableString| {
            let mut h = h.borrow_mut();
            let text = format!("[{level}] {message}");
            if h.charge(text.len()).is_ok() {
                h.lines.push(text);
            }
        },
    );
    let h = host.clone();
    runtime.register_fn(
        "select",
        move |items: Array, mode: ImmutableString| -> ScriptResult<Dynamic> {
            let mut paths = Vec::new();
            for item in items {
                let v = value(&item)?;
                paths.push(
                    v["path"]
                        .as_str()
                        .ok_or_else(|| exception("select expects query results"))?
                        .to_string(),
                );
            }
            let query = if paths.is_empty() {
                "!all".into()
            } else {
                paths.join(" | ")
            };
            h.borrow_mut()
                .command("select", json!({"query":query,"mode":mode.as_str()}))
        },
    );
    let h = host.clone();
    runtime.register_fn("context", move || -> String {
        h.borrow().engine.context().path.to_string()
    });
    let h = host.clone();
    runtime.register_fn("enter", move |path: ImmutableString| -> ScriptResult<()> {
        let mut h = h.borrow_mut();
        let p = EntityPath::parse(&path, h.engine.document()).map_err(exception)?;
        h.context(EditContext {
            path: p.instances.child(p.target),
        })
    });
    let h = host.clone();
    runtime.register_fn("exit_context", move || -> ScriptResult<()> {
        let mut h = h.borrow_mut();
        let path = h
            .engine
            .context()
            .path
            .parent()
            .unwrap_or_else(InstancePath::root);
        h.context(EditContext { path })
    });
    runtime.register_fn(
        "group",
        |context: NativeCallContext, _label: ImmutableString, f: FnPtr| -> ScriptResult<Dynamic> {
            f.call_within_context(&context, ())
        },
    );
    let stop = control.clone();
    runtime.register_fn("cancelled", move || -> bool {
        stop.cancel.is_cancelled() || stop.interrupted.as_ref().is_some_and(|f| f())
    });
    let h = host.clone();
    runtime.register_fn(
        "progress",
        move |fraction: Dynamic, message: ImmutableString| -> ScriptResult<()> {
            let fraction = number(fraction)?;
            if !(0.0..=1.0).contains(&fraction) {
                return Err(exception("progress fraction must be between zero and one"));
            }
            let text = format!("{:.0}% {message}", fraction * 100.0);
            let mut h = h.borrow_mut();
            h.charge(text.len())?;
            h.lines.push(text);
            Ok(())
        },
    );
    let mut scope = rhai::Scope::new();
    scope.push_constant("args", dynamic(args).map_err(script_error)?);
    scope.push_constant("doc", dynamic(&doc).map_err(script_error)?);
    let ast = runtime
        .compile_with_scope(&scope, source)
        .map_err(|e| script_error(e.into()))?;
    let mut result = runtime
        .eval_ast_with_scope::<Dynamic>(&mut scope, &ast)
        .map_err(script_error)?;
    if let Some(entry) = entry {
        result = runtime
            .call_fn_with_options::<Dynamic>(
                rhai::CallFnOptions::new().eval_ast(false),
                &mut scope,
                &ast,
                entry,
                (dynamic(args).map_err(script_error)?,),
            )
            .map_err(script_error)?;
    }
    let value = value(&result).map_err(script_error)?;
    drop(runtime);
    let mut host = host.borrow_mut();
    host.charge(value.to_string().len()).map_err(script_error)?;
    let actions = std::mem::take(&mut host.actions);
    let commands = actions
        .iter()
        .filter(|a| matches!(a, Action::Command(..)))
        .count();
    Ok(Plan {
        actions,
        output: ScriptOutput {
            value,
            output: std::mem::take(&mut host.lines),
            commands,
        },
    })
}

pub(crate) fn replay(
    tx: &mut Transaction<'_>,
    registry: &CommandRegistry,
    actions: Vec<Action>,
) -> Result<(), CommandError> {
    for action in actions {
        match action {
            Action::Command(name, params) => {
                tx.execute_nested(registry, &name, params)?;
            }
            Action::Context(context) => tx.enter_script_context(context)?,
        }
    }
    Ok(())
}

/// Executes a sandboxed script atomically as one undo step. Bare numbers are
/// millimeters and radians. File and network APIs are absent.
pub fn eval(
    engine: &mut Engine,
    source: &str,
    args: &Value,
    limits: ScriptLimits,
    control: ScriptControl,
) -> Result<ScriptOutput, CommandError> {
    let registry = engine.registry_handle();
    let mut preview = Engine::new(engine.document().fork_for_preview(), registry.clone());
    preview.set_selection(engine.selection().clone());
    preview.set_context(engine.context().clone())?;
    preview.set_last_created(engine.last_created().to_vec());
    let plan = plan(preview, source, args, None, limits, control)?;
    let meta = CommandMeta {
        name: "script.eval".into(),
        category: CommandCategory::Edit,
        label: "Script".into(),
        view_only: false,
        steps: Vec::new(),
    };
    engine.run(&meta, |tx| replay(tx, &registry, plan.actions))?;
    Ok(plan.output)
}
