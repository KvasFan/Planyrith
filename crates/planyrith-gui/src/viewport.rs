//! The 3D viewport (docs/07 §1–§3, §5; docs/06 §7–§10): the renderer draws
//! into its offscreen texture, which egui shows. Middle-drag orbits about the
//! point under the cursor (Shift pans) and the wheel zooms toward the cursor;
//! the primary button belongs to the active tool, which receives the
//! inference engine's answer for the pointer (docs/03 §7). Hovering an entity
//! for 400 ms remembers it as a reference; Shift locks the current inference
//! while held. Inference marks, guide lines, labels and the tool's rubber
//! bands are painted over the image; a tool's preview renders a copy of the
//! document instead of the document.

use std::collections::BTreeSet;

use egui::{Color32, PointerButton, Sense, Stroke, Vec2};
use glam::{DVec2, DVec3};
use planyrith_core::document::{
    Camera, ContainerRef, Document, EditContext, EntityKey, EntityPath, InstancePath,
};
use planyrith_core::ids::EntityId;
use planyrith_core::inference::{
    Inference, InferenceEngine, InferenceKind, InferenceRequest, Lock, ViewMetrics,
};
use planyrith_core::io::render_mesh::draw_list;
use planyrith_core::math::{Aabb, Axis, Ray};
use planyrith_core::spatial::HitFilter;
use planyrith_render::{FrameRequest, PickKind, Renderer, RendererOptions};

use crate::session::Session;
use crate::tools::{self, Cx, Flow, Pointer, Shape, Tool};

/// Pixel tolerance for picking edges and vertices (docs/07 §5).
const PICK_PX: f64 = 8.0;
/// Radians per pixel of orbit drag.
const ORBIT_SPEED: f64 = 0.008;
/// Hover time that makes an entity a reference (docs/03 §7).
const REMEMBER_AFTER_S: f64 = 0.4;
/// Hover time before an inference label shows (docs/07 §5).
const LABEL_AFTER_S: f64 = 0.3;

/// What a navigation drag started on.
#[derive(Debug, Clone, Copy)]
struct NavDrag {
    pivot: DVec3,
    depth: f64,
}

/// A tool's preview: a copy of the document with the command applied, and
/// the containers that differ from the document (docs/07 §3).
pub struct Preview {
    /// The copy.
    pub doc: Document,
    /// Containers the preview changed.
    pub touched: BTreeSet<ContainerRef>,
    /// Identifies this preview (a new one invalidates the render cache).
    pub serial: u64,
}

/// Viewport state.
#[derive(Default)]
pub struct Viewport {
    renderer: Option<Renderer>,
    texture: Option<egui::TextureId>,
    /// Size of the texture registered with egui (it is recreated on resize).
    texture_size: (u32, u32),
    /// Draw hidden geometry (View › Hidden Geometry).
    pub show_hidden: bool,
    nav: Option<NavDrag>,
    last_hover: Option<egui::Pos2>,
    /// What the pointer is over, for the status bar.
    pub hover: Option<String>,
    /// The last error, for the status bar.
    pub error: Option<String>,
    /// The inference engine (references and locks).
    pub inference: InferenceEngine,
    /// The last request and answer (locks engage relative to them).
    last: Option<(InferenceRequest, Inference)>,
    hovered_since: Option<(EntityPath, f64, bool)>,
    shift_was_down: bool,
    tool_dragging: bool,
    /// The preview drawn last frame and the containers it changed.
    preview_drawn: Option<u64>,
    preview_touched: BTreeSet<ContainerRef>,
    /// The viewport rectangle of the last frame, in points.
    rect: Option<egui::Rect>,
    /// The pointer of the last frame it was over the viewport.
    pointer: Option<Pointer>,
    /// `Tab` presses since the pointer last moved, and where it was.
    cycle: (usize, Option<egui::Pos2>),
    /// The inference whose label is pending, and since when it is shown.
    label: Option<(InferenceKind, Option<EntityPath>, f64)>,
}

impl Viewport {
    /// Locks the inference (an arrow key or `↓`, docs/05 §2); the same lock
    /// again releases it.
    pub fn lock(&mut self, lock: Lock) {
        if self.inference.lock() == Some(lock) {
            self.inference.release_lock();
            return;
        }
        if let Some((req, current)) = &self.last {
            self.inference.engage_lock(lock, current, req);
        }
    }

    /// After a command ran: locks end with the step (docs/05 §2).
    pub fn after_run(&mut self) {
        self.inference.release_lock();
    }

    /// The last inference, for the status bar.
    pub fn current_inference(&self) -> Option<&Inference> {
        self.last.as_ref().map(|(_, i)| i)
    }

    /// `Tab` (docs/07 §5): the next candidate when several lie within the
    /// pixel tolerance, until the pointer moves.
    pub fn cycle(&mut self) {
        self.cycle = (self.cycle.0 + 1, self.pointer.as_ref().map(|p| p.screen));
    }

    /// The pointer as of the last frame it was over the viewport (`Enter`
    /// with an empty value field confirms it, docs/05 §1).
    pub fn pointer(&self) -> Option<&Pointer> {
        self.pointer.as_ref()
    }

    /// The tool context as of the last frame (for the app's value field,
    /// preview and Tool Options outside the viewport).
    pub fn cx<'a>(&self, session: &'a Session) -> Cx<'a> {
        let rect = self.rect.unwrap_or_else(|| {
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))
        });
        let camera = Camera {
            aspect: f64::from(rect.width().max(1.0)) / f64::from(rect.height().max(1.0)),
            ..session.doc().camera
        };
        let depth = camera.fit_depth(&session.doc().aabb());
        cx_for(
            session,
            camera,
            depth,
            rect,
            session.doc().info.display_units,
        )
    }

    /// Draws the viewport, handles navigation and gives pointer input to
    /// `tool`; returns what the tool asked for.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        render_state: Option<&egui_wgpu::RenderState>,
        session: &mut Session,
        tool: &mut dyn Tool,
        preview: Option<&Preview>,
    ) -> Vec<Flow> {
        let mut flows = Vec::new();
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.rect = Some(rect);
        let ppp = ui.ctx().pixels_per_point();
        let width = (rect.width() * ppp).round().max(1.0) as u32;
        let height = (rect.height() * ppp).round().max(1.0) as u32;
        let aspect = f64::from(width) / f64::from(height);
        let mut camera = Camera {
            aspect,
            ..session.doc().camera
        };
        let to_ndc = |pos: egui::Pos2| {
            DVec2::new(
                f64::from((pos.x - rect.left()) / rect.width()) * 2.0 - 1.0,
                1.0 - f64::from((pos.y - rect.top()) / rect.height()) * 2.0,
            )
        };

        // Navigation (docs/06 §7).
        let mut moved = false;
        if response.drag_started_by(PointerButton::Middle)
            && let Some(pos) = response.interact_pointer_pos()
        {
            let ray = camera.ray_through_ndc(to_ndc(pos));
            let pivot = cast(session, &camera, &ray, f64::from(height))
                .map(|(_, p)| p)
                .unwrap_or_else(|| visible_center(session, &camera));
            self.nav = Some(NavDrag {
                pivot,
                depth: camera.depth_of(pivot).max(1.0),
            });
        }
        if response.dragged_by(PointerButton::Middle)
            && let Some(nav) = self.nav
        {
            let delta = response.drag_delta() * ppp;
            if ui.input(|i| i.modifiers.shift) {
                camera.pan(camera.pan_delta_for_pixels(
                    f64::from(delta.x),
                    f64::from(delta.y),
                    f64::from(height),
                    nav.depth,
                ));
            } else {
                camera.orbit(
                    nav.pivot,
                    -f64::from(delta.x) * ORBIT_SPEED,
                    -f64::from(delta.y) * ORBIT_SPEED,
                );
            }
            moved = true;
        }
        if response.drag_stopped_by(PointerButton::Middle) {
            self.nav = None;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0
                && let Some(pos) = response.hover_pos()
            {
                let ray = camera.ray_through_ndc(to_ndc(pos));
                let toward = cast(session, &camera, &ray, f64::from(height))
                    .map(|(_, p)| p)
                    .unwrap_or_else(|| {
                        ray.origin + ray.dir.normalize() * camera.depth_of(camera.target).max(1.0)
                    });
                camera.zoom(f64::from(scroll * 0.0025).exp(), Some(toward));
                moved = true;
            }
        }
        if moved {
            session.engine.set_camera(Camera {
                aspect: session.doc().camera.aspect,
                ..camera
            });
            ui.ctx().request_repaint();
        }

        // The pointer, through the inference engine (docs/03 §7).
        let unit = session.doc().info.display_units;
        let depth = camera.fit_depth(&session.doc().aabb());
        let pointer_pos = response
            .hover_pos()
            .or_else(|| response.interact_pointer_pos());
        let pointer = pointer_pos.filter(|_| self.nav.is_none()).map(|pos| {
            let ray = camera.ray_through_ndc(to_ndc(pos));
            let setup = {
                let cx = cx_for(session, camera, depth, rect, unit);
                tool.setup(&cx)
            };
            let req = InferenceRequest {
                ray,
                pixel_tolerance: InferenceRequest::DEFAULT_PIXEL_TOLERANCE,
                view: ViewMetrics::for_camera(&camera, f64::from(rect.height())),
                constraint: setup.constraint,
                reference_point: setup.reference,
                last_point: setup.reference,
            };
            if self.cycle.1.is_some_and(|at| at.distance(pos) > 1.0) {
                self.cycle = (0, None);
            }
            let inference = match self.cycle.0 {
                0 => session.infer(&self.inference, &req),
                n => {
                    let list = session.candidates(&self.inference, &req);
                    match list.len() {
                        0 => session.infer(&self.inference, &req),
                        len => list[n % len].clone(),
                    }
                }
            };
            let target = cast(session, &camera, &ray, f64::from(height)).and_then(|(path, _)| {
                let context = session.engine.context().path.clone();
                path.selectable_in(session.doc(), &context)
            });
            let faces_only = HitFilter {
                vertices: false,
                edges: false,
                ..HitFilter::default()
            };
            let face = session
                .ray_cast(&ray, &faces_only)
                .into_iter()
                .next()
                .map(|h| h.path);
            self.last = Some((req, inference.clone()));
            Pointer {
                inference,
                modifiers: ui.input(|i| i.modifiers),
                face,
                target,
                screen: pos,
            }
        });

        // Shift locks the current inference while held (docs/05 §2).
        let shift = ui.input(|i| i.modifiers.shift);
        if shift && !self.shift_was_down && tool.step() > 0 {
            self.lock(Lock::Current);
        } else if !shift && self.shift_was_down && self.inference.lock() == Some(Lock::Current) {
            self.inference.release_lock();
        }
        self.shift_was_down = shift;

        // Hover memory.
        let now = ui.input(|i| i.time);
        if let Some(p) = &pointer {
            match (&p.inference.on, &mut self.hovered_since) {
                (Some(on), Some((path, since, remembered))) if on == path => {
                    if !*remembered && now - *since > REMEMBER_AFTER_S {
                        self.inference.remember(&p.inference);
                        *remembered = true;
                    }
                }
                (Some(on), _) => self.hovered_since = Some((on.clone(), now, false)),
                (None, _) => self.hovered_since = None,
            }
            if p.inference.on.is_some() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(450));
            }
        }

        // Tool input.
        if let Some(p) = &pointer {
            let cx = cx_for(session, camera, depth, rect, unit);
            tool.hover(p, &cx);
            if response.drag_started_by(PointerButton::Primary) {
                self.tool_dragging = true;
                flows.push(tool.drag(tools::Drag::Start, p, p.screen, &cx));
            } else if self.tool_dragging && response.dragged_by(PointerButton::Primary) {
                flows.push(tool.drag(tools::Drag::Move, p, p.screen, &cx));
            }
            if self.tool_dragging && response.drag_stopped_by(PointerButton::Primary) {
                self.tool_dragging = false;
                flows.push(tool.drag(tools::Drag::End, p, p.screen, &cx));
            }
            if response.clicked_by(PointerButton::Primary) {
                flows.push(tool.click(p, &cx));
            }
            if response.double_clicked_by(PointerButton::Primary) {
                flows.push(tool.double_click(p, &cx));
            }
            if response.triple_clicked_by(PointerButton::Primary) {
                flows.push(tool.triple_click(p, &cx));
            }
        }
        flows.retain(|f| *f != Flow::Continue);
        if pointer.is_some() {
            self.pointer = pointer.clone();
        }

        // Render (the preview copy while a tool previews).
        let Some(render_state) = render_state else {
            return flows;
        };
        let renderer = self.renderer.get_or_insert_with(|| {
            Renderer::new(
                render_state.device.clone(),
                render_state.queue.clone(),
                RendererOptions {
                    msaa: 4,
                    pixels_per_point: ppp,
                },
            )
        });
        renderer.set_pixels_per_point(ppp);
        renderer.apply_deltas(&session.take_deltas());
        // Containers drawn from another version than last frame's.
        let serial = preview.map(|p| p.serial);
        if serial != self.preview_drawn {
            let touched: BTreeSet<ContainerRef> =
                preview.map(|p| p.touched.clone()).unwrap_or_default();
            for c in self.preview_touched.iter().chain(touched.iter()) {
                renderer.invalidate(*c);
            }
            self.preview_touched = touched;
            self.preview_drawn = serial;
        }
        let doc = preview.map_or(session.doc(), |p| &p.doc);
        let items = draw_list(
            doc,
            session.engine.selection(),
            session.engine.context(),
            self.show_hidden,
        );
        let style = doc.styles.active_style().cloned().unwrap_or_default();
        let hover = response.hover_pos();
        let want_ids = hover.is_some() && self.nav.is_none() && hover != self.last_hover;
        let frame_camera = Camera {
            aspect,
            ..session.doc().camera
        };
        if let Err(e) = renderer.render(&FrameRequest {
            doc,
            items: &items,
            camera: &frame_camera,
            style: &style,
            width,
            height,
            show_hidden: self.show_hidden,
            ids: want_ids,
            transparent_background: false,
        }) {
            self.error = Some(e.to_string());
            return flows;
        }
        if want_ids && let Some(pos) = hover {
            self.last_hover = Some(pos);
            let px = ((pos.x - rect.left()) * ppp) as u32;
            let py = ((pos.y - rect.top()) * ppp) as u32;
            self.hover = match renderer.pick(px, py, (4.0 * ppp) as u32) {
                Ok(Some(hit)) => items.get(hit.item).map(|item| {
                    let target = match hit.kind {
                        PickKind::Face(f) => EntityId::Face(f),
                        PickKind::Edge(e) => EntityId::Edge(e),
                    };
                    let path = EntityPath::new(item.path.clone(), EntityKey::Id(target));
                    path.canonical(doc).display_with(doc)
                }),
                _ => None,
            };
        }
        if hover.is_none() {
            self.hover = None;
            self.last_hover = None;
        }
        // egui composites in gamma space: show the unorm view.
        let Some(view) = renderer.display_view() else {
            return flows;
        };
        // The renderer recreates its targets when the viewport is resized.
        if self.texture.is_none() || self.texture_size != (width, height) {
            let mut egui_renderer = render_state.renderer.write();
            match self.texture {
                Some(id) => egui_renderer.update_egui_texture_from_wgpu_texture(
                    &render_state.device,
                    view,
                    wgpu::FilterMode::Nearest,
                    id,
                ),
                None => {
                    self.texture = Some(egui_renderer.register_native_texture(
                        &render_state.device,
                        view,
                        wgpu::FilterMode::Nearest,
                    ));
                }
            }
            self.texture_size = (width, height);
        }
        if let Some(id) = self.texture {
            ui.painter().image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // Overlays: the tool's hints, then the inference (docs/06 §10).
        let cx = cx_for(session, camera, depth, rect, unit);
        let painter = ui.painter_at(rect);
        let mut shapes = Vec::new();
        tool.shapes(&mut shapes);
        for shape in &shapes {
            paint_shape(&painter, &cx, shape);
        }
        if let Some(p) = &pointer
            && tool.command() != "select"
            && tool.command() != "erase"
        {
            // Labels show after the inference has held for 300 ms (docs/07 §5).
            let key = (p.inference.kind, p.inference.on.clone());
            let since = match &self.label {
                Some((kind, on, since)) if (*kind, on.clone()) == key => *since,
                _ => {
                    self.label = Some((key.0, key.1, now));
                    now
                }
            };
            let show_label = now - since >= LABEL_AFTER_S;
            if !show_label {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(LABEL_AFTER_S));
            }
            paint_inference(&painter, &cx, &p.inference, p.screen, show_label);
        }
        flows
    }
}

/// The tool context for this frame.
pub fn cx_for<'a>(
    session: &'a Session,
    camera: Camera,
    depth: planyrith_core::document::DepthRange,
    rect: egui::Rect,
    unit: planyrith_core::units::LengthUnit,
) -> Cx<'a> {
    Cx {
        doc: session.doc(),
        selection: session.engine.selection(),
        context: session.engine.context(),
        unit,
        camera,
        depth,
        rect,
    }
}

/// The nearest hit under a ray: the occurrence path and the hit point.
fn cast(
    session: &mut Session,
    camera: &Camera,
    ray: &Ray,
    height_px: f64,
) -> Option<(EntityPath, DVec3)> {
    let proximity = camera.world_per_pixel_at(camera.distance(), height_px) * PICK_PX;
    let filter = HitFilter {
        proximity,
        ..HitFilter::default()
    };
    session
        .ray_cast(ray, &filter)
        .into_iter()
        .next()
        .map(|h| (h.path, h.point))
}

/// The center of the visible model, else the camera target.
fn visible_center(session: &Session, camera: &Camera) -> DVec3 {
    let bounds: Aabb = session.doc().aabb();
    if bounds.is_empty() {
        camera.target
    } else {
        bounds.center()
    }
}

fn axis_color(axis: Option<Axis>) -> Color32 {
    match axis {
        Some(Axis::X) => Color32::from_rgb(220, 30, 30),
        Some(Axis::Y) => Color32::from_rgb(30, 160, 30),
        Some(Axis::Z) => Color32::from_rgb(40, 70, 230),
        None => Color32::from_rgb(200, 0, 200),
    }
}

fn paint_shape(painter: &egui::Painter, cx: &Cx<'_>, shape: &Shape) {
    match shape {
        Shape::Line {
            a,
            b,
            color,
            dashed,
        } => {
            if let (Some(a), Some(b)) = (cx.to_screen(*a), cx.to_screen(*b)) {
                let stroke = Stroke::new(1.5, *color);
                if *dashed {
                    painter.extend(egui::Shape::dashed_line(&[a, b], stroke, 6.0, 4.0));
                } else {
                    painter.line_segment([a, b], stroke);
                }
            }
        }
        Shape::Path {
            points,
            closed,
            color,
        } => {
            let mut screen: Vec<egui::Pos2> =
                points.iter().filter_map(|p| cx.to_screen(*p)).collect();
            if screen.len() == points.len() && screen.len() >= 2 {
                if *closed {
                    screen.push(screen[0]);
                }
                painter.add(egui::Shape::line(screen, Stroke::new(1.5, *color)));
            }
        }
        Shape::Marker { p, color } => {
            if let Some(s) = cx.to_screen(*p) {
                painter.rect_filled(
                    egui::Rect::from_center_size(s, egui::vec2(8.0, 8.0)),
                    1.0,
                    *color,
                );
            }
        }
        Shape::ScreenRect { a, b, crossing } => {
            let rect = egui::Rect::from_two_pos(*a, *b);
            let stroke = Stroke::new(1.0, Color32::from_rgb(30, 30, 30));
            if *crossing {
                let c = [
                    rect.left_top(),
                    rect.right_top(),
                    rect.right_bottom(),
                    rect.left_bottom(),
                    rect.left_top(),
                ];
                painter.extend(egui::Shape::dashed_line(&c, stroke, 5.0, 4.0));
            } else {
                painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
            }
        }
    }
}

/// Inference marks and labels (docs/06 §10, docs/07 §5).
fn paint_inference(
    painter: &egui::Painter,
    cx: &Cx<'_>,
    inf: &Inference,
    cursor: egui::Pos2,
    show_label: bool,
) {
    let Some(s) = cx.to_screen(inf.point) else {
        return;
    };
    if inf.is_directional()
        && let Some(from) = inf.from.and_then(|f| cx.to_screen(f))
    {
        let width = if inf.locked { 3.0 } else { 1.5 };
        let color = axis_color(inf.axis);
        painter.extend(egui::Shape::dashed_line(
            &[from, s],
            Stroke::new(width, color),
            4.0,
            3.0,
        ));
    } else if inf.locked
        && let (Some(from), Some(dir)) = (inf.from, inf.direction)
        && let (Some(a), Some(b)) = (cx.to_screen(from), cx.to_screen(from + dir * 1.0e5))
    {
        painter.line_segment([a, b], Stroke::new(3.0, Color32::from_rgb(120, 120, 120)));
    }
    let green = Color32::from_rgb(0, 170, 0);
    let red = Color32::from_rgb(220, 30, 30);
    let blue = Color32::from_rgb(40, 70, 230);
    match inf.kind {
        InferenceKind::Endpoint => {
            painter.circle(s, 4.5, green, Stroke::new(1.0, Color32::BLACK));
        }
        InferenceKind::Midpoint => {
            painter.circle(
                s,
                4.5,
                Color32::from_rgb(0, 190, 210),
                Stroke::new(1.0, Color32::BLACK),
            );
        }
        InferenceKind::Intersection => {
            let d = 5.0;
            let stroke = Stroke::new(2.0, red);
            painter.line_segment([s + Vec2::new(-d, -d), s + Vec2::new(d, d)], stroke);
            painter.line_segment([s + Vec2::new(-d, d), s + Vec2::new(d, -d)], stroke);
        }
        InferenceKind::Center => {
            painter.circle_stroke(s, 5.0, Stroke::new(1.5, blue));
            painter.circle_filled(s, 1.5, blue);
        }
        InferenceKind::OnEdge => {
            painter.rect_stroke(
                egui::Rect::from_center_size(s, egui::vec2(9.0, 9.0)),
                0.0,
                Stroke::new(1.5, red),
                egui::StrokeKind::Inside,
            );
        }
        InferenceKind::OnFace => {
            let d = 5.0;
            let points = vec![
                s + Vec2::new(0.0, -d),
                s + Vec2::new(d, 0.0),
                s + Vec2::new(0.0, d),
                s + Vec2::new(-d, 0.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                points,
                Color32::TRANSPARENT,
                Stroke::new(1.5, blue),
            ));
        }
        InferenceKind::Free | InferenceKind::OnPlane => {}
        _ => {
            painter.circle_filled(s, 2.5, axis_color(inf.axis));
        }
    }
    let mut label = inf.label.clone();
    if inf.locked {
        label = if label.is_empty() {
            "Locked".into()
        } else {
            format!("{label} (locked)")
        };
    }
    if show_label && !label.is_empty() {
        let pos = cursor + Vec2::new(14.0, 14.0);
        let galley =
            painter.layout_no_wrap(label, egui::FontId::proportional(12.0), Color32::BLACK);
        let bg = egui::Rect::from_min_size(pos, galley.size()).expand(3.0);
        painter.rect_filled(bg, 3.0, Color32::from_rgba_unmultiplied(255, 255, 220, 230));
        painter.galley(pos, galley, Color32::BLACK);
    }
}

/// Leaves the innermost open group or component (docs/07 §10 `Esc`).
pub fn exit_context(session: &mut Session) {
    let parent = session
        .engine
        .context()
        .path
        .parent()
        .unwrap_or_else(InstancePath::root);
    let _ = session.engine.set_context(EditContext { path: parent });
}

/// Applies a tool's selection and context requests (the app runs commands).
pub fn apply_view_flow(session: &mut Session, flow: &Flow) -> Result<(), String> {
    match flow {
        Flow::Select(selection) => {
            session.engine.set_selection(selection.clone());
            Ok(())
        }
        Flow::Enter(path) => {
            let context = EditContext {
                path: path.instances.child(path.target),
            };
            session
                .engine
                .set_context(context)
                .map_err(|e| e.to_string())?;
            session
                .engine
                .set_selection(planyrith_core::document::Selection::new());
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;
    use crate::tools::{LineTool, RectangleTool, SelectTool};

    /// Drives the viewport with synthetic egui input, frame by frame, and
    /// applies what the tool asks for the way the application does.
    struct Harness {
        ctx: egui::Context,
        time: f64,
        viewport: Viewport,
        session: Session,
        tool: Box<dyn Tool>,
    }

    const SCREEN: egui::Vec2 = egui::vec2(800.0, 600.0);

    impl Harness {
        fn new(session: Session) -> Self {
            let mut h = Harness {
                ctx: egui::Context::default(),
                time: 0.0,
                viewport: Viewport::default(),
                session,
                tool: Box::new(SelectTool::default()),
            };
            h.frame(Vec::new());
            h
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (viewport, session, tool) = (&mut self.viewport, &mut self.session, &mut self.tool);
            let mut flows = Vec::new();
            let mut output = self.ctx.run_ui(input, |ui| {
                flows = viewport.ui(ui, None, session, tool.as_mut(), None);
            });
            output.textures_delta.clear();
            for flow in flows {
                match flow {
                    Flow::Run(commands) => {
                        for (name, params) in commands {
                            self.session.execute(&name, params).unwrap();
                        }
                        self.viewport.after_run();
                    }
                    other => apply_view_flow(&mut self.session, &other).unwrap(),
                }
            }
        }

        /// Where a world point appears on screen.
        fn screen(&self, p: DVec3) -> egui::Pos2 {
            let camera = Camera {
                aspect: f64::from(SCREEN.x / SCREEN.y),
                ..self.session.doc().camera
            };
            let depth = camera.fit_depth(&self.session.doc().aabb());
            let ndc = camera.project(p, depth).unwrap();
            let pos = egui::pos2(
                ((ndc.x + 1.0) * 0.5) as f32 * SCREEN.x,
                ((1.0 - ndc.y) * 0.5) as f32 * SCREEN.y,
            );
            assert!(
                egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN).contains(pos),
                "{p} is off screen at {pos:?}"
            );
            pos
        }

        fn button(&mut self, pos: egui::Pos2, button: PointerButton, pressed: bool) {
            self.frame(vec![egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }

        fn click(&mut self, pos: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            self.button(pos, PointerButton::Primary, true);
            self.button(pos, PointerButton::Primary, false);
            // Long enough apart not to make double clicks.
            self.time += 1.0;
        }
    }

    fn two_boxes() -> Session {
        let mut session = Session::new();
        for x in [0, 2000] {
            session
                .execute(
                    "rectangle",
                    json!({ "corner_a": [x, 0, 0], "corner_b": [x + 1000, 1000, 0] }),
                )
                .unwrap();
            session
                .execute(
                    "push-pull",
                    json!({ "face": { "Query": "face & last" }, "distance": 1000.0 }),
                )
                .unwrap();
        }
        // Group the first box only.
        let first_box: Vec<String> = session
            .doc()
            .model
            .order()
            .iter()
            .filter(|id| {
                let bounds = match id {
                    EntityId::Face(f) => session.doc().model.mesh().face_aabb(*f).ok(),
                    EntityId::Edge(e) => session
                        .doc()
                        .model
                        .mesh()
                        .edge_segment(*e)
                        .ok()
                        .map(|s| s.aabb()),
                    _ => None,
                };
                bounds.is_some_and(|b| b.max.x <= 1000.5)
            })
            .map(|id| format!("@{}", session.doc().model.pid_of(*id).unwrap().0))
            .collect();
        session
            .execute("make-group", json!({ "targets": first_box.join(" | ") }))
            .unwrap();
        session
            .execute("camera-standard", json!({ "view": "iso", "extents": true }))
            .unwrap();
        session
            .engine
            .set_selection(planyrith_core::document::Selection::new());
        session
    }

    #[test]
    fn select_tool_selects_opens_and_the_viewport_navigates() {
        let mut h = Harness::new(two_boxes());

        // A loose face is selected itself.
        let loose_top = h.screen(DVec3::new(2500.0, 500.0, 1000.0));
        h.click(loose_top);
        let selection: Vec<EntityPath> = h.session.engine.selection().iter().cloned().collect();
        assert_eq!(selection.len(), 1);
        let r = h.session.doc().resolve(&selection[0]).unwrap();
        assert!(matches!(r.entity, EntityId::Face(_)), "{r:?}");

        // A face inside the group selects the group from the model...
        let grouped_top = h.screen(DVec3::new(500.0, 500.0, 1000.0));
        h.click(grouped_top);
        let selection: Vec<EntityPath> = h.session.engine.selection().iter().cloned().collect();
        let r = h.session.doc().resolve(&selection[0]).unwrap();
        assert!(matches!(r.entity, EntityId::Group(_)), "{r:?}");

        // ...and a double-click opens it.
        h.frame(vec![egui::Event::PointerMoved(grouped_top)]);
        for _ in 0..2 {
            h.button(grouped_top, PointerButton::Primary, true);
            h.button(grouped_top, PointerButton::Primary, false);
        }
        assert!(
            !h.session.engine.context().path.is_root(),
            "the group is open ({:?})",
            h.viewport.error
        );
        exit_context(&mut h.session);

        // Middle-drag orbits about the point under the cursor.
        let before = h.session.doc().camera;
        let start = h.screen(DVec3::new(2500.0, 500.0, 1000.0));
        h.frame(vec![egui::Event::PointerMoved(start)]);
        h.button(start, PointerButton::Middle, true);
        for i in 1..=10 {
            h.frame(vec![egui::Event::PointerMoved(
                start + egui::vec2(8.0 * i as f32, 0.0),
            )]);
        }
        h.button(start + egui::vec2(80.0, 0.0), PointerButton::Middle, false);
        let after = h.session.doc().camera;
        assert_ne!(after.eye, before.eye);
        let pivot = DVec3::new(2500.0, 500.0, 1000.0);
        assert!(
            (after.eye.distance(pivot) - before.eye.distance(pivot)).abs() < 50.0,
            "orbiting keeps the distance to the picked point"
        );

        // The wheel zooms toward the cursor.
        let before = h.session.doc().camera;
        h.frame(vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 120.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        }]);
        h.frame(Vec::new());
        h.frame(Vec::new());
        let after = h.session.doc().camera;
        assert!(after.distance() < before.distance(), "zoomed in");
    }

    #[test]
    fn drawing_tools_snap_to_the_model() {
        let mut session = two_boxes();
        session.execute("zoom", json!({ "factor": 0.4 })).unwrap();
        let mut h = Harness::new(session);
        let edges = h.session.doc().model.counts().edges;

        // A line from the loose box's top corner to a point on the ground.
        h.tool = Box::new(LineTool::default());
        let corner = DVec3::new(3000.0, 1000.0, 1000.0);
        h.click(h.screen(corner + DVec3::new(3.0, -3.0, 0.0)));
        h.click(h.screen(DVec3::new(4500.0, -1500.0, 0.0)));
        assert_eq!(h.session.doc().model.counts().edges, edges + 1);
        let last = h.session.engine.last_created().to_vec();
        let (container, id) = h.session.doc().locate_pid(last[0]).unwrap();
        assert_eq!(container, ContainerRef::Model);
        let EntityId::Edge(e) = id else {
            panic!("{id:?}")
        };
        let seg = h.session.doc().model.mesh().edge_segment(e).unwrap();
        assert!(
            seg.a.distance(corner) < 1e-6 || seg.b.distance(corner) < 1e-6,
            "the line starts at the snapped corner: {seg:?}"
        );

        // Two clicks on the ground make a rectangle.
        h.tool = Box::new(RectangleTool::default());
        let faces = h.session.doc().model.counts().faces;
        h.click(h.screen(DVec3::new(4000.0, -3000.0, 0.0)));
        h.click(h.screen(DVec3::new(5000.0, -2200.0, 0.0)));
        assert_eq!(h.session.doc().model.counts().faces, faces + 1);
    }
}
