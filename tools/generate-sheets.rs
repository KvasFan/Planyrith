//! Reproduces docs/15 §8 house fixtures and target/tmp/sheets exports.
//! Run: cargo run -p planyrith-core --example generate-sheets
use planyrith_core::commands::{CommandRegistry, Engine};
use planyrith_core::document::Document;
use planyrith_core::io::storage::NativeFs;
use planyrith_core::io::{NoProgress, Storage, StorageKey};
use planyrith_core::sheets::{export, layout};
use serde_json::{Value, json};
use std::sync::Arc;
fn run(e: &mut Engine, name: &str, p: Value) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(e.execute_erased(name, p)?)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    run(
        &mut e,
        "rectangle",
        json!({"corner_a":[0,0,0],"corner_b":[6000,4000,0]}),
    )?;
    run(
        &mut e,
        "offset",
        json!({"face":{"Query":"face"},"distance":-200}),
    )?;
    run(
        &mut e,
        "push-pull",
        json!({"face":{"Query":"face & area<5000000"},"distance":2700}),
    )?;
    run(
        &mut e,
        "make-group",
        json!({"targets":"all","name":"Walls"}),
    )?;
    run(
        &mut e,
        "rectangle",
        json!({"corner_a":[2000,1500,0],"corner_b":[3500,2500,0]}),
    )?;
    run(
        &mut e,
        "push-pull",
        json!({"face":{"Query":"face & last"},"distance":750}),
    )?;
    run(
        &mut e,
        "make-group",
        json!({"targets":"face | edge","name":"Table"}),
    )?;
    let cut = run(
        &mut e,
        "section-plane-add",
        json!({"plane":{"normal":[0,0,1],"d":-1200},"name":"Plan Cut"}),
    )?;
    let plan_cut = cut["section_plane"].clone();
    run(
        &mut e,
        "sheet-settings",
        json!({"title_block":{"fields":{"Project":"House","Revision":"A","Date":"2026-10-29"}}}),
    )?;
    let mut section_cut = Value::Null;
    for (page, name, eye, target, up) in [
        (
            "A101",
            "Plan",
            [3000, 2000, 10000],
            [3000, 2000, 0],
            [0, 1, 0],
        ),
        (
            "A201",
            "Section",
            [3000, -10000, 1350],
            [3000, 2000, 1350],
            [0, 0, 1],
        ),
        (
            "A301",
            "Elevation",
            [3000, -10000, 1350],
            [3000, 2000, 1350],
            [0, 0, 1],
        ),
    ] {
        if name == "Section" {
            let cut = run(
                &mut e,
                "section-plane-add",
                json!({"plane":{"normal":[0,-1,0],"d":2000},"name":"Section Cut"}),
            )?;
            section_cut = cut["section_plane"].clone();
        }
        if name == "Elevation" {
            run(
                &mut e,
                "section-plane-toggle",
                json!({"section":{"Pid":section_cut},"active":false}),
            )?;
            run(
                &mut e,
                "section-plane-toggle",
                json!({"section":{"Pid":plan_cut},"active":false}),
            )?;
        }
        run(
            &mut e,
            "camera-set",
            json!({"eye":eye,"target":target,"up":up,"height":6000,"aspect":1.5}),
        )?;
        run(&mut e, "scene-add", json!({"name":name}))?;
        run(&mut e, "sheet-add-page", json!({"page":page}))?;
        run(
            &mut e,
            "sheet-add-view",
            json!({"page":page,"id":"view","scene":name,"scale":"1:50","frame":{"x":20,"y":30,"width":180,"height":140}}),
        )?;
        run(
            &mut e,
            "sheet-add-text",
            json!({"page":page,"text":format!("{name} — 1:50"),"frame":{"x":20,"y":180,"width":180,"height":18}}),
        )?;
        run(
            &mut e,
            "sheet-add-title-block",
            json!({"page":page,"frame":{"x":210,"y":20,"width":70,"height":65}}),
        )?;
        run(
            &mut e,
            "sheet-add-shape",
            json!({"page":page,"shape":"rectangle","points":[[10,10],[287,200]]}),
        )?;
        if name == "Plan" {
            run(
                &mut e,
                "sheet-add-dimension",
                json!({"page":page,"view":"view","model_start":[0,0,0],"model_end":[6000,0,0],"offset":[0,-12]}),
            )?;
            run(
                &mut e,
                "sheet-add-table",
                json!({"page":page,"query":"group","columns":["name","volume"],"frame":{"x":210,"y":100,"width":70,"height":35}}),
            )?;
        }
    }
    let storage = NativeFs::new();
    let root = env!("CARGO_MANIFEST_DIR");
    let saved = planyrith_core::io::native::write_document(
        e.document(),
        &[],
        &Default::default(),
        &NoProgress,
    )?;
    storage.write(
        &StorageKey::new(format!("{root}/tests/fixtures/sheets/house.plyr")),
        &saved,
    )?;
    let pages = e
        .document()
        .sheets
        .pages
        .iter()
        .map(|p| layout::layout_page(e.document(), p, 300, None))
        .collect::<Result<Vec<_>, _>>()?;
    storage.write(
        &StorageKey::new(format!("{root}/tests/fixtures/sheets/house-drawings.json")),
        &serde_json::to_vec_pretty(&pages)?,
    )?;
    let destination = format!("{root}/../../target/tmp/sheets/house.pdf");
    for (path, bytes) in export::files(&pages, &destination, "House", "Planyrith")? {
        storage.write(&StorageKey::new(path), &bytes)?;
    }
    for ext in ["svg", "dxf"] {
        let destination = format!("{root}/../../target/tmp/sheets/house.{ext}");
        for (path, bytes) in export::files(&pages, &destination, "House", "Planyrith")? {
            storage.write(&StorageKey::new(path), &bytes)?;
        }
    }
    Ok(())
}
