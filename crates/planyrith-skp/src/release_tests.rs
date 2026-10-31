//! Regressions from models saved by real releases (docs/09 §5, docs/dev/skp-fidelity.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use planyrith_core::document::annotations;
use planyrith_core::{diagnostics, document::*};

fn native(bytes: &[u8]) -> Document {
    let converted = convert("decode", bytes).unwrap();
    native::read_document(&converted.bytes, &NoProgress)
        .unwrap()
        .document
}

fn faces(doc: &Document) -> usize {
    doc.containers()
        .iter()
        .map(|c| doc.container(*c).unwrap().mesh().face_count())
        .sum()
}

/// Open, native save/reopen, unchanged restoration and regeneration in both
/// writers. Returns the imported document and its native bytes.
fn chain(input: &[u8], expected_faces: usize) -> Document {
    let decoded = convert("decode", input).unwrap();
    let doc = native::read_document(&decoded.bytes, &NoProgress)
        .unwrap()
        .document;
    assert_eq!(faces(&doc), expected_faces);
    let report = diagnostics::validate(&doc);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let saved = super::tests::bytes(&doc);
    assert_eq!(
        faces(&native::read_document(&saved, &NoProgress).unwrap().document),
        expected_faces
    );
    assert_eq!(
        convert("encode-preserve", &decoded.bytes).unwrap().bytes,
        input
    );
    for writer in ["encode-2017", "encode-2026"] {
        let output = convert(writer, &decoded.bytes).unwrap();
        let again = native(&output.bytes);
        assert_eq!(faces(&again), expected_faces, "{writer}");
        assert_eq!(again.materials.iter().count(), doc.materials.iter().count());
    }
    doc
}

#[test]
fn releases_2018_to_2022_complete_the_exchange_chain() {
    for (name, input, expected) in [
        (
            "2018",
            include_bytes!("../tests/fixtures/release/HexCrater.skp").as_slice(),
            4690,
        ),
        (
            "2021 classic",
            include_bytes!("../tests/fixtures/release/LiveComponent.skp").as_slice(),
            534,
        ),
        (
            "2021",
            include_bytes!("../tests/fixtures/release/skp-vff-hard-block.skp").as_slice(),
            64,
        ),
        (
            "2021 tiled",
            include_bytes!("../tests/fixtures/release/uv_tile_at.skp").as_slice(),
            6,
        ),
        (
            "2022",
            include_bytes!("../tests/fixtures/release/skp-vff-soft-block.skp").as_slice(),
            69,
        ),
    ] {
        eprintln!("{name}");
        chain(input, expected);
    }
}

#[test]
fn a_custom_tag_ahead_of_the_default_layer_and_an_overlong_root_list_open() {
    let doc = native(include_bytes!(
        "../tests/fixtures/intermediate/zero_material_custom_layer_v18.skp"
    ));
    assert!(doc.tags.iter().any(|(_, t)| t.name == "Guide"));
    assert_eq!(doc.model.guides().count(), 2);
}

#[test]
fn text_attached_to_a_face_keeps_a_point_on_that_face() {
    let doc = chain(
        include_bytes!("../tests/fixtures/intermediate/capilla_quiroz_v17.skp"),
        181,
    );
    let (_, text) = doc
        .model
        .texts()
        .find(|(_, t)| t.text == "4.06 m²")
        .unwrap();
    let anchor = text.anchor.as_ref().unwrap();
    assert!(matches!(anchor.kind, AnchorKind::Point(_)));
    let at = annotations::anchor_position(&doc, ContainerRef::Model, anchor).unwrap();
    // The roof plane through the source face's corners.
    let normal = glam::DVec3::new(-0.7259749229209046, 0.0, -0.6877211726346564);
    let corner = glam::DVec3::new(1006.1665943684206, 3197.212281780265, 2325.0);
    assert!((at - corner).dot(normal).abs() < 1e-6);
    assert!(at.y > 3197.0 && at.y < 5643.0);
}

#[test]
fn faces_that_cross_themselves_and_slivers_stay_valid() {
    let doc = chain(
        include_bytes!("../tests/fixtures/release/ReversedFaces-SmallModel.skp"),
        338,
    );
    assert!(faces(&doc) > 0);
    // Crossing rings retain their surface as facets, and every source
    // vertex, in private groups where edges would otherwise cross.
    native(include_bytes!(
        "../tests/fixtures/release/PrusaI3_Extruder2.skp"
    ));
    native(include_bytes!(
        "../tests/fixtures/intermediate/Untitled.skp"
    ));
}

#[test]
fn degenerate_texture_placements_and_images_without_materials_do_not_stop_a_save() {
    let decoded = convert(
        "decode",
        include_bytes!("../tests/fixtures/release/degenerate-uv-box.skp"),
    )
    .unwrap();
    let output = convert("encode-2026", &decoded.bytes).unwrap();
    assert!(
        output
            .warnings
            .iter()
            .any(|w| w.contains("texture placements"))
    );
    let decoded = convert(
        "decode",
        include_bytes!("../tests/fixtures/release/missing_material.skp"),
    )
    .unwrap();
    native(&convert("encode-2017", &decoded.bytes).unwrap().bytes);
}

#[test]
fn texture_tints_survive_both_writers() {
    let (mut engine, _) = super::tests::textured_cube(image::ImageFormat::Png);
    engine
        .execute_erased(
            "material-edit",
            serde_json::json!({"material":"Tile","color":"#c8281e","colorize":"tint"}),
        )
        .unwrap();
    let saved = super::tests::bytes(engine.document());
    for writer in ["encode-2017", "encode-2026"] {
        let output = convert(writer, &saved).unwrap();
        assert!(
            !output.warnings.iter().any(|w| w.contains("tint")),
            "{writer}"
        );
        let again = native(&output.bytes);
        let (_, tinted) = again
            .materials
            .iter()
            .find(|(_, m)| m.texture.is_some())
            .unwrap();
        assert_eq!(
            tinted.texture.as_ref().unwrap().colorize,
            Some(ColorizeMode::Tint),
            "{writer}"
        );
        assert_eq!(tinted.color.r, 200, "{writer}");
    }
}

#[test]
fn releases_before_the_minimum_are_refused_by_name() {
    let error = convert(
        "decode",
        include_bytes!("../tests/fixtures/intermediate/nesting_v6.skp"),
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        error.contains("predates the supported 2013 minimum"),
        "{error}"
    );
}
