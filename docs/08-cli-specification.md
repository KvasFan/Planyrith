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
   `import`, `export`, `render`, `validate`, `repair`, `recover`, `script`, `batch`.
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
| `Vec<T>` | repeat the option or `;`-separate (lists of numbers also `,`) | `--points 0,0,0 --points 1m,0,0`, `--factors 2,1.5,1` |
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

### `planyrith import -f model.plyr <input> [--format obj|stl|gltf|dxf|step|image|csv|plyr] [--as group|component:<name>|image|texture] [--units <u>] [--merge-coplanar] [--soften <deg>] [--scale <f>] [--swap-yz] [--at x,y,z]`
Imports into the current context as a group (default) or component. Format autodetected from the
extension. Options are per format (`docs/09 §6`). Output lists created entities and warnings. The verb
runs the `import` command (`planyrith tool import --help` lists its options); `--units` is the
file's unit (`file_units`) and `--z-up` keeps a Y-up format's axes as they are (`--no-swap-yz`).

### `planyrith export -f model.plyr <output> [--format …] [--scene <name>] [--selection-only] [--per-tag] [--triangulate] [--units <u>] [--include-hidden] [--copy-textures <dir>] …`
Exports 3D formats (`obj`, `stl`, `gltf`, `glb`, `dxf`, `step`, `plyr`, `json`), 2D
(`svg`, `pdf`, `dxf` with `--2d --scene <name> --scale 1:50`) and data (`csv` component counts,
`json` document dump). `--format json --pretty` is the debugging dump. Formats other than `json` run
the `export` command (`planyrith tool export --help`): `--ascii` (STL), `--z-up`, `--units` (the
file's unit), `--targets <query>`, `--include-hidden`, and for drawings `--2d` (DXF; SVG and PDF
always are), `--scene <name>`, `--scale 1:50`, `--paper-width <length>`; the document is not
saved.

### `planyrith render -f model.plyr <out.png> [--width 1920 --height 1080] [--scene <name> | --camera 'eye=…;target=…;fov=35'] [--style <name>] [--shadows] [--transparent] [--msaa 4] [--show-hidden] [--deterministic]`
Headless rendering (`docs/06 §11`). `--format png|jpg|webp` from extension. `--all-scenes
<dir>` renders every scene; `--animation <dir> --fps 30` renders scene transitions as frames.

### `planyrith validate -f model.plyr [--json]` / `planyrith repair -f model.plyr [--fix all|merge-vertices,degenerate-faces,dangling-refs]`
`docs/04 §13`. Exit code 3 if problems remain.

### `planyrith recover <model.plyr> [-o out.plyr] [--no-save] [--json]`
Rebuilds the unsaved work an interactive session left for `model.plyr` (`docs/11 §2`): its
autosave snapshot (`model.autosave.plyr`, else the document itself) with the journal
(`model.plyr.journal`) replayed, then saves it in place (or to `-o`) and deletes the autosave.
Output: `{replayed, skipped}` (`skipped` lists the journal rows that failed, `command: error`).
Exit code 4 (`E1403`) when there is nothing to recover. An untitled GUI document is autosaved as
`untitled.plyr` in the application's data folder; pass that path with `-o`.

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
| `server.auth` | `{token}` → `{authenticated}` (socket servers started with `--token`) |

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
| W2006 | damaged or unusable part dropped while loading (journal, unwritable part name, part missing from the container) |

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

- 2026-10-30 — **Editable annotation layout.** `tool text --layout JSON` (also
  `text` parameters in batch/RPC) accepts normalized `screen_position`,
  `world_offset` in mm, `leader_visible`, arrow/color and a `font` request with
  `family`, `bold`, `italic`, `size_pt`, optional `model_height_mm`. Layout survives
  native save/reopen and undo. SKP open uses the same fields for imported labels;
  merged imports retain nested associative targets. Exact external font faces
  may be substituted by the renderer's available font.

- 2026-10-30 — **Native-first SKP working copies.** `.plyr` is the primary
  document format. A whole-model SKP open carries its complete source into
  `-o working.plyr`; a later `-f working.plyr -o restored.skp` save restores
  the original bytes when the document is unchanged. View/geometry/asset/Sheets
  edits invalidate that shortcut; normal conversion writes the edited model
  and returns compatibility notices. Explicit `export --skp-version` retains
  its requested writer version. Full 2017-and-later fidelity remains in progress.

- 2026-10-30 — **Drawing completion.** `tool freehand --points x,y,z` accepts
  repeated point options and `--simplify-tolerance 0.5mm`. `tool rotated-rectangle
  --origin 0,0,0 --edge-end 1000,1000,0 --width 500mm` uses a 90° second side;
  `--angle 60deg --normal 0,0,1` sets an oblique side and plane. Both are typed
  registry commands, JSON-RPC/Rhai functions, CLI goldens and GUI tools (docs/05).

- 2026-10-30 — **SKP desktop entry points.** With the optional converter
  installed, `-f model.skp` opens a model and normal saving retains SKP;
  `-o model.plyr` saves a native copy. `import model.skp` merges into the edit
  context with normal placement/scale and one undo step. `export model.skp`
  writes 2017 by default; `--skp-version 2026` selects the newer container.
  RPC session open/save and the registered import/export commands use the same
  adapter. Incompatible data is listed in W2004 warnings (docs/09 Decisions).

- 2026-10-29 — **Sheets (0.9).** `planyrith sheet <operation>` forwards to the
  schema-driven `sheet-<operation>` command; no separate mutation path exists.
  See docs/15 §5.1 for commands and parameters. `sheet export --out` is the sheet
  file, `--pages all` prints every page, and `--vector|--raster|--hybrid` override
  viewport modes. Bare sheet lengths use paper millimeters independently of model
  display units. The same registry commands are available over RPC and Rhai;
  `sheet-list`/`sheet-refresh` are read-only. Native model `-f` is preserved by
  export, including when `--out` appears after the verb.

- 2026-10-28 — **Automation and solids (0.8).** Solid tool options are `--a` /
  `--b` (cutter then target for Subtract/Trim), or `--targets` for Union/Outer
  Shell, plus `--keep-component`. Sweep takes `--profile` and either `--path`
  or `--path-face`. Intersect Faces takes `--targets` and
  `--scope model|selection|context`. Soften/Smooth takes `--targets`, `--angle`
  (degrees), `--smooth`/`--no-smooth` and `--soften-coplanar`. Every command
  exposes a schema, help and interactive step metadata.
- 2026-10-28 — `planyrith -f model.plyr script file.rhai --arg width=1200`
  runs an atomic script; argument values are JSON when valid and strings
  otherwise. Bare script lengths are millimeters and angles radians. Batch
  lines beginning `script:` pass the rest of that line directly to Rhai, so
  semicolons belong to the script. `tool script-eval --source … --args …`
  is the journaling/recovery form. RPC `script.eval {doc_id,source,args?}`
  returns the usual command envelope with the value, captured output and count.
- 2026-10-28 — `ext check <folder|archive.plyrx>`, `ext install <archive.plyrx>`
  and `ext list` manage extensions. Tools appear as `ext:<id>/<name>` with
  `source: plugin` in command listings. An installed id replaces its bundled
  version. `PLANYRITH_PLUGIN_DIR` overrides the desktop user-data location.
  A plugin's parameter called `steps` uses `--steps <count>`; interactive help
  is then `--tool-steps` instead of `--steps`. Semantic form lengths/angles
  enrich the schema, so their CLI options accept units.
- 2026-10-28 — Native GUI attachment uses the existing framed local socket/TCP
  protocol and token authentication (TCP). Extension Manager › Attach accepts
  a port or Unix socket and a document id (`d1` for `serve -f`, empty creates
  one). Commands, scripts, save and undo/redo run on the server. The authenticated
  `document.snapshot {doc_id}` method returns deterministic native bytes as a
  JSON byte array, generation, canonical selection/context, last pids, path and
  history labels. Snapshots retain the 64 MiB frame limit. Browser attachment
  is unavailable because the browser cannot open a local socket.

- 2026-10-20 — `tool <name>` options are generated from `Params` schemas; no hand-written option parsing per tool.
- 2026-10-20 — JSON-RPC 2.0 over stdio/local socket is the GUI-attach protocol; no HTTP server in 1.0.
- 2026-10-20 — Selection query grammar in §5 is shared by CLI, RPC and scripting.
- 2026-10-20 — Codes added to §9 by the skeleton contract: `E1403` storage error (key missing,
  unreadable, quota — exit code 4), `E1701` operation cancelled (exit code 5), `E1901` internal
  error (exit code 70). `GeometryError::OutOfRange` maps to `E1002`, `GeometryError::NotFound` to
  `E1101`, mesh invariant violations to `E1302`.
- 2026-10-20 — In the core, `Query` is the source string carried by `Sel` parameters (serialized
  as a plain string) and `QueryExpr` is the parsed AST; `Query::parse` validates syntax without a
  document, `QueryExpr::eval` needs the document plus `selection`/`last`/context.
- 2026-10-20 — Query syntax details (§5). Whitespace is allowed around `:` and comparison
  operators and between a function keyword and its `(`; `!` may repeat (`!!a`). Parenthesized
  groups keep their structure, so `(a & b) & c` is not flattened. An unquoted value ends at
  whitespace or one of `| & ( )`; double-quoted values use `\"` and `\\` escapes. In
  `attr.<dict>.<key>` the key is the text after the last dot (dictionary names may contain dots);
  a bare `attr.d.k` (or `attr.d.k:""`) tests that the attribute exists, and `attr.d.k op number`
  compares integer, float and length attributes. A bare plural kind (`faces`) is `UnknownKind`,
  another bare word `UnknownKeyword`, an unknown `word:`/`word>` `UnknownKey`; a known key
  without `:value`/comparison, or with the wrong one (`tag>5`, `area:5`), is a syntax error.
  `#path` syntax is validated at parse time. `QueryExpr::to_source` writes `a & b`, `a | b`,
  `!a`, parentheses only where needed, numbers in core units and `layer` as `tag`.
- 2026-10-20 — Query numbers. The parser has no document, so bare numbers in comparisons are
  millimeters, mm² and mm³. Lengths use the §4 length grammar; areas and volumes accept a number
  followed by `mm`, `cm`, `m`, `in` or `ft` and `2`/`²` (area) or `3`/`³` (volume), e.g.
  `area>40m2`, `volume<2m³`; `id` takes digits only. `=` holds within the length tolerance (for
  areas `length² + 2·length·√A`, for volumes `length³ + 3·length·V^(2/3)`); `>`/`<` need a
  difference beyond it, `>=`/`<=` accept one within it.
- 2026-10-20 — Query domain and order. A query evaluates over the edit context's entities plus
  the occurrences that `children(…)`, `connected(…)`, `bounded-by(…)`, `inside(…)` and `#path`
  terms bring into view; atoms are predicates over that domain and `!q` is the domain minus `q`
  (so `children(group) & face` filters the children, `!children(group)` is the context).
  Results are in tree pre-order: container order (`Entities` order, then mesh edges and faces
  without a pid), a group's/instance's contents right after it.
- 2026-10-20 — Query semantics. `plane:`, `normal:`, `area`, `length` and `inside` measure in the
  edit context's local frame (world at the model root); `volume` is the solid definition's
  volume times the absolute determinant of the placement. `vertex` matches nothing (vertices are
  not entities). `hidden` = own hidden flag or an invisible tag (enclosing occurrences are not
  considered); `visible` is its complement. `selection` matches selected occurrences in the
  domain (`Id`-keyed items are normalized to their pid). `connected(q)` includes the seeds and
  treats a face as touching every vertex of its loops. `inside(q)` returns context entities
  whose bounds lie in the union of the matches' bounds (expanded by the length tolerance),
  excluding the matches; texts, dimensions, section planes and infinite guides have no bounds.
  `name:` globs the group/instance name and its definition's name; `attr.d.k:value` compares
  length attributes numerically and others by their text form. Unknown tags, materials,
  definitions and unresolvable `#path`s match nothing; malformed `plane:`/`normal:` values are
  evaluation errors.
- 2026-10-20 — Paths (§5.3). `#` is optional; the first segment is `Model`. `@42` or `42` is a pid;
  `Name[n]` is the n-th (0-based, default 0) group or instance with that name in container order,
  where an unnamed instance is named by its definition. Names that are all digits, start with
  `@`, or contain whitespace or `/ [ ] " \ | & ( )` are written double-quoted; entities without
  a name (empty names included) are written `@pid`. `display_with` output parses back to the same
  pid-keyed path.
- 2026-10-20 — **CLI draft (0.1).** `tool` options are built at run time from the command's
  `Params` schema: semantic types are recognised by their `$defs` name (`Length`, `Angle`,
  `Point`, `Vector`, `Plane`, `Transform`, `Query`, `EntityRef`), booleans get `--flag` /
  `--no-flag`, `Vec<T>` options repeat or take `;`-separated values, `--params-json` /
  `--params-file` give the whole object (options override its fields), and `--help`, `--schema`,
  `--steps`, `--example` describe the tool. Global options are accepted before or after the verb
  (and after the tool name). `tool` on a path that does not exist starts a new document there;
  `new` refuses to overwrite. `--dry-run` validates and prints `{"dry_run": true, "valid": true}`.
- 2026-10-20 — `Point` values do not accept `@pid` in 0.1 (vertices have no pid); give `x,y,z`.
- 2026-10-20 — **`last` across invocations.** Each journal row (`history.msgpack`) records the
  pids its command created (`created`, absent in older files); opening a document restores `last`
  from the final row, so `tool rectangle …` followed by `tool push-pull --face '{face & last}'`
  works as two separate invocations, like two lines of a batch.
- 2026-10-21 — **What `last` contains.** The entities that received a *new* pid in the previous
  command, except the extra pieces of existing edges it split: those are fragments of an edge the
  user already had (docs/03 Decisions, edge identity), not something the command made. After a
  line drawn as a chord across a face, `last` is the chord's edges and the face split off by it,
  so `erase --targets 'edge & last'` removes just the chord.
- 2026-10-20 — The `render` verb and the `planyrith-render` dependency are behind the CLI's
  optional `render` feature until milestone 0.2, so 0.1 builds do not compile the GPU stack.
  `export` writes `json` only in 0.1 (`E1401`-class error, exit code 4, for other formats).
- 2026-10-22 — Errors from evaluating a query name the parameter that holds it (`targets`,
  `query`, `face`, `edges`), not always `targets`.
- 2026-10-23 — **Dry runs run.** `tool --dry-run` and `batch --dry-run` execute the commands on the
  in-memory session and write and journal nothing, so a dry run reports the errors the real run
  would (a batch line sees what earlier lines created); the output is
  `{"dry_run": true, "valid": true, "output": …}`. This replaces the validate-only dry run of the
  2026-10-20 CLI draft entry for the CLI (`execute_erased` with `"dry_run": true` still only
  validates, for RPC callers that want that). `new` follows the same `--no-save`/`--dry-run`
  rules as the other session verbs and reports its document in the envelope.
- 2026-10-23 — **One JSON document, and streams.** `validate`/`repair` with problems left print a
  single envelope with `"ok": false`, the report under `output` and the error under `error`
  (exit code 3). In `--jsonl` mode a failing batch line is reported as a stream object
  `{"ok": false, "line": N, "command": …, "error": {…}}`, each line object carries the warnings
  its command raised, and the final envelope follows the last line. With `-o -` standard output
  carries the document, so the report goes to standard error. A closed standard output (the
  reader went away) ends the program quietly with the command's own exit code; it is not an
  internal error.
- 2026-10-23 — **Error codes of inputs.** A missing or unreadable batch script or `--params-file`
  is `E1403` (exit 4), like a missing document. A malformed option value is `E1001` and a value
  outside the schema's bounds (`minimum`/`maximum`, integer formats) `E1002`, both naming the
  parameter (exit 2); a malformed `query` is `E1001` with `param: "query"`.
- 2026-10-23 — `query --fields length,area,volume,bbox` and `--stats` measure in the edit
  context's frame, exactly as the `length`/`area`/`volume` predicates do
  (`document::measure_occurrence`); `volume` is reported for solid groups and instances only.
- 2026-10-25 — **`last` after a closed shape.** When a closed shape drawn on a face splits it,
  `last` is the shape's edges and the face inside the shape, not the remaining piece(s) of the old
  face, which keep their identity: the shape's face always takes a new pid and the old pid stays
  on a remaining piece (pids are swapped when the split rule left it on the shape). `push-pull --face '{face & last}'` after drawing a
  groove that cuts a face in three pushes the groove, never an ambiguous pair.
- 2026-10-25 — **Separate invocations equal one batch** in geometry, orientation and pids
  (docs/03 Decisions, slot order); §10's byte identity is promised for the same batch only,
  because group, instance and definition keys reserved for undo inside a session are not saved.
- 2026-10-25 — **`serve` in 0.1** (§3). Milestone 0.1 ships `serve` over standard input/output
  (`--stdio`, the default): every command is reachable through `command.execute`, as AGENTS.md
  requires. The socket transport with `--token`, `command.preview` and `document.render_mesh`
  arrive with the GUI attach mode in 0.2; until then the first two are a usage error and the
  methods answer `E1201`. Details: `doc_id`s are opaque strings (`d1`, `d2`, … in opening order;
  `-f` opens `d1` at start); §9 errors use the number of their code as the JSON-RPC `code`
  (`E1101` → 1101) with `data: {code, param, details}`, and protocol errors the reserved
  JSON-RPC codes; consecutive `command.execute` calls with the same `group` form one undo step
  (closed by a call without it or with another group, by undo/redo, saving or a context change),
  and `group` is recorded in the journal; `deltas_summary` is `{changed, generation, created}`;
  `document.get` returns every `query --fields` field; event notifications
  (`{doc_id, kind, data}`) are written before the reply of the request that caused them;
  `--readonly` refuses commands that change the document, undo/redo and saving (`E1501`).
  `repl` follows with the interactive tools.
- 2026-10-25 — **`render` (0.2).** The `render` feature is on by default (`--no-default-features`
  builds a CLI without the GPU stack). `--camera` takes `;`-separated `key=value` pairs: `eye`,
  `target`, `up` (points with units), `fov` (angle), `height` (length, parallel views) and
  `projection` (`perspective`, `parallel`, `two-point`); unspecified keys keep the document
  camera's values. `--all-scenes <dir>` writes `<scene name>.png` per scene (path separators and
  reserved characters become `_`), from the scene's camera or the document's. `--shadows` is
  accepted and prints a warning until shadows are rendered. A machine without any graphics
  adapter, hardware or software, fails with an I/O-class error (`E1403`, exit 4). JSON output:
  `{images, width, height, adapter}`. `--animation` follows with scene transitions.
- 2026-10-25 — **`serve` transports (0.2)**, completing the 0.1 entry. `--socket <port>` listens on
  127.0.0.1 (port 0 picks a free one) and requires `--token`, since any local user could connect;
  `--socket <path>` creates a Unix socket readable and writable by this user only (mode 0600),
  replacing a stale socket file but refusing one a server still answers on. Socket frames are the
  JSON text prefixed by its byte length as a little-endian `u32` (at most 64 MiB). The server
  prints `listening on <address>` on standard error and serves one client at a time; documents
  stay open across connections, while event subscriptions and authentication are per
  connection. With a token, every method but `server.auth {token}` answers JSON-RPC error
  −32001 until the client authenticated (compared in constant time).
- 2026-10-25 — **`document.render_mesh {doc_id, path?}`** returns the extraction of the model, or of
  the definition a group or component path opens, as JSON: buffers are base64 of little-endian
  arrays (`positions`, `normals`, `uvs` as `f32`; `face_ids`, `indices`, `edges.faces` as `u32`;
  `edges.flags` as `u8`), plus `ranges` (material name or `null`, side, start, end), `bounds`,
  `vertex_count`, `edges.count` and the pids of faces and edges by local index.
  **`command.preview {doc_id, name, params}`** runs the command on a copy of the document with the
  same selection, `last` and context, and returns `{output, containers}` with the render meshes of
  the containers it changed; the document is untouched (allowed with `--readonly`).
- 2026-10-25 — **`repl`** reads lines from standard input: each is a `batch` line run at once as its
  own undo step (an error is printed and the next line is read); `:undo`, `:redo`, `:save`,
  `:json on|off`, `:help`, `:q` (save and quit) and `:q!` (quit without saving); the end of input
  saves like `:q`, unless `--no-save`. A prompt is shown only on a terminal. Completion and line
  editing need a line-editor dependency and are deferred (docs/02 policy).
- 2026-10-25 — **`infer`** (§12): `--from` and `--dir` give the ray; `--ref <path>` (repeatable)
  remembers an entity (an edge with its direction and midpoint, a face with its center);
  `--reference` sets the reference point, `--last` the last placed point, `--plane <normal>` a
  constraint plane through the reference point (or the origin), `--lock x|y|z` an axis lock
  through the reference point; `--tolerance` is the snapping distance at the cursor (10 mm by
  default, standing for the 8-pixel tolerance of the GUI); `--all` lists every candidate, best
  first. JSON output: `{kind, label, point, on, axis, from, locked}`.
- 2026-10-26 — In queries, a dimension's `length` is what it measures (`query dimension --fields
  length`), absent when it is detached.
- 2026-10-26 — **`recover`** (§2, docs/11 §2) takes the document as a positional argument, like
  the spec's `planyrith recover <doc>`, and saves where the other session verbs save (in place,
  `-o`, or not with `--no-save`); the autosave is deleted only after the recovered document was
  written.
