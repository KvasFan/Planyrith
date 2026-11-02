//! docs/00 §6 "viewport frame time < 8 ms at 1080p for a 1M-triangle model"
//! (docs/12 Decisions, 2026-11-02). Ignored: run with `cargo test --release
//! -p planyrith-render --test frame_time -- --ignored --nocapture`.
//! `PLANYRITH_BENCH_CELLS` sets the terrain (cells², two triangles each;
//! 707 ≈ one million) and `PLANYRITH_BENCH_FILE` measures a native file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Instant;

use planyrith_core::document::{EditContext, Selection, Style};
use planyrith_core::io::render_mesh::draw_list;
use planyrith_core::io::{MemoryStorage, NoProgress, Storage, StorageKey};
use planyrith_core::{CommandRegistry, Document, Engine};
use planyrith_render::{FrameRequest, Gpu, Renderer, RendererOptions};
use serde_json::json;

/// A terrain of `n` × `n` cells as OBJ text, one meter per cell.
fn terrain_obj(n: u32) -> String {
    let mut s = String::with_capacity((n as usize + 1).pow(2) * 40);
    for j in 0..=n {
        for i in 0..=n {
            let h = (f64::from(i) * 0.37).sin() * (f64::from(j) * 0.23).cos() * 0.5
                + f64::from((i * 7919 + j * 104_729) % 97) / 970.0;
            s.push_str(&format!("v {i} {h:.4} {j}\n"));
        }
    }
    let at = |i: u32, j: u32| j * (n + 1) + i + 1;
    for j in 0..n {
        for i in 0..n {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            s.push_str(&format!("f {a} {b} {c}\nf {a} {c} {d}\n"));
        }
    }
    s
}

fn model() -> Document {
    if let Some(path) = std::env::var_os("PLANYRITH_BENCH_FILE") {
        let bytes = std::fs::read(path).unwrap();
        return planyrith_core::io::native::read_document(&bytes, &NoProgress)
            .unwrap()
            .document;
    }
    let n = std::env::var("PLANYRITH_BENCH_CELLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(707);
    let storage = Arc::new(MemoryStorage::new());
    storage
        .write(&StorageKey::new("t.obj"), terrain_obj(n).as_bytes())
        .unwrap();
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    e.set_storage(storage);
    e.execute_erased("import", json!({ "path": "t.obj", "file_units": "m" }))
        .unwrap();
    e.execute_erased("camera-standard", json!({ "view": "iso", "extents": true }))
        .unwrap();
    e.document().clone()
}

#[test]
#[ignore = "benchmark"]
fn frame_time_benchmark() {
    let gpu = Gpu::headless().expect("a graphics adapter");
    let doc = model();
    let items = draw_list(&doc, &Selection::new(), &EditContext::model(), false);
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let style = Style::default();
    let request = FrameRequest {
        doc: &doc,
        items: &items,
        camera: &doc.camera,
        style: &style,
        width: 1920,
        height: 1080,
        show_hidden: false,
        ids: false,
        transparent_background: false,
        axes: false,
        overlay: None,
    };
    // Mesh extraction alone (core), for the first frame's cost.
    let t = Instant::now();
    let mut triangles = 0;
    for item in &items {
        if let Ok(mesh) =
            planyrith_core::io::render_mesh::extract(&doc, item.container, Default::default())
        {
            triangles += mesh.indices.len() / 3;
        }
    }
    eprintln!(
        "extraction: {:.0} ms for {} draw items, {triangles} triangles",
        t.elapsed().as_secs_f64() * 1e3,
        items.len()
    );
    let wait = || {
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    };
    let t = Instant::now();
    renderer.render(&request).unwrap();
    wait();
    let first = t.elapsed().as_secs_f64();
    let mut times: Vec<f64> = (0..30)
        .map(|_| {
            let t = Instant::now();
            renderer.render(&request).unwrap();
            wait();
            t.elapsed().as_secs_f64()
        })
        .collect();
    times.sort_by(f64::total_cmp);
    eprintln!(
        "{} on {}: first frame {:.0} ms (meshes uploaded); then median {:.2} ms, worst {:.2} ms \
         at 1920 × 1080 (target < 8 ms on an integrated GPU)",
        if std::env::var_os("PLANYRITH_BENCH_FILE").is_some() {
            "file"
        } else {
            "terrain"
        },
        gpu.adapter.name,
        first * 1e3,
        times[times.len() / 2] * 1e3,
        times[times.len() - 1] * 1e3
    );
}
