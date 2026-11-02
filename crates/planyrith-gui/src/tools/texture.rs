//! Texture pins (docs/05 §5.9 "Interactive: 4 pins"): the corners of one
//! texture repeat, shown on the face; dragging one runs `texture-position`
//! with pins. In fixed mode the first pin moves the texture, the second
//! turns and scales it, the third and fourth skew it; in free mode all four
//! give a perspective placement.

use egui::Color32;
use glam::{DVec2, DVec3};
use planyrith_core::document::{EntityPath, UvMap};
use planyrith_core::ids::EntityId;
use planyrith_core::math::Plane;
use serde_json::{Value, json};

use super::{Cx, Flow, Pointer, Shape, Tool};

/// Texture points of the pins, in repeats.
const UV: [[f64; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
/// How near a click must be to a pin to take it, in points.
const GRAB_PT: f32 = 12.0;
const COLORS: [Color32; 4] = [
    Color32::from_rgb(220, 40, 40),
    Color32::from_rgb(40, 170, 40),
    Color32::from_rgb(40, 90, 230),
    Color32::from_rgb(220, 170, 20),
];

/// The texture pin tool.
#[derive(Debug, Default)]
pub struct TexturePinTool {
    face: Option<EntityPath>,
    back: bool,
    free: bool,
    /// The pins in world coordinates and the face's plane, or why there
    /// are none.
    pins: Option<Result<([DVec3; 4], Plane), String>>,
    /// The pin being moved and where it is now.
    moving: Option<(usize, DVec3)>,
}

impl TexturePinTool {
    /// Pins the texture of `face` (a full path to it).
    pub fn new(face: EntityPath) -> Self {
        TexturePinTool {
            face: Some(face),
            ..Self::default()
        }
    }

    /// Where the corners of the first texture repeat are on the face.
    fn locate(&self, cx: &Cx<'_>) -> Result<([DVec3; 4], Plane), String> {
        let face = self
            .face
            .as_ref()
            .ok_or("open Position Texture on a face first")?;
        let r = cx.doc.resolve(face).map_err(|e| e.to_string())?;
        let EntityId::Face(f) = r.entity else {
            return Err("not a face".into());
        };
        let face = cx
            .doc
            .container(r.container)
            .and_then(|c| c.mesh().face(f))
            .ok_or("the face is gone")?;
        let (material, map) = if self.back {
            (face.sides.material_back, &face.sides.uv_back)
        } else {
            (face.sides.material_front, &face.sides.uv_front)
        };
        let tile = material
            .and_then(|m| cx.doc.materials.get(m))
            .and_then(|m| m.texture.as_ref())
            .map(|t| t.size_mm)
            .ok_or("this side has no texture")?;
        let frame = face.plane.frame();
        let mut at = [DVec3::ZERO; 4];
        for (k, uv) in UV.iter().enumerate() {
            let t = DVec2::from(*uv) * tile;
            let f = match map {
                None => t,
                Some(UvMap::Affine(a)) => a.inverse().transform_point2(t),
                Some(UvMap::Projective(m)) => {
                    let h = m.inverse() * t.extend(1.0);
                    if h.z.abs() < 1e-12 {
                        return Err("a pin is at infinity".into());
                    }
                    h.truncate() / h.z
                }
                Some(_) => {
                    return Err("pins move textures laid on the face; this one is projected".into());
                }
            };
            at[k] = r.world.transform_point3(frame.from_2d(f));
        }
        Ok((at, face.plane.transformed(&r.world)))
    }

    /// The `texture-position` request with pin `moved` at `to`.
    fn request(&self, pins: &[DVec3; 4], moved: usize, to: DVec3) -> Option<Value> {
        let face = self.face.as_ref()?;
        let fixed: &[usize] = match moved {
            0 => &[0],
            1 => &[0, 1],
            2 => &[0, 1, 2],
            _ => &[0, 1, 3],
        };
        let kept: Vec<usize> = if self.free {
            vec![0, 1, 2, 3]
        } else {
            fixed.to_vec()
        };
        let pins: Vec<Value> = kept
            .into_iter()
            .map(|k| {
                let p = if k == moved { to } else { pins[k] };
                json!({ "uv": UV[k], "at": [p.x, p.y, p.z] })
            })
            .collect();
        Some(json!({
            "face": { "Path": face },
            "side": if self.back { "back" } else { "front" },
            "pins": pins,
            "projective": self.free,
        }))
    }

    fn located(&self) -> Option<&([DVec3; 4], Plane)> {
        self.pins.as_ref().and_then(|r| r.as_ref().ok())
    }
}

impl Tool for TexturePinTool {
    fn command(&self) -> &'static str {
        "texture-position"
    }

    fn label(&self) -> &'static str {
        "Texture Pins"
    }

    fn step(&self) -> usize {
        usize::from(self.moving.is_some())
    }

    fn hover(&mut self, p: &Pointer, cx: &Cx<'_>) {
        self.pins = Some(self.locate(cx));
        if let (Some((i, _)), Some((_, plane))) = (self.moving, self.located()) {
            self.moving = Some((i, plane.project(p.inference.point)));
        }
    }

    fn click(&mut self, p: &Pointer, cx: &Cx<'_>) -> Flow {
        let Some((pins, plane)) = self.locate(cx).ok() else {
            return Flow::Continue;
        };
        match self.moving.take() {
            Some((i, _)) => {
                let to = plane.project(p.inference.point);
                self.request(&pins, i, to).map_or(Flow::Continue, |params| {
                    Flow::run("texture-position", params)
                })
            }
            None => {
                let nearest = pins
                    .iter()
                    .enumerate()
                    .filter_map(|(k, at)| Some((k, cx.to_screen(*at)?.distance(p.screen))))
                    .filter(|(_, d)| *d <= GRAB_PT)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((k, _)) = nearest {
                    self.moving = Some((k, pins[k]));
                }
                Flow::Continue
            }
        }
    }

    fn value(&mut self, _text: &str, _cx: &Cx<'_>) -> Result<Flow, String> {
        Err("move pins with the mouse, or give exact pins to texture-position".into())
    }

    fn escape(&mut self) -> bool {
        self.moving.take().is_some()
    }

    fn shapes(&self, out: &mut Vec<Shape>) {
        let Some((pins, _)) = self.located() else {
            return;
        };
        let mut at = *pins;
        if let Some((i, to)) = self.moving {
            at[i] = to;
        }
        out.push(Shape::Path {
            points: at.to_vec(),
            closed: true,
            color: Color32::from_rgb(90, 90, 90),
        });
        for (p, color) in at.iter().zip(COLORS) {
            out.push(Shape::Marker { p: *p, color });
        }
    }

    fn preview(&self, _cx: &Cx<'_>) -> Option<(String, Value)> {
        let (i, to) = self.moving?;
        let (pins, _) = self.located()?;
        self.request(pins, i, to)
            .map(|params| ("texture-position".to_string(), params))
    }

    fn options(&mut self, ui: &mut egui::Ui, _cx: &Cx<'_>) {
        if let Some(Err(why)) = &self.pins {
            ui.weak(why.as_str());
        } else {
            ui.weak(
                "Click a pin, then click where it goes. Red moves the texture, green turns \
                 and scales it, blue and yellow skew it.",
            );
        }
        ui.checkbox(&mut self.free, "Free pins (perspective)")
            .on_hover_text("Each pin moves alone; four pins give a perspective placement");
        ui.checkbox(&mut self.back, "Back side");
    }
}
