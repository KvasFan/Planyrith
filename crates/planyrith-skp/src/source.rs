//! Preserves the complete SKP source through native working copies (docs/09 §5).
use super::Result;
use planyrith_core::{
    document::Document,
    io::{NoProgress, native},
};

const ORIGINAL: &str = "ext/org.planyrith.skp/source.skp";
const BASELINE: &str = "ext/org.planyrith.skp/baseline-v1.blake3";

fn fingerprint(doc: &Document) -> Result<blake3::Hash> {
    // Includes images, Sheets, cameras and metadata; excludes the source itself,
    // session journal and caller timestamps. A generator independent of the
    // application version avoids invalidation solely because that version changed.
    // Undo intentionally does not rewind the PID allocator. Its high-water
    // mark must not turn a fully undone edit into a lossy SKP regeneration.
    // This normalized copy is only hashed; it is never exposed as a document.
    let mut canonical = doc.clone();
    canonical.info.next_pid = planyrith_core::ids::PidAllocator::starting_at(1);
    let bytes = native::write_document(
        &canonical,
        &[],
        &native::WriteOptions {
            deterministic: true,
            include_history: false,
            generator: "planyrith-skp-baseline-1".into(),
            ..Default::default()
        },
        &NoProgress,
    )?;
    Ok(blake3::hash(&bytes))
}

pub(super) fn options(doc: &Document, original: &[u8]) -> Result<native::WriteOptions> {
    Ok(native::WriteOptions {
        deterministic: true,
        include_history: false,
        extra_parts: [
            (ORIGINAL.into(), original.to_vec()),
            (BASELINE.into(), fingerprint(doc)?.as_bytes().to_vec()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    })
}

pub(super) fn version(loaded: &native::LoadedDocument) -> Result<Option<u16>> {
    let Some(original) = loaded.extra_parts.get(ORIGINAL) else {
        return Ok(None);
    };
    if original.len() > planyrith_core::io::adapter::MAX_BYTES {
        return Err("preserved SKP source exceeds 512 MiB".into());
    }
    if openskp::detect_container(original) == openskp::Container::Unknown {
        return Err("invalid preserved SKP source".into());
    }
    let (version, _) = openskp::header_info(original).ok_or("missing SKP source version")?;
    let major = version
        .trim_start_matches('{')
        .split('.')
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("invalid SKP source version")?;
    Ok(Some(
        major
            .checked_add(2000)
            .ok_or("invalid SKP source version")?,
    ))
}

pub(super) fn unchanged(loaded: &native::LoadedDocument) -> Result<Option<Vec<u8>>> {
    let Some(original) = loaded.extra_parts.get(ORIGINAL) else {
        return Ok(None);
    };
    let baseline = loaded
        .extra_parts
        .get(BASELINE)
        .filter(|hash| hash.len() == 32)
        .ok_or("missing or invalid SKP source baseline")?;
    if baseline.as_slice() == fingerprint(&loaded.document)?.as_bytes() {
        Ok(Some(original.clone()))
    } else {
        Ok(None)
    }
}

pub(super) fn matches_container(loaded: &native::LoadedDocument, version: u16) -> bool {
    loaded.extra_parts.get(ORIGINAL).is_some_and(|original| {
        openskp::detect_container(original)
            == if version == 2017 {
                openskp::Container::Carchive2017
            } else {
                openskp::Container::Zip
            }
    })
}

#[cfg(test)]
pub(super) fn original(loaded: &native::LoadedDocument) -> Option<&[u8]> {
    loaded.extra_parts.get(ORIGINAL).map(Vec::as_slice)
}
