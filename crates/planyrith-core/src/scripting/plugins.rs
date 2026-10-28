//! Implements docs/10-extensions-api.md §3 plugin manifests and registry tools.

/// The three plugins shipped with the application.
pub fn bundled() -> Result<Vec<Plugin>, CommandError> {
    Ok(vec![
        Plugin::from_files(
            [
                (
                    "plugin.toml".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.stairs/plugin.toml").to_vec(),
                ),
                (
                    "main.rhai".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.stairs/main.rhai").to_vec(),
                ),
                (
                    "params.json".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.stairs/params.json").to_vec(),
                ),
                (
                    "form.json".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.stairs/form.json").to_vec(),
                ),
            ]
            .into_iter()
            .collect(),
        )?,
        Plugin::from_files(
            [
                (
                    "plugin.toml".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.cabinet/plugin.toml")
                        .to_vec(),
                ),
                (
                    "main.rhai".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.cabinet/main.rhai").to_vec(),
                ),
                (
                    "params.json".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.cabinet/params.json")
                        .to_vec(),
                ),
                (
                    "form.json".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.cabinet/form.json").to_vec(),
                ),
            ]
            .into_iter()
            .collect(),
        )?,
        Plugin::from_files(
            [
                (
                    "plugin.toml".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.csv-points/plugin.toml")
                        .to_vec(),
                ),
                (
                    "main.rhai".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.csv-points/main.rhai")
                        .to_vec(),
                ),
                (
                    "params.json".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.csv-points/params.json")
                        .to_vec(),
                ),
                (
                    "form.json".to_string(),
                    include_bytes!("../../../../plugins/org.planyrith.csv-points/form.json")
                        .to_vec(),
                ),
            ]
            .into_iter()
            .collect(),
        )?,
    ])
}

/// The built-in registry with bundled and installed tools. An installed id
/// replaces its bundled version; loading a document never loads code.
pub fn registry(storage: &dyn Storage, root: &str) -> Result<CommandRegistry, CommandError> {
    let mut plugins = BTreeMap::new();
    for plugin in bundled()?.into_iter().chain(installed(storage, root)?) {
        plugins.insert(plugin.manifest.plugin.id.clone(), plugin);
    }
    let mut registry = CommandRegistry::with_builtins();
    for plugin in plugins.into_values() {
        if !storage.exists(&StorageKey::new(format!(
            "{}/{}.disabled",
            root.trim_end_matches('/'),
            plugin.manifest.plugin.id
        )))? {
            plugin.register(&mut registry)?;
        }
    }
    Ok(registry)
}

use super::{ScriptControl, ScriptLimits, plan, replay};
use crate::commands::{
    CommandCategory, CommandError, CommandMeta, CommandRegistry, Engine, ErasedCommand, ToolStep,
    Transaction,
};
use crate::document::Document;
use crate::io::{Storage, StorageKey};
use schemars::Schema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::sync::Arc;

/// Metadata shown by both Extension Managers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInfo {
    /// Stable reverse-DNS identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Semantic version.
    pub version: String,
    /// Authors.
    #[serde(default)]
    pub authors: Vec<String>,
    /// SPDX license.
    pub license: String,
    /// Minimum compatible Planyrith version.
    pub min_planyrith: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Optional project URL.
    #[serde(default)]
    pub homepage: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    script: String,
}
/// Requested host capabilities. Unsupported grants are refused on install.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capabilities {
    /// Granted read roots (reserved for a later host adapter).
    pub filesystem_read: Vec<String>,
    /// Granted write roots (reserved).
    pub filesystem_write: Vec<String>,
    /// Network hosts (post-1.0).
    pub network: Vec<String>,
    /// Clock access (reserved).
    pub time: bool,
    /// Randomness (reserved).
    pub random: bool,
    /// Declarative forms.
    pub ui_forms: bool,
}
/// Declarative interactive step.
#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    /// Instruction to display.
    pub prompt: String,
    /// Value kind.
    pub kind: String,
    /// Optional geometric constraint.
    pub constraint: Option<String>,
}
/// One manifest tool.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    /// Local tool name.
    pub name: String,
    /// Display label.
    pub label: String,
    /// Optional icon resource.
    pub icon: Option<String>,
    /// Registry category.
    #[serde(default = "draw")]
    pub category: CommandCategory,
    /// JSON parameter schema resource.
    pub params_schema: String,
    /// Optional property form resource.
    pub form: Option<String>,
    /// Interactive steps.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Script function to call.
    pub on_execute: String,
}
fn draw() -> CommandCategory {
    CommandCategory::Draw
}
/// Declarative menu item.
#[derive(Debug, Clone, Deserialize)]
pub struct Menu {
    /// Slash-separated menu path.
    pub path: String,
    /// Callback function receiving a parameter map.
    pub on_click: String,
}
/// Declarative toolbar.
#[derive(Debug, Clone, Deserialize)]
pub struct Toolbar {
    /// Display name.
    pub name: String,
    /// `tools:<name>` or `menus:<path>` references.
    pub items: Vec<String>,
}
/// Declarative form panel.
#[derive(Debug, Clone, Deserialize)]
pub struct Panel {
    /// Display title.
    pub name: String,
    /// Form resource.
    pub form: String,
    /// Callback receiving the form values.
    pub on_change: String,
}
/// Parsed, validated plugin manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Metadata.
    pub plugin: PluginInfo,
    entry: Entry,
    /// Requested host capabilities.
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Tools.
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// Menus.
    #[serde(default)]
    pub menus: Vec<Menu>,
    /// Toolbars.
    #[serde(default)]
    pub toolbars: Vec<Toolbar>,
    /// Panels.
    #[serde(default)]
    pub panels: Vec<Panel>,
}

/// Loaded plugin resources. Loading never executes its script.
#[derive(Debug, Clone)]
pub struct Plugin {
    /// Validated manifest.
    pub manifest: Manifest,
    source: String,
    files: BTreeMap<String, Vec<u8>>,
    schemas: Vec<Schema>,
}
fn invalid(reason: impl Into<String>) -> CommandError {
    CommandError::invalid_param("plugin", reason)
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
fn resource<'a>(
    files: &'a BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<&'a [u8], CommandError> {
    if !safe_path(path) {
        return Err(invalid(format!("unsafe resource path `{path}`")));
    }
    files
        .get(path)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid(format!("missing resource `{path}`")))
}

impl Plugin {
    /// Source for a declared callback, evaluated through the script command.
    pub fn callback_source(&self, function: &str) -> Result<String, CommandError> {
        if function.is_empty()
            || !function
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(invalid("invalid callback name"));
        }
        Ok(format!("{}\n{}(args)", self.source, function))
    }
    /// Validates an in-memory plugin folder, including script syntax/schemas.
    pub fn from_files(files: BTreeMap<String, Vec<u8>>) -> Result<Self, CommandError> {
        if files.len() > 256
            || files.values().map(Vec::len).sum::<usize>() > 32 * 1024 * 1024
            || files.keys().any(|p| !safe_path(p))
        {
            return Err(invalid("plugin resource limits or paths are invalid"));
        }
        let text = std::str::from_utf8(resource(&files, "plugin.toml")?)
            .map_err(|e| invalid(e.to_string()))?;
        let manifest: Manifest = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
        let id = &manifest.plugin.id;
        if id.split('.').count() < 2 || !id.split('.').all(identifier) {
            return Err(invalid("id must be a lowercase reverse-DNS identifier"));
        }
        let parse_version = |s: &str| -> Option<(u32, u32, u32)> {
            let v = s
                .split('.')
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            if v.len() != 3 {
                return None;
            }
            Some((v[0], v[1], v[2]))
        };
        if parse_version(&manifest.plugin.version).is_none()
            || parse_version(&manifest.plugin.min_planyrith).is_none_or(|v| v > (0, 8, 0))
        {
            return Err(invalid(
                "invalid version or plugin requires a newer application",
            ));
        }
        if !matches!(
            manifest.plugin.license.as_str(),
            "MIT"
                | "Apache-2.0"
                | "MIT OR Apache-2.0"
                | "BSD-2-Clause"
                | "BSD-3-Clause"
                | "ISC"
                | "Zlib"
                | "MPL-2.0"
        ) {
            return Err(invalid(
                "plugin license is outside the permitted license policy",
            ));
        }
        let c = &manifest.capabilities;
        if !c.ui_forms
            && (!manifest.panels.is_empty() || manifest.tools.iter().any(|t| t.form.is_some()))
        {
            return Err(invalid("declarative forms require the ui_forms capability"));
        }
        if !c.filesystem_read.is_empty()
            || !c.filesystem_write.is_empty()
            || !c.network.is_empty()
            || c.time
            || c.random
        {
            return Err(invalid(
                "this host supports ui_forms only; filesystem, network, time and random grants are unavailable",
            ));
        }
        let source = std::str::from_utf8(resource(&files, &manifest.entry.script)?)
            .map_err(|e| invalid(e.to_string()))?
            .to_owned();
        let mut runtime = rhai::Engine::new();
        runtime.disable_symbol("eval");
        runtime.set_max_expr_depths(64, 64);
        let ast = runtime
            .compile(&source)
            .map_err(|e| invalid(e.to_string()))?;
        let callback = |name: &str| -> Result<(), CommandError> {
            if !ast
                .iter_functions()
                .any(|f| f.name == name && f.params.len() == 1)
            {
                return Err(invalid(format!(
                    "callback `{name}` must be a declared function taking one parameter map"
                )));
            }
            Ok(())
        };
        let mut schemas = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for tool in &manifest.tools {
            callback(&tool.on_execute)?;
            if !identifier(&tool.name) || !names.insert(tool.name.clone()) {
                return Err(invalid("invalid or duplicate tool name"));
            }
            let mut schema: Value = serde_json::from_slice(resource(&files, &tool.params_schema)?)
                .map_err(|e| invalid(e.to_string()))?;
            if let Some(form) = &tool.form {
                let form: Value = serde_json::from_slice(resource(&files, form)?)
                    .map_err(|e| invalid(e.to_string()))?;
                if let Some(fields) = form["fields"].as_array() {
                    for field in fields {
                        if let (Some(key), Some(kind)) =
                            (field["key"].as_str(), field["type"].as_str())
                            && matches!(kind, "length" | "angle")
                        {
                            let name = if kind == "length" { "Length" } else { "Angle" };
                            if let Some(property) = schema["properties"][key].as_object_mut() {
                                property.remove("type");
                                property.insert(
                                    "$ref".into(),
                                    serde_json::json!(format!("#/$defs/{name}")),
                                );
                            }
                            schema["$defs"][name] = serde_json::json!({"type":"number"});
                        }
                    }
                }
            }
            validate_schema(&schema)?;
            schemas.push(Schema::try_from(schema).map_err(|e| invalid(e.to_string()))?);
            if let Some(form) = &tool.form {
                validate_form(resource(&files, form)?)?;
            }
            if let Some(icon) = &tool.icon {
                resource(&files, icon)?;
            }
        }
        for panel in &manifest.panels {
            callback(&panel.on_change)?;
            validate_form(resource(&files, &panel.form)?)?;
        }
        for menu in &manifest.menus {
            callback(&menu.on_click)?;
        }
        for toolbar in &manifest.toolbars {
            for item in &toolbar.items {
                if !item
                    .strip_prefix("tools:")
                    .is_some_and(|name| manifest.tools.iter().any(|t| t.name == name))
                    && !item
                        .strip_prefix("menus:")
                        .is_some_and(|path| manifest.menus.iter().any(|m| m.path == path))
                {
                    return Err(invalid(format!("toolbar item `{item}` does not resolve")));
                }
            }
        }
        Ok(Self {
            manifest,
            source,
            files,
            schemas,
        })
    }

    /// Loads a `.plyrx` archive without extracting files to the filesystem.
    pub fn from_archive(bytes: &[u8]) -> Result<Self, CommandError> {
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(invalid("plugin archive exceeds 32 MiB"));
        }
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| invalid(e.to_string()))?;
        if archive.len() > 256 {
            return Err(invalid("too many plugin resources"));
        }
        let mut files = BTreeMap::new();
        let mut total = 0;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| invalid(e.to_string()))?;
            if file.is_dir() {
                continue;
            }
            let path = file
                .name()
                .map_err(|e| invalid(e.to_string()))?
                .into_owned();
            if !safe_path(&path) || file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
                return Err(invalid("archive contains an unsafe path or symbolic link"));
            }
            if file.size() > 16 * 1024 * 1024 {
                return Err(invalid("plugin resource exceeds 16 MiB"));
            }
            let mut data = Vec::new();
            file.by_ref()
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map_err(|e| invalid(e.to_string()))?;
            total += data.len();
            if total > 32 * 1024 * 1024 {
                return Err(invalid("expanded plugin exceeds 32 MiB"));
            }
            if files.insert(path, data).is_some() {
                return Err(invalid("duplicate resource"));
            }
        }
        Self::from_files(files)
    }

    /// Loads a plugin directory through the storage abstraction.
    pub fn from_folder(storage: &dyn Storage, prefix: &str) -> Result<Self, CommandError> {
        let prefix = format!("{}/", prefix.trim_end_matches('/'));
        let mut files = BTreeMap::new();
        for entry in storage.list(&prefix)? {
            if let Some(path) = entry.key.as_str().strip_prefix(&prefix) {
                files.insert(path.to_string(), storage.read(&entry.key)?);
            }
        }
        Self::from_files(files)
    }

    /// Reads a JSON form resource for the frontend.
    pub fn form(&self, path: &str) -> Result<Value, CommandError> {
        serde_json::from_slice(resource(&self.files, path)?).map_err(|e| invalid(e.to_string()))
    }

    /// Registers each tool under `ext:<plugin-id>/<name>`.
    pub fn register(&self, registry: &mut CommandRegistry) -> Result<(), CommandError> {
        for (i, tool) in self.manifest.tools.iter().enumerate() {
            let name = format!("ext:{}/{}", self.manifest.plugin.id, tool.name);
            if registry.contains(&name) {
                return Err(invalid(format!("duplicate plugin command `{name}`")));
            }
            registry.register_plugin(Box::new(PluginCommand {
                name,
                tool: tool.clone(),
                source: self.source.clone(),
                schema: self.schemas[i].clone(),
            }));
        }
        Ok(())
    }
}

fn validate_form(bytes: &[u8]) -> Result<(), CommandError> {
    let form: Value = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
    let fields = form["fields"]
        .as_array()
        .ok_or_else(|| invalid("form needs fields"))?;
    for field in fields {
        if field["key"].as_str().is_none()
            || !matches!(
                field["type"].as_str(),
                Some(
                    "number"
                        | "int"
                        | "length"
                        | "angle"
                        | "text"
                        | "choice"
                        | "bool"
                        | "color"
                        | "material"
                        | "entity-pick"
                )
            )
        {
            return Err(invalid("invalid form field"));
        }
    }
    Ok(())
}
fn validate_schema(schema: &Value) -> Result<(), CommandError> {
    if schema["type"] != "object" || !schema["properties"].is_object() {
        return Err(invalid("tool schema must be an object with properties"));
    }
    fn supported(root: &Value, schema: &Value, depth: usize) -> Result<(), CommandError> {
        if depth > 32 {
            return Err(invalid("schema nesting exceeds 32"));
        }
        if let Some(reference) = schema["$ref"].as_str()
            && (!reference.starts_with("#/") || root.pointer(&reference[1..]).is_none())
        {
            return Err(invalid(
                "schema references must resolve within the same file",
            ));
        }
        if let Some(object) = schema.as_object() {
            for key in object.keys() {
                if !matches!(
                    key.as_str(),
                    "$schema"
                        | "$id"
                        | "$ref"
                        | "$defs"
                        | "title"
                        | "description"
                        | "default"
                        | "examples"
                        | "type"
                        | "properties"
                        | "required"
                        | "additionalProperties"
                        | "items"
                        | "minItems"
                        | "maxItems"
                        | "minLength"
                        | "maxLength"
                        | "minimum"
                        | "maximum"
                        | "enum"
                        | "const"
                        | "anyOf"
                        | "oneOf"
                        | "allOf"
                ) {
                    return Err(invalid(format!("unsupported schema keyword `{key}`")));
                }
            }
            for key in ["properties", "$defs"] {
                if let Some(children) = schema[key].as_object() {
                    for child in children.values() {
                        supported(root, child, depth + 1)?;
                    }
                }
            }
            if let Some(items) = schema.get("items") {
                supported(root, items, depth + 1)?;
            }
            for key in ["anyOf", "oneOf", "allOf"] {
                if let Some(children) = schema[key].as_array() {
                    for child in children {
                        supported(root, child, depth + 1)?;
                    }
                }
            }
        } else if !schema.is_boolean() {
            return Err(invalid("a schema must be an object or boolean"));
        }
        Ok(())
    }
    supported(schema, schema, 0)
}

fn check_value(
    root: &Value,
    schema: &Value,
    value: &Value,
    path: &str,
    depth: usize,
) -> Result<(), CommandError> {
    let bad = |why: &str| CommandError::invalid_param(path, why);
    if depth > 64 {
        return Err(bad("schema or value nesting exceeds 64"));
    }
    if schema == &Value::Bool(false) {
        return Err(bad("value is forbidden"));
    }
    if let Some(reference) = schema["$ref"].as_str() {
        let target = root
            .pointer(reference.trim_start_matches('#'))
            .ok_or_else(|| bad("invalid schema reference"))?;
        check_value(root, target, value, path, depth + 1)?;
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(choices) = schema[key].as_array() {
            let count = choices
                .iter()
                .filter(|s| check_value(root, s, value, path, depth + 1).is_ok())
                .count();
            if (key == "anyOf" && count == 0)
                || (key == "oneOf" && count != 1)
                || (key == "allOf" && count != choices.len())
            {
                return Err(bad("value does not match the schema alternatives"));
            }
        }
    }
    let has_type = |ty: &str| match ty {
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => false,
    };
    if let Some(ty) = schema["type"].as_str() {
        if !has_type(ty) {
            return Err(bad("wrong type"));
        }
    } else if let Some(types) = schema["type"].as_array()
        && !types.iter().filter_map(Value::as_str).any(has_type)
    {
        return Err(bad("wrong type"));
    }
    if schema
        .get("const")
        .is_some_and(|expected| expected != value)
        || schema["enum"]
            .as_array()
            .is_some_and(|choices| !choices.contains(value))
    {
        return Err(bad("value is outside the allowed choices"));
    }
    if let Some(n) = value.as_f64()
        && (!n.is_finite()
            || schema["minimum"].as_f64().is_some_and(|min| n < min)
            || schema["maximum"].as_f64().is_some_and(|max| n > max))
    {
        return Err(CommandError::OutOfRange {
            name: path.into(),
            reason: "outside the schema bounds".into(),
        });
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if schema["minLength"].as_u64().is_some_and(|min| length < min)
            || schema["maxLength"].as_u64().is_some_and(|max| length > max)
        {
            return Err(bad("string length is outside the schema bounds"));
        }
    }
    if let Some(array) = value.as_array() {
        if schema["minItems"]
            .as_u64()
            .is_some_and(|min| (array.len() as u64) < min)
            || schema["maxItems"]
                .as_u64()
                .is_some_and(|max| (array.len() as u64) > max)
        {
            return Err(bad("array size is outside the schema bounds"));
        }
        if let Some(items) = schema.get("items") {
            for (i, item) in array.iter().enumerate() {
                check_value(root, items, item, &format!("{path}[{i}]"), depth + 1)?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema["required"].as_array() {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(CommandError::invalid_param(key, "required"));
                }
            }
        }
        for (key, value) in object {
            if depth == 0 && matches!(key.as_str(), "context" | "dry_run") {
                continue;
            }
            if let Some(property) = schema["properties"].get(key) {
                check_value(root, property, value, key, depth + 1)?;
            } else if schema["additionalProperties"] == false {
                return Err(CommandError::invalid_param(key, "unknown parameter"));
            } else if schema["additionalProperties"].is_object() {
                check_value(root, &schema["additionalProperties"], value, key, depth + 1)?;
            }
        }
    }
    Ok(())
}
fn params(schema: &Schema, mut value: Value) -> Result<Value, CommandError> {
    let properties = schema.as_value()["properties"]
        .as_object()
        .ok_or_else(|| invalid("schema properties missing"))?;
    let map = value
        .as_object_mut()
        .ok_or_else(|| CommandError::invalid_param("params", "expected an object"))?;
    for (key, field) in properties {
        if !map.contains_key(key)
            && let Some(default) = field.get("default")
        {
            map.insert(key.clone(), default.clone());
        }
    }
    if let Some(required) = schema.as_value()["required"].as_array() {
        for name in required.iter().filter_map(Value::as_str) {
            if !map.contains_key(name) {
                return Err(CommandError::invalid_param(name, "required"));
            }
        }
    }
    for (key, v) in map.iter() {
        if matches!(key.as_str(), "dry_run" | "context") {
            continue;
        }
        let field = properties
            .get(key)
            .ok_or_else(|| CommandError::invalid_param(key, "unknown parameter"))?;
        let field = if let Some(reference) = field["$ref"].as_str() {
            schema
                .as_value()
                .pointer(reference.trim_start_matches('#'))
                .ok_or_else(|| invalid("invalid schema reference"))?
        } else {
            field
        };
        let valid = match field["type"].as_str() {
            Some("number") => v.is_number(),
            Some("integer") => v.as_i64().is_some() || v.as_u64().is_some(),
            Some("boolean") => v.is_boolean(),
            Some("string") => v.is_string(),
            Some("array") => v.is_array(),
            Some("object") => v.is_object(),
            _ => true,
        };
        if !valid {
            return Err(CommandError::invalid_param(key, "wrong type"));
        }
        if let Some(options) = field["enum"].as_array()
            && !options.contains(v)
        {
            return Err(CommandError::invalid_param(
                key,
                "value is outside the allowed choices",
            ));
        }
        if let Some(n) = v.as_f64()
            && (!n.is_finite()
                || field["minimum"].as_f64().is_some_and(|min| n < min)
                || field["maximum"].as_f64().is_some_and(|max| n > max))
        {
            return Err(CommandError::OutOfRange {
                name: key.clone(),
                reason: "outside the schema bounds".into(),
            });
        }
    }
    check_value(schema.as_value(), schema.as_value(), &value, "params", 0)?;
    Ok(value)
}

struct PluginCommand {
    name: String,
    tool: Tool,
    source: String,
    schema: Schema,
}
impl ErasedCommand for PluginCommand {
    fn name(&self) -> &str {
        &self.name
    }
    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: self.name.clone(),
            label: self.tool.label.clone(),
            category: self.tool.category,
            view_only: false,
            steps: self
                .tool
                .steps
                .iter()
                .map(|s| ToolStep {
                    prompt: s.prompt.clone(),
                    value_field: s.kind.clone(),
                    inference: crate::inference::InferenceKind::ALL.to_vec(),
                })
                .collect(),
        }
    }
    fn params_schema(&self) -> Schema {
        self.schema.clone()
    }
    fn output_schema(&self) -> Schema {
        schemars::schema_for!(Value)
    }
    fn validate_json(&self, _: &Document, p: &Value) -> Result<(), CommandError> {
        params(&self.schema, p.clone())?;
        Ok(())
    }
    fn apply_json(&self, tx: &mut Transaction<'_>, p: Value) -> Result<Value, CommandError> {
        let p = params(&self.schema, p)?;
        let registry = Arc::new(CommandRegistry::with_builtins());
        let mut engine = Engine::new(tx.document().fork_for_preview(), registry.clone());
        engine.set_selection(tx.query_env().selection.clone());
        engine.set_last_created(tx.query_env().last.to_vec());
        engine.set_context(tx.context().clone())?;
        let plan = plan(
            engine,
            &self.source,
            &p,
            Some(&self.tool.on_execute),
            ScriptLimits::default(),
            ScriptControl::default(),
        )?;
        replay(tx, &registry, plan.actions)?;
        Ok(plan.output.value)
    }
}

/// Installs a validated archive under its id. Installation does not execute it.
pub fn install(
    storage: &dyn Storage,
    root: &str,
    bytes: &[u8],
) -> Result<PluginInfo, CommandError> {
    let plugin = Plugin::from_archive(bytes)?;
    let key = StorageKey::new(format!(
        "{}/{}.plyrx",
        root.trim_end_matches('/'),
        plugin.manifest.plugin.id
    ));
    storage.write(&key, bytes)?;
    Ok(plugin.manifest.plugin)
}

/// Loads installed archives in stable storage-key order.
pub fn installed(storage: &dyn Storage, root: &str) -> Result<Vec<Plugin>, CommandError> {
    let mut plugins = Vec::new();
    for entry in storage.list(&format!("{}/", root.trim_end_matches('/')))? {
        if entry.key.as_str().ends_with(".plyrx") {
            plugins.push(Plugin::from_archive(&storage.read(&entry.key)?)?);
        }
    }
    Ok(plugins)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_paths_capabilities_callbacks_and_schemas_are_checked() {
        let original = bundled().unwrap().remove(0);
        for replacement in ["ui_forms = false", "ui_forms = true\ntime = true"] {
            let mut files = original.files.clone();
            let manifest = String::from_utf8(files["plugin.toml"].clone())
                .unwrap()
                .replace("ui_forms = true", replacement);
            files.insert("plugin.toml".into(), manifest.into_bytes());
            assert!(Plugin::from_files(files).is_err());
        }
        let mut files = original.files.clone();
        files.insert("../escape".into(), Vec::new());
        assert!(Plugin::from_files(files).is_err());
        let mut files = original.files.clone();
        let manifest = String::from_utf8(files["plugin.toml"].clone())
            .unwrap()
            .replace("on_execute = \"build\"", "on_execute = \"missing\"");
        files.insert("plugin.toml".into(), manifest.into_bytes());
        assert!(Plugin::from_files(files).is_err());
        let schema = serde_json::json!({"type":"object","properties":{"point":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3}},"required":["point"],"additionalProperties":false});
        validate_schema(&schema).unwrap();
        let schema = Schema::try_from(schema).unwrap();
        assert!(params(&schema, serde_json::json!({"point":[1,2,3]})).is_ok());
        for value in [
            serde_json::json!({"point":[1,"bad",3]}),
            serde_json::json!({"point":[1,2]}),
            serde_json::json!({"unknown":1}),
        ] {
            assert!(params(&schema, value).is_err());
        }
    }
}
