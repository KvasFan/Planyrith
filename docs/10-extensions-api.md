# 10 — Extensions and scripting API

**Audience:** implementing agents. Normative. Lives in `planyrith-core::scripting` (feature
`scripting`) and `planyrith-gui::extensions`.

## 1. Principles

1. **Everything through commands.** Scripts and plugins call the same command registry as the
   CLI and GUI; they cannot reach private mutators. Therefore anything scriptable is undoable,
   previewable, deterministic and testable.
2. **Declarative UI contributions.** Plugins describe toolbars, menus, panels and tool steps in a
   manifest; the GUI renders them. Plugins do not draw arbitrary UI in 1.0 (a constrained
   "property form" schema is provided).
3. **Sandboxed.** Scripts have no filesystem/network access unless the host grants a capability.
   Operation and memory limits prevent runaway scripts from freezing the app.
4. **Same on desktop and web.** The scripting language (rhai) and native WebAssembly plugins
   (§7, run by an interpreter) run in both.

## 2. Scripting language binding (rhai)

### 2.1 Entry points
- CLI: `planyrith script -f m.plyr file.rhai --arg k=v`; `planyrith batch` lines may call
  `script:<expr>`.
- GUI: Window › Console (REPL), Extensions menu items bound to script files, startup scripts in
  the user's `scripts/` folder, and plugin manifests.
- RPC: `script.eval { source, args }`.

### 2.2 Bound API (generated from the registry)

```rhai
// Every command becomes a function with the same kebab→snake name and a params map.
let r = push_pull(#{ face: sel("face & last"), distance: mm(3000) });
print(r.new_faces.len());

// Query and selection
let walls = query("group & name:Wall*");       // -> array of EntityRef
select(walls, "replace");
let ctx = context();                           // current edit context path
enter("#Model/Kitchen"); exit_context();

// Units helpers: mm(), cm(), m(), inch(), ft(), deg(), rad()
// Geometry helpers: pt(x,y,z), vec(x,y,z), plane(origin, normal), bbox(sel)
// Document access (read-only views): doc.info, doc.materials, doc.tags, doc.scenes, doc.definitions
// Entity accessors: e.kind, e.pid, e.path, e.tag, e.material, e.bbox, e.area, e.length, e.volume, e.attrs["dict"]["key"], e.vertices (for edges/faces), e.transform (instances)
// Attributes: set_attr(e, "ext.my-plugin.kind", "stair"); get_attr(...)
// Grouping undo: group("Make stairs", || { ... });   // one undo step for all commands inside
// Events (GUI/serve only): on("selection_changed", |sel| { ... }); on("document_changed", |deltas| {...});
// Progress/cancel: progress(0.5, "building"); if cancelled() { return; }
// UI (GUI only, via host capability): message("..."), ask_number("Steps", 12), ask_choice(...), form(schema) -> map
```

- Parameters are validated against the command's JSON schema; errors raise rhai exceptions with
  `code`/`message`/`param`.
- Numbers without a unit helper are interpreted in **millimeters**; angle fields accept `deg()`
  /`rad()` and default to radians to match the core (documented prominently).
- Limits: default 50 M operations per invocation, 256 MB, 60 s wall time (GUI shows a cancel
  dialog); configurable per plugin manifest with user consent.

### 2.3 Determinism
Scripts have no access to time/randomness unless they request the `time`/`random` capabilities;
with `--deterministic` those return fixed values.

## 3. Plugin manifest (`plugin.toml`)

```toml
[plugin]
id = "com.example.stairs"          # reverse-DNS, used as attribute-dictionary prefix "ext.<id>"
name = "Stair Builder"
version = "1.2.0"
authors = ["..."]
license = "MIT"
min_planyrith = "0.8.0"
description = "Parametric stairs as components"
homepage = "https://..."

[entry]
script = "main.rhai"               # rhai entry; defines functions referenced below

[capabilities]                     # all default to false; the user approves on install
filesystem_read = ["$PLUGIN", "$DOCUMENT_DIR"]
filesystem_write = ["$PLUGIN/cache"]
network = []
time = true
random = false
ui_forms = true

[[tools]]                          # interactive tools contributed to the GUI and CLI
name = "stairs"                    # becomes `planyrith tool ext:com.example.stairs/stairs`
label = "Stairs"
icon = "icons/stairs.svg"
category = "draw"
params_schema = "schemas/stairs.json"     # JSON Schema; the CLI generates options from it
steps = [
  { prompt = "Pick stair start point", kind = "point" },
  { prompt = "Pick stair end point", kind = "point", constraint = "ground" },
  { prompt = "Enter total rise", kind = "length" },
]
on_execute = "build_stairs"        # rhai fn (params) -> output map

[[menus]]
path = "Extensions/Stairs/Settings…"
on_click = "open_settings"

[[toolbars]]
name = "Stairs"
items = ["tools:stairs", "menus:Extensions/Stairs/Settings…"]

[[panels]]
name = "Stair Options"
form = "forms/options.json"        # constrained form schema (number, length, angle, text, choice, bool, color, material, entity-pick)
on_change = "options_changed"

[[file_formats]]                   # importer/exporter contributions (scripted)
extension = "stairs.json"
import = "import_stairs"
export = "export_stairs"
```

- Plugins live in `<user data>/planyrith/plugins/<id>/` (desktop) or in IndexedDB (web, zipped).
- Installation: from a `.plyrx` (zip of the plugin folder) via Extension Manager or
  `planyrith ext install file.plyrx`; versions and capability grants are stored per user.
- Plugin tools appear in the CLI as `tool ext:<id>/<name>` and in RPC `command.list` with
  `source: "plugin"`, so the "CLI first" rule extends to plugins automatically.

## 4. Forms schema (constrained UI)

```json
{ "fields": [
  { "key": "rise", "type": "length", "label": "Total rise", "default": "2800mm", "min": "1mm" },
  { "key": "steps", "type": "int", "label": "Steps", "default": 14, "min": 2, "max": 60 },
  { "key": "material", "type": "material", "label": "Tread material" },
  { "key": "handrail", "type": "bool", "label": "Handrail", "default": true },
  { "key": "side", "type": "choice", "options": ["left", "right", "both"], "default": "both" }
] }
```
Rendered by egui in a panel or modal; values are passed to rhai as a map. The CLI exposes the
same form as options.

## 5. Host API for plugins (capability-gated)

| Capability | API |
|---|---|
| `filesystem_read/write` | `read_text(path)`, `read_bytes`, `write_text`, `write_bytes`, `list_dir` within granted roots (`$PLUGIN`, `$DOCUMENT_DIR`, `$USER_DATA`). Web: virtual FS in IndexedDB. |
| `network` | `http_get(url)` to allow-listed hosts only (post-1.0). |
| `ui_forms` | `form(schema)`, `message`, `ask_*`, `progress`. |
| `time` / `random` | `now()`, `rand()` |
| always | commands, queries, attributes, events, `log(level, msg)` |

## 6. Versioning and stability

- Scripting API follows the command `schema_version`; breaking changes bump the major version and
  ship a deprecation shim for one minor release.
- Within `schema_version` 1 (from 1.0), a command keeps its name; its parameters keep accepting
  every value they accepted (new parameters are optional, value lists and ranges only widen) and
  its output keeps every field with the same type (fields and enum values may be added; readers
  ignore what they do not know). Server methods and event kinds are not removed. The frozen
  schemas are in `crates/planyrith-cli/tests/golden/api-v1.json`, checked on every build.
- `planyrith ext check <plugin>` validates a manifest and dry-runs all declared tools' schemas.

## 7. Native WebAssembly plugins (1.3)

A plugin whose manifest says `[entry] wasm = "plugin.wasm"` (instead of `script`) is a
WebAssembly module, written in any language that compiles to it. It contributes tools, menus,
toolbars and panels exactly as a scripted plugin (§3) and has the same host API (§5), with the
same plan-and-replay semantics: a callback runs against an isolated copy of the document, its
registry calls replay in one transaction when it returns, and a failure commits nothing.

### 7.1 Module ABI
- **Exports:** `memory`; `planyrith_alloc(size: i32) -> i32`, which returns `size` bytes the host
  may write; one function per callback named in the manifest, `(args_ptr: i32, args_len: i32) ->
  i64`. A callback receives its parameters as UTF-8 JSON and returns `ptr << 32 | len` of its
  JSON result in its memory (0 for null), which becomes the command's output.
- **Imports** (module `planyrith`, all optional): `call(name_ptr, name_len, params_ptr,
  params_len) -> i64` runs a registry command with JSON parameters and answers
  `{"ok": output}` or `{"error": {"code", "message", "param"}}`; `query(ptr, len) -> i64` answers
  `{"ok": [records]}` with the records scripts get from `query` (§2.2); `log(ptr, len)` adds a
  line to the invocation's output. Answers are written into memory from `planyrith_alloc`, and
  the call returns their `ptr << 32 | len`. Plugin commands (`ext:…`) and `script-eval` cannot be
  called. Any other import makes the module invalid.
- **Checks on load:** the module is validated and its exports and imports checked against the
  manifest; nothing runs until a callback is invoked.
- **Limits:** the script operation budget as fuel (50 million by default; exhausting it is an
  error), the data budget for everything crossing the boundary, linear memory up to 256 MiB, one
  instance. No filesystem, network, clock or randomness exist to import; floating point follows
  the deterministic profile.
- **Callbacks as commands:** a native plugin's menu and panel callbacks are registered as
  `ext:<id>/<callback>` taking the form values as parameters, so the GUI, CLI and RPC run them
  alike (a scripted plugin's still run through `script-eval`).

```wat
(module
  (import "planyrith" "call" (func $call (param i32 i32 i32 i32) (result i64)))
  (memory (export "memory") 1)
  (global $next (mut i32) (i32.const 4096))
  (data (i32.const 0) "rectangle")
  (data (i32.const 16) "{\"corner_a\":[0,0,0],\"corner_b\":[1000,500,0]}")
  (func (export "planyrith_alloc") (param $n i32) (result i32)
    (global.get $next)
    (global.set $next (i32.add (global.get $next) (local.get $n))))
  (func (export "build") (param i32 i32) (result i64)
    (call $call (i32.const 0) (i32.const 9) (i32.const 16) (i32.const 44))))
```

## 8. Security

- Plugins are untrusted. The host enforces capability checks on every call; rhai is configured with
  `disable_symbol("eval")`, no `import` of arbitrary files (modules resolve only within `$PLUGIN`),
  operation/memory/time limits, and no access to `std::process`.
- Documents may embed scripts? **No.** Documents never execute code on open; `ext/` payloads are
  opaque data for plugins that are already installed.

## 9. Testing

- Each bundled example plugin (stairs, parametric cabinet, CSV point importer) has CLI tests
  executing its tools headless.
- Property: for every registered command, the generated rhai binding round-trips a random valid
  `Params` (from the JSON schema) through `command.execute` without schema errors.

## Decisions

- 2026-11-08 — **Native plugins** (§7). An interpreter (`wasmi`, docs/02 §2.6) instead of
  `wasmtime`: pure Rust, small, runs in the web build as well as on the desktop, meters
  operations deterministically and has the deterministic floating-point profile; plugin code is
  slower than compiled code, which suits callbacks that mostly call commands. A core-module ABI
  with JSON across the boundary instead of the component model: the component model needs
  `wasmtime`, and JSON is what scripts, the CLI and RPC already speak, so a plugin needs no
  generated bindings. `min_planyrith` is now checked against the running version rather than a
  fixed 0.8.0.

- 2026-10-28 — **0.8 invocation semantics.** A script plans registry calls on
  an isolated document, then replays them in one transaction. Errors commit
  nothing; the invocation (including nested `group` callbacks) is one undo step.
  Queries observe earlier planned calls. Registry functions use snake_case and
  parameter maps; `command(name,params)` also calls extension tools.
  `script-eval` cannot recursively evaluate scripts. `sel`, unit/point/vector/
  plane helpers, query/select/context/enter/exit, read-only document/entity views,
  `bbox(query)`, namespaced `set_attr`/`get_attr`, print/log and cancellation are
  provided. Console executes one source buffer per invocation, capturing output.
- 2026-10-28 — **Limits.** Default interpreter budget is 50 million operations,
  32 call levels, 256 variables/functions, 1 MiB strings, 65,536 array entries and
  4,096 map entries. Source is capped at 4 MiB; source/argument/output/command
  data share a 256 MiB accounting budget. These are data/interpreter limits,
  not a process heap allocator quota. CLI/GUI script hosts additionally impose
  a 60-second deadline checked between interpreter operations; native geometry
  calls complete before the next interruption check. Progress reports are
  captured with the invocation output. Plugin tools also enforce interpreter
  limits. Raising budgets and asynchronous progress/cancel UI remain future work.
- 2026-10-28 — **0.8 plugin boundary.** Only `ui_forms` is accepted; filesystem,
  network, clock/random capabilities and external module imports are refused.
  No document executes code on open. Archives are validated before writing:
  safe relative paths, no symlinks/duplicates, at most 256 resources, 16 MiB per
  resource and 32 MiB expanded. Installed archives are stored by id; disabled
  markers persist through Extension Manager. The manager refreshes the local
  registry without changing the document or history. Restart a server after
  changing its extensions. Bundled examples are stairs, cabinet and CSV guide
  points; CSV data is a tool parameter, requiring no filesystem grant.
- 2026-10-28 — Forms cover the types in §4; entity fields can take the current
  selection. Menus, optional toolbars and property panels are manifest-driven;
  panel Apply invokes its callback once as an atomic script. Callbacks must be
  declared functions taking one map. Tool schemas support local references,
  primitive/object/array types, required/default values, enums, numeric/size
  bounds and alternatives; unsupported schema keywords are refused on load.
  Persistent event callbacks, startup scripts, scripted format registration,
  dynamic `ask_*`/`form()` dialogs and additional host capabilities are reserved
  beyond 0.8. Declarative forms are the supported UI extension boundary.

- 2026-10-20 — rhai is the single scripting language for 1.0; WASM plugins reserved post-1.0.
- 2026-10-20 — Plugins contribute UI declaratively only; no arbitrary widget drawing in 1.0.
- 2026-10-20 — Documents never execute embedded code.
