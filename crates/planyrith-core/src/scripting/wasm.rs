//! Implements docs/10-extensions-api.md §7 native plugins: WebAssembly
//! modules run by a sandboxed interpreter with the host API of §5. Like a
//! script, a plugin callback plans registry calls against an isolated copy of
//! the document; the calls replay in one transaction when it returns, and a
//! failure commits nothing.
//!
//! The module ABI (docs/10 §7.1): the module exports its `memory`,
//! `planyrith_alloc(size: i32) -> i32` and one function per callback,
//! `(args_ptr: i32, args_len: i32) -> i64`, which receives its arguments as
//! UTF-8 JSON and returns `ptr << 32 | len` of its JSON result (0 for none).
//! It may import from module `planyrith`:
//! `call(name_ptr, name_len, params_ptr, params_len) -> i64` (a registry
//! command; the answer is `{"ok": output}` or `{"error": {...}}`),
//! `query(ptr, len) -> i64` (entities matching a query: `{"ok": [...]}`) and
//! `log(ptr, len)`. Answers are written into memory the host obtains from
//! `planyrith_alloc`. Operations are metered (the script operation budget)
//! and memory is limited to 256 MiB; there is no filesystem, network, clock
//! or randomness.

use serde_json::{Value, json};
use wasmi::{Caller, Config, Engine as Vm, Extern, Linker, Memory, Module, Store, StoreLimits};

use crate::commands::{CommandError, Engine};
use crate::scripting::{Action, Plan, ScriptLimits, ScriptOutput, query_records};

/// Largest linear memory a plugin may grow to.
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

struct Host {
    engine: Engine,
    actions: Vec<Action>,
    lines: Vec<String>,
    bytes: usize,
    data_limit: usize,
    limits: StoreLimits,
}

fn plugin_error(message: impl std::fmt::Display) -> CommandError {
    CommandError::Script {
        line: 1,
        col: 1,
        message: format!("plugin: {message}"),
    }
}

fn compile(vm: &Vm, bytes: &[u8]) -> Result<Module, CommandError> {
    Module::new(vm, bytes).map_err(|e| plugin_error(format!("invalid module: {e}")))
}

fn vm() -> Vm {
    let mut config = Config::default();
    config.consume_fuel(true);
    Vm::new(&config)
}

/// Checks a module without running it: it compiles and exports `memory`,
/// `planyrith_alloc` and every callback in `callbacks` with the ABI's types.
pub(crate) fn check(bytes: &[u8], callbacks: &[&str]) -> Result<(), CommandError> {
    let vm = vm();
    let module = compile(&vm, bytes)?;
    let export = |name: &str| module.exports().find(|e| e.name() == name);
    if !export("memory").is_some_and(|e| e.ty().memory().is_some()) {
        return Err(plugin_error("the module must export its `memory`"));
    }
    let func = |name: &str, params: &[wasmi::ValType], results: &[wasmi::ValType]| {
        export(name)
            .and_then(|e| e.ty().func().cloned())
            .is_some_and(|f| f.params() == params && f.results() == results)
    };
    use wasmi::ValType::{I32, I64};
    if !func("planyrith_alloc", &[I32], &[I32]) {
        return Err(plugin_error(
            "the module must export `planyrith_alloc(i32) -> i32`",
        ));
    }
    for name in callbacks {
        if !func(name, &[I32, I32], &[I64]) {
            return Err(plugin_error(format!(
                "callback `{name}` must be an exported function (i32, i32) -> i64"
            )));
        }
    }
    for import in module.imports() {
        if import.module() != "planyrith" || !matches!(import.name(), "call" | "query" | "log") {
            return Err(plugin_error(format!(
                "unknown import `{}.{}`",
                import.module(),
                import.name()
            )));
        }
    }
    Ok(())
}

fn memory(caller: &Caller<'_, Host>) -> Result<Memory, wasmi::Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmi::Error::new("the module has no memory"))
}

fn read(caller: &Caller<'_, Host>, ptr: i32, len: i32) -> Result<String, wasmi::Error> {
    let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
    if len > MEMORY_LIMIT {
        return Err(wasmi::Error::new("a string is too long"));
    }
    let mut buf = vec![0; len];
    memory(caller)?
        .read(caller, ptr, &mut buf)
        .map_err(|e| wasmi::Error::new(e.to_string()))?;
    String::from_utf8(buf).map_err(|_| wasmi::Error::new("a string is not UTF-8"))
}

/// Copies `bytes` into memory from the module's allocator; `ptr << 32 | len`.
fn give(caller: &mut Caller<'_, Host>, bytes: &[u8]) -> Result<i64, wasmi::Error> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let alloc = caller
        .get_export("planyrith_alloc")
        .and_then(Extern::into_func)
        .ok_or_else(|| wasmi::Error::new("the module has no planyrith_alloc"))?
        .typed::<i32, i32>(&*caller)?;
    let len = i32::try_from(bytes.len()).map_err(|_| wasmi::Error::new("an answer is too long"))?;
    let ptr = alloc.call(&mut *caller, len)?;
    memory(caller)?
        .write(&mut *caller, ptr as u32 as usize, bytes)
        .map_err(|e| wasmi::Error::new(e.to_string()))?;
    Ok((i64::from(ptr as u32) << 32) | i64::from(len as u32))
}

fn charge(host: &mut Host, size: usize) -> Result<(), wasmi::Error> {
    host.bytes = host.bytes.saturating_add(size);
    if host.bytes > host.data_limit {
        return Err(wasmi::Error::new("plugin data budget exceeded"));
    }
    Ok(())
}

fn linker(vm: &Vm) -> Result<Linker<Host>, CommandError> {
    let mut linker = Linker::<Host>::new(vm);
    linker
        .func_wrap(
            "planyrith",
            "call",
            |mut caller: Caller<'_, Host>, np: i32, nl: i32, pp: i32, pl: i32| -> Result<i64, wasmi::Error> {
                let name = read(&caller, np, nl)?;
                let params = read(&caller, pp, pl)?;
                charge(caller.data_mut(), name.len() + params.len())?;
                let answer = if name.contains(':') || name == "script-eval" {
                    json!({"error": {"code": "E1001", "message": format!("`{name}` cannot be called from a plugin"), "param": null}})
                } else {
                    match serde_json::from_str::<Value>(&params) {
                        Err(e) => json!({"error": {"code": "E1001", "message": format!("parameters are not JSON: {e}"), "param": null}}),
                        Ok(params) => {
                            let host = caller.data_mut();
                            match host.engine.execute_erased(&name, params.clone()) {
                                Ok(output) => {
                                    host.actions.push(Action::Command(name, params));
                                    json!({ "ok": output })
                                }
                                Err(e) => json!({"error": {"code": e.code(), "message": e.to_string(), "param": e.param()}}),
                            }
                        }
                    }
                };
                let text = answer.to_string();
                charge(caller.data_mut(), text.len())?;
                give(&mut caller, text.as_bytes())
            },
        )
        .and_then(|l| {
            l.func_wrap(
                "planyrith",
                "query",
                |mut caller: Caller<'_, Host>, ptr: i32, len: i32| -> Result<i64, wasmi::Error> {
                    let source = read(&caller, ptr, len)?;
                    let answer = match query_records(&caller.data().engine, &source) {
                        Ok(records) => json!({ "ok": records }),
                        Err(e) => json!({"error": {"code": "E1001", "message": e, "param": null}}),
                    };
                    let text = answer.to_string();
                    charge(caller.data_mut(), text.len())?;
                    give(&mut caller, text.as_bytes())
                },
            )
        })
        .and_then(|l| {
            l.func_wrap(
                "planyrith",
                "log",
                |mut caller: Caller<'_, Host>, ptr: i32, len: i32| -> Result<(), wasmi::Error> {
                    let line = read(&caller, ptr, len)?;
                    charge(caller.data_mut(), line.len())?;
                    caller.data_mut().lines.push(line);
                    Ok(())
                },
            )
        })
        .map_err(|e| plugin_error(e.to_string()))?;
    Ok(linker)
}

/// Runs callback `entry` of the module `bytes` with `args` against `engine`
/// (an isolated copy of the document); returns the planned calls.
pub(crate) fn plan(
    engine: Engine,
    bytes: &[u8],
    entry: &str,
    args: &Value,
    limits: ScriptLimits,
) -> Result<Plan, CommandError> {
    let vm = vm();
    let module = compile(&vm, bytes)?;
    let args = args.to_string();
    let host = Host {
        engine,
        actions: Vec::new(),
        lines: Vec::new(),
        bytes: args.len(),
        data_limit: limits.data_bytes,
        limits: wasmi::StoreLimitsBuilder::new()
            .memory_size(MEMORY_LIMIT)
            .instances(1)
            .build(),
    };
    let mut store = Store::new(&vm, host);
    store.limiter(|h| &mut h.limits);
    store
        .set_fuel(limits.operations)
        .map_err(|e| plugin_error(e.to_string()))?;
    let trap = |e: wasmi::Error| {
        if e.as_trap_code() == Some(wasmi::TrapCode::OutOfFuel) {
            plugin_error("the operation budget is exceeded")
        } else {
            plugin_error(e)
        }
    };
    let instance = linker(&vm)?
        .instantiate_and_start(&mut store, &module)
        .map_err(trap)?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&store, "planyrith_alloc")
        .map_err(trap)?;
    let memory = instance
        .get_memory(&store, "memory")
        .ok_or_else(|| plugin_error("the module has no memory"))?;
    let len = i32::try_from(args.len()).map_err(|_| plugin_error("arguments are too long"))?;
    let ptr = alloc.call(&mut store, len).map_err(trap)?;
    memory
        .write(&mut store, ptr as u32 as usize, args.as_bytes())
        .map_err(plugin_error)?;
    let callback = instance
        .get_typed_func::<(i32, i32), i64>(&store, entry)
        .map_err(trap)?;
    let packed = callback.call(&mut store, (ptr, len)).map_err(trap)?;
    let (rptr, rlen) = ((packed >> 32) as u32 as usize, packed as u32 as usize);
    let value = if packed == 0 {
        Value::Null
    } else {
        if rlen > limits.data_bytes {
            return Err(plugin_error("the result exceeds the data budget"));
        }
        let mut buf = vec![0; rlen];
        memory.read(&store, rptr, &mut buf).map_err(plugin_error)?;
        serde_json::from_slice(&buf)
            .map_err(|e| plugin_error(format!("the result is not JSON: {e}")))?
    };
    let host = store.into_data();
    let commands = host.actions.len();
    Ok(Plan {
        actions: host.actions,
        output: ScriptOutput {
            value,
            output: host.lines,
            commands,
        },
    })
}
