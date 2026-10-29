//! docs/12 §1 (fuzz: command sequences) and §2 (invariants, undo/redo
//! determinism) at the engine level: random sequences of 0.1 commands on
//! random targets. After every successful command the document validates
//! clean (docs/04 §13); a failed command leaves it unchanged; replaying the
//! sequence gives the same bytes (docs/08 §10); saving and reopening between
//! commands (separate CLI invocations) gives the same document up to the
//! numbering of the group, instance and definition keys a session reserves
//! for undo; undoing
//! everything restores the empty document and redoing everything the final
//! one.
//!
//! `PLANYRITH_FUZZ_CASES=500 cargo test -p planyrith-core --release
//! random_command_sequences` runs a long session.

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::prelude::*;
use serde_json::{Value, json};

use crate::commands::{CommandRegistry, Engine};
use crate::diagnostics;
use crate::document::Document;
use crate::ids::{EntityId, Pid, PidAllocator};
use crate::io::NoProgress;
use crate::io::export::json::{canonicalize, document_to_json};
use crate::io::native::{WriteOptions, read_document, write_document};

/// One fuzz step. Lattice coordinates are multiples of 100 mm; `pick`
/// chooses among the current top-level entities of the right kind.
#[derive(Debug, Clone)]
enum Cmd {
    Rect([i8; 3], [i8; 2], u8),
    Line([i8; 3], [i8; 3]),
    Circle([i8; 3], u8, u8),
    PushPull(usize, i8),
    Offset(usize, i8),
    Move(Pick, [i8; 3], u8),
    Rotate(Pick, i8, u8),
    Scale(Pick, [u8; 3]),
    Erase(usize, u8),
    Delete(usize),
    Group(bool),
    Explode(usize),
    Hide(usize),
    UnhideAll,
    Tag(usize),
    Paint(usize, u8),
    Solids(u8, usize, usize),
    Intersect,
    Soften(u8),
    Sweep(usize),
    Sheets(u8, usize),
}

/// What a transform applies to.
#[derive(Debug, Clone, Copy)]
enum Pick {
    Face(usize),
    Edge(usize),
    Object(usize),
    All,
}

fn lattice() -> impl Strategy<Value = [i8; 3]> {
    (0i8..6, 0i8..6, prop_oneof![3 => Just(0i8), 1 => 0i8..4]).prop_map(|(x, y, z)| [x, y, z])
}

fn pick() -> impl Strategy<Value = Pick> {
    prop_oneof![
        3 => (0usize..64).prop_map(Pick::Face),
        2 => (0usize..64).prop_map(Pick::Edge),
        2 => (0usize..64).prop_map(Pick::Object),
        1 => Just(Pick::All),
    ]
}

fn cmd() -> impl Strategy<Value = Cmd> {
    prop_oneof![
        4 => (lattice(), (1i8..4, 1i8..4), 0u8..3).prop_map(|(c, (u, v), n)| Cmd::Rect(c, [u, v], n)),
        2 => (lattice(), lattice()).prop_map(|(a, b)| Cmd::Line(a, b)),
        1 => (lattice(), 1u8..3, 0u8..3).prop_map(|(c, r, n)| Cmd::Circle(c, r, n)),
        4 => (0usize..64, prop_oneof![-3i8..0, 1i8..5]).prop_map(|(f, d)| Cmd::PushPull(f, d)),
        1 => (0usize..64, prop_oneof![-2i8..0, 1i8..3]).prop_map(|(f, d)| Cmd::Offset(f, d)),
        2 => (pick(), (-2i8..3, -2i8..3, -1i8..2), 0u8..3)
            .prop_map(|(p, (x, y, z), c)| Cmd::Move(p, [x, y, z], c)),
        1 => (pick(), -3i8..4, 0u8..3).prop_map(|(p, q, c)| Cmd::Rotate(p, q, c)),
        1 => (pick(), (1u8..4, 1u8..4, 1u8..4)).prop_map(|(p, (x, y, z))| Cmd::Scale(p, [x, y, z])),
        2 => (0usize..64, 0u8..4).prop_map(|(e, m)| Cmd::Erase(e, m)),
        1 => (0usize..64).prop_map(Cmd::Delete),
        1 => any::<bool>().prop_map(Cmd::Group),
        1 => (0usize..64).prop_map(Cmd::Explode),
        1 => (0usize..64).prop_map(Cmd::Hide),
        1 => Just(Cmd::UnhideAll),
        1 => (0usize..64).prop_map(Cmd::Tag),
        1 => (0usize..64, 0u8..3).prop_map(|(f, s)| Cmd::Paint(f, s)),
        1 => (0u8..6,0usize..64,0usize..64).prop_map(|(op,a,b)|Cmd::Solids(op,a,b)),
        1 => Just(Cmd::Intersect),
        1 => (0u8..180).prop_map(Cmd::Soften),
        1 => (0usize..64).prop_map(Cmd::Sweep),
        1 => (0u8..6,0usize..8).prop_map(|(op,i)|Cmd::Sheets(op,i)),
    ]
}

fn at(p: [i8; 3]) -> Value {
    json!([
        f64::from(p[0]) * 100.0,
        f64::from(p[1]) * 100.0,
        f64::from(p[2]) * 100.0
    ])
}

fn axis(n: u8) -> Value {
    match n % 3 {
        0 => json!([1.0, 0.0, 0.0]),
        1 => json!([0.0, 1.0, 0.0]),
        _ => json!([0.0, 0.0, 1.0]),
    }
}

/// Top-level pids of one kind, in container order.
fn pids(e: &Engine, keep: impl Fn(EntityId) -> bool) -> Vec<Pid> {
    let model = &e.document().model;
    model
        .order()
        .iter()
        .filter(|&&id| keep(id))
        .filter_map(|&id| model.pid_of(id))
        .collect()
}

fn nth(list: &[Pid], i: usize) -> Option<Pid> {
    (!list.is_empty()).then(|| list[i % list.len()])
}

fn faces(e: &Engine) -> Vec<Pid> {
    pids(e, |id| matches!(id, EntityId::Face(_)))
}

fn edges(e: &Engine) -> Vec<Pid> {
    pids(e, |id| matches!(id, EntityId::Edge(_)))
}

fn objects(e: &Engine) -> Vec<Pid> {
    pids(e, |id| {
        matches!(id, EntityId::Group(_) | EntityId::Instance(_))
    })
}

fn target(e: &Engine, p: Pick) -> Option<String> {
    match p {
        Pick::Face(i) => nth(&faces(e), i).map(|pid| format!("@{}", pid.0)),
        Pick::Edge(i) => nth(&edges(e), i).map(|pid| format!("@{}", pid.0)),
        Pick::Object(i) => nth(&objects(e), i).map(|pid| format!("@{}", pid.0)),
        Pick::All => Some("all".into()),
    }
}

/// The command and its parameters, or `None` when there is nothing to pick.
fn resolve(e: &Engine, c: &Cmd) -> Option<(&'static str, Value)> {
    let face_ref = |i: usize| nth(&faces(e), i).map(|pid| json!({ "Pid": pid.0 }));
    Some(match *c {
        Cmd::Rect(c0, [u, v], n) => {
            let (iu, iv) = match n % 3 {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let mut c1 = c0;
            c1[iu] += u;
            c1[iv] += v;
            (
                "rectangle",
                json!({ "corner_a": at(c0), "corner_b": at(c1) }),
            )
        }
        Cmd::Line(a, b) => ("line", json!({ "points": [at(a), at(b)] })),
        Cmd::Circle(c0, r, n) => (
            "circle",
            json!({ "center": at(c0), "radius": f64::from(r) * 100.0, "normal": axis(n), "segments": 8 }),
        ),
        Cmd::PushPull(i, d) => (
            "push-pull",
            json!({ "face": face_ref(i)?, "distance": f64::from(d) * 100.0 }),
        ),
        Cmd::Offset(i, d) => (
            "offset",
            json!({ "face": face_ref(i)?, "distance": f64::from(d) * 25.0 }),
        ),
        Cmd::Move(p, d, copies) => (
            "move",
            json!({ "targets": target(e, p)?, "delta": at(d), "copy": copies > 0, "copies": copies.max(1) }),
        ),
        Cmd::Rotate(p, q, copies) => (
            "rotate",
            json!({
                "targets": target(e, p)?,
                "center": [0.0, 0.0, 0.0],
                "angle": f64::from(q) * std::f64::consts::FRAC_PI_2,
                "copy": copies > 0,
                "copies": copies.max(1),
            }),
        ),
        Cmd::Scale(p, [x, y, z]) => (
            "scale",
            json!({
                "targets": target(e, p)?,
                "factors": [f64::from(x) * 0.5, f64::from(y) * 0.5, f64::from(z) * 0.5],
            }),
        ),
        Cmd::Erase(i, m) => (
            "erase",
            json!({
                "targets": format!("@{}", nth(&edges(e), i)?.0),
                "mode": (["erase", "soften", "hide", "erase"][usize::from(m % 4)]),
            }),
        ),
        Cmd::Delete(i) => (
            "delete",
            json!({ "targets": format!("@{}", nth(&faces(e), i)?.0) }),
        ),
        Cmd::Group(component) => {
            if faces(e).is_empty() && edges(e).is_empty() {
                return None;
            }
            if component {
                let n = e.document().definitions.len();
                (
                    "make-component",
                    json!({ "targets": "face | edge", "name": format!("C{n}") }),
                )
            } else {
                ("make-group", json!({ "targets": "face | edge" }))
            }
        }
        Cmd::Explode(i) => (
            "explode",
            json!({ "targets": format!("@{}", nth(&objects(e), i)?.0) }),
        ),
        Cmd::Hide(i) => (
            "hide",
            json!({ "targets": format!("@{}", nth(&faces(e), i)?.0) }),
        ),
        Cmd::UnhideAll => ("unhide", json!({ "targets": "all" })),
        Cmd::Tag(i) => {
            let pid = nth(&faces(e), i)?;
            if e.document().tags.by_name("T").is_none() {
                ("tag-create", json!({ "name": "T" }))
            } else {
                (
                    "tag-assign",
                    json!({ "targets": format!("@{}", pid.0), "tag": "T" }),
                )
            }
        }
        Cmd::Solids(op, a, b) => {
            let objects = objects(e);
            let a = nth(&objects, a)?;
            let b = nth(&objects, b)?;
            (
                [
                    "solid-union",
                    "solid-subtract",
                    "solid-intersect",
                    "solid-trim",
                    "solid-split",
                    "outer-shell",
                ][op as usize],
                json!({"a":a.to_string(),"b":b.to_string()}),
            )
        }
        Cmd::Intersect => (
            "intersect-faces",
            json!({"targets":"face","scope":"selection"}),
        ),
        Cmd::Soften(degrees) => (
            "soften-smooth",
            json!({"targets":"edge","angle":f64::from(degrees)*std::f64::consts::PI/180.0,"smooth":true}),
        ),
        Cmd::Sheets(op, i) => {
            let name = format!("S{}", i % 3);
            match op {
                0 => ("sheet-add-page", json!({"page":name})),
                1 => ("sheet-remove-page", json!({"page":name})),
                2 => ("sheet-add-text", json!({"page":name,"text":"Fuzz note"})),
                3 => ("sheet-add-view", json!({"page":name,"scale":"1:20"})),
                4 => (
                    "sheet-layer-set",
                    json!({"page":name,"layer":"Notes","visible":i%2==0}),
                ),
                _ => ("sheet-add-dimension", json!({"page":name})),
            }
        }
        Cmd::Sweep(i) => ("sweep", json!({"profile":face_ref(i)?,"path":"edge"})),
        Cmd::Paint(i, side) => {
            let pid = nth(&faces(e), i)?;
            if e.document().materials.by_name("M").is_none() {
                (
                    "material-create",
                    json!({ "name": "M", "color": "#A0522D" }),
                )
            } else {
                (
                    "material-apply",
                    json!({
                        "targets": format!("@{}", pid.0),
                        "material": "M",
                        "side": (["front", "back", "both"][usize::from(side % 3)]),
                    }),
                )
            }
        }
    })
}

fn bytes(doc: &Document, counter: PidAllocator) -> Vec<u8> {
    let mut doc = doc.clone();
    doc.info.next_pid = counter;
    let opts = WriteOptions {
        deterministic: true,
        include_history: false,
        ..WriteOptions::default()
    };
    write_document(&doc, &[], &opts, &NoProgress).unwrap()
}

/// The document as canonical JSON with the keys a session reserves for undo
/// replaced by stable anchors: groups and instances by their pid (from their
/// container's order list), definitions by their uuid. `[key, value]` lists
/// are sorted by the anchors.
fn shape(doc: &Document, counter: PidAllocator) -> Value {
    fn key(v: &Value) -> Option<(u64, u64)> {
        let map = v.as_object()?;
        (map.len() == 2).then_some(())?;
        Some((map.get("idx")?.as_u64()?, map.get("version")?.as_u64()?))
    }
    type Anchors = BTreeMap<(&'static str, (u64, u64)), Value>;
    fn anchor(v: &mut Value, kind: &'static str, anchors: &Anchors) {
        if let Some(a) = key(v).and_then(|k| anchors.get(&(kind, k))) {
            *v = a.clone();
        }
    }
    fn replace(v: &mut Value, anchors: &Anchors) {
        match v {
            Value::Object(map) => {
                for (name, child) in map.iter_mut() {
                    match name.as_str() {
                        "Group" | "Instance" => anchor(
                            child,
                            if name == "Group" { "group" } else { "instance" },
                            anchors,
                        ),
                        "definition" => anchor(child, "definition", anchors),
                        _ => replace(child, anchors),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|c| replace(c, anchors)),
            _ => {}
        }
    }
    fn sort_pairs(list: &mut Value, kind: &'static str, anchors: &Anchors) {
        if let Some(items) = list.as_array_mut() {
            for item in items.iter_mut() {
                anchor(&mut item[0], kind, anchors);
            }
            items.sort_by_key(|i| i[0].to_string());
        }
    }
    let mut doc = doc.clone();
    doc.info.next_pid = counter;
    let mut value = document_to_json(&doc).unwrap();
    canonicalize(&mut value);

    let mut definitions = BTreeMap::new();
    for pair in value["definitions"].as_array().into_iter().flatten() {
        if let Some(k) = key(&pair[0]) {
            definitions.insert(("definition", k), pair[1]["uuid"].clone());
        }
    }
    let mut containers = vec![value["model"].take()];
    let count = value["definitions"].as_array().map_or(0, Vec::len);
    for i in 0..count {
        containers.push(value["definitions"][i][1]["entities"].take());
    }
    for c in &mut containers {
        let mut anchors = definitions.clone();
        for entry in c["order"].as_array().into_iter().flatten() {
            for kind in ["Group", "Instance"] {
                if let Some(k) = key(&entry[0]["Entity"][kind]) {
                    let name = if kind == "Group" { "group" } else { "instance" };
                    anchors.insert((name, k), entry[1].clone());
                }
            }
        }
        sort_pairs(&mut c["groups"], "group", &anchors);
        sort_pairs(&mut c["instances"], "instance", &anchors);
        replace(c, &anchors);
    }
    let mut containers = containers.into_iter();
    value["model"] = containers.next().unwrap_or_default();
    for i in 0..count {
        value["definitions"][i][1]["entities"] = containers.next().unwrap_or_default();
    }
    for k in value["definition_order"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        anchor(k, "definition", &definitions);
    }
    sort_pairs(&mut value["definitions"], "definition", &definitions);
    value
}

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}

/// Saves and reopens the document, as a CLI session boundary does (`last`
/// carried over through the journal, docs/08 Decisions).
fn reopen(e: &Engine) -> Engine {
    let opts = WriteOptions {
        deterministic: true,
        include_history: false,
        ..WriteOptions::default()
    };
    let file = write_document(e.document(), &[], &opts, &NoProgress).unwrap();
    let loaded = read_document(&file, &NoProgress).unwrap();
    let mut fresh = Engine::new(loaded.document, Arc::new(CommandRegistry::with_builtins()));
    fresh.set_last_created(e.last_created().to_vec());
    fresh
}

/// Runs the sequence; returns the commands that succeeded. With `reload`,
/// the document is saved and reopened after every command.
fn run(e: &mut Engine, cmds: &[Cmd], reload: bool) -> Result<Vec<String>, TestCaseError> {
    let mut applied = Vec::new();
    for c in cmds {
        let Some((name, params)) = resolve(e, c) else {
            continue;
        };
        let counter = e.document().info.next_pid;
        let before = bytes(e.document(), counter);
        match e.execute_erased(name, params.clone()) {
            Ok(_) => {
                let report = diagnostics::validate(e.document());
                prop_assert!(
                    !report.has_errors(),
                    "{name} {params} left an invalid document after {applied:#?}: {report:#?}"
                );
                applied.push(format!("{name} {params}"));
            }
            Err(err) => {
                prop_assert!(
                    bytes(e.document(), counter) == before,
                    "a failed {name} {params} ({err}) changed the document"
                );
            }
        }
        if reload {
            *e = reopen(e);
        }
    }
    Ok(applied)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PLANYRITH_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(24)
    ))]

    /// See the module documentation.
    #[test]
    fn random_command_sequences(cmds in prop::collection::vec(cmd(), 1..14)) {
        let mut e = engine();
        let empty = e.document().clone();
        let applied = run(&mut e, &cmds, false)?;
        let counter = e.document().info.next_pid;
        let last = bytes(e.document(), counter);

        // Replaying gives the same bytes; separate sessions the same document.
        let mut again = engine();
        run(&mut again, &cmds, false)?;
        prop_assert!(bytes(again.document(), counter) == last, "replay differs: {applied:#?}");
        let mut reopened = engine();
        run(&mut reopened, &cmds, true)?;
        let a = shape(reopened.document(), counter);
        let b = shape(e.document(), counter);
        prop_assert!(
            a == b,
            "reopening between commands changes the result of {applied:#?}:\n{}\n{}",
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );

        let steps = e.history().applied();
        for k in 0..steps {
            e.undo().map_err(|err| {
                TestCaseError::fail(format!("undo {k}: {err} after {applied:#?}"))
            })?;
            let report = diagnostics::validate(e.document());
            prop_assert!(!report.has_errors(), "undo {k} left an invalid document: {report:#?}");
        }
        prop_assert!(
            bytes(e.document(), counter) == bytes(&empty, counter),
            "undo all differs from the empty document after {applied:#?}"
        );
        for k in 0..steps {
            e.redo().map_err(|err| {
                TestCaseError::fail(format!("redo {k}: {err} after {applied:#?}"))
            })?;
        }
        prop_assert!(bytes(e.document(), counter) == last, "redo all differs after {applied:#?}");
    }
}
