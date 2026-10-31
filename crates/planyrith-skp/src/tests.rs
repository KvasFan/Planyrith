//! Model-level compatibility, scale, determinism and damaged input checks.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use planyrith_core::{
    commands::{CommandRegistry, Engine},
    document::Document,
    io::native::{WriteOptions, read_document, write_document},
};
use serde_json::json;
use std::sync::Arc;

pub(super) fn cube() -> Engine {
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    e.execute_erased(
        "rectangle",
        json!({"corner_a":[0,0,0],"corner_b":[1000,1000,0]}),
    )
    .unwrap();
    e.execute_erased(
        "push-pull",
        json!({"face":{"Query":"face"},"distance":1000}),
    )
    .unwrap();
    e
}
pub(super) fn bytes(doc: &Document) -> Vec<u8> {
    write_document(
        doc,
        &[],
        &WriteOptions {
            deterministic: true,
            ..Default::default()
        },
        &NoProgress,
    )
    .unwrap()
}

#[test]
fn unchanged_skp_survives_native_working_copies_byte_for_byte() {
    for original in [
        include_bytes!("../tests/fixtures/box-2017.skp").as_slice(),
        include_bytes!("../tests/fixtures/box-2026.skp").as_slice(),
    ] {
        let decoded = convert("decode", original).unwrap();
        assert!(decoded.bytes.starts_with(b"PK"));
        let loaded = read_document(&decoded.bytes, &NoProgress).unwrap();
        assert_eq!(source::original(&loaded), Some(original));
        let history = [native::HistoryEntry {
            command: "select".into(),
            params: json!({"query":"all"}),
            group: None,
            timestamp: Some("2026-10-30T12:00:00Z".into()),
            created: Vec::new(),
        }];
        let working = write_document(
            &loaded.document,
            &history,
            &WriteOptions {
                generator: "planyrith-gui working copy".into(),
                modified: "2026-10-30T12:00:00Z".into(),
                ..Default::default()
            }
            .carry_over(&loaded),
            &NoProgress,
        )
        .unwrap();
        let saved = convert("encode-preserve", &working).unwrap();
        assert_eq!(saved.bytes, original);
        assert!(saved.warnings.is_empty());
    }
}

#[test]
fn source_preservation_keeps_foreign_streams_but_never_reverts_edits() {
    let original = convert("encode-2026", &bytes(cube().document())).unwrap();
    let mut archive = openskp::Document::open(&original.bytes).unwrap();
    archive.set_entry(
        "foreign-plugin/state.bin",
        b"uninterpreted payload".to_vec(),
    );
    let original = archive.write().unwrap();
    let decoded = convert("decode", &original).unwrap();
    assert_eq!(
        convert("encode-preserve", &decoded.bytes).unwrap().bytes,
        original
    );
    let loaded = read_document(&decoded.bytes, &NoProgress).unwrap();
    let mut e = Engine::new(
        loaded.document.clone(),
        Arc::new(CommandRegistry::with_builtins()),
    );
    e.execute_erased("move", json!({"targets":"all","delta":[100,0,0]}))
        .unwrap();
    let working = write_document(
        e.document(),
        &[],
        &WriteOptions::default().carry_over(&loaded),
        &NoProgress,
    )
    .unwrap();
    let saved = convert("encode-preserve", &working).unwrap();
    assert_ne!(saved.bytes, original);
    assert!(!saved.warnings.is_empty());
    assert_eq!(
        openskp::detect_container(&saved.bytes),
        openskp::Container::Zip
    );
    let back = convert("decode", &saved.bytes).unwrap();
    let back = read_document(&back.bytes, &NoProgress).unwrap();
    assert!((back.document.aabb().min.x - 100.0).abs() < 1e-6);
    // The authoritative native document still holds the complete source even
    // though a regenerated SKP cannot yet retain every foreign stream.
    let working = read_document(&working, &NoProgress).unwrap();
    assert_eq!(source::original(&working), Some(original.as_slice()));
}

#[test]
fn failed_legacy_geometry_decode_is_not_a_successful_empty_document() {
    let mut model = openskp::Model {
        version: "{13.0.1}".into(),
        ..Default::default()
    };
    model
        .diagnostics
        .push(openskp::Diagnostic::ContinuousFallback {
            stage: "calibration".into(),
            at: 9453,
            detail: "<bad pid preamble>".into(),
        });
    let error = decode::document(&model, &[0; 32]).unwrap_err().to_string();
    assert!(error.contains("geometry could not be decoded"));
    assert!(error.contains("13.0.1"));
    // A genuinely empty, clean file still opens.
    assert!(decode::document(&openskp::Model::default(), &[0; 32]).is_ok());
}

#[test]
fn intermediate_version_fixtures_measure_actual_coverage() {
    for (input, root_faces, definitions) in [
        (
            include_bytes!("../tests/fixtures/intermediate/arcs_2021.skp").as_slice(),
            2,
            0,
        ),
        (
            include_bytes!("../tests/fixtures/intermediate/nesting_2021.skp").as_slice(),
            0,
            3,
        ),
    ] {
        assert_eq!(openskp::header_info(input).unwrap().0, "{25.0.575}");
        let decoded = convert("decode", input).unwrap();
        let loaded = read_document(&decoded.bytes, &NoProgress).unwrap();
        assert_eq!(loaded.document.model.mesh().face_count(), root_faces);
        assert_eq!(loaded.document.definitions.len(), definitions);
        assert!(!loaded.document.aabb().is_empty());
        assert_eq!(
            convert("encode-preserve", &decoded.bytes).unwrap().bytes,
            input
        );
        for operation in ["encode-2017", "encode-2026"] {
            let encoded = convert(operation, &decoded.bytes).unwrap();
            let back = convert("decode", &encoded.bytes).unwrap();
            let back = read_document(&back.bytes, &NoProgress).unwrap();
            assert_eq!(back.document.model.mesh().face_count(), root_faces);
            assert_eq!(back.document.definitions.len(), definitions);
            assert!((back.document.aabb().max - loaded.document.aabb().max).length() < 1e-6);
        }
        let mut e = Engine::new(
            loaded.document.clone(),
            Arc::new(CommandRegistry::with_builtins()),
        );
        e.execute_erased(
            "camera-set",
            json!({"eye":[10000,10000,10000],"target":[0,0,0]}),
        )
        .unwrap();
        let edited = write_document(
            e.document(),
            &[],
            &WriteOptions::default().carry_over(&loaded),
            &NoProgress,
        )
        .unwrap();
        let saved = convert("encode-preserve", &edited).unwrap();
        assert!(
            openskp::header_info(&saved.bytes)
                .unwrap()
                .0
                .starts_with("{17.")
        );
        assert!(
            saved
                .warnings
                .iter()
                .any(|w| w.contains("2025") && w.contains("2017"))
        );
    }
}

#[test]
fn historical_2013_through_2016_boxes_open_and_restore_original_versions() {
    for (year, original) in [
        (
            2013,
            include_bytes!("../tests/fixtures/legacy/box-2013.skp").as_slice(),
        ),
        (
            2014,
            include_bytes!("../tests/fixtures/legacy/box-2014.skp").as_slice(),
        ),
        (
            2015,
            include_bytes!("../tests/fixtures/legacy/box-2015.skp").as_slice(),
        ),
        (
            2016,
            include_bytes!("../tests/fixtures/legacy/box-2016.skp").as_slice(),
        ),
    ] {
        let decoded = convert("decode", original).unwrap_or_else(|e| panic!("{year}: {e}"));
        let loaded = read_document(&decoded.bytes, &NoProgress).unwrap();
        let doc = &loaded.document;
        assert_eq!(doc.model.mesh().face_count(), 6, "{year}");
        assert_eq!(doc.model.mesh().edge_count(), 12, "{year}");
        assert!(
            (doc.aabb().max - DVec3::splat(1000.0)).length() < 1e-6,
            "{year}: {:?}",
            doc.aabb()
        );
        assert!(doc.aabb().min.length() < 1e-6);
        assert!(planyrith_core::diagnostics::validate(doc).is_clean());
        assert_eq!(source::version(&loaded).unwrap(), Some(year));
        assert_eq!(
            convert("encode-preserve", &decoded.bytes).unwrap().bytes,
            original
        );
        let mut edited = Engine::new(doc.clone(), Arc::new(CommandRegistry::with_builtins()));
        edited
            .execute_erased("move", json!({"targets":"all","delta":[100,0,0]}))
            .unwrap();
        let working = write_document(
            edited.document(),
            &[],
            &WriteOptions::default().carry_over(&loaded),
            &NoProgress,
        )
        .unwrap();
        let saved = convert("encode-preserve", &working).unwrap();
        assert!(saved.warnings.iter().any(|w| w.contains("2017 container")));
        let back = convert("decode", &saved.bytes).unwrap();
        let back = read_document(&back.bytes, &NoProgress).unwrap();
        assert_eq!(back.document.model.mesh().face_count(), 6);
        assert!((back.document.aabb().min.x - 100.0).abs() < 1e-6);
    }
}

#[test]
fn classic_sdk_saved_models_import_geometry() {
    for input in [
        include_bytes!("../tests/fixtures/intermediate/sdk_saved_2019.skp").as_slice(),
        include_bytes!("../tests/fixtures/intermediate/sdk_saved_2020.skp").as_slice(),
    ] {
        let decoded = convert("decode", input).unwrap();
        let doc = read_document(&decoded.bytes, &NoProgress).unwrap().document;
        assert_eq!(doc.model.mesh().vertex_count(), 4);
        assert_eq!(doc.model.mesh().edge_count(), 4);
        assert_eq!(doc.model.mesh().face_count(), 1);
        assert!((doc.aabb().max - DVec3::new(254.0, 254.0, 0.0)).length() < 1e-6);
    }
}

#[test]
fn native_only_edits_invalidate_source_restore_without_losing_native_state() {
    let original = include_bytes!("../tests/fixtures/box-2017.skp");
    let decoded = convert("decode", original).unwrap();
    let loaded = read_document(&decoded.bytes, &NoProgress).unwrap();
    let mut e = Engine::new(
        loaded.document.clone(),
        Arc::new(CommandRegistry::with_builtins()),
    );
    e.execute_erased("sheet-add-page", json!({"page":"Plan","paper":"A4"}))
        .unwrap();
    let native = write_document(
        e.document(),
        &[],
        &WriteOptions::default().carry_over(&loaded),
        &NoProgress,
    )
    .unwrap();
    let saved = convert("encode-preserve", &native).unwrap();
    assert_ne!(saved.bytes, original.as_slice());
    assert!(saved.warnings.iter().any(|w| w.contains("Sheets")));
    let working = read_document(&native, &NoProgress).unwrap();
    assert!(working.document.sheets.page("Plan").is_some());
    assert_eq!(source::original(&working), Some(original.as_slice()));
}

#[test]
fn fully_undone_edits_restore_original_source_despite_consumed_pids() {
    let original = include_bytes!("../tests/fixtures/box-2026.skp");
    let decoded = convert("decode", original).unwrap();
    let loaded = read_document(&decoded.bytes, &NoProgress).unwrap();
    let mut e = Engine::new(
        loaded.document.clone(),
        Arc::new(CommandRegistry::with_builtins()),
    );
    e.execute_erased("line", json!({"points":[[3000,0,0],[4000,0,0]]}))
        .unwrap();
    e.undo().unwrap();
    let native = write_document(
        e.document(),
        &[],
        &WriteOptions::default().carry_over(&loaded),
        &NoProgress,
    )
    .unwrap();
    assert_eq!(
        convert("encode-preserve", &native).unwrap().bytes,
        original.as_slice()
    );
}

#[test]
fn real_containers_preserve_cube_scale_faces_and_determinism() {
    let native = include_bytes!("../../planyrith-core/tests/fixtures/skp/cube.plyr").to_vec();
    for operation in ["encode-2017", "encode-2026"] {
        let out = convert(operation, &native).unwrap();
        assert!(out.bytes.starts_with(b"\xff\xfe\xff\x0e"));
        assert_eq!(out.bytes, convert(operation, &native).unwrap().bytes);
        let model = openskp::Model::parse(&out.bytes).unwrap();
        assert_eq!(
            model
                .geometry
                .iter()
                .map(|g| g.mesh.faces.len())
                .sum::<usize>(),
            6
        );
        let back = convert("decode", &out.bytes).unwrap();
        let doc = read_document(&back.bytes, &NoProgress).unwrap().document;
        assert_eq!(doc.model.mesh().face_count(), 6);
        assert_eq!(doc.model.mesh().edge_count(), 12);
        assert!((doc.aabb().max - DVec3::splat(1000.0)).length() < 1e-6);
        assert!(planyrith_core::diagnostics::validate(&doc).is_clean());
    }
}

#[test]
fn nested_reused_components_materials_tags_and_camera_round_trip() {
    let mut e = cube();
    e.execute_erased("material-create", json!({"name":"Blue","color":"#1248ad"}))
        .unwrap();
    e.execute_erased(
        "material-apply",
        json!({"material":"Blue","targets":"face"}),
    )
    .unwrap();
    e.execute_erased("make-component", json!({"targets":"all","name":"Block"}))
        .unwrap();
    e.execute_erased(
        "move",
        json!({"targets":"all","delta":[2000,0,0],"copy":true}),
    )
    .unwrap();
    e.execute_erased("make-group", json!({"targets":"all","name":"Pair"}))
        .unwrap();
    e.execute_erased("tag-create", json!({"name":"Furniture"}))
        .unwrap();
    e.execute_erased("tag-assign", json!({"tag":"Furniture","targets":"all"}))
        .unwrap();
    e.execute_erased("scene-add", json!({"name":"Overview"}))
        .unwrap();
    let native = bytes(e.document());
    for operation in ["encode-2017", "encode-2026"] {
        let out = convert(operation, &native).unwrap();
        let back = convert("decode", &out.bytes).unwrap();
        let doc = read_document(&back.bytes, &NoProgress).unwrap().document;
        assert!(doc.definition_by_name("Block").is_some());
        let id = doc.definition_by_name("Block").unwrap();
        assert_eq!(doc.usage_counts()[&id], 2);
        assert!(doc.materials.by_name("Blue").is_some());
        assert!(doc.tags.by_name("Furniture").is_some());
        assert_eq!(doc.scenes.len(), 1);
        assert!((doc.aabb().max.x - 3000.0).abs() < 1e-6);
    }
}

#[test]
fn damaged_containers_and_unknown_operations_are_errors() {
    for bytes in [b"".as_slice(), b"not a model", b"\xff\xfe\xff\x0e"] {
        assert!(convert("decode", bytes).is_err());
    }
    assert!(convert("not-a-command", b"").is_err());
    let out = convert("encode-2017", &bytes(cube().document())).unwrap();
    assert!(convert("decode", &out.bytes[..32]).is_err());
}

#[test]
fn retail_authored_blank_template_fixtures_open_at_one_metre() {
    for bytes in [
        include_bytes!("../tests/fixtures/box-2017.skp").as_slice(),
        include_bytes!("../tests/fixtures/box-2026.skp").as_slice(),
    ] {
        let decoded = convert("decode", bytes).unwrap();
        let doc = read_document(&decoded.bytes, &NoProgress).unwrap().document;
        assert_eq!(doc.model.mesh().face_count(), 6);
        assert_eq!(doc.model.mesh().edge_count(), 12);
        assert!((doc.aabb().max - DVec3::splat(1000.0)).length() < 1e-6);
        assert!(doc.aabb().min.length() < 1e-6);
    }
}

#[test]
fn undecoded_annotation_counts_still_produce_compatibility_notices() {
    let model = openskp::Model {
        geometry: vec![openskp::GeometryRun {
            topology: openskp::Topology {
                texts: 2,
                dimensions: 3,
                images_placed: 1,
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    };
    let (_, warnings) = super::decode::document(&model, &[0; 32]).unwrap();
    assert!(warnings.iter().any(|w| w.contains("2 texts, 3 dimensions")));
    assert!(warnings.iter().any(|w| w.contains("1 placed images")));
}

#[test]
fn display_units_snap_increments_and_model_axes_round_trip() {
    use planyrith_core::units::{Angle, Length, LengthUnit};
    for unit in LengthUnit::ALL {
        let mut doc = cube().document().clone();
        doc.info.display_units = unit;
        doc.info.length_snap = Some(Length::mm(12.7));
        doc.info.angle_snap = Some(Angle::degrees(15.0));
        doc.info.axes = glam::DAffine3::from_translation(DVec3::new(100.0, 200.0, 300.0));
        for operation in ["encode-2017", "encode-2026"] {
            let encoded = convert(operation, &bytes(&doc)).unwrap();
            let decoded = convert("decode", &encoded.bytes).unwrap();
            let back = read_document(&decoded.bytes, &NoProgress).unwrap().document;
            assert_eq!(back.info.display_units, unit);
            assert!((back.info.length_snap.unwrap().as_mm() - 12.7).abs() < 1e-9);
            assert!((back.info.angle_snap.unwrap().to_degrees() - 15.0).abs() < 1e-9);
            assert!((back.info.axes.translation - doc.info.axes.translation).length() < 1e-6);
        }
    }
}

#[test]
fn non_manifold_faces_and_hidden_edge_copies_are_preserved() {
    let shared = openskp::MeshEdge {
        pid: 42,
        v0: 0,
        v1: 1,
        ..Default::default()
    };
    let mesh = openskp::Mesh {
        vertices: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        edges: vec![shared],
        faces: vec![
            openskp::MeshFace {
                outer: vec![0, 1, 2],
                normal: [0.0, 0.0, 1.0],
                ..Default::default()
            },
            openskp::MeshFace {
                outer: vec![1, 0, 3],
                normal: [0.0, 0.0, 1.0],
                ..Default::default()
            },
            openskp::MeshFace {
                outer: vec![0, 1, 4],
                normal: [0.0, -1.0, 0.0],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let model = openskp::Model {
        geometry: vec![openskp::GeometryRun {
            mesh,
            ..Default::default()
        }],
        scenes: vec![openskp::Scene {
            name: "Hidden edge".into(),
            hidden_entities: vec![42],
            ..Default::default()
        }],
        ..Default::default()
    };
    let (doc, warnings) = super::decode::document(&model, &[0; 32]).unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("private surface groups"))
    );
    assert_eq!(doc.model.groups().count(), 2);
    assert_eq!(
        doc.containers()
            .iter()
            .map(|c| doc.container(*c).unwrap().mesh().face_count())
            .sum::<usize>(),
        3
    );
    assert_eq!(
        doc.scenes
            .iter()
            .next()
            .unwrap()
            .1
            .hidden_entities
            .as_ref()
            .unwrap()
            .len(),
        2
    );
    assert!(!planyrith_core::diagnostics::validate(&doc).has_errors());
    for operation in ["encode-2017", "encode-2026"] {
        let file = convert(operation, &bytes(&doc)).unwrap();
        let back = convert("decode", &file.bytes).unwrap();
        let doc = read_document(&back.bytes, &NoProgress).unwrap().document;
        assert_eq!(
            doc.containers()
                .iter()
                .map(|c| doc.container(*c).unwrap().mesh().face_count())
                .sum::<usize>(),
            3
        );
        assert_eq!(
            doc.scenes
                .iter()
                .next()
                .unwrap()
                .1
                .hidden_entities
                .as_ref()
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn holes_loose_edges_sections_and_attributes_survive() {
    use planyrith_core::{
        commands::{Transaction, transaction::AttrTarget},
        document::*,
        geom::{FaceSides, mesh::*},
        ids::EntityId,
    };
    let mut doc = Document::new();
    let vertices = [
        [0, 0, 0],
        [2000, 0, 0],
        [2000, 2000, 0],
        [0, 2000, 0],
        [500, 500, 0],
        [500, 1500, 0],
        [1500, 1500, 0],
        [1500, 500, 0],
        [3000, 0, 0],
        [4000, 0, 0],
    ]
    .map(|p| DVec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])))
    .to_vec();
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (8, 9),
    ]
    .map(|(a, b)| IndexedEdge {
        a,
        b,
        flags: EdgeFlags::NONE,
        curve: None,
    })
    .to_vec();
    let piece = IndexedMesh {
        vertices,
        edges,
        faces: vec![IndexedFace {
            loops: vec![
                (0..4).map(DirectedEdgeIndex::forward).collect(),
                (4..8).map(DirectedEdgeIndex::forward).collect(),
            ],
            plane: Some(planyrith_core::math::Plane::XY),
            sides: FaceSides::default(),
        }],
        curves: Vec::new(),
    };
    let mut tx = Transaction::new(&mut doc, "fixture");
    let ids = tx
        .with_mesh(ContainerRef::Model, |m, t, d| {
            m.insert_disjoint(&piece, t, d)
        })
        .unwrap()
        .unwrap();
    tx.set_attribute(
        AttrTarget::Entity {
            container: ContainerRef::Model,
            id: EntityId::Face(ids.faces[0]),
        },
        "spec",
        "name",
        Some(Value::Str("opening".into())),
    )
    .unwrap();
    tx.insert_entity(
        ContainerRef::Model,
        EntityRecord::SectionPlane(SectionPlane {
            plane: planyrith_core::math::Plane::XY,
            name: "Cut".into(),
            symbol: "A".into(),
            active: true,
            fill_cap: true,
            cuts_hidden: false,
        }),
    )
    .unwrap();
    tx.commit();
    for operation in ["encode-2017", "encode-2026"] {
        let encoded = convert(operation, &bytes(&doc)).unwrap();
        let decoded = convert("decode", &encoded.bytes).unwrap();
        let doc = read_document(&decoded.bytes, &NoProgress).unwrap().document;
        assert_eq!(doc.model.mesh().face_count(), 1);
        assert_eq!(doc.model.mesh().edge_count(), 9);
        let (face, f) = doc.model.mesh().faces().next().unwrap();
        assert_eq!(f.holes.len(), 1);
        assert!((doc.model.mesh().face_area(face).unwrap() - 3_000_000.0).abs() < 1e-6);
        assert_eq!(
            doc.model
                .attrs(EntityId::Face(face))
                .unwrap()
                .get("spec", "name"),
            Some(&Value::Str("opening".into()))
        );
        assert!(doc.model.section_planes().next().unwrap().1.active);
    }
}

pub(super) fn textured_cube(format: image::ImageFormat) -> (Engine, Vec<u8>) {
    use planyrith_core::io::{MemoryStorage, Storage, StorageKey};
    let storage = Arc::new(MemoryStorage::new());
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([180, 30, 80, 255]));
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, format).unwrap();
    storage
        .write(&StorageKey::new("tile.png"), png.get_ref())
        .unwrap();
    let mut e = cube();
    e.set_storage(storage);
    e.execute_erased(
        "material-create",
        json!({"name":"Tile","color":"#ffffff","texture":"tile.png","texture_width":250}),
    )
    .unwrap();
    e.execute_erased(
        "material-apply",
        json!({"targets":"face","material":"Tile"}),
    )
    .unwrap();
    e.execute_erased(
        "material-apply",
        json!({"targets":"face","material":"Tile","side":"back"}),
    )
    .unwrap();
    (e, png.into_inner())
}

#[test]
fn texture_images_and_per_corner_uvs_round_trip() {
    let (e, png) = textured_cube(image::ImageFormat::Png);
    for operation in ["encode-2017", "encode-2026"] {
        let encoded = convert(operation, &bytes(e.document())).unwrap();
        let decoded = convert("decode", &encoded.bytes).unwrap();
        let doc = read_document(&decoded.bytes, &NoProgress).unwrap().document;
        let material = doc
            .materials
            .get(doc.materials.by_name("Tile").unwrap())
            .unwrap();
        let texture = material.texture.as_ref().unwrap();
        assert!((texture.size_mm.x - 250.0).abs() < 1e-6);
        assert_eq!(&doc.images.get(texture.image).unwrap().bytes, &png);
        assert_cube_uvs(&doc);
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(20))]
    #[test]
    fn scaled_boxes_keep_dimensions(width in 1u32..10000,height in 1u32..10000) {
        let mut e=Engine::new(Document::new(),Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased("rectangle",json!({"corner_a":[0,0,0],"corner_b":[width,width,0]})).unwrap();
        e.execute_erased("push-pull",json!({"face":{"Query":"face"},"distance":height})).unwrap();
        let out=convert("encode-2017",&bytes(e.document())).unwrap();
        let back=convert("decode",&out.bytes).unwrap();
        let doc=read_document(&back.bytes,&NoProgress).unwrap().document;
        proptest::prop_assert!((doc.aabb().max-DVec3::new(f64::from(width),f64::from(width),f64::from(height))).length()<1e-6);
    }
}

fn assert_cube_uvs(doc: &Document) {
    let buffers = planyrith_core::io::render_mesh::extract(
        doc,
        planyrith_core::document::ContainerRef::Model,
        Default::default(),
    )
    .unwrap();
    for (i, (position, uv)) in buffers.positions.iter().zip(&buffers.uvs).enumerate() {
        let p = DVec3::new(
            f64::from(position[0]),
            f64::from(position[1]),
            f64::from(position[2]),
        );
        // Each buffer vertex retains its face index; both painted sides
        // use that face's physical texture frame.
        let index = buffers.face_ids[i];
        let face = doc
            .model
            .mesh()
            .face(buffers.face_lookup[index as usize])
            .unwrap();
        let expected = face.plane.frame().to_2d(p) / glam::DVec2::splat(250.0);
        assert!((glam::DVec2::new(f64::from(uv[0]), f64::from(uv[1])) - expected).length() < 1e-5);
    }
}

#[test]
fn webp_textures_export_as_png_without_pixel_changes() {
    let (e, webp) = textured_cube(image::ImageFormat::WebP);
    let original = image::load_from_memory(&webp).unwrap().to_rgba8();
    for operation in ["encode-2017", "encode-2026"] {
        let encoded = convert(operation, &bytes(e.document())).unwrap();
        let decoded = convert("decode", &encoded.bytes).unwrap();
        let doc = read_document(&decoded.bytes, &NoProgress).unwrap().document;
        let material = doc
            .materials
            .get(doc.materials.by_name("Tile").unwrap())
            .unwrap();
        let texture = material.texture.as_ref().unwrap();
        let image = doc.images.get(texture.image).unwrap();
        assert_eq!(
            image::guess_format(&image.bytes).unwrap(),
            image::ImageFormat::Png
        );
        assert_eq!(
            image::load_from_memory(&image.bytes).unwrap().to_rgba8(),
            original
        );
        assert_cube_uvs(&doc);
    }
}

#[test]
fn texture_placement_survives_reordered_source_vertices_and_native_save() {
    let (e, _) = textured_cube(image::ImageFormat::Png);
    let (mut model, _) = encode::model(e.document()).unwrap();
    for run in &mut model.geometry {
        let mesh = &mut run.mesh;
        mesh.vertices.reverse();
        let last = mesh.vertices.len() as u32 - 1;
        for edge in &mut mesh.edges {
            edge.v0 = last - edge.v0;
            edge.v1 = last - edge.v1;
        }
        for face in &mut mesh.faces {
            for vertex in face.outer.iter_mut().chain(face.holes.iter_mut().flatten()) {
                *vertex = last - *vertex;
            }
        }
    }
    let (doc, _) = decode::document(&model, &[7; 32]).unwrap();
    let saved = bytes(&doc);
    let doc = read_document(&saved, &NoProgress).unwrap().document;
    assert_cube_uvs(&doc);
    for operation in ["encode-2017", "encode-2026"] {
        let encoded = convert(operation, &saved).unwrap();
        let decoded = convert("decode", &encoded.bytes).unwrap();
        let doc = read_document(&decoded.bytes, &NoProgress).unwrap().document;
        assert_cube_uvs(&doc);
    }
}
