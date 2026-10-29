//! Implements docs/09-file-formats.md §5 2D export of a view (SVG, PDF,
//! DXF with `--2d`): the lines a camera sees, hidden lines removed in
//! software. Drawn lines are hard edges and the profiles of soft surfaces
//! (soft edges between a face turned toward the eye and one turned away);
//! boundaries and profiles are "profile" lines, drawn thicker. Each line is
//! split where any projected edge crosses it, and each piece is kept when
//! nothing lies between its middle and the eye (rays against the visible
//! faces' triangles, found through a screen grid). Pieces are clipped to
//! the view's frame and mapped to paper: a parallel view at `scale`
//! (`1:50`), otherwise fitted to the paper width.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use glam::{DVec2, DVec3};

use crate::document::{Camera, Document, Projection};
use crate::geom::EdgeFlags;
use crate::ids::EntityId;
use std::io::Write;

use crate::io::export::obj::num;
use crate::io::export::scene::collect;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::{IoError, Progress};

/// Default paper width when a view is fitted (landscape A4), mm.
pub const DEFAULT_PAPER_WIDTH_MM: f64 = 297.0;

/// The kind of a 2D line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LineKind {
    /// An edge seen inside the outline.
    Edge,
    /// A boundary or profile (silhouette) edge.
    Profile,
}

/// A 2D drawing on paper: millimeters, origin at the lower left, y up.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Drawing {
    /// Paper width, mm.
    pub width_mm: f64,
    /// Paper height, mm.
    pub height_mm: f64,
    /// Model millimeters per paper millimeter (parallel views; 0 for
    /// perspective), so a drawing can go back to model units.
    pub model_per_paper: f64,
    /// Visible line pieces.
    pub lines: Vec<(DVec2, DVec2, LineKind)>,
    /// Effective model tag for each visible line (Sheets pen tables).
    pub line_tags: Vec<Option<crate::ids::TagId>>,
}

/// Paper placement of a view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paper {
    /// Paper per model length for parallel views (`1:50` → 0.02).
    pub scale: Option<f64>,
    /// Paper width for fitted views, mm.
    pub width_mm: f64,
}

/// The camera's frame: eye, axes, and how points project.
struct View {
    eye: DVec3,
    forward: DVec3,
    right: DVec3,
    up: DVec3,
    /// `Some(focal)` for perspective (image plane at 1, scaled by focal).
    focal: Option<f64>,
    near: f64,
    /// Half extents of the view's frame in screen units.
    half: DVec2,
}

impl View {
    fn new(camera: &Camera) -> Self {
        let forward = camera.forward();
        let right = camera.right();
        let up = right.cross(forward).normalize_or(camera.up);
        let aspect = if camera.aspect > 0.0 {
            camera.aspect
        } else {
            1.0
        };
        let (focal, half) = match camera.projection {
            Projection::Parallel { height_mm } => {
                (None, DVec2::new(height_mm * aspect / 2.0, height_mm / 2.0))
            }
            Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => {
                let t = libm::tan(fov_y_deg.clamp(1.0, 170.0).to_radians() / 2.0);
                (Some(1.0 / t), DVec2::new(aspect, 1.0))
            }
        };
        View {
            eye: camera.eye,
            forward,
            right,
            up,
            focal,
            near: (camera.distance() * 1e-4).max(1e-3),
            half,
        }
    }

    fn depth(&self, p: DVec3) -> f64 {
        (p - self.eye).dot(self.forward)
    }

    /// Screen position (perspective: on the image plane times focal).
    fn screen(&self, p: DVec3) -> DVec2 {
        let d = p - self.eye;
        let s = DVec2::new(d.dot(self.right), d.dot(self.up));
        match self.focal {
            Some(f) => s / d.dot(self.forward).max(self.near) * f,
            None => s,
        }
    }

    /// The direction from `p` toward the eye.
    fn to_eye(&self, p: DVec3) -> DVec3 {
        match self.focal {
            Some(_) => (self.eye - p).normalize_or(-self.forward),
            None => -self.forward,
        }
    }

    /// The part of a segment in front of the near plane.
    fn clip_near(&self, a: DVec3, b: DVec3) -> Option<(DVec3, DVec3)> {
        if self.focal.is_none() {
            return Some((a, b));
        }
        let (da, db) = (self.depth(a) - self.near, self.depth(b) - self.near);
        match (da >= 0.0, db >= 0.0) {
            (true, true) => Some((a, b)),
            (false, false) => None,
            (true, false) => Some((a, a + (b - a) * (da / (da - db)))),
            (false, true) => Some((a + (b - a) * (da / (da - db)), b)),
        }
    }

    /// The 3D parameter on `a`→`b` of the screen parameter `s` (perspective
    /// division makes them differ).
    fn param(&self, a: DVec3, b: DVec3, s: f64) -> f64 {
        match self.focal {
            None => s,
            Some(_) => {
                let (da, db) = (self.depth(a), self.depth(b));
                let den = (1.0 - s) * db + s * da;
                if den.abs() <= f64::MIN_POSITIVE {
                    s
                } else {
                    s * da / den
                }
            }
        }
    }
}

/// A world triangle and its screen bounding box.
struct Tri {
    p: [DVec3; 3],
    lo: DVec2,
    hi: DVec2,
}

/// A uniform grid over screen space holding item indices.
struct Grid {
    lo: DVec2,
    cell: f64,
    n: [usize; 2],
    cells: Vec<Vec<u32>>,
}

impl Grid {
    fn new(lo: DVec2, hi: DVec2, count: usize) -> Self {
        let size = (hi - lo).max(DVec2::splat(1e-9));
        let cells_per_side = (count as f64).sqrt().clamp(1.0, 512.0);
        let cell = (size.x.max(size.y) / cells_per_side).max(1e-9);
        let n = [
            ((size.x / cell).ceil() as usize).clamp(1, 1024),
            ((size.y / cell).ceil() as usize).clamp(1, 1024),
        ];
        Grid {
            lo,
            cell,
            n,
            cells: vec![Vec::new(); n[0] * n[1]],
        }
    }

    fn index(&self, p: DVec2) -> [usize; 2] {
        let q = (p - self.lo) / self.cell;
        [
            (q.x.floor().max(0.0) as usize).min(self.n[0] - 1),
            (q.y.floor().max(0.0) as usize).min(self.n[1] - 1),
        ]
    }

    fn insert(&mut self, lo: DVec2, hi: DVec2, item: u32) {
        let (a, b) = (self.index(lo), self.index(hi));
        for y in a[1]..=b[1] {
            for x in a[0]..=b[0] {
                self.cells[y * self.n[0] + x].push(item);
            }
        }
    }

    fn at(&self, p: DVec2) -> &[u32] {
        let [x, y] = self.index(p);
        &self.cells[y * self.n[0] + x]
    }

    /// Items in the cells a box covers (with repeats).
    fn covering(&self, lo: DVec2, hi: DVec2, out: &mut Vec<u32>) {
        let (a, b) = (self.index(lo), self.index(hi));
        for y in a[1]..=b[1] {
            for x in a[0]..=b[0] {
                out.extend_from_slice(&self.cells[y * self.n[0] + x]);
            }
        }
    }
}

/// Ray–triangle distance along `dir` from `origin` (Möller–Trumbore).
fn ray_triangle(origin: DVec3, dir: DVec3, t: &[DVec3; 3]) -> Option<f64> {
    let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - t[0];
    let u = s.dot(p) * inv;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < -1e-9 || u + v > 1.0 + 1e-9 {
        return None;
    }
    Some(e2.dot(q) * inv)
}

/// Screen parameter of the crossing of segments `a`→`b` and `c`→`d`, on
/// the first, strictly inside both.
fn crossing(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> Option<f64> {
    let r = b - a;
    let s = d - c;
    let den = r.perp_dot(s);
    if den.abs() <= 1e-12 * r.length() * s.length() {
        return None;
    }
    let t = (c - a).perp_dot(s) / den;
    let u = (c - a).perp_dot(r) / den;
    (1e-9..1.0 - 1e-9)
        .contains(&t)
        .then_some(t)
        .filter(|_| (-1e-9..=1.0 + 1e-9).contains(&u))
}

/// Liang–Barsky clipping of a segment to a rectangle.
fn clip_rect(a: DVec2, b: DVec2, lo: DVec2, hi: DVec2) -> Option<(DVec2, DVec2)> {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-d.x, a.x - lo.x),
        (d.x, hi.x - a.x),
        (-d.y, a.y - lo.y),
        (d.y, hi.y - a.y),
    ] {
        if p.abs() <= f64::MIN_POSITIVE {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    (t0 < t1).then(|| (a + d * t0, a + d * t1))
}

/// Clips a polygon to the retained side of a section plane.
pub(crate) fn clip_section_polygon(points: &[DVec3], plane: crate::math::Plane) -> Vec<DVec3> {
    let mut out = Vec::new();
    for (a, b) in points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
    {
        let (da, db) = (plane.signed_distance(*a), plane.signed_distance(*b));
        if da <= 0.0 {
            out.push(*a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            out.push(*a + (*b - *a) * (da / (da - db)));
        }
    }
    out
}

/// The visible lines of `camera`'s view, on paper.
pub fn draw_view(doc: &Document, scope: &ExportScope, camera: &Camera, paper: Paper) -> Drawing {
    let view = View::new(camera);
    let scene = collect(doc, scope);

    // Clip each occurrence before HLR so removed section geometry cannot hide
    // retained edges. Section caps occlude like ordinary faces (docs/15 §3).
    let mut tris: Vec<Tri> = Vec::new();
    for occ in &scene.occurrences {
        let Some(mesh) = scene.mesh(occ.container) else {
            continue;
        };
        let section = crate::io::render_mesh::occurrence_section(doc, &occ.path, &occ.world);
        let mut push = |p: [DVec3; 3]| {
            if view.focal.is_some() && p.iter().any(|q| view.depth(*q) < view.near) {
                return;
            }
            let s = p.map(|q| view.screen(q));
            tris.push(Tri {
                p,
                lo: s[0].min(s[1]).min(s[2]),
                hi: s[0].max(s[1]).max(s[2]),
            });
        };
        for face in &mesh.faces {
            for t in &face.triangles {
                let p = t.map(|i| face.positions[i as usize]);
                let polygon = match section {
                    Some(cut) => clip_section_polygon(&p, cut.plane),
                    None => p.to_vec(),
                };
                for i in 1..polygon.len().saturating_sub(1) {
                    push(
                        [polygon[0], polygon[i], polygon[i + 1]]
                            .map(|q| occ.world.transform_point3(q)),
                    );
                }
            }
        }
        if let (Some(cut), Some(entities)) = (section, doc.container(occ.container)) {
            let cap = crate::io::render_mesh::section_cap(
                entities,
                &cut.plane,
                cut.fill,
                doc.tolerances(),
            );
            for p in cap.triangles.as_chunks::<3>().0.iter() {
                push([p[0], p[1], p[2]].map(|q| {
                    occ.world
                        .transform_point3(glam::Vec3::from_array(q).as_dvec3())
                }));
            }
        }
    }

    // Lines: every edge splits; hard edges and profiles are drawn.
    struct Line {
        a: DVec3,
        b: DVec3,
        sa: DVec2,
        sb: DVec2,
        kind: Option<LineKind>,
        tag: Option<crate::ids::TagId>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for occ in &scene.occurrences {
        let Some(entities) = doc.container(occ.container) else {
            continue;
        };
        let mesh = entities.mesh();
        let normal_matrix = occ.world.matrix3.inverse().transpose();
        for &id in entities.order() {
            let EntityId::Edge(e) = id else { continue };
            let Some(edge) = mesh.edge(e) else { continue };
            if edge.flags.contains(EdgeFlags::HIDDEN)
                || (entities.is_hidden(id) && !scope.include_hidden)
            {
                continue;
            }
            let Ok(seg) = mesh.edge_segment(e) else {
                continue;
            };
            let mut a = seg.a;
            let mut b = seg.b;
            if let Some(cut) =
                crate::io::render_mesh::occurrence_section(doc, &occ.path, &occ.world)
            {
                let da = cut.plane.signed_distance(a);
                let db = cut.plane.signed_distance(b);
                if da > doc.tolerances().length && db > doc.tolerances().length {
                    continue;
                }
                if da > 0.0 {
                    a = a + (b - a) * (da / (da - db));
                } else if db > 0.0 {
                    b = a + (b - a) * (da / (da - db));
                }
            }
            let (a, b) = (occ.world.transform_point3(a), occ.world.transform_point3(b));
            let faces = mesh.edge_faces(e).unwrap_or([None, None]);
            let toward = view.to_eye((a + b) / 2.0);
            let facing: Vec<bool> = faces
                .iter()
                .flatten()
                .filter_map(|f| mesh.face_normal(*f).ok())
                .map(|n| (normal_matrix * n).dot(toward) > 0.0)
                .collect();
            let silhouette = match facing.as_slice() {
                [x, y] => x != y,
                _ => true,
            };
            let kind = if silhouette {
                Some(LineKind::Profile)
            } else if edge.flags.contains(EdgeFlags::SOFT) {
                None
            } else {
                Some(LineKind::Edge)
            };
            let Some((a, b)) = view.clip_near(a, b) else {
                continue;
            };
            let mut inherited = None;
            let mut parent = crate::document::ContainerRef::Model;
            for step in &occ.path.steps {
                if let Some(entities) = doc.container(parent)
                    && let Some(id) = entities.key_to_id(*step)
                {
                    if let Some(tag) = entities.tag_of(id)
                        && tag != doc.tags.untagged()
                    {
                        inherited = Some(tag);
                    }
                    if let Some(def) = entities.definition_of(id) {
                        parent = crate::document::ContainerRef::Definition(def);
                    }
                }
            }
            let own = entities.tag_of(id).filter(|t| *t != doc.tags.untagged());
            let tag = own.or(inherited);
            if !scope.include_hidden && tag.is_some_and(|t| !doc.tags.is_visible(t)) {
                continue;
            }
            lines.push(Line {
                a,
                b,
                sa: view.screen(a),
                sb: view.screen(b),
                kind,
                tag,
            });
        }
    }

    for occ in &scene.occurrences {
        if let (Some(cut), Some(entities)) = (
            crate::io::render_mesh::occurrence_section(doc, &occ.path, &occ.world),
            doc.container(occ.container),
        ) {
            for (a, b) in crate::geom::section::slice(entities.mesh(), &cut.plane, doc.tolerances())
            {
                let (a, b) = (occ.world.transform_point3(a), occ.world.transform_point3(b));
                if let Some((a, b)) = view.clip_near(a, b) {
                    lines.push(Line {
                        a,
                        b,
                        sa: view.screen(a),
                        sb: view.screen(b),
                        kind: Some(LineKind::Profile),
                        tag: None,
                    });
                }
            }
        }
    }

    // Screen grids.
    let mut lo = DVec2::splat(f64::MAX);
    let mut hi = DVec2::splat(f64::MIN);
    for l in &lines {
        lo = lo.min(l.sa).min(l.sb);
        hi = hi.max(l.sa).max(l.sb);
    }
    for t in &tris {
        lo = lo.min(t.lo);
        hi = hi.max(t.hi);
    }
    let mut pieces: Vec<(DVec2, DVec2, LineKind, Option<crate::ids::TagId>)> = Vec::new();
    if !lines.is_empty() {
        let mut tri_grid = Grid::new(lo, hi, tris.len().max(1));
        for (i, t) in tris.iter().enumerate() {
            tri_grid.insert(t.lo, t.hi, i as u32);
        }
        let mut line_grid = Grid::new(lo, hi, lines.len());
        for (i, l) in lines.iter().enumerate() {
            line_grid.insert(l.sa.min(l.sb), l.sa.max(l.sb), i as u32);
        }
        let occluded = |p: DVec3| -> bool {
            let s = view.screen(p);
            let dir = view.to_eye(p);
            let reach = match view.focal {
                Some(_) => (view.eye - p).length(),
                None => f64::MAX,
            };
            let eps = 1e-6 * view.depth(p).abs() + 1e-3;
            tri_grid.at(s).iter().any(|&k| {
                let t = &tris[k as usize];
                s.x >= t.lo.x - 1e-9
                    && s.x <= t.hi.x + 1e-9
                    && s.y >= t.lo.y - 1e-9
                    && s.y <= t.hi.y + 1e-9
                    && ray_triangle(p, dir, &t.p).is_some_and(|d| d > eps && d < reach)
            })
        };
        let mut candidates = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            let Some(kind) = l.kind else { continue };
            candidates.clear();
            line_grid.covering(l.sa.min(l.sb), l.sa.max(l.sb), &mut candidates);
            candidates.sort_unstable();
            candidates.dedup();
            let mut cuts: Vec<f64> = vec![0.0, 1.0];
            for &j in &candidates {
                if j as usize == i {
                    continue;
                }
                let o = &lines[j as usize];
                if let Some(s) = crossing(l.sa, l.sb, o.sa, o.sb) {
                    cuts.push(s);
                }
            }
            cuts.sort_by(f64::total_cmp);
            cuts.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
            let mut open: Option<DVec2> = None;
            for w in cuts.windows(2) {
                let (s0, s1) = (w[0], w[1]);
                let t = view.param(l.a, l.b, (s0 + s1) / 2.0);
                let mid = l.a + (l.b - l.a) * t;
                let p0 = l.sa + (l.sb - l.sa) * s0;
                if occluded(mid) {
                    if let Some(start) = open.take() {
                        pieces.push((start, p0, kind, l.tag));
                    }
                } else if open.is_none() {
                    open = Some(p0);
                }
            }
            if let Some(start) = open {
                pieces.push((start, l.sb, kind, l.tag));
            }
        }
    }

    // The view's frame on paper.
    let frame = view.half;
    let (to_paper, width, height, model_per_paper) = match (view.focal, paper.scale) {
        (None, Some(scale)) if scale > 0.0 => (
            scale,
            frame.x * 2.0 * scale,
            frame.y * 2.0 * scale,
            1.0 / scale,
        ),
        (None, _) => {
            let k = paper.width_mm / (frame.x * 2.0);
            (k, paper.width_mm, frame.y * 2.0 * k, 1.0 / k)
        }
        (Some(_), _) => {
            let k = paper.width_mm / (frame.x * 2.0);
            (k, paper.width_mm, frame.y * 2.0 * k, 0.0)
        }
    };
    let mut drawing = Drawing {
        width_mm: width,
        height_mm: height,
        model_per_paper,
        lines: Vec::new(),
        line_tags: Vec::new(),
    };
    for (a, b, kind, tag) in pieces {
        if let Some((a, b)) = clip_rect(a, b, -frame, frame) {
            let map = |p: DVec2| (p + frame) * to_paper;
            drawing.lines.push((map(a), map(b), kind));
            drawing.line_tags.push(tag);
        }
    }
    drawing
}

/// The drawing as SVG (paper millimeters; profiles 0.5 mm, edges 0.25 mm).
pub fn to_svg(d: &Drawing) -> String {
    let (w, h) = (num(d.width_mm), num(d.height_mm));
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" \
         width=\"{w}mm\" height=\"{h}mm\" viewBox=\"0 0 {w} {h}\">\n\
         <g fill=\"none\" stroke=\"#000\" stroke-linecap=\"round\">\n"
    );
    let mut by_kind: BTreeMap<LineKind, String> = BTreeMap::new();
    for (a, b, kind) in &d.lines {
        let path = by_kind.entry(*kind).or_default();
        let _ = write!(
            path,
            "M{} {}L{} {}",
            num(a.x),
            num(d.height_mm - a.y),
            num(b.x),
            num(d.height_mm - b.y)
        );
    }
    for (kind, path) in by_kind {
        let width = match kind {
            LineKind::Edge => "0.25",
            LineKind::Profile => "0.5",
        };
        let _ = writeln!(out, "<path stroke-width=\"{width}\" d=\"{path}\"/>");
    }
    out.push_str("</g>\n</svg>\n");
    out
}

/// The drawing as a one-page PDF (no metadata, so the bytes depend only on
/// the drawing).
pub fn to_pdf(d: &Drawing) -> Vec<u8> {
    use pdf_writer::{Content, Pdf, Rect, Ref};
    let pt = 72.0 / 25.4;
    let (catalog, tree, page, contents) = (Ref::new(1), Ref::new(2), Ref::new(3), Ref::new(4));
    let mut pdf = Pdf::new();
    pdf.catalog(catalog).pages(tree);
    pdf.pages(tree).kids([page]).count(1);
    pdf.page(page)
        .parent(tree)
        .media_box(Rect::new(
            0.0,
            0.0,
            (d.width_mm * pt) as f32,
            (d.height_mm * pt) as f32,
        ))
        .contents(contents)
        .resources();
    let mut content = Content::new();
    content.set_line_cap(pdf_writer::types::LineCapStyle::RoundCap);
    for kind in [LineKind::Edge, LineKind::Profile] {
        let width = match kind {
            LineKind::Edge => 0.25,
            LineKind::Profile => 0.5,
        };
        content.set_line_width((width * pt) as f32);
        let mut any = false;
        for (a, b, k) in &d.lines {
            if *k == kind {
                content.move_to((a.x * pt) as f32, (a.y * pt) as f32);
                content.line_to((b.x * pt) as f32, (b.y * pt) as f32);
                any = true;
            }
        }
        if any {
            content.stroke();
        }
    }
    let data = content.finish();
    pdf.stream(contents, &data);
    pdf.finish()
}

/// The drawing's lines as DXF `LINE`s: model units of the view plane for
/// parallel views (`model_per_paper`, then `unit_mm`), paper millimeters
/// for perspective; layers `Edges` and `Profiles`.
pub fn to_dxf_entities(d: &Drawing, unit_mm: f64) -> (String, Vec<String>) {
    let factor = if d.model_per_paper > 0.0 {
        d.model_per_paper / unit_mm
    } else {
        1.0
    };
    let mut body = String::new();
    for (a, b, kind) in &d.lines {
        let layer = match kind {
            LineKind::Edge => "Edges",
            LineKind::Profile => "Profiles",
        };
        let (a, b) = (*a * factor, *b * factor);
        let _ = write!(
            body,
            "0\nLINE\n8\n{layer}\n10\n{}\n20\n{}\n30\n0\n11\n{}\n21\n{}\n31\n0\n",
            num(a.x),
            num(a.y),
            num(b.x),
            num(b.y)
        );
    }
    (body, vec!["0".into(), "Edges".into(), "Profiles".into()])
}

/// The drawing of the document's camera for `opts`.
pub fn drawing(doc: &Document, scope: &ExportScope, opts: &ExportOptions) -> Drawing {
    draw_view(
        doc,
        scope,
        &doc.camera,
        Paper {
            scale: opts.scale,
            width_mm: opts.paper_width_mm.unwrap_or(DEFAULT_PAPER_WIDTH_MM),
        },
    )
}

fn write_all(dst: &mut dyn Write, bytes: &[u8], format: &str) -> Result<(), IoError> {
    dst.write_all(bytes).map_err(|e| IoError::Storage {
        key: format.into(),
        reason: e.to_string(),
    })
}

/// The SVG view exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct SvgExporter;

impl Exporter for SvgExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Svg
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        let d = drawing(doc, scope, opts);
        progress.report(0.8, "writing SVG");
        let text = to_svg(&d);
        write_all(dst, text.as_bytes(), "svg")?;
        Ok(ExportReport {
            bytes_written: text.len() as u64,
            entities: d.lines.len(),
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}

/// The PDF view exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct PdfExporter;

impl Exporter for PdfExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Pdf
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        let d = drawing(doc, scope, opts);
        progress.report(0.8, "writing PDF");
        let bytes = to_pdf(&d);
        write_all(dst, &bytes, "pdf")?;
        Ok(ExportReport {
            bytes_written: bytes.len() as u64,
            entities: d.lines.len(),
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}
