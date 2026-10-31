//! Edited native -> SKP -> native fidelity regressions for docs/13 0.11.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use planyrith_core::{
    commands::{CommandRegistry, Engine},
    document::annotations::anchor_position,
    document::*,
    geom::CurveDescriptor,
    io::native::{WriteOptions, read_document, write_document},
};
use serde_json::json;
use std::sync::Arc;

fn round_trip(doc: &Document, year: u32) -> Document {
    let encoded = convert(&format!("encode-{year}"), &super::tests::bytes(doc)).unwrap();
    assert!(
        !encoded
            .warnings
            .iter()
            .any(|w| w.contains("annotation/image not transferred")
                || w.contains("curve descriptors are retained as segmented")
                || w.contains("rendering styles"))
    );
    let decoded = convert("decode", &encoded.bytes).unwrap();
    read_document(&decoded.bytes, &NoProgress).unwrap().document
}
#[test]
fn edited_skp_keeps_annotation_fonts_colors_curves_and_live_anchors() {
    let mut e = super::tests::cube();
    for (name, params) in [
        ("text", json!({"at":[500,0,0],"text":"Width"})),
        (
            "dimension",
            json!({"start":[0,0,0],"end":[1000,0,0],"offset":[0,-200,0]}),
        ),
        (
            "circle",
            json!({"center":[2000,0,0],"radius":300,"segments":24}),
        ),
        (
            "dimension",
            json!({"kind":"radius","curve":{"Query":"edge & length<100"},"offset":[200,0,0]}),
        ),
        (
            "label-style",
            json!({"targets":"text | dimension","font":{"family":"Inter","size_pt":16,"bold":true},"color":"#318ac0","arrow":"closed"}),
        ),
    ] {
        e.execute_erased(name, params).unwrap();
    }
    for year in [2017, 2026] {
        let original = convert(
            &format!("encode-{year}"),
            &super::tests::bytes(e.document()),
        )
        .unwrap();
        let decoded = convert("decode", &original.bytes).unwrap();
        let loaded = read_document(&decoded.bytes, &NoProgress).unwrap();
        let label = loaded.document.model.texts().next().unwrap().1;
        let initial = anchor_position(
            &loaded.document,
            ContainerRef::Model,
            label.anchor.as_ref().unwrap(),
        )
        .unwrap();
        let mut edited = Engine::new(
            loaded.document.clone(),
            Arc::new(CommandRegistry::with_builtins()),
        );
        edited
            .execute_erased("move", json!({"targets":"edge","delta":[50,0,0]}))
            .unwrap();
        edited
            .execute_erased(
                "label-style",
                json!({"targets":"text | dimension","color":"#cc4400","arrow":"open"}),
            )
            .unwrap();
        let working = write_document(
            edited.document(),
            &[],
            &WriteOptions::default().carry_over(&loaded),
            &NoProgress,
        )
        .unwrap();
        let saved = convert("encode-preserve", &working).unwrap();
        assert_ne!(saved.bytes, original.bytes);
        let back = convert("decode", &saved.bytes).unwrap();
        let doc = read_document(&back.bytes, &NoProgress).unwrap().document;
        assert_eq!(doc.model.texts().count(), 1);
        assert_eq!(doc.model.dimensions().count(), 2);
        assert!(doc.model.mesh().curves().any(
            |(_, c)| matches!(c,CurveDescriptor::Circle{radius,..} if (*radius-300.0).abs()<1e-6)
        ));
        let text = doc.model.texts().next().unwrap().1;
        let layout = text.layout.as_ref().unwrap();
        assert_eq!(layout.font.as_ref().unwrap().size_pt, 16.0);
        assert!(layout.font.as_ref().unwrap().bold);
        assert_eq!(layout.color, Some(Rgba8::rgb(0xcc, 0x44, 0)));
        assert_eq!(layout.arrow, ArrowStyle::Open);
        assert!(
            (anchor_position(&doc, ContainerRef::Model, text.anchor.as_ref().unwrap()).unwrap()
                - initial
                - DVec3::X * 50.0)
                .length()
                < 1e-6
        );
        for (_, dim) in doc.model.dimensions() {
            let style = match dim {
                Dimension::Linear { style, .. } | Dimension::Radial { style, .. } => style,
            };
            assert_eq!(style.color, Some(Rgba8::rgb(0xcc, 0x44, 0)));
            assert_eq!(style.arrow, ArrowStyle::Open);
            assert!(
                !planyrith_core::document::annotations::dimension_view(
                    &doc,
                    ContainerRef::Model,
                    dim
                )
                .detached
            );
        }
    }
}

#[test]
fn image_entities_round_trip_without_accumulating_private_definitions() {
    let (mut e, png) = super::tests::textured_cube(image::ImageFormat::Png);
    e.execute_erased(
        "image-insert",
        json!({"path":"tile.png","position":[2000,400,0],"width":200,"height":100}),
    )
    .unwrap();
    for year in [2017, 2026] {
        let mut doc = e.document().clone();
        for _ in 0..3 {
            doc = round_trip(&doc, year);
            assert_eq!(doc.model.images().count(), 1);
            assert!(doc.definitions.is_empty());
            let image = doc.model.images().next().unwrap().1;
            assert!(
                (image.width.as_mm() - 200.0).abs() < 1e-6
                    && (image.height.as_mm() - 100.0).abs() < 1e-6
            );
            assert!((image.transform.translation - DVec3::new(2000.0, 400.0, 0.0)).length() < 1e-6);
            let texture = doc
                .materials
                .get(image.material)
                .unwrap()
                .texture
                .as_ref()
                .unwrap();
            assert_eq!(doc.images.get(texture.image).unwrap().bytes, png);
        }
    }
}

#[test]
fn styles_watermarks_geographic_sun_and_scene_appearance_round_trip() {
    let (mut e, _) = super::tests::textured_cube(image::ImageFormat::Png);
    for (name, params) in [
        ("style-create", json!({"name":"Presentation"})),
        (
            "style-edit",
            json!({"faces":"monochrome","background":"#123456","profiles":4,"back_edges":true}),
        ),
        (
            "style-watermark-add",
            json!({"image":"tile.png","placement":"tiled","scale":0.3,"opacity":0.6}),
        ),
        (
            "shadows-set",
            json!({"enabled":true,"date":"2026-06-21","time":"16:00","latitude":45,"longitude":9,"utc_offset":2,"north_angle":std::f64::consts::PI/6.0}),
        ),
        ("scene-add", json!({"name":"Summer"})),
    ] {
        e.execute_erased(name, params).unwrap();
    }
    let before = e.document();
    for year in [2017, 2026] {
        let doc = round_trip(before, year);
        let style = doc.styles.active_style().unwrap();
        assert_eq!(style.name, "Presentation");
        assert_eq!(style.faces.mode, FaceMode::Monochrome);
        assert_eq!(style.background.color, Rgba8::rgb(0x12, 0x34, 0x56));
        assert_eq!(style.edges.profiles, Some(4));
        assert!(style.edges.back_edges);
        assert_eq!(style.watermarks.len(), 1);
        assert_eq!(style.watermarks[0].placement, WatermarkPlacement::Tiled);
        assert!((style.watermarks[0].opacity - 0.6).abs() < 1e-6);
        assert!(
            (doc.rendering.shadows.sun.direction() - before.rendering.shadows.sun.direction())
                .length()
                < 1e-9
        );
        let scene = doc.scenes.iter().next().unwrap().1;
        let style = doc.styles.get(scene.style.unwrap()).unwrap();
        assert_eq!(style.faces.mode, FaceMode::Monochrome);
        assert!(
            (scene.shadows.unwrap().sun.direction() - before.rendering.shadows.sun.direction())
                .length()
                < 1e-9
        );
    }
}

#[test]
fn nested_annotation_paths_export_to_both_containers() {
    let mut e = super::tests::cube();
    e.execute_erased("make-component", json!({"targets":"all","name":"Block"}))
        .unwrap();
    e.execute_erased("text", json!({"at":[0,0,0],"text":"Nested"}))
        .unwrap();
    let before = e
        .document()
        .model
        .texts()
        .next()
        .unwrap()
        .1
        .anchor
        .clone()
        .unwrap();
    assert!(!before.path.instances.steps.is_empty());
    for year in [2017, 2026] {
        let doc = round_trip(e.document(), year);
        let text = doc.model.texts().next().unwrap().1;
        assert!(
            !text
                .anchor
                .as_ref()
                .unwrap()
                .path
                .instances
                .steps
                .is_empty()
        );
        assert!(
            anchor_position(&doc, ContainerRef::Model, text.anchor.as_ref().unwrap())
                .unwrap()
                .length()
                < 1e-6
        );
    }
}
