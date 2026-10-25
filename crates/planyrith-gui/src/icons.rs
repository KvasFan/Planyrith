//! Toolbar icons (docs/07 §1, §12), drawn as vector strokes so they stay
//! sharp at every scale and follow the theme's text color. Every icon lives
//! in a unit square that [`paint`] maps onto the target rectangle.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

/// An icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Select tool.
    Select,
    /// Eraser tool.
    Eraser,
    /// Paint tool.
    Paint,
    /// Line tool.
    Line,
    /// Arc through a bulge.
    Arc,
    /// Arc from its center.
    ArcCenter,
    /// Arc through three points.
    Arc3,
    /// Pie.
    Pie,
    /// Rectangle tool.
    Rectangle,
    /// Circle tool.
    Circle,
    /// Polygon tool.
    Polygon,
    /// Push/Pull tool.
    PushPull,
    /// Offset tool.
    Offset,
    /// Move tool.
    Move,
    /// Rotate tool.
    Rotate,
    /// Scale tool.
    Scale,
    /// Three-quarter view.
    ViewIso,
    /// Top view.
    ViewTop,
    /// Front view.
    ViewFront,
    /// Right view.
    ViewRight,
    /// Zoom extents.
    ZoomExtents,
    /// Wireframe face style.
    Wireframe,
    /// Hidden line face style.
    HiddenLine,
    /// Shaded face style.
    Shaded,
    /// Shaded with textures.
    Textured,
    /// X-ray face style.
    XRay,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// New document.
    New,
    /// Open a document.
    Open,
    /// Save the document.
    Save,
}

impl Icon {
    /// The icon of a tool, by its command name.
    pub fn for_command(command: &str) -> Option<Icon> {
        Some(match command {
            "select" => Icon::Select,
            "erase" => Icon::Eraser,
            "material-apply" => Icon::Paint,
            "line" => Icon::Line,
            "arc-bulge" => Icon::Arc,
            "arc-2point" => Icon::ArcCenter,
            "arc-3point" => Icon::Arc3,
            "pie" => Icon::Pie,
            "rectangle" => Icon::Rectangle,
            "circle" => Icon::Circle,
            "polygon" => Icon::Polygon,
            "push-pull" => Icon::PushPull,
            "offset" => Icon::Offset,
            "move" => Icon::Move,
            "rotate" => Icon::Rotate,
            "scale" => Icon::Scale,
            _ => return None,
        })
    }
}

/// Maps the unit square onto `rect`.
struct Pen<'a> {
    painter: &'a Painter,
    rect: Rect,
    stroke: Stroke,
    fill: Color32,
}

impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.rect.min + Vec2::new(x * self.rect.width(), y * self.rect.height())
    }

    fn line(&self, points: &[(f32, f32)]) {
        let points: Vec<Pos2> = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter.add(Shape::line(points, self.stroke));
    }

    fn closed(&self, points: &[(f32, f32)]) {
        let points: Vec<Pos2> = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter.add(Shape::closed_line(points, self.stroke));
    }

    /// A filled convex polygon with the outline on top.
    fn filled(&self, points: &[(f32, f32)], alpha: f32) {
        let points: Vec<Pos2> = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter.add(Shape::convex_polygon(
            points,
            self.fill.gamma_multiply(alpha),
            self.stroke,
        ));
    }

    fn dashed(&self, a: (f32, f32), b: (f32, f32)) {
        let stroke = Stroke::new(self.stroke.width * 0.8, self.stroke.color);
        self.painter.extend(Shape::dashed_line(
            &[self.p(a.0, a.1), self.p(b.0, b.1)],
            stroke,
            2.0,
            2.0,
        ));
    }

    fn dot(&self, x: f32, y: f32) {
        self.painter
            .circle_filled(self.p(x, y), self.stroke.width * 1.3, self.stroke.color);
    }

    fn circle(&self, x: f32, y: f32, r: f32) {
        self.painter
            .circle_stroke(self.p(x, y), r * self.rect.width(), self.stroke);
    }

    /// Points on an arc of the circle (cx, cy, r) from angle a0 to a1
    /// (radians, y down).
    fn arc(&self, c: (f32, f32), r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
        let n = 24;
        (0..=n)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                (c.0 + r * a.cos(), c.1 + r * a.sin())
            })
            .collect()
    }

    fn arrow_head(&self, tip: (f32, f32), from: (f32, f32)) {
        let d = Vec2::new(tip.0 - from.0, tip.1 - from.1).normalized();
        let n = Vec2::new(-d.y, d.x);
        let size = 0.16;
        let back = (tip.0 - d.x * size, tip.1 - d.y * size);
        let points = [
            tip,
            (back.0 + n.x * size * 0.6, back.1 + n.y * size * 0.6),
            (back.0 - n.x * size * 0.6, back.1 - n.y * size * 0.6),
        ];
        let points: Vec<Pos2> = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter.add(Shape::convex_polygon(
            points,
            self.stroke.color,
            Stroke::NONE,
        ));
    }

    fn arrow(&self, from: (f32, f32), to: (f32, f32)) {
        let d = Vec2::new(to.0 - from.0, to.1 - from.1).normalized();
        self.line(&[from, (to.0 - d.x * 0.1, to.1 - d.y * 0.1)]);
        self.arrow_head(to, from);
    }

    /// A small box in a three-quarter view; `face` 0 none, 1 top, 2 front,
    /// 3 right is filled; `hidden` draws the hidden edges dashed.
    fn cube(&self, face: u8, shade: bool, hidden: bool) {
        let f = [(0.14, 0.42), (0.58, 0.42), (0.58, 0.88), (0.14, 0.88)];
        let o = (0.26, -0.26);
        let b = |p: (f32, f32)| (p.0 + o.0, p.1 + o.1);
        let top = [f[0], f[1], b(f[1]), b(f[0])];
        let right = [f[1], f[2], b(f[2]), b(f[1])];
        if shade {
            self.filled(&top, 0.15);
            self.filled(&f, 0.35);
            self.filled(&right, 0.6);
        }
        match face {
            1 => self.filled(&top, 0.85),
            2 => self.filled(&f, 0.85),
            3 => self.filled(&right, 0.85),
            _ => {}
        }
        if hidden {
            let back = b(f[3]);
            self.dashed(back, b(f[0]));
            self.dashed(back, f[3]);
            self.dashed(back, b(f[2]));
        }
        self.closed(&f);
        self.closed(&top);
        self.closed(&right);
    }
}

/// Paints `icon` into `rect` with `color`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let pen = Pen {
        painter,
        rect,
        stroke: Stroke::new((rect.width() / 13.0).max(1.2), color),
        fill: color,
    };
    let tau = std::f32::consts::TAU;
    match icon {
        Icon::Select => pen.filled(
            &[
                (0.28, 0.08),
                (0.28, 0.82),
                (0.45, 0.66),
                (0.56, 0.92),
                (0.68, 0.87),
                (0.57, 0.61),
                (0.80, 0.60),
            ],
            0.0,
        ),
        Icon::Eraser => {
            pen.closed(&[(0.12, 0.62), (0.50, 0.24), (0.86, 0.60), (0.48, 0.98)]);
            pen.line(&[(0.31, 0.43), (0.67, 0.79)]);
            pen.filled(
                &[(0.12, 0.62), (0.31, 0.43), (0.67, 0.79), (0.48, 0.98)],
                0.6,
            );
        }
        Icon::Paint => {
            pen.closed(&[(0.18, 0.42), (0.62, 0.42), (0.56, 0.90), (0.24, 0.90)]);
            pen.line(&[(0.24, 0.42), (0.40, 0.14), (0.56, 0.42)]);
            pen.line(&[(0.62, 0.48), (0.80, 0.62)]);
            pen.painter
                .circle_filled(pen.p(0.82, 0.74), rect.width() * 0.07, color);
        }
        Icon::Line => {
            pen.line(&[(0.16, 0.84), (0.84, 0.16)]);
            pen.dot(0.16, 0.84);
            pen.dot(0.84, 0.16);
        }
        Icon::Arc => {
            pen.line(&pen.arc((0.5, 0.86), 0.42, tau * 0.5, tau));
            pen.dashed((0.08, 0.86), (0.92, 0.86));
            pen.dot(0.08, 0.86);
            pen.dot(0.92, 0.86);
        }
        Icon::ArcCenter => {
            pen.line(&pen.arc((0.24, 0.76), 0.6, -tau * 0.25, 0.0));
            pen.line(&[(0.24, 0.16), (0.24, 0.76), (0.84, 0.76)]);
            pen.dot(0.24, 0.76);
        }
        Icon::Arc3 => {
            pen.line(&pen.arc((0.5, 0.86), 0.42, tau * 0.5, tau));
            pen.dot(0.08, 0.86);
            pen.dot(0.5, 0.44);
            pen.dot(0.92, 0.86);
        }
        Icon::Pie => {
            let mut points = vec![(0.2, 0.8)];
            points.extend(pen.arc((0.2, 0.8), 0.66, -tau * 0.25, 0.0));
            points.push((0.2, 0.8));
            pen.line(&points);
            let fill: Vec<(f32, f32)> = points;
            let fill: Vec<Pos2> = fill.iter().map(|&(x, y)| pen.p(x, y)).collect();
            pen.painter.add(Shape::convex_polygon(
                fill,
                color.gamma_multiply(0.25),
                Stroke::NONE,
            ));
        }
        Icon::Rectangle => {
            pen.closed(&[(0.12, 0.26), (0.88, 0.26), (0.88, 0.74), (0.12, 0.74)]);
            pen.dot(0.12, 0.74);
            pen.dot(0.88, 0.26);
        }
        Icon::Circle => {
            pen.circle(0.5, 0.5, 0.38);
            pen.line(&[(0.5, 0.5), (0.77, 0.23)]);
            pen.dot(0.5, 0.5);
        }
        Icon::Polygon => {
            let points: Vec<(f32, f32)> = (0..6)
                .map(|i| {
                    let a = tau * (i as f32 / 6.0) + tau / 12.0;
                    (0.5 + 0.4 * a.cos(), 0.5 + 0.4 * a.sin())
                })
                .collect();
            pen.closed(&points);
            pen.dot(0.5, 0.5);
        }
        Icon::PushPull => {
            pen.filled(
                &[(0.08, 0.86), (0.56, 0.86), (0.92, 0.66), (0.44, 0.66)],
                0.35,
            );
            pen.arrow((0.5, 0.74), (0.5, 0.06));
        }
        Icon::Offset => {
            pen.closed(&[(0.10, 0.10), (0.90, 0.10), (0.90, 0.90), (0.10, 0.90)]);
            pen.filled(
                &[(0.30, 0.30), (0.70, 0.30), (0.70, 0.70), (0.30, 0.70)],
                0.3,
            );
        }
        Icon::Move => {
            pen.arrow((0.5, 0.5), (0.5, 0.04));
            pen.arrow((0.5, 0.5), (0.5, 0.96));
            pen.arrow((0.5, 0.5), (0.04, 0.5));
            pen.arrow((0.5, 0.5), (0.96, 0.5));
        }
        Icon::Rotate => {
            let points = pen.arc((0.5, 0.5), 0.42, -tau * 0.15, tau * 0.62);
            pen.line(&points);
            let end = points[points.len() - 1];
            let before = points[points.len() - 3];
            pen.arrow_head(
                (
                    end.0 + (end.0 - before.0) * 1.5,
                    end.1 + (end.1 - before.1) * 1.5,
                ),
                before,
            );
            pen.dot(0.5, 0.5);
        }
        Icon::Scale => {
            pen.closed(&[(0.30, 0.08), (0.92, 0.08), (0.92, 0.70), (0.30, 0.70)]);
            pen.filled(
                &[(0.08, 0.56), (0.44, 0.56), (0.44, 0.92), (0.08, 0.92)],
                0.35,
            );
            pen.arrow((0.40, 0.60), (0.80, 0.20));
        }
        Icon::ViewIso => pen.cube(0, false, false),
        Icon::ViewTop => pen.cube(1, false, false),
        Icon::ViewFront => pen.cube(2, false, false),
        Icon::ViewRight => pen.cube(3, false, false),
        Icon::ZoomExtents => {
            for (a, b, c) in [
                ((0.06, 0.30), (0.06, 0.06), (0.30, 0.06)),
                ((0.70, 0.06), (0.94, 0.06), (0.94, 0.30)),
                ((0.94, 0.70), (0.94, 0.94), (0.70, 0.94)),
                ((0.30, 0.94), (0.06, 0.94), (0.06, 0.70)),
            ] {
                pen.line(&[a, b, c]);
            }
            pen.circle(0.45, 0.45, 0.17);
            pen.line(&[(0.57, 0.57), (0.72, 0.72)]);
        }
        Icon::Wireframe => pen.cube(0, false, true),
        Icon::HiddenLine => pen.cube(0, false, false),
        Icon::Shaded => pen.cube(0, true, false),
        Icon::Textured => {
            pen.cube(0, true, false);
            // A checker hint on the front face.
            for (x, y) in [(0.14, 0.42), (0.36, 0.65)] {
                let r = Rect::from_min_size(pen.p(x, y), Vec2::splat(rect.width() * 0.22));
                pen.painter.rect_filled(r, 0.0, color.gamma_multiply(0.7));
            }
        }
        Icon::XRay => {
            let f = [(0.14, 0.42), (0.58, 0.42), (0.58, 0.88), (0.14, 0.88)];
            pen.filled(&f, 0.2);
            pen.cube(0, false, true);
        }
        Icon::Undo => {
            let points = pen.arc((0.52, 0.56), 0.32, -tau * 0.5, tau * 0.1);
            pen.line(&points);
            pen.arrow_head((0.14, 0.72), (0.2, 0.5));
        }
        Icon::Redo => {
            let points = pen.arc((0.48, 0.56), 0.32, tau * 0.4, tau);
            pen.line(&points);
            pen.arrow_head((0.86, 0.72), (0.8, 0.5));
        }
        Icon::New => {
            pen.closed(&[
                (0.2, 0.06),
                (0.62, 0.06),
                (0.82, 0.26),
                (0.82, 0.94),
                (0.2, 0.94),
            ]);
            pen.line(&[(0.62, 0.06), (0.62, 0.26), (0.82, 0.26)]);
        }
        Icon::Open => {
            pen.closed(&[
                (0.06, 0.20),
                (0.36, 0.20),
                (0.44, 0.30),
                (0.86, 0.30),
                (0.86, 0.86),
                (0.06, 0.86),
            ]);
            pen.filled(
                &[(0.06, 0.86), (0.22, 0.46), (0.98, 0.46), (0.86, 0.86)],
                0.3,
            );
        }
        Icon::Save => {
            pen.arrow((0.5, 0.04), (0.5, 0.62));
            pen.line(&[(0.10, 0.52), (0.10, 0.90), (0.90, 0.90), (0.90, 0.52)]);
        }
    }
}

/// A square toolbar button showing `icon`; highlighted when `selected`.
pub fn button(ui: &mut egui::Ui, icon: Icon, selected: bool, tooltip: &str) -> egui::Response {
    let size = Vec2::splat(30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.visuals();
    let (fill, stroke, color) = if selected {
        (
            visuals.selection.bg_fill,
            visuals.selection.stroke,
            visuals.strong_text_color(),
        )
    } else if response.hovered() {
        (
            visuals.widgets.hovered.weak_bg_fill,
            visuals.widgets.hovered.bg_stroke,
            visuals.strong_text_color(),
        )
    } else {
        (Color32::TRANSPARENT, Stroke::NONE, visuals.text_color())
    };
    ui.painter()
        .rect(rect, 4.0, fill, stroke, egui::StrokeKind::Inside);
    paint(ui.painter(), rect.shrink(6.0), icon, color);
    response.on_hover_text(tooltip)
}
