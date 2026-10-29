//! Implements docs/15 §7: deterministic multi-page PDF with subset Unicode
//! fonts, SVG in millimeters, and paper-space DXF with raster side files.

use super::Color;
use super::layout::{Drawing, Graphic, Result, SheetError, clip_segment};
use pdf_writer::{Content, Finish, Name, Pdf, Ref, Str, TextStr};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;

/// Named binary side resources of an exported sheet.
pub type SideFiles = Vec<(String, Vec<u8>)>;

fn invalid(e: impl std::fmt::Display) -> SheetError {
    SheetError::Invalid(e.to_string())
}
fn n(x: f64) -> String {
    if x.abs() < 1e-10 {
        "0".into()
    } else {
        format!("{x:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .into()
    }
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn rgb(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}
/// Base64 encoder for self-contained SVG image and font resources.
pub fn base64(bytes: &[u8]) -> String {
    const C: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for p in bytes.chunks(3) {
        let a = u32::from(p[0]);
        let b = u32::from(*p.get(1).unwrap_or(&0));
        let c = u32::from(*p.get(2).unwrap_or(&0));
        let v = (a << 16) | (b << 8) | c;
        out.push(C[((v >> 18) & 63) as usize] as char);
        out.push(C[((v >> 12) & 63) as usize] as char);
        out.push(if p.len() > 1 {
            C[((v >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if p.len() > 2 {
            C[(v & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
fn mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(b"RIFF") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}
/// Standalone SVG page: millimeter user coordinates and embedded image/font bytes.
pub fn svg(page: &Drawing) -> String {
    let h = page.paper.height;
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}mm\" height=\"{}mm\" viewBox=\"0 0 {} {}\"><title>{}</title>\n",
        n(page.paper.width),
        n(h),
        n(page.paper.width),
        n(h),
        xml(&page.name)
    );
    if page
        .groups
        .iter()
        .any(|g| g.graphics.iter().any(|p| matches!(p, Graphic::Text { .. })))
    {
        let _ = writeln!(
            out,
            "<style>@font-face{{font-family:Planyrith;src:url(data:font/ttf;base64,{}) format('truetype')}}</style>",
            base64(crate::fonts::INTER)
        );
    }
    for (i, g) in page.groups.iter().enumerate() {
        let _ = write!(out, "<g data-layer=\"{}\" id=\"item-{i}\">", xml(&g.layer));
        if !g.clip.is_empty() {
            let points = g
                .clip
                .iter()
                .map(|p| format!("{},{}", n(p[0]), n(h - p[1])))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(
                out,
                "<defs><clipPath id=\"clip-{i}\"><polygon points=\"{points}\"/></clipPath></defs><g clip-path=\"url(#clip-{i})\">"
            );
        }
        for graphic in &g.graphics {
            match graphic {
                Graphic::Path {
                    points,
                    closed,
                    stroke,
                    fill,
                    tag,
                } => {
                    let shape = if *closed { "polygon" } else { "polyline" };
                    if let Some(tag) = tag {
                        let _ = write!(out, "<g data-tag=\"{}\">", xml(tag));
                    }
                    let points = points
                        .iter()
                        .map(|p| format!("{},{}", n(p[0]), n(h - p[1])))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = write!(
                        out,
                        "<{shape} points=\"{points}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"{}\" stroke-opacity=\"{}\"",
                        fill.map_or_else(|| "none".into(), rgb),
                        rgb(stroke.color),
                        n(stroke.width),
                        n(f64::from(stroke.color[3]) / 255.0)
                    );
                    if let Some(fill) = fill {
                        let _ = write!(out, " fill-opacity=\"{}\"", n(f64::from(fill[3]) / 255.0));
                    }
                    if !stroke.dash.is_empty() {
                        let _ = write!(
                            out,
                            " stroke-dasharray=\"{}\"",
                            stroke
                                .dash
                                .iter()
                                .map(|x| n(*x))
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                    }
                    out.push_str("/>\n");
                    if tag.is_some() {
                        out.push_str("</g>");
                    }
                }
                Graphic::Text {
                    text,
                    at,
                    height,
                    color,
                } => {
                    let _ = writeln!(
                        out,
                        "<text x=\"{}\" y=\"{}\" font-family=\"Planyrith\" font-size=\"{}\" fill=\"{}\" fill-opacity=\"{}\">{}</text>",
                        n(at[0]),
                        n(h - at[1]),
                        n(*height),
                        rgb(*color),
                        n(f64::from(color[3]) / 255.0),
                        xml(text)
                    );
                }
                Graphic::Image { bytes, frame: r } => {
                    let _ = writeln!(
                        out,
                        "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" href=\"data:{};base64,{}\"/>",
                        n(r.x),
                        n(h - r.y - r.height),
                        n(r.width),
                        n(r.height),
                        mime(bytes),
                        base64(bytes)
                    );
                }
            }
        }
        if !g.clip.is_empty() {
            out.push_str("</g>");
        }
        out.push_str("</g>\n");
    }
    out.push_str("</svg>\n");
    out
}
fn compressed(data: &[u8]) -> Result<Vec<u8>> {
    let mut writer = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    writer.write_all(data).map_err(invalid)?;
    writer.finish().map_err(invalid)
}
fn pdf_path(content: &mut Content, points: &[[f64; 2]], closed: bool) {
    if let Some(p) = points.first() {
        content.move_to(p[0] as f32, p[1] as f32);
        for p in &points[1..] {
            content.line_to(p[0] as f32, p[1] as f32);
        }
        if closed {
            content.close_path();
        }
    }
}
fn flat(c: Color) -> [f32; 3] {
    [c[0], c[1], c[2]].map(|x| f32::from(x) / 255.0)
}
fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(invalid)?;
    let (w, h) = reader.into_dimensions().map_err(invalid)?;
    if u64::from(w) * u64::from(h) > 16_777_216 {
        return Err(SheetError::Limit(
            "embedded sheet image exceeds 16 megapixels".into(),
        ));
    }
    image::load_from_memory(bytes).map_err(invalid)
}
/// PDF pages in print order, with exact mm-to-point mapping and embedded subset font.
pub fn pdf(pages: &[Drawing], title: &str, author: &str) -> Result<Vec<u8>> {
    use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
    if pages.is_empty() {
        return Err(invalid("no sheet pages selected"));
    }
    let mut pdf = Pdf::new();
    let mut next = 10;
    let mut alloc = || {
        let id = Ref::new(next);
        next += 1;
        id
    };
    let (catalog, tree, font, cid, desc, font_file, cmap, info) = (
        Ref::new(1),
        Ref::new(2),
        Ref::new(3),
        Ref::new(4),
        Ref::new(5),
        Ref::new(6),
        Ref::new(7),
        Ref::new(8),
    );
    let page_ids = pages.iter().map(|_| alloc()).collect::<Vec<_>>();
    let layer_names = pages
        .iter()
        .flat_map(|p| p.groups.iter().map(|g| g.layer.clone()))
        .collect::<BTreeSet<_>>();
    let layers = layer_names
        .into_iter()
        .map(|n| (n, alloc()))
        .collect::<BTreeMap<_, _>>();
    let alphas = pages
        .iter()
        .flat_map(|p| &p.groups)
        .flat_map(|g| &g.graphics)
        .filter_map(|p| match p {
            Graphic::Path { stroke, fill, .. } => {
                Some((stroke.color[3], fill.map_or(255, |c| c[3])))
            }
            Graphic::Text { color, .. } => Some((255, color[3])),
            _ => None,
        })
        .chain(std::iter::once((255, 255)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|a| (a, alloc()))
        .collect::<BTreeMap<_, _>>();
    let labels = pages.iter().map(|_| alloc()).collect::<Vec<_>>();
    {
        let mut catalog = pdf.catalog(catalog);
        catalog.pages(tree);
        {
            let mut tree = catalog.page_labels();
            let mut nums = tree.nums();
            for (i, label) in labels.iter().enumerate() {
                nums.insert(i as i32, *label);
            }
        }
        let mut oc = catalog.insert(Name(b"OCProperties")).dict();
        oc.insert(Name(b"OCGs"))
            .array()
            .items(layers.values().copied());
        let mut config = oc.insert(Name(b"D")).dict();
        config.pair(Name(b"Name"), TextStr("Sheet layers"));
        config.pair(Name(b"BaseState"), Name(b"ON"));
        config
            .insert(Name(b"Order"))
            .array()
            .items(layers.values().copied());
    }
    for ((stroke, fill), id) in &alphas {
        pdf.ext_graphics(*id)
            .stroking_alpha(f32::from(*stroke) / 255.0)
            .non_stroking_alpha(f32::from(*fill) / 255.0);
    }
    pdf.pages(tree)
        .kids(page_ids.iter().copied())
        .count(pages.len() as i32);
    for ((page, label), _) in pages.iter().zip(&labels).zip(0..) {
        pdf.indirect(*label)
            .dict()
            .pair(Name(b"P"), TextStr(&page.name));
    }
    for (name, id) in &layers {
        pdf.indirect(*id)
            .dict()
            .pair(Name(b"Type"), Name(b"OCG"))
            .pair(Name(b"Name"), TextStr(name));
    }
    pdf.document_info(info)
        .title(TextStr(title))
        .author(TextStr(author))
        .creator(TextStr("Planyrith Sheets"));
    let face = ttf_parser::Face::parse(crate::fonts::INTER, 0).map_err(invalid)?;
    let chars = pages
        .iter()
        .flat_map(|p| &p.groups)
        .flat_map(|g| &g.graphics)
        .filter_map(|g| {
            if let Graphic::Text { text, .. } = g {
                Some(text.chars())
            } else {
                None
            }
        })
        .flatten()
        .collect::<BTreeSet<_>>();
    let mut mapper = subsetter::GlyphRemapper::new();
    let mut codes = BTreeMap::new();
    for c in chars {
        let old = face.glyph_index(c).unwrap_or(ttf_parser::GlyphId(0)).0;
        let id = mapper.remap(old);
        codes.insert(c, (id, old));
    }
    let subset = subsetter::subset(crate::fonts::INTER, 0, &mapper).map_err(invalid)?;
    let font_name = Name(b"PLANYR+Inter-Regular");
    pdf.type0_font(font)
        .base_font(font_name)
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(cid)
        .to_unicode(cmap);
    let system = SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    };
    let mut cid_font = pdf.cid_font(cid);
    cid_font
        .subtype(CidFontType::Type2)
        .base_font(font_name)
        .system_info(system)
        .font_descriptor(desc)
        .cid_to_gid_map_predefined(Name(b"Identity"));
    let units = f32::from(face.units_per_em());
    let widths = codes
        .values()
        .map(|(new, old)| {
            (
                *new,
                face.glyph_hor_advance(ttf_parser::GlyphId(*old))
                    .unwrap_or(0),
            )
        })
        .collect::<BTreeMap<_, _>>();
    {
        let mut array = cid_font.widths();
        for (id, width) in widths {
            array.consecutive(id, [f32::from(width) * 1000.0 / units]);
        }
    }
    cid_font.finish();
    let bbox = face.global_bounding_box();
    let k = 1000.0 / units;
    pdf.font_descriptor(desc)
        .name(font_name)
        .flags(FontFlags::NON_SYMBOLIC)
        .bbox(pdf_writer::Rect::new(
            f32::from(bbox.x_min) * k,
            f32::from(bbox.y_min) * k,
            f32::from(bbox.x_max) * k,
            f32::from(bbox.y_max) * k,
        ))
        .italic_angle(0.0)
        .ascent(f32::from(face.ascender()) * k)
        .descent(f32::from(face.descender()) * k)
        .cap_height(f32::from(face.capital_height().unwrap_or(face.ascender())) * k)
        .stem_v(80.0)
        .font_file2(font_file);
    pdf.stream(font_file, &compressed(&subset)?)
        .filter(pdf_writer::Filter::FlateDecode)
        .pair(Name(b"Length1"), subset.len() as i32);
    let mut unicode = UnicodeCmap::new(
        Name(b"PlanyrithUnicode"),
        SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"UCS"),
            supplement: 0,
        },
    );
    let mut seen = BTreeSet::new();
    for (c, (id, _)) in &codes {
        if seen.insert(*id) {
            unicode.pair(*id, *c);
        }
    }
    pdf.stream(cmap, &unicode.finish());
    for (page, page_id) in pages.iter().zip(page_ids) {
        let content_id = alloc();
        let mut content = Content::new();
        let pt = 72.0f32 / 25.4;
        content.transform([pt, 0.0, 0.0, pt, 0.0, 0.0]);
        let mut images = Vec::new();
        for g in &page.groups {
            content.save_state();
            let layer = layers
                .get(&g.layer)
                .copied()
                .ok_or_else(|| invalid("PDF layer missing"))?;
            let layer_name = format!("L{}", layer.get());
            content
                .begin_marked_content_with_properties(Name(b"OC"))
                .properties_named(Name(layer_name.as_bytes()));
            if !g.clip.is_empty() {
                pdf_path(&mut content, &g.clip, true);
                content.clip_nonzero();
                content.end_path();
            }
            for graphic in &g.graphics {
                let alpha = match graphic {
                    Graphic::Path { stroke, fill, .. } => {
                        (stroke.color[3], fill.map_or(255, |c| c[3]))
                    }
                    Graphic::Text { color, .. } => (255, color[3]),
                    _ => (255, 255),
                };
                if let Some(id) = alphas.get(&alpha) {
                    let name = format!("A{}", id.get());
                    content.set_parameters(Name(name.as_bytes()));
                }
                match graphic {
                    Graphic::Path {
                        points,
                        closed,
                        stroke,
                        fill,
                        ..
                    } => {
                        let [r, g, b] = flat(stroke.color);
                        content.set_stroke_rgb(r, g, b);
                        content.set_line_width(stroke.width as f32);
                        content.set_dash_pattern(stroke.dash.iter().map(|x| *x as f32), 0.0);
                        pdf_path(&mut content, points, *closed);
                        if let Some(color) = fill {
                            let [r, g, b] = flat(*color);
                            content.set_fill_rgb(r, g, b);
                            content.fill_nonzero_and_stroke();
                        } else {
                            content.stroke();
                        }
                    }
                    Graphic::Text {
                        text,
                        at,
                        height,
                        color,
                    } => {
                        let [r, g, b] = flat(*color);
                        content.set_fill_rgb(r, g, b);
                        content.begin_text();
                        content.set_font(Name(b"F0"), *height as f32);
                        content.next_line(at[0] as f32, at[1] as f32);
                        let encoded = text
                            .chars()
                            .flat_map(|c| codes.get(&c).map_or(0, |(id, _)| *id).to_be_bytes())
                            .collect::<Vec<_>>();
                        content.show(Str(&encoded));
                        content.end_text();
                    }
                    Graphic::Image { bytes, frame: r } => {
                        let dynamic = decode_image(bytes)?;
                        let image_id = alloc();
                        let name = format!("Im{}", image_id.get());
                        let rgb = dynamic.to_rgb8();
                        let encoded = compressed(rgb.as_raw())?;
                        let mask = dynamic.color().has_alpha().then(&mut alloc);
                        {
                            let mut image = pdf.image_xobject(image_id, &encoded);
                            image.filter(pdf_writer::Filter::FlateDecode);
                            image
                                .width(dynamic.width() as i32)
                                .height(dynamic.height() as i32)
                                .bits_per_component(8);
                            image.color_space().device_rgb();
                            if let Some(mask) = mask {
                                image.s_mask(mask);
                            }
                        }
                        if let Some(mask) = mask {
                            let alpha = dynamic
                                .to_rgba8()
                                .pixels()
                                .map(|p| p[3])
                                .collect::<Vec<_>>();
                            let encoded = compressed(&alpha)?;
                            let mut image = pdf.image_xobject(mask, &encoded);
                            image.filter(pdf_writer::Filter::FlateDecode);
                            image
                                .width(dynamic.width() as i32)
                                .height(dynamic.height() as i32)
                                .bits_per_component(8);
                            image.color_space().device_gray();
                        }
                        content.save_state();
                        content.transform([
                            r.width as f32,
                            0.0,
                            0.0,
                            r.height as f32,
                            r.x as f32,
                            r.y as f32,
                        ]);
                        content.x_object(Name(name.as_bytes()));
                        content.restore_state();
                        images.push((name, image_id));
                    }
                }
            }
            content.end_marked_content();
            content.restore_state();
        }
        let mut p = pdf.page(page_id);
        p.parent(tree)
            .media_box(pdf_writer::Rect::new(
                0.0,
                0.0,
                page.paper.width as f32 * pt,
                page.paper.height as f32 * pt,
            ))
            .contents(content_id);
        {
            let mut resources = p.resources();
            resources.fonts().pair(Name(b"F0"), font);
            {
                let mut objects = resources.x_objects();
                for (name, id) in &images {
                    objects.pair(Name(name.as_bytes()), *id);
                }
            }
            {
                let mut states = resources.ext_g_states();
                for id in alphas.values() {
                    let name = format!("A{}", id.get());
                    states.pair(Name(name.as_bytes()), *id);
                }
            }
            let mut props = resources.insert(Name(b"Properties")).dict();
            for id in layers.values() {
                let name = format!("L{}", id.get());
                props.pair(Name(name.as_bytes()), *id);
            }
        }
        p.finish();
        pdf.stream(content_id, &content.finish());
    }
    Ok(pdf.finish())
}
fn dxf_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii() && !c.is_control() {
            out.push(c);
        } else {
            let mut units = [0; 2];
            for x in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\U+{x:04X}");
            }
        }
    }
    out
}
fn dxf_header(kind: &str, layer: &str, handle: &mut u32) -> String {
    let id = *handle;
    *handle += 1;
    let subclass = match kind {
        "LINE" => "AcDbLine",
        "TEXT" => "AcDbText",
        "SOLID" => "AcDbTrace",
        "IMAGE" => "AcDbRasterImage",
        _ => "AcDbEntity",
    };
    format!(
        "0\n{kind}\n5\n{id:X}\n100\nAcDbEntity\n8\n{}\n67\n0\n410\nModel\n100\n{subclass}\n",
        dxf_text(layer)
    )
}
fn dxf_color(out: &mut String, color: Color) {
    let _ = write!(
        out,
        "420\n{}\n440\n{}\n",
        u32::from(color[0]) * 65536 + u32::from(color[1]) * 256 + u32::from(color[2]),
        0x02000000u32 | u32::from(255 - color[3])
    );
}
fn dxf_layer(layer: &str, tag: Option<&str>) -> String {
    let layer = tag.map_or_else(|| layer.into(), |tag| format!("{layer}_{tag}"));
    layer
        .chars()
        .map(|c| {
            if "<>/\\\":;?*|=,`".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}
fn dxf_weight(width: f64) -> i32 {
    let value = width * 100.0;
    [
        0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120, 140, 158,
        200, 211,
    ]
    .into_iter()
    .min_by(|a, b| {
        (f64::from(*a) - value)
            .abs()
            .total_cmp(&(f64::from(*b) - value).abs())
    })
    .unwrap_or(25)
}
fn dash_key(dash: &[f64]) -> String {
    dash.iter().map(|x| n(*x)).collect::<Vec<_>>().join(",")
}
/// One DXF per page, in model space at printed millimeter scale, plus PNGs.
pub fn dxf(page: &Drawing, stem: &str) -> Result<(String, SideFiles)> {
    let mut handle = 0x100u32;
    let mut entities = String::new();
    let mut objects = String::new();
    let mut images = Vec::new();
    let mut definitions = Vec::new();
    let mut layers = BTreeSet::new();
    let mut patterns = BTreeMap::new();
    for group in &page.groups {
        for graphic in &group.graphics {
            match graphic {
                Graphic::Path { tag, stroke, .. } => {
                    layers.insert(dxf_layer(&group.layer, tag.as_deref()));
                    if !stroke.dash.is_empty() {
                        patterns.insert(dash_key(&stroke.dash), stroke.dash.clone());
                    }
                }
                _ => {
                    layers.insert(dxf_layer(&group.layer, None));
                }
            }
        }
    }
    if patterns.len() > 4096 {
        return Err(SheetError::Limit(
            "DXF has more than 4096 distinct dash patterns".into(),
        ));
    }
    let mut out = format!(
        "0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1015\n9\n$INSUNITS\n70\n4\n9\n$TILEMODE\n70\n1\n999\nSheet {} - paper millimeters\n0\nENDSEC\n0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n5\n10\n100\nAcDbSymbolTable\n70\n{}\n",
        dxf_text(&page.name),
        layers.len()
    );
    for layer in layers {
        let id = handle;
        handle += 1;
        let _ = write!(
            out,
            "0\nLAYER\n5\n{id:X}\n100\nAcDbSymbolTableRecord\n100\nAcDbLayerTableRecord\n2\n{}\n70\n0\n62\n7\n6\nCONTINUOUS\n",
            dxf_text(&layer)
        );
    }
    out.push_str("0\nENDTAB\n0\nTABLE\n2\nLTYPE\n5\n11\n100\nAcDbSymbolTable\n70\n0\n");
    let mut types = BTreeMap::new();
    for (i, (key, pattern)) in patterns.into_iter().enumerate() {
        let id = handle;
        handle += 1;
        let name = format!("P{}", i + 1);
        let _ = write!(
            out,
            "0\nLTYPE\n5\n{id:X}\n100\nAcDbSymbolTableRecord\n100\nAcDbLinetypeTableRecord\n2\n{name}\n70\n0\n3\nSheet pen\n72\n65\n73\n{}\n40\n{}\n",
            pattern.len(),
            n(pattern.iter().sum())
        );
        for (i, d) in pattern.iter().enumerate() {
            let _ = write!(out, "49\n{}\n74\n0\n", n(if i % 2 == 0 { *d } else { -*d }));
        }
        types.insert(key, name);
    }
    out.push_str("0\nENDTAB\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n");
    for group in &page.groups {
        for graphic in &group.graphics {
            match graphic {
                Graphic::Path {
                    points,
                    closed,
                    stroke,
                    fill,
                    tag,
                } => {
                    let layer = dxf_layer(&group.layer, tag.as_deref());
                    if let Some(fill) = fill
                        && *closed
                    {
                        let polygon = super::layout::clip_polygon(points, &group.clip);
                        let coords = polygon.iter().flat_map(|p| *p).collect::<Vec<_>>();
                        if polygon.len() >= 3 {
                            let triangles = earcutr::earcut(&coords, &[], 2).map_err(invalid)?;
                            for tri in triangles.as_chunks::<3>().0 {
                                entities.push_str(&dxf_header("SOLID", &layer, &mut handle));
                                dxf_color(&mut entities, *fill);
                                for (i, j) in
                                    [tri[0], tri[1], tri[2], tri[2]].into_iter().enumerate()
                                {
                                    let p = polygon[j];
                                    let _ = write!(
                                        entities,
                                        "{}\n{}\n{}\n{}\n{}\n0\n",
                                        10 + i,
                                        n(p[0]),
                                        20 + i,
                                        n(p[1]),
                                        30 + i
                                    );
                                }
                            }
                        }
                    }
                    for (a, b) in
                        points
                            .iter()
                            .zip(points.iter().cycle().skip(1))
                            .take(if *closed {
                                points.len()
                            } else {
                                points.len().saturating_sub(1)
                            })
                    {
                        if let Some([a, b]) = clip_segment(*a, *b, &group.clip) {
                            entities.push_str(&dxf_header("LINE", &layer, &mut handle));
                            dxf_color(&mut entities, stroke.color);
                            let _ = write!(
                                entities,
                                "370\n{}\n10\n{}\n20\n{}\n30\n0\n11\n{}\n21\n{}\n31\n0\n",
                                dxf_weight(stroke.width),
                                n(a[0]),
                                n(a[1]),
                                n(b[0]),
                                n(b[1])
                            );
                            if let Some(name) = types.get(&dash_key(&stroke.dash)) {
                                let _ = write!(entities, "6\n{name}\n");
                            }
                        }
                    }
                }
                Graphic::Text {
                    text,
                    at,
                    height,
                    color,
                } => {
                    if !group.clip.is_empty() && clip_segment(*at, *at, &group.clip).is_none() {
                        continue;
                    }
                    entities.push_str(&dxf_header(
                        "TEXT",
                        &dxf_layer(&group.layer, None),
                        &mut handle,
                    ));
                    dxf_color(&mut entities, *color);
                    let _ = write!(
                        entities,
                        "10\n{}\n20\n{}\n30\n0\n40\n{}\n1\n{}\n100\nAcDbText\n",
                        n(at[0]),
                        n(at[1]),
                        n(*height),
                        dxf_text(text)
                    );
                }
                Graphic::Image { bytes, frame: r } => {
                    let mut dynamic = decode_image(bytes)?.to_rgba8();
                    if !group.clip.is_empty() {
                        let (w, h) = dynamic.dimensions();
                        for (x, y, p) in dynamic.enumerate_pixels_mut() {
                            let paper = [
                                r.x + (f64::from(x) + 0.5) / f64::from(w) * r.width,
                                r.y + r.height - (f64::from(y) + 0.5) / f64::from(h) * r.height,
                            ];
                            if clip_segment(paper, paper, &group.clip).is_none() {
                                p[3] = 0;
                            }
                        }
                    }
                    let mut png = std::io::Cursor::new(Vec::new());
                    dynamic
                        .write_to(&mut png, image::ImageFormat::Png)
                        .map_err(invalid)?;
                    let name = format!("{stem}-image-{}.png", images.len() + 1);
                    entities.push_str(&dxf_header(
                        "IMAGE",
                        &dxf_layer(&group.layer, None),
                        &mut handle,
                    ));
                    let def = handle;
                    handle += 1;
                    let _ = write!(
                        entities,
                        "90\n0\n10\n{}\n20\n{}\n30\n0\n11\n{}\n21\n0\n31\n0\n12\n0\n22\n{}\n32\n0\n13\n{}\n23\n{}\n340\n{def:X}\n70\n11\n280\n0\n281\n50\n282\n50\n283\n0\n",
                        n(r.x),
                        n(r.y),
                        n(r.width / f64::from(dynamic.width())),
                        n(r.height / f64::from(dynamic.height())),
                        dynamic.width(),
                        dynamic.height()
                    );
                    let _ = write!(
                        objects,
                        "0\nIMAGEDEF\n5\n{def:X}\n330\nF1\n100\nAcDbRasterImageDef\n90\n0\n1\n{}\n10\n{}\n20\n{}\n11\n1\n21\n1\n280\n1\n281\n0\n",
                        dxf_text(&name),
                        dynamic.width(),
                        dynamic.height()
                    );
                    definitions.push((format!("Image{}", images.len() + 1), def));
                    images.push((name, png.into_inner()));
                }
            }
        }
    }
    out.push_str(&entities);
    out.push_str("0\nENDSEC\n0\nSECTION\n2\nOBJECTS\n");
    if !definitions.is_empty() {
        out.push_str("0\nDICTIONARY\n5\nF0\n330\n0\n100\nAcDbDictionary\n281\n1\n3\nACAD_IMAGE_DICT\n350\nF1\n0\nDICTIONARY\n5\nF1\n330\nF0\n100\nAcDbDictionary\n281\n1\n");
        for (name, id) in definitions {
            let _ = write!(out, "3\n{name}\n350\n{id:X}\n");
        }
    }
    out.push_str(&objects);
    out.push_str("0\nENDSEC\n0\nEOF\n");
    Ok((out, images))
}
/// Stable file-name component for a page; indices disambiguate sanitized names.
pub fn file_name(name: &str) -> String {
    let s = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect::<String>();
    if s.is_empty() { "sheet".into() } else { s }
}
/// Exports resolved pages; PDF is multi-page, SVG/DXF write one file per page.
pub fn files(
    pages: &[Drawing],
    destination: &str,
    title: &str,
    author: &str,
) -> Result<Vec<(String, Vec<u8>)>> {
    if pages.is_empty() {
        return Err(invalid("no sheet pages selected"));
    }
    let (stem, ext) = destination
        .rsplit_once('.')
        .ok_or_else(|| invalid("sheet destination needs .pdf, .svg or .dxf"))?;
    let ext = ext.to_ascii_lowercase();
    if ext == "pdf" {
        return Ok(vec![(destination.into(), pdf(pages, title, author)?)]);
    }
    let mut out = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let path = if pages.len() == 1 {
            destination.to_string()
        } else {
            format!("{stem}-{:02}-{}.{}", i + 1, file_name(&page.name), ext)
        };
        match ext.as_str() {
            "svg" => out.push((path, svg(page).into_bytes())),
            "dxf" => {
                let filename = path.rsplit(['/', '\\']).next().unwrap_or(&path);
                let image_stem = filename.strip_suffix(".dxf").unwrap_or(filename);
                let (dxf, images) = dxf(page, image_stem)?;
                out.push((path.clone(), dxf.into_bytes()));
                for (name, bytes) in images {
                    out.push((crate::io::import::sibling(&path, &name), bytes));
                }
            }
            _ => return Err(invalid("sheet destination needs .pdf, .svg or .dxf")),
        }
    }
    Ok(out)
}
