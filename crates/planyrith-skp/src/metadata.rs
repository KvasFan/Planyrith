//! Retains decoded settings without native equivalents as inspectable data (docs/09 §5).
use super::Result;
use openskp::Model;
use planyrith_core::{
    commands::{Transaction, transaction::AttrTarget},
    document::Value,
};

pub(super) fn import(tx: &mut Transaction<'_>, model: &Model) -> Result<()> {
    let mut json: serde_json::Value = serde_json::from_str(&model.to_json())?;
    // Geometry and textures are native entities/assets, not duplicated JSON.
    // Keep the metadata catalog, including formats/flags that have no direct
    // native renderer counterpart, available to the normal attribute API.
    if let Some(object) = json.as_object_mut() {
        for key in ["geometry", "geometry_runs", "instances", "guides"] {
            object.remove(key);
        }
    }
    tx.set_attribute(
        AttrTarget::Document,
        "org.planyrith.skp",
        "decoded-settings",
        Some(Value::Str(serde_json::to_string(&json)?)),
    )?;
    for (i, c) in model.classifications.iter().enumerate() {
        tx.set_attribute(
            AttrTarget::Document,
            "org.planyrith.skp.classifications",
            &format!("{i}:{}", c.name),
            Some(Value::Bytes(c.data.clone())),
        )?;
    }
    for (i, s) in model.styles.iter().enumerate() {
        if let Some(strokes) = &s.strokes {
            let mut refs = Vec::new();
            for bytes in &strokes.images {
                refs.push(Value::Str(tx.insert_image(bytes.clone())?.to_hex()));
            }
            tx.set_attribute(
                AttrTarget::Document,
                "org.planyrith.skp.strokes",
                &i.to_string(),
                Some(Value::Array(refs)),
            )?;
        }
    }
    Ok(())
}
