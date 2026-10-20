# 08 — Command-line interface specification

**Audience:** implementing agents. Normative for `planyrith-cli` (binary `planyrith`). The CLI is
the reference frontend: **every** registered command is reachable here, and anything the GUI can
do to a document can be scripted here.

## 1. Invocation model

```
planyrith [GLOBAL OPTIONS] <SUBCOMMAND> [ARGS]
```

Two families of subcommands:

1. **Session verbs** that operate on a document file and exit: `new`, `info`, `query`, `tool`,
   `import`, `export`, `render`, `validate`, `repair`, `script`, `batch`.
2. **Server verbs**: `serve` (JSON-RPC over stdio or a local socket), `repl` (interactive shell).

A **session** = open (or create) a document → apply zero or more commands → save (unless
`--no-save`/`--dry-run`) → print output → exit with a status code.

### 1.1 Global options

| Option | Meaning |
|---|---|
| `-f, --file <path>` | Document to operate on (`.plyr`). Required for most verbs; `-` = stdin (read) and stdout (write with `--out -`). |
| `-o, --out <path>` | Write the result to another file instead of in place. |
| `--no-save` | Apply but do not write (useful with `--json` to inspect outputs). |
| `--dry-run` | Validate only; nothing is applied. |
| `--json` | Machine-readable output on stdout (one JSON document; see §7). |
| `--jsonl` | One JSON object per line (streaming for `batch`, `script`, `events`). |
| `-q, --quiet` / `-v, --verbose` | Log level (stderr). `-vv` enables `tracing` spans. |
| `--units <u>` | Interpret bare numbers as `mm|cm|m|in|ft` (default: document display units). |
| `--context <path>` | Edit context for the session (`docs/08 §5.3`). |
| `--select <query>` | Initial selection (`§5`). |
| `--deterministic` | Fix all seeds/timestamps (CI). |
| `--color <auto|always|never>` | Terminal colors. |
| `--version`, `--help` | Standard. Shell completions via `planyrith completions <shell>`. |

## 2. Session verbs

### `planyrith new [-f out.plyr] [--template <name|path>] [--units <u>] [--title <s>]`
Creates a document from a template (`default`, `architectural-mm`, `woodworking-mm`,
`imperial-ft-in`, `product-mm`, `blank`) and saves it.

### `planyrith info -f model.plyr [--json]`
Prints title, units, counts (edges, faces, groups, instances, definitions, materials, tags,
scenes), bounds, format version, file size, and warnings from a quick validation.

### `planyrith query -f model.plyr <QUERY> [--fields a,b,c] [--json]`
Lists entities matching a selection query (§5) with requested fields: `pid`, `kind`, `path`,
`tag`, `material`, `bbox`, `length`, `area`, `volume`, `name`, `definition`, `transform`,
`attrs[dict.key]`, `is_solid`, `hidden`, `locked`. Also `--count`, `--stats` (sums).

### `planyrith tool <name> [PARAMS] [-f model.plyr]`
Runs one registered command. `PARAMS` are generated from the command's `Params` JSON schema:
every field is a `--kebab-case` option; types map as:

| Param type | CLI syntax | Example |
|---|---|---|
| `Length` | number with optional unit | `--distance 2m`, `--distance 1500` |
| `Angle` | number with `deg`/`rad`, default deg | `--angle 45` |
| `Point` | `x,y,z` with units, or `@pid` (vertex) | `--center 0,0,0 --radius 500mm` |
| `Vector` | `dx,dy,dz` or `x\|y\|z\|-x…` | `--normal z` |
| `Plane` | `origin;normal` or `xy\|yz\|xz[@offset]` or `@pid` (face) | `--plane xy@1200` |
| `EntityRef` | `@pid`, `#path`, or a query in `{}` | `--face @42`, `--face '{face & material:Glass}'` |
| `Sel` | query string (§5) | `--targets 'tag:Walls'` |
| `Count` | integer | `--segments 48` |
| `bool` | flag / `--no-flag` | `--copy`, `--no-auto-fold` |
| enum | lowercase variant | `--mode add` |
| `Transform` | `t=x,y,z;r=axis,angle;s=sx,sy,sz` | `--placement 't=0,0,0;r=z,90'` |
| `Vec<T>` | repeat the option or `;`-separate | `--points 0,0,0 --points 1m,0,0` |
| struct | JSON | `--behavior '{"glue_to":"horizontal"}'` |

Alternative: `--params-json '<json>'` or `--params-file p.json` for the whole `Params`.

Extra flags: `--steps` (print interactive step metadata), `--schema` (JSON schema of params and
output), `--example`. Help lists every option with units and defaults.

Examples:
```sh
planyrith tool rectangle -f house.plyr --corner-a 0,0,0 --corner-b 8m,6m,0
planyrith tool push-pull -f house.plyr --face '{face & plane:xy & area>40m2}' --distance 3m
planyrith tool move -f house.plyr --targets 'name:Chair' --delta 0,1200,0 --copy --copies 4 --array 'linear:between'
planyrith tool material-apply -f house.plyr --targets 'face & normal:z' --material "Oak Floor"
planyrith tool scene-add -f house.plyr --name "Plan" --capture camera,tags
```

### `planyrith import -f model.plyr <input> [--format obj|stl|gltf|dxf|step|image|csv|plyr] [--as group|component:<name>|image] [--units <u>] [--merge-coplanar] [--soften <deg>] [--scale <f>] [--swap-yz] [--at x,y,z]`
Imports into the current context as a group (default) or component. Format autodetected from the
extension. Options are per format (`docs/09 §6`). Output lists created entities and warnings.

### `planyrith export -f model.plyr <output> [--format …] [--scene <name>] [--selection-only] [--per-tag] [--triangulate] [--units <u>] [--include-hidden] [--copy-textures <dir>] …`
Exports 3D formats (`obj`, `stl`, `gltf`, `glb`, `dxf`, `step`, `plyr`, `json`), 2D
(`svg`, `pdf`, `dxf` with `--2d --scene <name> --scale 1:50`) and data (`csv` component counts,
`json` document dump). `--format json --pretty` is the debugging dump.

### `planyrith render -f model.plyr <out.png> [--width 1920 --height 1080] [--scene <name> | --camera 'eye=…;target=…;fov=35'] [--style <name>] [--shadows] [--transparent] [--msaa 4] [--deterministic]`
Headless rendering (`docs/06 §11`). `--format png|jpg|webp` from extension. `--all-scenes
<dir>` renders every scene; `--animation <dir> --fps 30` renders scene transitions as frames.

### `planyrith validate -f model.plyr [--json]` / `planyrith repair -f model.plyr [--fix all|merge-vertices,degenerate-faces,dangling-refs]`
`docs/04 §13`. Exit code 3 if problems remain.

### `planyrith script -f model.plyr <file.rhai|-> [--arg k=v]…`
Runs an embedded-language script (`docs/10`) with the document bound as `doc`.

### `planyrith batch -f model.plyr [<file.plyrb>|-]`
Executes a **command batch**: one command per line, `tool`-style syntax without the `planyrith
tool` prefix, `#` comments, blank lines ignored; `--jsonl` streams one result per line; the whole
batch is a single undo group and a single save. Example:
```
# build a 4-leg table
rectangle --corner-a 0,0,0 --corner-b 1200,800,0
push-pull --face '{face & last}' --distance 30
select 'face & plane:xy@0' ; move --targets selection --delta 0,0,700
```
`;` separates commands on one line. `last` refers to entities created by the previous command.

## 3. Server verbs

### `planyrith serve [--stdio | --socket <path|port>] [-f model.plyr] [--readonly] [--token <secret>]`
JSON-RPC 2.0 server (newline-delimited JSON frames over stdio; or length-prefixed frames over
a Unix socket / localhost TCP). Methods:

| Method | Params → Result |
|---|---|
| `session.open` | `{path}` → `{doc_id, info}`; `session.new`, `session.save`, `session.close` |
| `command.list` | → `[{name, category, label, schema_version}]` |
| `command.schema` | `{name}` → `{params: JsonSchema, output: JsonSchema, meta}` |
| `command.execute` | `{doc_id, name, params, group?: string}` → `{output, warnings, deltas_summary}` |
| `command.preview` | `{doc_id, name, params}` → `{render_mesh_patch}` (for remote GUIs) |
| `history.undo` / `history.redo` / `history.list` | |
| `document.query` | `{doc_id, query, fields}` → `[...]` |
| `document.get` | `{doc_id, pid}` → full entity JSON |
| `document.render_mesh` | `{doc_id, container}` → binary attachment (base64 or side-channel) |
| `selection.get` / `selection.set` / `context.get` / `context.set` | |
| `events.subscribe` | `{kinds}` → notifications `event` with deltas (`docs/01 §8`) |
| `server.capabilities` | → `{schema_version, formats, features}` |

Errors use JSON-RPC error objects with `code` from §9 and `data.details`. Concurrency: one
writer; `command.execute` calls are serialized per document. The GUI's "attach to server" mode
uses exactly this protocol, so it is tested by the GUI's integration tests.

### `planyrith repl -f model.plyr`
Interactive line editor with completion over command names and params, history, `:undo`,
`:redo`, `:q`, `:json on|off`. Each line is a `batch` line.

## 4. Units and numbers (`units.rs`)

- Bare numbers use the session unit (`--units`) or the document's display unit.
- Accepted lengths: `1500`, `1.5m`, `150cm`, `1500mm`, `59in`, `59"`, `4'11"`, `4' 11 1/2"`,
  `4.9ft`. Thousands separators are not accepted. Decimal separator is `.` in the CLI regardless
  of locale.
- Angles: `45`, `45deg`, `0.7854rad`, slopes `1:12`, percentages `8%` (grade).
- Output formatting follows document precision unless `--json` (full `f64`).

## 5. Selection queries (`document::selection::Query`)

A small expression language shared by CLI, RPC and scripting.

```
query   := or
or      := and ( "|" and )*
and     := not ( ("&" | " ") not )*
not     := "!" primary | primary
primary := "(" query ")" | atom
atom    := kind | "@" pid | "#" path | key ":" value | key op number | keyword
kind    := "edge" | "face" | "vertex" | "group" | "instance" | "guide" | "dimension" | "text" | "image" | "section"
key     := "tag" | "material" | "name" | "definition" | "layer"(alias tag) | "attr." dict "." key | "plane" | "normal" | "area" | "length" | "volume" | "id"
op      := ">" | "<" | ">=" | "<=" | "="
keyword := "all" | "selection" | "last" | "solid" | "hidden" | "visible" | "locked" | "connected(" query ")" | "bounded-by(" query ")" | "inside(" query ")" | "children(" query ")"
```
Values with spaces are quoted. `plane:xy@1200` = faces coplanar with Z = 1200 mm; `normal:z`
= faces with +Z normal (`-z` for down); `name:` matches groups/instances/definitions (glob `*`).

### 5.3 Paths
`#` paths address occurrences: `#Model/Kitchen/Cabinet[2]/@42` = the entity with pid 42 inside
the third instance named Cabinet inside the group Kitchen. `--context '#Model/Kitchen'` opens
that group as the edit context (like double-clicking in the GUI).

## 6. File verbs inside `tool` (category `file`)
`tool save`, `tool save-as --path`, `tool revert`, `tool purge-unused` exist for batches; the
session verbs above are the convenient wrappers.

## 7. Output format

Human mode: concise lines, warnings to stderr prefixed `warning:`. JSON mode (one object):
```json
{
  "ok": true,
  "command": "push-pull",
  "output": { "new_faces": [91, 92, 93, 94, 95], "removed_faces": [42], "through_hole": false },
  "warnings": [ { "code": "W2001", "message": "2 coplanar faces merged" } ],
  "document": { "path": "house.plyr", "saved": true, "generation": 1283 },
  "timing_ms": 3.2
}
```
On failure: `{"ok": false, "error": {"code": "E1002", "message": "...", "param": "distance"}}`.

## 8. Exit codes

| Code | Meaning |
|---|---|
| 0 | success (warnings allowed) |
| 1 | usage error (bad arguments) |
| 2 | command error (`CommandError` other than I/O) |
| 3 | validation problems remain (`validate`/`repair`) |
| 4 | I/O or format error (file missing, unreadable, unsupported version) |
| 5 | cancelled / interrupted |
| 70 | internal error (bug; a panic was caught and reported) |

## 9. Error and warning codes (stable)

| Code | Meaning |
|---|---|
| E1001 | invalid parameter value |
| E1002 | parameter out of range / unit mismatch |
| E1101 | entity not found |
| E1102 | ambiguous selection (query matched > 1 where 1 required) |
| E1201 | not applicable (e.g. push-pull on a non-face, sweep profile not perpendicular) |
| E1301 | geometry degenerate (zero-length edge, zero-area face) |
| E1302 | geometry non-robust (boolean failed; see hint) |
| E1401 | document format unsupported / newer than reader |
| E1402 | corrupt container |
| E1501 | permission / read-only session |
| E1601 | script error (with line/col) |
| W2001 | faces merged automatically |
| W2002 | vertices merged by tolerance |
| W2003 | detached dimension anchors |
| W2004 | unsupported import entity skipped |
| W2005 | texture downscaled |

## 10. Determinism and reproducibility

- Same binary + same input file + same batch ⇒ byte-identical output `.plyr` on all platforms
  (`--deterministic` fixes the modified timestamp and any seeds).
- `planyrith export --format json --canonical` emits a canonical JSON (sorted keys, fixed float
  formatting) suitable for diffing in version control.

## 11. Performance expectations

- Startup (no document) < 50 ms; open 100 MB `.plyr` < 3 s; `tool` overhead per command < 1 ms
  beyond the operation itself; `serve` round-trip for a trivial command < 2 ms on localhost.

## 12. Testing hooks

- `planyrith infer --from x,y,z --dir dx,dy,dz [--ref @pid]…` prints the inference result
  (`docs/03 §7`) for golden tests.
- `planyrith debug mesh-dump -f m.plyr --container #path` prints half-edge tables.
- `planyrith debug render-mesh -f m.plyr --container #path --out mesh.bin` dumps extraction output.

## Decisions

- 2026-10-20 — `tool <name>` options are generated from `Params` schemas; no hand-written option parsing per tool.
- 2026-10-20 — JSON-RPC 2.0 over stdio/local socket is the GUI-attach protocol; no HTTP server in 1.0.
- 2026-10-20 — Selection query grammar in §5 is shared by CLI, RPC and scripting.
