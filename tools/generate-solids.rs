//! Reproduces docs/09 STEP fixtures. Run:
//! cargo run -p planyrith-core --features brep --example generate-solids

use glam::{DAffine3, DVec3};
use planyrith_core::commands::{CommandRegistry, Engine};
use planyrith_core::document::Document;
use planyrith_core::geom::{
    HalfEdgeMesh,
    boolean::{BooleanOp, boolean},
};
use planyrith_core::io::storage::NativeFs;
use planyrith_core::io::{Storage, StorageKey};
use serde_json::json;
use std::sync::Arc;

fn cube(origin: DVec3, size: DVec3) -> Result<HalfEdgeMesh, Box<dyn std::error::Error>> {
    let mut engine = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    engine.execute_erased("rectangle",json!({"corner_a":origin.to_array(),"corner_b":(origin+DVec3::new(size.x,size.y,0.0)).to_array()}))?;
    engine.execute_erased(
        "push-pull",
        json!({"face":{"Query":"face"},"distance":size.z}),
    )?;
    Ok(engine.document().model.mesh().clone())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let storage = NativeFs::new();
    let tol = planyrith_core::Tolerances::default();
    let box_mesh = cube(
        DVec3::new(12.345, 23.456, 34.567),
        DVec3::new(100.125, 200.25, 300.5),
    )?;
    let outer = cube(DVec3::ZERO, DVec3::splat(100.0))?;
    let cutter = cube(DVec3::new(25.0, 25.0, -50.0), DVec3::new(50.0, 50.0, 200.0))?;
    let inner = cube(DVec3::splat(25.0), DVec3::splat(50.0))?;
    let cut = |b: &HalfEdgeMesh| {
        boolean(
            BooleanOp::Subtract,
            &outer,
            &DAffine3::IDENTITY,
            b,
            &DAffine3::IDENTITY,
            &tol,
        )
        .map(|mut r| r.solids.remove(0))
    };
    for (name, mesh) in [
        ("box", box_mesh),
        ("through-hole", cut(&cutter)?),
        ("hollow", cut(&inner)?),
    ] {
        let root = format!("{}/tests/fixtures/step/{name}", env!("CARGO_MANIFEST_DIR"));
        let step = planyrith_core::io::step::write_solids(std::slice::from_ref(&mesh))?;
        let step = step.lines().map(str::trim_end).collect::<Vec<_>>().join("\n") + "\n";
        storage.write(
            &StorageKey::new(format!("{root}.step")),
            step.as_bytes(),
        )?;
        storage.write(
            &StorageKey::new(format!("{root}.mesh.json")),
            &serde_json::to_vec_pretty(&mesh.to_indexed().0)?,
        )?;
    }
    Ok(())
}
