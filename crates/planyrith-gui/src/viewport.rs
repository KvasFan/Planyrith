//! The 3D viewport (docs/07 §1–§2, docs/06 §7–§9): the renderer draws into
//! its offscreen texture, which egui shows; middle-drag orbits about the point
//! under the cursor (Shift pans), the wheel zooms toward the cursor, a click
//! selects (CPU ray cast, authoritative), a double-click opens a group or
//! component for editing. While the pointer rests, the id buffer names what
//! is under it for the status bar.

use egui::{PointerButton, Sense, Vec2};
use glam::{DVec2, DVec3};
use planyrith_core::document::{
    Camera, EditContext, EntityKey, EntityPath, InstancePath, Selection,
};
use planyrith_core::ids::EntityId;
use planyrith_core::io::render_mesh::draw_list;
use planyrith_core::math::{Aabb, Ray};
use planyrith_core::spatial::HitFilter;
use planyrith_render::{FrameRequest, PickKind, Renderer, RendererOptions};

use crate::session::Session;

/// Pixel tolerance for picking edges and vertices (docs/07 §5).
const PICK_PX: f64 = 8.0;
/// Radians per pixel of orbit drag.
const ORBIT_SPEED: f64 = 0.008;

/// What a drag started on.
#[derive(Debug, Clone, Copy)]
struct Drag {
    pivot: DVec3,
    depth: f64,
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
    drag: Option<Drag>,
    last_hover: Option<egui::Pos2>,
    /// What the pointer is over, for the status bar.
    pub hover: Option<String>,
    /// The last error, for the status bar.
    pub error: Option<String>,
}

/// The selection change a click makes (docs/05 §4.1 modifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickMode {
    Replace,
    Add,
    Toggle,
    Remove,
}

impl Viewport {
    /// Draws the viewport and handles its input.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        render_state: Option<&egui_wgpu::RenderState>,
        session: &mut Session,
    ) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
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
            let pivot = self
                .cast(session, &camera, &ray, f64::from(height))
                .map(|(_, p)| p)
                .unwrap_or_else(|| visible_center(session, &camera));
            self.drag = Some(Drag {
                pivot,
                depth: camera.depth_of(pivot).max(1.0),
            });
        }
        if response.dragged_by(PointerButton::Middle)
            && let Some(drag) = self.drag
        {
            let delta = response.drag_delta() * ppp;
            if ui.input(|i| i.modifiers.shift) {
                camera.pan(camera.pan_delta_for_pixels(
                    f64::from(delta.x),
                    f64::from(delta.y),
                    f64::from(height),
                    drag.depth,
                ));
            } else {
                camera.orbit(
                    drag.pivot,
                    -f64::from(delta.x) * ORBIT_SPEED,
                    -f64::from(delta.y) * ORBIT_SPEED,
                );
            }
            moved = true;
        }
        if response.drag_stopped_by(PointerButton::Middle) {
            self.drag = None;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0
                && let Some(pos) = response.hover_pos()
            {
                let ray = camera.ray_through_ndc(to_ndc(pos));
                let toward = self
                    .cast(session, &camera, &ray, f64::from(height))
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

        // Selection (docs/07 §10).
        if response.clicked_by(PointerButton::Primary)
            && let Some(pos) = response.interact_pointer_pos()
        {
            let modifiers = ui.input(|i| i.modifiers);
            let mode = match (modifiers.command, modifiers.shift) {
                (true, true) => ClickMode::Remove,
                (true, false) => ClickMode::Add,
                (false, true) => ClickMode::Toggle,
                (false, false) => ClickMode::Replace,
            };
            let ray = camera.ray_through_ndc(to_ndc(pos));
            let hit = self.selectable_hit(session, &camera, &ray, f64::from(height));
            apply_click(session, hit, mode);
        }
        if response.double_clicked_by(PointerButton::Primary)
            && let Some(pos) = response.interact_pointer_pos()
        {
            let ray = camera.ray_through_ndc(to_ndc(pos));
            if let Some(path) = self.selectable_hit(session, &camera, &ray, f64::from(height)) {
                let doc = session.doc();
                let opens = doc
                    .resolve(&path)
                    .is_ok_and(|r| matches!(r.entity, EntityId::Group(_) | EntityId::Instance(_)));
                if opens {
                    let context = EditContext {
                        path: path.instances.child(path.target),
                    };
                    match session.engine.set_context(context) {
                        Ok(()) => session.engine.set_selection(Selection::new()),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
            }
        }

        // Render.
        let Some(render_state) = render_state else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "The 3D viewport needs the wgpu renderer.",
                egui::FontId::proportional(16.0),
                ui.visuals().text_color(),
            );
            return;
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
        let doc = session.doc();
        let items = draw_list(
            doc,
            session.engine.selection(),
            session.engine.context(),
            self.show_hidden,
        );
        let style = doc.styles.active_style().cloned().unwrap_or_default();
        let hover = response.hover_pos();
        let want_ids = hover.is_some() && self.drag.is_none() && hover != self.last_hover;
        let frame_camera = Camera {
            aspect,
            ..doc.camera
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
            return;
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
            return;
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
                egui::Color32::WHITE,
            );
        }
        // Keep hover information fresh while the pointer moves.
        if hover.is_some() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
            ui.ctx().request_repaint();
        }
    }

    /// The nearest hit under a ray: the occurrence path and the hit point.
    fn cast(
        &self,
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

    /// What a click on `ray` selects in the current edit context.
    fn selectable_hit(
        &self,
        session: &mut Session,
        camera: &Camera,
        ray: &Ray,
        height_px: f64,
    ) -> Option<EntityPath> {
        let (path, _) = self.cast(session, camera, ray, height_px)?;
        let context = session.engine.context().path.clone();
        path.selectable_in(session.doc(), &context)
    }
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

fn apply_click(session: &mut Session, hit: Option<EntityPath>, mode: ClickMode) {
    let doc = session.doc();
    let mut items: Vec<EntityPath> = session
        .engine
        .selection()
        .iter()
        .map(|p| p.canonical(doc))
        .collect();
    match (hit, mode) {
        (None, ClickMode::Replace) => items.clear(),
        (None, _) => return,
        (Some(path), ClickMode::Replace) => items = vec![path],
        (Some(path), ClickMode::Add) => {
            if !items.contains(&path) {
                items.push(path);
            }
        }
        (Some(path), ClickMode::Remove) => items.retain(|p| *p != path),
        (Some(path), ClickMode::Toggle) => {
            if items.contains(&path) {
                items.retain(|p| *p != path);
            } else {
                items.push(path);
            }
        }
    }
    let mut selection = Selection::new();
    for path in items {
        selection.insert(path);
    }
    session.engine.set_selection(selection);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    #[test]
    fn clicks_replace_add_toggle_and_remove() {
        let mut s = Session::new();
        for x in [0, 2000] {
            s.execute(
                "rectangle",
                json!({ "corner_a": [x, 0, 0], "corner_b": [x + 1000, 1000, 0] }),
            )
            .unwrap();
        }
        let faces: Vec<EntityPath> = s
            .doc()
            .model
            .order()
            .iter()
            .filter(|id| matches!(id, EntityId::Face(_)))
            .map(|id| EntityPath::in_model(EntityKey::Pid(s.doc().model.pid_of(*id).unwrap())))
            .collect();
        apply_click(&mut s, Some(faces[0].clone()), ClickMode::Replace);
        assert_eq!(s.engine.selection().len(), 1);
        apply_click(&mut s, Some(faces[1].clone()), ClickMode::Add);
        assert_eq!(s.engine.selection().len(), 2);
        apply_click(&mut s, Some(faces[0].clone()), ClickMode::Toggle);
        assert_eq!(s.engine.selection().len(), 1);
        apply_click(&mut s, Some(faces[1].clone()), ClickMode::Remove);
        assert!(s.engine.selection().is_empty());
        apply_click(&mut s, Some(faces[1].clone()), ClickMode::Add);
        apply_click(&mut s, None, ClickMode::Add);
        assert_eq!(
            s.engine.selection().len(),
            1,
            "a miss with a modifier keeps the selection"
        );
        apply_click(&mut s, None, ClickMode::Replace);
        assert!(s.engine.selection().is_empty());

        // Open a group, then leave it.
        s.execute("make-group", json!({ "targets": "all" }))
            .unwrap();
        let group = s
            .doc()
            .model
            .order()
            .iter()
            .find(|id| matches!(id, EntityId::Group(_)))
            .map(|id| s.doc().model.pid_of(*id).unwrap())
            .unwrap();
        s.engine
            .set_context(EditContext {
                path: InstancePath::root().child(EntityKey::Pid(group)),
            })
            .unwrap();
        exit_context(&mut s);
        assert!(s.engine.context().path.is_root());
    }

    /// Drives the viewport with synthetic egui input, frame by frame.
    struct Harness {
        ctx: egui::Context,
        time: f64,
        viewport: Viewport,
        session: Session,
    }

    const SCREEN: egui::Vec2 = egui::vec2(800.0, 600.0);

    impl Harness {
        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (viewport, session) = (&mut self.viewport, &mut self.session);
            let mut output = self.ctx.run_ui(input, |ui| viewport.ui(ui, None, session));
            output.textures_delta.clear();
        }

        /// Where a world point appears on screen.
        fn screen(&self, p: DVec3) -> egui::Pos2 {
            let camera = Camera {
                aspect: f64::from(SCREEN.x / SCREEN.y),
                ..self.session.doc().camera
            };
            let depth = camera.fit_depth(&self.session.doc().aabb());
            let ndc = camera.project(p, depth).unwrap();
            egui::pos2(
                ((ndc.x + 1.0) * 0.5) as f32 * SCREEN.x,
                ((1.0 - ndc.y) * 0.5) as f32 * SCREEN.y,
            )
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
        }
    }

    #[test]
    fn the_viewport_selects_opens_orbits_and_zooms() {
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
        session.engine.set_selection(Selection::new());

        let mut h = Harness {
            ctx: egui::Context::default(),
            time: 0.0,
            viewport: Viewport::default(),
            session,
        };
        h.frame(Vec::new());

        // A loose face is selected itself.
        let loose_top = h.screen(DVec3::new(2500.0, 500.0, 1000.0));
        h.click(loose_top);
        let selection: Vec<EntityPath> = h.session.engine.selection().iter().cloned().collect();
        assert_eq!(selection.len(), 1);
        let r = h.session.doc().resolve(&selection[0]).unwrap();
        assert!(matches!(r.entity, EntityId::Face(_)), "{r:?}");

        // A face inside the group selects the group from the model...
        let grouped_top = h.screen(DVec3::new(500.0, 500.0, 1000.0));
        // Long enough after the last click not to make it a triple click.
        h.time += 1.0;
        h.frame(Vec::new());
        h.click(grouped_top);
        let selection: Vec<EntityPath> = h.session.engine.selection().iter().cloned().collect();
        let r = h.session.doc().resolve(&selection[0]).unwrap();
        assert!(matches!(r.entity, EntityId::Group(_)), "{r:?}");

        // ...and a double-click opens it.
        h.click(grouped_top);
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
}
