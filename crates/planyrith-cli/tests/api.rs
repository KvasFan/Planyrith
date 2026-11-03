//! Implements docs/08-cli-specification.md §3 and docs/10-extensions-api.md §6:
//! the command and server API is stable within `schema_version` 1.
//!
//! `tests/golden/api-v1.json` holds every built-in command's parameter and
//! output schema, and the server's methods and event kinds, as 1.0 shipped
//! them. It only ever gains entries (`PLANYRITH_FREEZE_API=1` adds new
//! commands, never changes existing ones). The current API must stay
//! compatible: parameters accept everything they accepted (no removed
//! parameter, no new required one, no narrowed type, value list or range) and
//! outputs keep every field they had, with the same types. Descriptions are not
//! part of the API.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::Path;

use assert_cmd::Command;
use planyrith_core::commands::CommandRegistry;
use serde_json::{Map, Value, json};

const FROZEN: &str = "tests/golden/api-v1.json";
const METHOD_NOT_FOUND: i64 = -32601;

/// Drops what is documentation rather than contract.
fn strip(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for key in ["description", "title", "examples", "$schema"] {
                object.remove(key);
            }
            object.values_mut().for_each(strip);
        }
        Value::Array(items) => items.iter_mut().for_each(strip),
        _ => {}
    }
}

fn capabilities() -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": "server.capabilities", "params": {}});
    let output = Command::cargo_bin("planyrith")
        .unwrap()
        .arg("serve")
        .write_stdin(format!("{frame}\n"))
        .output()
        .unwrap();
    let reply: Value = serde_json::from_slice(
        output
            .stdout
            .split(|b| *b == b'\n')
            .next()
            .expect("a reply"),
    )
    .unwrap();
    reply["result"].clone()
}

fn current() -> Value {
    let registry = CommandRegistry::with_builtins();
    let mut commands = Map::new();
    for info in registry.list() {
        if info.source != "builtin" {
            continue;
        }
        let command = registry.get(&info.name).unwrap();
        let mut entry = json!({
            "params": serde_json::to_value(command.params_schema()).unwrap(),
            "output": serde_json::to_value(command.output_schema()).unwrap(),
        });
        strip(&mut entry);
        commands.insert(info.name, entry);
    }
    let capabilities = capabilities();
    json!({
        "schema_version": capabilities["schema_version"],
        "commands": commands,
        "methods": capabilities["features"]["methods"],
        "events": capabilities["features"]["events"],
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    /// Values callers send: the new schema must accept all the old one did.
    Input,
    /// Values callers read: the new schema must still provide all it did.
    Output,
}

struct Check<'a> {
    old_root: &'a Value,
    new_root: &'a Value,
    role: Role,
    seen: BTreeSet<(String, String)>,
}

fn resolve<'a>(root: &'a Value, mut schema: &'a Value) -> (&'a Value, Option<&'a str>) {
    let mut name = None;
    for _ in 0..32 {
        match schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix("#/$defs/"))
        {
            Some(def) => {
                name = Some(def);
                schema = &root["$defs"][def];
            }
            None => break,
        }
    }
    (schema, name)
}

fn types(schema: &Value) -> Option<BTreeSet<&str>> {
    match schema.get("type")? {
        Value::String(t) => Some([t.as_str()].into()),
        Value::Array(ts) => Some(ts.iter().filter_map(Value::as_str).collect()),
        _ => None,
    }
}

fn values(schema: &Value) -> Option<Vec<&Value>> {
    if let Some(Value::Array(values)) = schema.get("enum") {
        return Some(values.iter().collect());
    }
    schema.get("const").map(|v| vec![v])
}

/// `inner ⊆ outer`, counting integers as numbers.
fn covered(inner: &BTreeSet<&str>, outer: &BTreeSet<&str>) -> bool {
    inner
        .iter()
        .all(|t| outer.contains(t) || (*t == "integer" && outer.contains("number")))
}

impl Check<'_> {
    fn compare(&mut self, old: &Value, new: &Value, path: &str, out: &mut Vec<String>) {
        let (old, old_name) = resolve(self.old_root, old);
        let (new, new_name) = resolve(self.new_root, new);
        if let (Some(a), Some(b)) = (old_name, new_name)
            && !self.seen.insert((a.to_owned(), b.to_owned()))
        {
            return; // Already compared (recursive types).
        }
        let alternatives = |root, s: &Value| -> Vec<Value> {
            match s.get("anyOf").or_else(|| s.get("oneOf")) {
                Some(Value::Array(alts)) => {
                    alts.iter().map(|a| resolve(root, a).0.clone()).collect()
                }
                _ => vec![s.clone()],
            }
        };
        let (olds, news) = (
            alternatives(self.old_root, old),
            alternatives(self.new_root, new),
        );
        if olds.len() == 1 && news.len() == 1 {
            self.single(old, new, path, out);
            return;
        }
        // Every old alternative must still have a compatible new one.
        for (i, o) in olds.iter().enumerate() {
            let mut best: Option<Vec<String>> = None;
            for n in &news {
                let mut trial = Vec::new();
                let seen = self.seen.clone();
                self.compare(o, n, &format!("{path}|{i}"), &mut trial);
                self.seen = seen;
                if trial.is_empty() {
                    best = Some(trial);
                    break;
                }
                if best.as_ref().is_none_or(|b| trial.len() < b.len()) {
                    best = Some(trial);
                }
            }
            out.extend(best.unwrap_or_else(|| vec![format!("{path}|{i}: no counterpart")]));
        }
    }

    fn single(&mut self, old: &Value, new: &Value, path: &str, out: &mut Vec<String>) {
        if new == &Value::Bool(true) || new.as_object().is_some_and(Map::is_empty) {
            return; // Accepts or promises anything.
        }
        if let (Some(o), Some(n)) = (types(old), types(new)) {
            let ok = match self.role {
                Role::Input => covered(&o, &n),
                Role::Output => covered(&n, &o),
            };
            if !ok {
                out.push(format!("{path}: type {o:?} became {n:?}"));
            }
        }
        if let (Some(o), Some(n)) = (values(old), values(new)) {
            for value in o {
                if !n.contains(&value) {
                    out.push(format!("{path}: value {value} removed"));
                }
            }
        } else if values(old).is_none() && values(new).is_some() && self.role == Role::Input {
            out.push(format!("{path}: values restricted"));
        }
        for (low, high) in [
            ("minimum", "maximum"),
            ("exclusiveMinimum", "exclusiveMaximum"),
            ("minItems", "maxItems"),
            ("minLength", "maxLength"),
        ] {
            let bound = |s: &Value, k| s.get(k).and_then(Value::as_f64);
            let (narrower, wider) = match self.role {
                Role::Input => (old, new),
                Role::Output => (new, old),
            };
            // The wider side's bounds must not cut into the narrower side's.
            if let Some(w) = bound(wider, low)
                && bound(narrower, low).is_none_or(|n| n < w)
            {
                out.push(format!("{path}: {low} narrowed"));
            }
            if let Some(w) = bound(wider, high)
                && bound(narrower, high).is_none_or(|n| n > w)
            {
                out.push(format!("{path}: {high} narrowed"));
            }
        }
        let required = |s: &Value| -> BTreeSet<String> {
            s.get("required")
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let (old_required, new_required) = (required(old), required(new));
        let (more, fewer, what) = match self.role {
            Role::Input => (&new_required, &old_required, "now required"),
            Role::Output => (&old_required, &new_required, "no longer always present"),
        };
        for name in more.difference(fewer) {
            out.push(format!("{path}.{name}: {what}"));
        }
        if let Some(Value::Object(old_properties)) = old.get("properties") {
            let empty = Map::new();
            let new_properties = new
                .get("properties")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            for (name, schema) in old_properties {
                match new_properties.get(name) {
                    Some(other) => self.compare(schema, other, &format!("{path}.{name}"), out),
                    None => out.push(format!("{path}.{name}: removed")),
                }
            }
        }
        for key in ["items", "additionalProperties"] {
            if let (Some(o @ Value::Object(_)), Some(n @ Value::Object(_))) =
                (old.get(key), new.get(key))
            {
                self.compare(o, n, &format!("{path}[]"), out);
            }
        }
        if let (Some(Value::Array(o)), Some(Value::Array(n))) =
            (old.get("prefixItems"), new.get("prefixItems"))
        {
            if o.len() == n.len() {
                for (i, (a, b)) in o.iter().zip(n).enumerate() {
                    self.compare(a, b, &format!("{path}[{i}]"), out);
                }
            } else {
                out.push(format!("{path}: tuple length changed"));
            }
        }
    }
}

fn compatible(old: &Value, new: &Value, role: Role) -> Vec<String> {
    let mut check = Check {
        old_root: old,
        new_root: new,
        role,
        seen: BTreeSet::new(),
    };
    let mut out = Vec::new();
    check.compare(old, new, "", &mut out);
    out
}

/// Compact, one command per line: small, and diffs name the command.
fn render(frozen: &Value) -> String {
    let mut out = String::from("{\n");
    for key in ["schema_version", "methods", "events"] {
        out.push_str(&format!("{}:{},\n", json!(key), frozen[key]));
    }
    out.push_str("\"commands\":{\n");
    let commands = frozen["commands"].as_object().unwrap();
    let lines: Vec<String> = commands
        .iter()
        .map(|(name, entry)| format!("{}:{entry}", json!(name)))
        .collect();
    out.push_str(&lines.join(",\n"));
    out.push_str("\n}}\n");
    out
}

#[test]
fn the_api_stays_compatible_with_version_1() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FROZEN);
    let now = current();
    if std::env::var_os("PLANYRITH_FREEZE_API").is_some() {
        // Add what is new; never touch what was frozen.
        let mut frozen = std::fs::read(&path)
            .map(|b| serde_json::from_slice(&b).unwrap())
            .unwrap_or_else(|_| now.clone());
        for (name, entry) in now["commands"].as_object().unwrap() {
            let commands = frozen["commands"].as_object_mut().unwrap();
            commands.entry(name).or_insert_with(|| entry.clone());
        }
        std::fs::write(&path, render(&frozen)).unwrap();
    }
    let frozen: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(frozen["schema_version"], now["schema_version"]);

    let mut problems = Vec::new();
    for (name, entry) in frozen["commands"].as_object().unwrap() {
        let Some(current) = now["commands"].get(name) else {
            problems.push(format!("{name}: removed"));
            continue;
        };
        for (part, role) in [("params", Role::Input), ("output", Role::Output)] {
            problems.extend(
                compatible(&entry[part], &current[part], role)
                    .into_iter()
                    .map(|p| format!("{name} {part}{p}")),
            );
        }
    }
    for list in ["methods", "events"] {
        for item in frozen[list].as_array().unwrap() {
            if !now[list].as_array().unwrap().contains(item) {
                problems.push(format!("{list}: {item} removed"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Every frozen server method still dispatches (whatever its answer to
/// empty parameters).
#[test]
fn every_frozen_method_is_still_served() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FROZEN);
    let frozen: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let methods = frozen["methods"].as_array().unwrap();
    let frames: String = methods
        .iter()
        .enumerate()
        .map(|(i, m)| {
            format!(
                "{}\n",
                json!({"jsonrpc": "2.0", "id": i, "method": m, "params": {}})
            )
        })
        .collect();
    let output = Command::cargo_bin("planyrith")
        .unwrap()
        .arg("serve")
        .write_stdin(frames)
        .output()
        .unwrap();
    let mut answered = 0;
    for line in output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
    {
        let reply: Value = serde_json::from_slice(line).unwrap();
        if reply.get("id").is_none() {
            continue; // An event notification.
        }
        answered += 1;
        assert_ne!(
            reply["error"]["code"].as_i64(),
            Some(METHOD_NOT_FOUND),
            "{}",
            methods[reply["id"].as_u64().unwrap() as usize]
        );
    }
    assert_eq!(answered, methods.len());
}

#[test]
fn the_check_catches_breaking_changes() {
    let params = json!({
        "type": "object",
        "required": ["width"],
        "properties": {
            "width": {"type": "number", "minimum": 0.0},
            "mode": {"enum": ["fast", "exact"]},
            "count": {"type": "integer", "maximum": 10},
        }
    });
    let changed = |edit: &dyn Fn(&mut Value)| {
        let mut new = params.clone();
        edit(&mut new);
        compatible(&params, &new, Role::Input)
    };
    assert!(changed(&|_| {}).is_empty());
    assert!(changed(&|v| v["properties"]["extra"] = json!({"type": "string"})).is_empty());
    assert!(changed(&|v| v["properties"]["count"]["type"] = json!("number")).is_empty());
    assert!(
        changed(&|v| v["properties"]["mode"]["enum"] = json!(["fast", "exact", "draft"]))
            .is_empty()
    );
    assert!(changed(&|v| v["required"] = json!([])).is_empty());
    assert!(
        !changed(&|v| {
            v["properties"].as_object_mut().unwrap().remove("mode");
        })
        .is_empty()
    );
    assert!(!changed(&|v| v["required"] = json!(["width", "mode"])).is_empty());
    assert!(!changed(&|v| v["properties"]["mode"]["enum"] = json!(["fast"])).is_empty());
    assert!(!changed(&|v| v["properties"]["width"]["type"] = json!("integer")).is_empty());
    assert!(!changed(&|v| v["properties"]["width"]["minimum"] = json!(1.0)).is_empty());
    assert!(!changed(&|v| v["properties"]["count"]["maximum"] = json!(5)).is_empty());

    let output = json!({"type": "object", "required": ["moved"], "properties": {
        "moved": {"type": "integer"}, "copies": {"type": "array", "items": {"type": "integer"}}}});
    let produced = |edit: &dyn Fn(&mut Value)| {
        let mut new = output.clone();
        edit(&mut new);
        compatible(&output, &new, Role::Output)
    };
    assert!(produced(&|v| v["properties"]["extra"] = json!({"type": "string"})).is_empty());
    assert!(!produced(&|v| v["required"] = json!([])).is_empty());
    assert!(!produced(&|v| v["properties"]["moved"]["type"] = json!("number")).is_empty());
    assert!(
        !produced(&|v| v["properties"]["copies"]["items"]["type"] = json!("string")).is_empty()
    );

    // An optional choice (`anyOf` [`oneOf` of constants, null]) losing a value.
    let choice = |values: &[&str]| {
        let alts: Vec<Value> = values
            .iter()
            .map(|v| json!({"const": v, "type": "string"}))
            .collect();
        json!({"type": "object", "properties": {"view": {"anyOf": [{"$ref": "#/$defs/View"}, {"type": "null"}]}},
               "$defs": {"View": {"oneOf": alts}}})
    };
    let (old, wider, narrower) = (
        choice(&["top", "front"]),
        choice(&["top", "front", "iso"]),
        choice(&["top"]),
    );
    assert!(compatible(&old, &wider, Role::Input).is_empty());
    assert!(!compatible(&old, &narrower, Role::Input).is_empty());
}
