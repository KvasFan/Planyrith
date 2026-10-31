//! Import regressions for editable annotations, curves and appearance (docs/09 §5).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use planyrith_core::document::annotations;
use planyrith_core::{document::*, geom::CurveDescriptor, ids::EntityId};

fn native(bytes: &[u8]) -> Document {
    let converted = convert("decode", bytes).unwrap();
    let loaded = native::read_document(&converted.bytes, &NoProgress).unwrap();
    // Import must survive a native save/reopen independently of the helper.
    let saved = super::tests::bytes(&loaded.document);
    native::read_document(&saved, &NoProgress).unwrap().document
}

fn encoded(model: &openskp::Model, year: u32) -> Vec<u8> {
    let env = openskp::WriteEnv {
        unix_time: 0,
        seed: 12,
    };
    if year == 2017 {
        openskp::Document17::from_model_with(model, env)
            .unwrap()
            .write()
    } else {
        openskp::Document::from_model_with(model, env)
            .unwrap()
            .write()
            .unwrap()
    }
}

#[test]
fn classic_color_attributes_and_trailing_definitions_import_without_data_loss() {
    let mut engine = super::tests::cube();
    engine
        .execute_erased(
            "make-component",
            serde_json::json!({"targets":"all","name":"Block"}),
        )
        .unwrap();
    engine
        .execute_erased(
            "make-component",
            serde_json::json!({"targets":"all","name":"Assembly"}),
        )
        .unwrap();
    let (mut model, _) = encode::model(engine.document()).unwrap();
    assert_eq!(model.definitions.len(), 2);
    model.dictionaries.push(openskp::Attributes {
        owner: openskp::Owner::Definition(0),
        dictionaries: vec![openskp::Dictionary {
            name: "classic-colors".into(),
            flag: 0,
            entries: vec![
                ("rgba".into(), openskp::Value::Int(0x493b2711)),
                ("following-value".into(), openskp::Value::Double(4.0)),
            ],
        }],
    });
    let mut source = encoded(&model, 2017);
    // The writer exposes integers but not classic packed colors. Change only
    // the type byte; both records carry exactly four little-endian bytes.
    let marker = [4, 0x11, 0x27, 0x3b, 0x49];
    let at = source
        .windows(marker.len())
        .position(|b| b == marker)
        .unwrap();
    source[at] = 8;
    // Additional definition records may follow the declared definition list.
    let marker = b"CComponentDefinition";
    let at = source
        .windows(marker.len())
        .position(|b| b == marker)
        .unwrap();
    let count = at - 10;
    assert_eq!(&source[count..count + 4], &2u32.to_le_bytes());
    source[count..count + 4].copy_from_slice(&1u32.to_le_bytes());
    let decoded = convert("decode", &source).unwrap();
    let loaded = native::read_document(&decoded.bytes, &NoProgress).unwrap();
    assert_eq!(loaded.document.definitions.len(), 2);
    assert_eq!(loaded.document.model.instances().count(), 1);
    assert_eq!(
        loaded
            .document
            .containers()
            .iter()
            .map(|c| loaded.document.container(*c).unwrap().mesh().face_count())
            .sum::<usize>(),
        6
    );
    let attributes = loaded
        .document
        .definitions
        .iter()
        .find_map(|(_, d)| d.attrs.get("classic-colors", "rgba").map(|_| &d.attrs))
        .unwrap();
    assert_eq!(
        attributes.get("classic-colors", "rgba"),
        Some(&Value::Int(0x493b2711))
    );
    assert_eq!(
        attributes.get("classic-colors", "following-value"),
        Some(&Value::Float(4.0))
    );
    assert_eq!(
        convert("encode-preserve", &decoded.bytes).unwrap().bytes,
        source
    );
}

#[test]
fn bitmap_assets_import_losslessly_into_native_textures() {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(3, 2, |x, y| {
        image::Rgba([x as u8 * 100, y as u8 * 200, 37, 255 - x as u8 * 50])
    }));
    let mut bitmap = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bitmap, image::ImageFormat::Bmp)
        .unwrap();
    let bitmap = bitmap.into_inner();
    let png = images::asset(&bitmap).unwrap();
    assert_eq!(image::guess_format(&png).unwrap(), image::ImageFormat::Png);
    assert_eq!(
        image::load_from_memory(&png).unwrap().to_rgba8(),
        image.to_rgba8()
    );
    assert_eq!(images::asset(&png).unwrap(), png);
    assert!(images::asset(&bitmap[..20]).is_err());
    let (mut model, _) = encode::model(super::tests::cube().document()).unwrap();
    model.materials.push(openskp::Material::Textured {
        name: "Legacy bitmap".into(),
        texture: Some("texture.bmp".into()),
        applied_size_in: Some((12.0, 8.0)),
        jpeg_quality: None,
        image_bytes: Some(bitmap),
        avg_rgba: None,
        texture_avg_rgba: None,
        colorized: false,
        colorize_type: 0,
        opacity: 1.0,
    });
    model.material_links.push((17, 0));
    model.geometry[0].mesh.faces[0].front_material = Some(17);
    for year in [2017, 2026] {
        let doc = native(&encoded(&model, year));
        let (_, material) = doc
            .materials
            .iter()
            .find(|(_, m)| m.name == "Legacy bitmap")
            .unwrap();
        let texture = material.texture.as_ref().unwrap();
        assert!((texture.size_mm.x - 304.8).abs() < 1e-9);
        assert!((texture.size_mm.y - 203.2).abs() < 1e-9);
        let asset = doc.images.get(texture.image).unwrap();
        assert_eq!(
            image::load_from_memory(&asset.bytes).unwrap().to_rgba8(),
            image.to_rgba8()
        );
    }
}

#[test]
fn warped_source_faces_retain_vertices_attributes_and_valid_topology() {
    let (mut model, _) = encode::model(super::tests::cube().document()).unwrap();
    model.geometry[0].mesh.vertices[0][2] += 0.003;
    for i in 0..6 {
        model.dictionaries.push(openskp::Attributes {
            owner: openskp::Owner::Face(0, i),
            dictionaries: vec![openskp::Dictionary {
                name: "facet-source".into(),
                flag: 0,
                entries: vec![("index".into(), openskp::Value::Int(i as i32))],
            }],
        });
    }
    let (doc, warnings) = decode::document(&model, &[0; 32]).unwrap();
    assert!(warnings.iter().any(|w| w.contains("warped source faces")));
    assert_eq!(doc.model.mesh().vertex_count(), 8);
    assert_eq!(doc.model.mesh().face_count(), 7);
    assert!(
        planyrith_core::diagnostics::validate(&doc)
            .problems
            .is_empty()
    );
    for position in &model.geometry[0].mesh.vertices {
        assert!(
            doc.model
                .mesh()
                .vertices()
                .any(|(_, v)| (v.position - DVec3::from_array(*position) * 1000.0).length() < 1e-9)
        );
    }
    let mut counts = [0; 6];
    for (id, _) in doc.model.mesh().faces() {
        let Some(Value::Int(index)) = doc
            .model
            .attrs(EntityId::Face(id))
            .and_then(|a| a.get("facet-source", "index"))
        else {
            panic!("facet lost source attributes")
        };
        counts[*index as usize] += 1;
    }
    assert!(counts.iter().all(|count| *count >= 1));
    assert!(counts.contains(&2));
    let saved = super::tests::bytes(&doc);
    assert_eq!(
        super::tests::bytes(&native::read_document(&saved, &NoProgress).unwrap().document),
        saved
    );
    // A stale normal on a genuinely planar face is fitted without splitting.
    let (mut model, _) = encode::model(super::tests::cube().document()).unwrap();
    model.geometry[0].mesh.faces[0].normal[0] += 0.01;
    let (doc, warnings) = decode::document(&model, &[1; 32]).unwrap();
    assert_eq!(doc.model.mesh().face_count(), 6);
    assert!(!warnings.iter().any(|w| w.contains("warped source faces")));
}

#[test]
fn interrupted_circular_curves_import_as_analytic_fragments() {
    use planyrith_core::commands::{CommandRegistry, Engine};
    use std::sync::Arc;
    let mut engine = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    engine
        .execute_erased(
            "circle",
            serde_json::json!({"center":[0,0,0],"radius":500,"segments":12}),
        )
        .unwrap();
    let (mut model, _) = encode::model(engine.document()).unwrap();
    model.geometry[0].mesh.edges[2].curve = None;
    model.geometry[0].mesh.edges[8].curve = None;
    let (doc, warnings) = decode::document(&model, &[0; 32]).unwrap();
    assert_eq!(doc.model.mesh().edge_count(), 12);
    assert_eq!(doc.model.mesh().face_count(), 1);
    assert_eq!(doc.model.mesh().curve_count(), 2);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("interrupted source curves"))
    );
    for (_, curve) in doc.model.mesh().curves() {
        let CurveDescriptor::Arc {
            radius,
            segments,
            start_angle,
            end_angle,
            ..
        } = curve
        else {
            panic!("fragment lost its analytic radius")
        };
        assert!((radius - 500.0).abs() < 1e-9);
        assert_eq!(*segments, 5);
        assert!(
            ((end_angle.0 - start_angle.0).abs() - std::f64::consts::TAU * 5.0 / 12.0).abs() < 1e-9
        );
    }
    assert!(
        planyrith_core::diagnostics::validate(&doc)
            .problems
            .is_empty()
    );
    let saved = super::tests::bytes(&doc);
    assert_eq!(
        super::tests::bytes(&native::read_document(&saved, &NoProgress).unwrap().document),
        saved
    );
}

#[test]
fn near_source_vertices_keep_exact_positions_and_edge_attributes_in_separate_groups() {
    let (mut model, _) = encode::model(super::tests::cube().document()).unwrap();
    let mut near = model.geometry[0].mesh.vertices[0];
    near[0] += 0.0000004;
    let mesh = &mut model.geometry[0].mesh;
    mesh.vertices.extend([near, [-2.0, -2.0, -2.0]]);
    let mut edge = mesh.edges[0];
    edge.v0 = 8;
    edge.v1 = 9;
    edge.pid = 50000;
    edge.curve = None;
    mesh.edges.push(edge);
    model.dictionaries.push(openskp::Attributes {
        owner: openskp::Owner::Edge(0, 12),
        dictionaries: vec![openskp::Dictionary {
            name: "weld-regression".into(),
            flag: 0,
            entries: vec![("label".into(), openskp::Value::Text("loose edge".into()))],
        }],
    });
    let (doc, warnings) = decode::document(&model, &[0; 32]).unwrap();
    let meshes = doc
        .containers()
        .into_iter()
        .map(|c| doc.container(c).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        meshes
            .iter()
            .map(|c| c.mesh().vertex_count())
            .sum::<usize>(),
        10
    );
    assert_eq!(
        meshes.iter().map(|c| c.mesh().edge_count()).sum::<usize>(),
        13
    );
    assert_eq!(
        meshes.iter().map(|c| c.mesh().face_count()).sum::<usize>(),
        6
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("private surface groups"))
    );
    assert!(meshes.iter().any(|c| {
        c.mesh()
            .vertices()
            .any(|(_, v)| (v.position - DVec3::from_array(near) * 1000.0).length() < 1e-12)
    }));
    assert!(meshes.iter().any(|c| c.mesh().edges().any(|(id, _)| {
        c.attrs(EntityId::Edge(id))
            .and_then(|a| a.get("weld-regression", "label"))
            == Some(&Value::Str("loose edge".into()))
    })));
    assert!(
        planyrith_core::diagnostics::validate(&doc)
            .problems
            .is_empty()
    );
}

#[test]
fn crossed_source_wires_keep_both_segments_without_spurious_topology() {
    let (mut model, _) = encode::model(super::tests::cube().document()).unwrap();
    let mut edge = model.geometry[0].mesh.edges[0];
    edge.v0 = 0;
    edge.v1 = 1;
    edge.curve = None;
    edge.pid = 100;
    let mut second = edge;
    second.v0 = 2;
    second.v1 = 3;
    second.pid = 101;
    let mesh = &mut model.geometry[0].mesh;
    mesh.vertices = vec![
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    mesh.edges = vec![edge, second];
    mesh.faces.clear();
    let (doc, warnings) = decode::document(&model, &[0; 32]).unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("private surface groups"))
    );
    assert_eq!(
        doc.containers()
            .iter()
            .map(|c| doc.container(*c).unwrap().mesh().edge_count())
            .sum::<usize>(),
        2
    );
    assert_eq!(
        doc.containers()
            .iter()
            .map(|c| doc.container(*c).unwrap().mesh().vertex_count())
            .sum::<usize>(),
        4
    );
    assert!(
        planyrith_core::diagnostics::validate(&doc)
            .problems
            .is_empty()
    );
}

#[test]
fn classic_extended_fixtures_keep_topology_sections_and_curves() {
    for (input, faces, definitions, curves, sections) in [
        (
            include_bytes!("../tests/fixtures/intermediate/arcs_2020.skp").as_slice(),
            2,
            0,
            3,
            0,
        ),
        (
            include_bytes!("../tests/fixtures/intermediate/nesting_2020.skp").as_slice(),
            3,
            3,
            0,
            0,
        ),
        (
            include_bytes!("../tests/fixtures/intermediate/sdk_saved_sections_2019.skp").as_slice(),
            1,
            0,
            0,
            0,
        ),
        (
            include_bytes!("../tests/fixtures/intermediate/sdk_saved_sections_2020.skp").as_slice(),
            1,
            0,
            0,
            0,
        ),
        (
            include_bytes!("../tests/fixtures/intermediate/legacy_v2019_synthetic.skp").as_slice(),
            4,
            4,
            0,
            0,
        ),
        (
            include_bytes!("../tests/fixtures/intermediate/legacy_v2020_textured.skp").as_slice(),
            1,
            0,
            0,
            0,
        ),
    ] {
        let doc = native(input);
        assert_eq!(doc.definitions.len(), definitions);
        assert_eq!(
            doc.containers()
                .iter()
                .map(|c| doc.container(*c).unwrap().mesh().face_count())
                .sum::<usize>(),
            faces
        );
        assert_eq!(
            doc.containers()
                .iter()
                .map(|c| doc.container(*c).unwrap().mesh().curves().count())
                .sum::<usize>(),
            curves
        );
        assert_eq!(
            doc.containers()
                .iter()
                .map(|c| doc.container(*c).unwrap().section_planes().count())
                .sum::<usize>(),
            sections
        );
        let decoded = convert("decode", input).unwrap();
        assert_eq!(
            convert("encode-preserve", &decoded.bytes).unwrap().bytes,
            input
        );
    }
}

#[test]
fn annotation_fonts_layout_and_edge_parameters_survive_both_containers() {
    let (mut model, _) = encode::model(super::tests::cube().document()).unwrap();
    let edge = model.geometry[0].mesh.edges[0];
    model.dictionaries.push(openskp::Attributes {
        owner: openskp::Owner::Model,
        dictionaries: vec![openskp::Dictionary {
            name: "import-regression".into(),
            flag: 0,
            entries: vec![("root".into(), openskp::Value::Text("preserved".into()))],
        }],
    });
    model.fonts.push(openskp::Font {
        family: "Example family".into(),
        bold: true,
        italic: true,
        size_pt: 18,
        model_height: false,
        height_m: 0.02,
    });
    model.texts = vec![
        openskp::Text {
            pid: 9001,
            content: "Screen α".into(),
            screen_position: [0.2, 0.3],
            font: Some(0),
            ..Default::default()
        },
        openskp::Text {
            pid: 9002,
            content: "Edge quarter".into(),
            anchor: openskp::Anchor {
                kind: 5,
                entity: Some(edge.pid),
                point_m: [0.25, 0.0, 0.0],
                ..Default::default()
            },
            leader: openskp::Leader::Pushpin,
            leader_offset: [0.1, 0.2, 0.0],
            arrow: 4,
            font: Some(0),
            ..Default::default()
        },
    ];
    model.dimensions.push(openskp::Dimension {
        pid: 9003,
        def_index: None,
        text_override: "L".into(),
        start: openskp::Anchor {
            kind: 4,
            entity: Some(edge.pid),
            ..Default::default()
        },
        end: openskp::Anchor {
            kind: 1,
            point_m: [2.0, 0.0, 0.0],
            ..Default::default()
        },
        normal: [0.0, 0.0, 1.0],
        x_axis: [1.0, 0.0, 0.0],
        offset_m: 0.2,
        text_position: 0,
        aligned: true,
        font: Some(0),
        arrow: 3,
        layer: 0,
        material: None,
        hidden: false,
        casts_shadows: true,
        receives_shadows: true,
        variant: 1,
        undecoded: 0.0,
    });
    for year in [2017, 2026] {
        let input = encoded(&model, year);
        if year == 2017 {
            let classic = classic::read(&input, openskp::Model::parse(&input).unwrap()).unwrap();
            assert_eq!(classic.texts.len(), 2);
            assert_eq!(classic.dimensions.len(), 1);
            let (doc, _) = decode::document(&classic, &[0; 32]).unwrap();
            assert_eq!(doc.model.texts().count(), 2);
            assert_eq!(
                doc.attrs.get("import-regression", "root"),
                Some(&Value::Str("preserved".into()))
            );
        }
        let doc = native(&input);
        let texts = doc.model.texts().map(|(_, t)| t).collect::<Vec<_>>();
        assert_eq!(texts.len(), 2);
        let screen = texts.iter().find(|t| t.text == "Screen α").unwrap();
        let layout = screen.layout.as_ref().unwrap();
        assert!((layout.screen_position.unwrap() - glam::DVec2::new(0.2, 0.3)).length() < 1e-9);
        assert_eq!(layout.font.as_ref().unwrap().family, "Example family");
        assert!(layout.font.as_ref().unwrap().bold && layout.font.as_ref().unwrap().italic);
        let attached = texts.iter().find(|t| t.text == "Edge quarter").unwrap();
        assert!(
            matches!(attached.anchor.as_ref().unwrap().position,Some(AnchorPosition::EdgeParameter(t)) if (t-0.25).abs() < 1e-9)
        );
        assert!(
            (attached.layout.as_ref().unwrap().world_offset.unwrap()
                - DVec3::new(100.0, 200.0, 0.0))
            .length()
                < 1e-6
        );
        let anchor = attached.anchor.as_ref().unwrap().clone();
        let before = annotations::anchor_position(&doc, ContainerRef::Model, &anchor).unwrap();
        let mut engine = planyrith_core::commands::Engine::new(
            doc,
            std::sync::Arc::new(planyrith_core::commands::CommandRegistry::with_builtins()),
        );
        engine
            .execute_erased(
                "move",
                serde_json::json!({"targets":"edge","delta":[100,0,0]}),
            )
            .unwrap();
        let after =
            annotations::anchor_position(engine.document(), ContainerRef::Model, &anchor).unwrap();
        assert!((after - before - DVec3::X * 100.0).length() < 1e-6);
        engine.undo().unwrap();
        assert!(
            (annotations::anchor_position(engine.document(), ContainerRef::Model, &anchor)
                .unwrap()
                - before)
                .length()
                < 1e-6
        );
        let (_, dimension) = engine.document().model.dimensions().next().unwrap();
        let Dimension::Linear { style, offset, .. } = dimension else {
            panic!("expected linear dimension")
        };
        assert!((offset.y - 200.0).abs() < 1e-6);
        assert_eq!(style.font.as_ref().unwrap().family, "Example family");
    }
}

#[test]
fn circular_descriptors_import_as_native_curves() {
    for file in [
        include_bytes!("../tests/fixtures/intermediate/arcs_2017.skp").as_slice(),
        include_bytes!("../tests/fixtures/intermediate/arcs_2020.skp").as_slice(),
        include_bytes!("../tests/fixtures/intermediate/arcs_2021.skp").as_slice(),
    ] {
        let doc = native(file);
        assert_eq!(doc.model.mesh().curves().count(), 3);
        assert_eq!(
            doc.model
                .mesh()
                .curves()
                .filter(|(_, c)| matches!(
                    c,
                    CurveDescriptor::Arc { .. } | CurveDescriptor::Circle { .. }
                ))
                .count(),
            3
        );
    }
}

#[test]
fn intersection_anchors_follow_both_lines() {
    let mut model = openskp::Model::default();
    let mut run = openskp::GeometryRun::default();
    run.mesh.vertices = vec![
        [-1.0, 0.0, 0.0],
        [-0.5, 0.0, 0.0],
        [0.0, 0.1, 0.0],
        [0.0, 1.0, 0.0],
    ];
    run.mesh.edges = vec![
        openskp::MeshEdge {
            pid: 1,
            v0: 0,
            v1: 1,
            ..Default::default()
        },
        openskp::MeshEdge {
            pid: 2,
            v0: 2,
            v1: 3,
            ..Default::default()
        },
    ];
    model.geometry.push(run);
    model.texts.push(openskp::Text {
        content: "Crossing".into(),
        anchor: openskp::Anchor {
            kind: 9,
            entity: Some(1),
            second: Some(2),
            ..Default::default()
        },
        ..Default::default()
    });
    let (doc, _) = decode::document(&model, &[0; 32]).unwrap();
    let anchor = doc
        .model
        .texts()
        .next()
        .unwrap()
        .1
        .anchor
        .as_ref()
        .unwrap()
        .clone();
    assert!(
        annotations::anchor_position(&doc, ContainerRef::Model, &anchor)
            .unwrap()
            .length()
            < 1e-6
    );
    let EntityId::Edge(_) = doc.model.key_to_id(anchor.path.target).unwrap() else {
        panic!("edge anchor")
    };
    let other = if let Some(AnchorPosition::Intersection(path)) = &anchor.position {
        path.target
    } else {
        panic!("intersection")
    };
    let pid = if let EntityKey::Pid(pid) = other {
        pid
    } else {
        panic!("persistent target")
    };
    let mut engine = planyrith_core::commands::Engine::new(
        doc,
        std::sync::Arc::new(planyrith_core::commands::CommandRegistry::with_builtins()),
    );
    engine
        .execute_erased(
            "move",
            serde_json::json!({"targets":pid.to_string(),"delta":[100,0,0]}),
        )
        .unwrap();
    assert!(
        (annotations::anchor_position(engine.document(), ContainerRef::Model, &anchor).unwrap()
            - DVec3::X * 100.0)
            .length()
            < 1e-6
    );
}

#[test]
fn appearance_sun_scene_and_watermarks_are_native_after_reopen() {
    let (mut model, _) = encode::model(super::tests::cube().document()).unwrap();
    let (_, png) = super::tests::textured_cube(image::ImageFormat::Png);
    let rendering = openskp::RenderingOptions {
        face_style: 5,
        edges: true,
        profiles: true,
        profiles_width: 3,
        background: [20, 40, 60, 255],
        sky: true,
        sky_color: [80, 100, 120, 255],
        fog: true,
        fog_start_m: 2.0,
        fog_end_m: 20.0,
        watermarks: true,
        section_display: 3,
        section_fill: Some(true),
        ..Default::default()
    };
    model.rendering = Some(rendering.clone());
    model.watermarks = vec![openskp::Watermark {
        name: "Mark".into(),
        source_file: String::new(),
        background: true,
        tiled: false,
        stretched: false,
        keep_aspect_ratio: true,
        position: 8,
        mask: false,
        blend: 0.75,
        scale: 0.4,
        image_bytes: Some(png),
        time: 0,
    }];
    model.shadows = Some(openskp::ShadowInfo {
        time: 1_730_304_000,
        latitude: 45.0,
        longitude: 9.0,
        tz_offset_h: 1.0,
        displayed: true,
        on_faces: true,
        on_ground: true,
        light: 80,
        dark: 20,
        ..Default::default()
    });
    model.scenes = vec![openskp::Scene {
        name: "Fog view".into(),
        rendering: Some(rendering),
        shadows: model.shadows.clone(),
        ..Default::default()
    }];
    for year in [2017, 2026] {
        let doc = native(&encoded(&model, year));
        let style = doc.styles.active_style().unwrap();
        assert_eq!(style.faces.mode, FaceMode::Monochrome);
        assert_eq!(style.edges.profiles, Some(3));
        assert_eq!(style.background.color, Rgba8::rgb(20, 40, 60));
        let fog = style.fog.as_ref().unwrap();
        assert!((fog.start - 2000.0).abs() < 1e-6 && (fog.end - 20000.0).abs() < 1e-6);
        assert_eq!(style.watermarks.len(), 1);
        assert_eq!(
            style.watermarks[0].placement,
            WatermarkPlacement::Anchored(1.0, 1.0)
        );
        assert!((style.watermarks[0].opacity - 0.75).abs() < 1e-6);
        assert!(doc.images.get(style.watermarks[0].image).is_some());
        assert!(doc.rendering.shadows.enabled);
        assert!(
            matches!(doc.rendering.shadows.sun,SunModel::Geo{lat,lon,..} if (lat-45.0).abs()<1e-6 && (lon-9.0).abs()<1e-6)
        );
        let scene = doc.scenes.iter().next().unwrap().1;
        assert!(scene.shadows.as_ref().unwrap().enabled);
        assert_eq!(
            doc.styles.get(scene.style.unwrap()).unwrap().faces.mode,
            FaceMode::Monochrome
        );
        assert!(
            doc.attrs
                .get("org.planyrith.skp", "decoded-settings")
                .is_some()
        );
    }
    // Zero is valid for stretched watermarks: their size ignores this field.
    model.watermarks[0].scale = 0.0;
    model.watermarks[0].stretched = true;
    assert!(decode::document(&model, &[0; 32]).is_ok());
}

#[test]
fn image_entities_and_spatial_polylines_keep_assets_and_placement() {
    let (engine, _) = super::tests::textured_cube(image::ImageFormat::Png);
    let (mut model, _) = encode::model(engine.document()).unwrap();
    let material = model.geometry[0].mesh.faces[0].front_material;
    model.definitions = vec![openskp::Definition {
        name: "Image".into(),
        guid: "1234567890abcdef1234567890abcdef".into(),
        kind: openskp::DefinitionKind::Image,
        map_index: Some(50),
        ..Default::default()
    }];
    model.definition_links = vec![(50, 0)];
    let mut image = openskp::GeometryRun {
        def_index: Some(50),
        ..Default::default()
    };
    image.mesh.vertices = vec![
        [0.0, 0.0, 0.0],
        [0.2, 0.0, 0.0],
        [0.2, 0.1, 0.0],
        [0.0, 0.1, 0.0],
    ];
    image.mesh.edges = (0..4)
        .map(|i| openskp::MeshEdge {
            pid: 100 + i,
            v0: i,
            v1: (i + 1) % 4,
            ..Default::default()
        })
        .collect();
    image.mesh.faces.push(openskp::MeshFace {
        pid: 110,
        outer: vec![0, 1, 2, 3],
        normal: [0.0, 0.0, 1.0],
        front_material: material,
        ..Default::default()
    });
    let mut root = openskp::GeometryRun::default();
    root.images.push(openskp::ImagePlacement {
        pid: 120,
        defref: 50,
        transform: [
            1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 10.0, 20.0, 0.0, 1.0,
        ],
        hidden: false,
        layer: 0,
        casts_shadows: false,
        receives_shadows: true,
    });
    root.polylines.push(openskp::Polyline {
        pid: 130,
        points_m: vec![[1.0, 0.0, 0.0], [2.0, 0.0, 0.1], [2.0, 1.0, 0.2]],
        ..Default::default()
    });
    model.geometry = vec![image, root];
    for year in [2017, 2026] {
        let doc = native(&encoded(&model, year));
        let (_, image) = doc.model.images().next().unwrap();
        assert!((image.width.as_mm() - 200.0).abs() < 1e-6);
        assert!((image.height.as_mm() - 100.0).abs() < 1e-6);
        assert!((image.transform.translation - DVec3::new(254.0, 508.0, 0.0)).length() < 1e-6);
        let material = doc.materials.get(image.material).unwrap();
        assert!(
            doc.images
                .get(material.texture.as_ref().unwrap().image)
                .is_some()
        );
        assert_eq!(doc.model.mesh().edge_count(), 2);
        assert!(
            matches!(doc.model.mesh().curves().next().unwrap().1,CurveDescriptor::Freehand{points} if points.len()==3 && (points[2]-DVec3::new(2000.0,1000.0,200.0)).length()<1e-6)
        );
    }
}

#[test]
fn independent_text_arrows_and_style_fixtures_open() {
    let doc = native(include_bytes!(
        "../tests/fixtures/intermediate/legacy_text_arrows_2017.skp"
    ));
    assert_eq!(doc.model.texts().count(), 3);
    for arrow in [ArrowStyle::Slash, ArrowStyle::Dot, ArrowStyle::Open] {
        assert!(
            doc.model
                .texts()
                .any(|(_, t)| t.layout.as_ref().is_some_and(|l| l.arrow == arrow))
        );
    }
    let doc = native(include_bytes!(
        "../tests/fixtures/intermediate/style_watermark.skp"
    ));
    assert!(doc.styles.iter().any(|(_, s)| !s.watermarks.is_empty()));
}

#[test]
fn nested_annotation_paths_survive_merge_reuse_and_undo() {
    use planyrith_core::{
        commands::{CommandRegistry, Engine},
        io::{MemoryStorage, Storage, StorageKey},
    };
    use std::sync::Arc;
    let mut engine = super::tests::cube();
    engine
        .execute_erased(
            "make-component",
            serde_json::json!({"targets":"all","name":"Block"}),
        )
        .unwrap();
    engine
        .execute_erased(
            "make-group",
            serde_json::json!({"targets":"all","name":"Pair"}),
        )
        .unwrap();
    let (mut model, _) = encode::model(engine.document()).unwrap();
    let root = model
        .geometry
        .iter()
        .find(|r| r.def_index.is_none())
        .unwrap();
    let first = &root.placed[0];
    let inner = model
        .geometry
        .iter()
        .find(|r| r.def_index == Some(first.defref as usize))
        .unwrap();
    let second = &inner.placed[0];
    let block = model
        .geometry
        .iter()
        .find(|r| r.def_index == Some(second.defref as usize))
        .unwrap();
    let edge = &block.mesh.edges[0];
    let expected = (DVec3::from_array(block.mesh.vertices[edge.v0 as usize]).lerp(
        DVec3::from_array(block.mesh.vertices[edge.v1 as usize]),
        0.25,
    )) * 1000.0;
    model.texts.push(openskp::Text {
        pid: 10001,
        content: "Nested".into(),
        anchor: openskp::Anchor {
            kind: 5,
            entity: Some(edge.pid),
            point_m: [0.25, 0.0, 0.0],
            path: vec![first.pid, second.pid],
            ..Default::default()
        },
        ..Default::default()
    });
    let doc = native(&encoded(&model, 2026));
    let storage = Arc::new(MemoryStorage::new());
    storage
        .write(&StorageKey::new("nested.plyr"), &super::tests::bytes(&doc))
        .unwrap();
    let mut destination = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    destination.set_storage(storage);
    // Force source and destination pids to overlap across container boundaries.
    destination
        .execute_erased(
            "line",
            serde_json::json!({"points":[[-2000,0,0],[-1000,0,0]]}),
        )
        .unwrap();
    for count in [1, 2] {
        destination
            .execute_erased(
                "import",
                serde_json::json!({"path":"nested.plyr","at":[3000*count,0,0]}),
            )
            .unwrap();
        let mut found = 0;
        for c in destination.document().containers() {
            for (_, text) in destination.document().container(c).unwrap().texts() {
                if text.text == "Nested" {
                    found += 1;
                    assert!(
                        (annotations::anchor_position(
                            destination.document(),
                            c,
                            text.anchor.as_ref().unwrap()
                        )
                        .unwrap()
                            - expected)
                            .length()
                            < 1e-6
                    );
                }
            }
        }
        assert_eq!(found, count);
    }
    destination.undo().unwrap();
    destination.redo().unwrap();
    let reopened = native::read_document(&super::tests::bytes(destination.document()), &NoProgress)
        .unwrap()
        .document;
    for c in reopened.containers() {
        for (_, text) in reopened.container(c).unwrap().texts() {
            assert!(
                (annotations::anchor_position(&reopened, c, text.anchor.as_ref().unwrap())
                    .unwrap()
                    - expected)
                    .length()
                    < 1e-6
            );
        }
    }
}

#[test]
fn invalid_imported_annotation_coordinates_fail_without_a_partial_document() {
    let mut model = openskp::Model::default();
    model.texts.push(openskp::Text {
        screen_position: [f64::NAN, 0.0],
        ..Default::default()
    });
    assert!(decode::document(&model, &[0; 32]).is_err());
}

#[test]
fn radial_dimensions_keep_analytic_measurement_and_leader_angle() {
    let input = include_bytes!("../tests/fixtures/intermediate/arcs_2017.skp");
    let mut model = classic::read(input, openskp::Model::parse(input).unwrap()).unwrap();
    let run = model
        .geometry
        .iter()
        .find(|r| r.def_index.is_none())
        .unwrap();
    let edge = run.mesh.edges.iter().find(|e| e.curve.is_some()).unwrap();
    let arc = run.curves[edge.curve.unwrap() as usize]
        .arc
        .as_ref()
        .unwrap();
    let radius = DVec3::from_array(arc.x_axis_m).length() * 1000.0;
    model.radial_dimensions.push(openskp::RadialDimension {
        pid: 10002,
        edge: Some(edge.pid),
        leader: [0.7, -2.0],
        ..Default::default()
    });
    for year in [2017, 2026] {
        let doc = native(&encoded(&model, year));
        let (_, dimension) = doc.model.dimensions().next().unwrap();
        let view = annotations::dimension_view(&doc, ContainerRef::Model, dimension);
        assert!(!view.detached);
        assert!((view.value - radius).abs() < 1e-6);
        assert!(((view.line[1] - view.line[0]).length() - 50.8).abs() < 1e-6);
    }
}

#[test]
fn truncated_classic_archives_return_errors() {
    let bytes = include_bytes!("../tests/fixtures/intermediate/sdk_saved_2020.skp");
    for end in (0..bytes.len() / 2).step_by(31) {
        assert!(classic::read(&bytes[..end], openskp::Model::default()).is_err());
    }
}
