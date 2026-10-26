//! Implements docs/04-document-model.md §3.7 image entities: `image-insert`
//! places an image file as a textured rectangle (its material carries the
//! image at the rectangle's size), and `explode` turns one into a face
//! painted with that material and mapped so the image fills it exactly.

use glam::{DAffine2, DAffine3, DMat2, DMat3, DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::draw::{draw_chain, face_of_chain};
use crate::commands::organize::libraries::storage;
use crate::commands::params::{PointSchema, Target, VectorSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ContainerRef, Document, EntityRecord, EntityView, ImageEntity, Material, Rgba8, Texture, UvMap,
};
use crate::ids::{EntityId, Pid};
use crate::io::StorageKey;
use crate::units::Length;

fn up() -> DVec3 {
    DVec3::Z
}

/// `image-insert` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImageInsertParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Image file (PNG, JPEG, WebP).
    pub path: String,
    /// Where its lower left corner goes (world, mm; the origin).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub position: Option<DVec3>,
    /// Width (1 m unless the height is given; the other side follows the
    /// image's proportions).
    #[serde(default)]
    pub width: Option<Length>,
    /// Height.
    #[serde(default)]
    pub height: Option<Length>,
    /// Normal of the image's plane (+Z: lying on the ground).
    #[serde(default = "up")]
    #[schemars(with = "VectorSchema")]
    pub normal: DVec3,
}

/// `image-insert` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageInsertOutput {
    /// The image entity.
    pub image: Pid,
    /// The material made for it.
    pub material: String,
}

/// The `image-insert` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImageInsert;

/// Red horizontal unless the plane is horizontal (then world red), blue
/// along the normal (the gluing frame of components).
fn image_frame(normal: DVec3) -> DMat3 {
    let z = normal.normalize_or_zero();
    let x = DVec3::Z.cross(z).try_normalize().unwrap_or(DVec3::X);
    DMat3::from_cols(x, z.cross(x), z)
}

/// `name`, or `name #2`… whichever no material has yet.
fn unique_material_name(doc: &Document, name: &str) -> String {
    if doc.materials.by_name(name).is_none() {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name} #{n}"))
        .find(|c| doc.materials.by_name(c).is_none())
        .unwrap_or_else(|| name.to_string())
}

impl Command for ImageInsert {
    type Params = ImageInsertParams;
    type Output = ImageInsertOutput;

    fn name(&self) -> &'static str {
        "image-insert"
    }

    fn validate(&self, _doc: &Document, p: &ImageInsertParams) -> Result<(), CommandError> {
        if p.path.trim().is_empty() {
            return Err(CommandError::invalid_param("path", "must not be empty"));
        }
        if let Some(pos) = p.position {
            check_point("position", pos)?;
        }
        if p.normal.try_normalize().is_none() || !p.normal.is_finite() {
            return Err(CommandError::invalid_param(
                "normal",
                "must be a non-zero vector",
            ));
        }
        for (name, l) in [("width", p.width), ("height", p.height)] {
            if let Some(l) = l
                && !(l.as_mm() > 0.0 && l.as_mm().is_finite())
            {
                return Err(CommandError::OutOfRange {
                    name: name.into(),
                    reason: "must be a positive length".into(),
                });
            }
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ImageInsertParams,
    ) -> Result<ImageInsertOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let bytes = storage(tx, &p.path)?.read(&StorageKey::new(&p.path))?;
        let image = tx.insert_image(bytes)?;
        let (px_w, px_h) = tx
            .document()
            .images
            .get(image)
            .map(|d| (f64::from(d.width), f64::from(d.height)))
            .ok_or_else(|| CommandError::Internal("lost image".into()))?;
        let aspect = px_h / px_w.max(1.0);
        let (w, h) = match (p.width, p.height) {
            (Some(w), Some(h)) => (w.as_mm(), h.as_mm()),
            (Some(w), None) => (w.as_mm(), w.as_mm() * aspect),
            (None, Some(h)) => (h.as_mm() / aspect, h.as_mm()),
            (None, None) => (1000.0, 1000.0 * aspect),
        };
        let stem = p
            .path
            .rsplit(['/', '\\'])
            .next()
            .and_then(|f| f.split('.').next())
            .filter(|s| !s.is_empty())
            .unwrap_or("Image");
        let name = unique_material_name(tx.document(), stem);
        let material = tx.insert_material(Material {
            texture: Some(Texture {
                image,
                size_mm: DVec2::new(w, h),
                colorize: None,
            }),
            ..Material::solid(name.clone(), Rgba8::WHITE)
        })?;
        let world = DAffine3::from_mat3_translation(
            image_frame(p.normal),
            p.position.unwrap_or(DVec3::ZERO),
        );
        let (_, pid) = tx.insert_entity(
            target.container,
            EntityRecord::Image(ImageEntity {
                width: Length::mm(w),
                height: Length::mm(h),
                transform: target.local * world,
                material,
            }),
        )?;
        Ok(ImageInsertOutput {
            image: pid,
            material: name,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "image-insert".into(),
            category: CommandCategory::Organize,
            label: "Insert Image".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}

/// Explodes an image entity into a face painted with its material, the
/// texture filling the face exactly (docs/04 §3.7).
pub(crate) fn explode_image(
    tx: &mut Transaction<'_>,
    target: &Target,
    id: EntityId,
) -> Result<(), CommandError> {
    let container: ContainerRef = target.container;
    let Some(EntityView::Image(image)) =
        tx.document().container(container).and_then(|e| e.view(id))
    else {
        return Ok(());
    };
    let image = image.clone();
    let (w, h) = (image.width.as_mm(), image.height.as_mm());
    let local_corners = [
        DVec3::ZERO,
        DVec3::new(w, 0.0, 0.0),
        DVec3::new(w, h, 0.0),
        DVec3::new(0.0, h, 0.0),
    ]
    .map(|c| image.transform.transform_point3(c));
    let world: Vec<DVec3> = local_corners
        .iter()
        .map(|c| target.world.transform_point3(*c))
        .collect();
    tx.remove_entity(container, id)?;
    let drawn = draw_chain(tx, target, &world, true, None)?;
    let Some(face_pid) = face_of_chain(tx, target, &drawn)? else {
        return Ok(());
    };
    let Some(EntityId::Face(face)) = tx
        .document()
        .container(container)
        .and_then(|e| e.id_of(face_pid))
    else {
        return Ok(());
    };
    let Some(f) = tx
        .document()
        .container(container)
        .and_then(|e| e.mesh().face(face).cloned())
    else {
        return Ok(());
    };
    // The face frame's 2D coordinates of the image's corner and edges, mapped
    // to texture millimeters: corner → (0, 0), along the width → (w, 0), up
    // the height → (0, h).
    let frame = f.plane.frame();
    let c0 = frame.to_2d(local_corners[0]);
    let du = frame.to_2d(local_corners[1]) - c0;
    let dv = frame.to_2d(local_corners[3]) - c0;
    let basis = DMat2::from_cols(du, dv);
    if basis.determinant().abs() <= f64::MIN_POSITIVE {
        return Ok(());
    }
    let m = DMat2::from_cols(DVec2::new(w, 0.0), DVec2::new(0.0, h)) * basis.inverse();
    let map = UvMap::Affine(DAffine2::from_mat2_translation(m, -(m * c0)));
    let mut sides = f.sides.clone();
    let normal = image.transform.matrix3.z_axis;
    if f.plane.normal.dot(normal) >= 0.0 {
        sides.material_front = Some(image.material);
        sides.uv_front = Some(map);
    } else {
        sides.material_back = Some(image.material);
        sides.uv_back = Some(map);
    }
    tx.with_mesh(container, |m, _, d| m.set_face_sides(face, sides, d))?;
    Ok(())
}
