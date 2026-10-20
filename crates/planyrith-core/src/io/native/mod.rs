//! Implements docs/09-file-formats.md §1–§4 and §7–§8: the `.plyr` ZIP
//! container (`mimetype`, `manifest.json`, `document.msgpack`, `images/*`,
//! `thumbnail.png`, `history.msgpack`, `ext/*`), deterministic writing, part
//! checksums, size-checked reading and format migrations.
//!
//! Draft decisions (docs/09 Decisions, 2026-10-20):
//! - `document.msgpack` is the `Document` type serialized by its derived
//!   `serde` implementation in named-field mode. The index-based
//!   `EntitiesSer` forms of docs/09 §2 are introduced together with
//!   `HalfEdgeMesh::to_indexed`/`from_indexed`; the writer then emits them
//!   and the reader rebuilds the half-edge structure on load.
//! - ZIP metadata is fixed for every write, not only under `--deterministic`:
//!   entry timestamps are the DOS epoch (1980-01-01), the "made by" system is
//!   Unix with mode `0644`, there are no extra fields and the Deflate level is
//!   pinned. The core has no clock, so `deterministic` only forces the manifest
//!   timestamps to [`Manifest::DETERMINISTIC_TIMESTAMP`] and strips journal
//!   timestamps; otherwise the caller supplies `created`/`modified`.
//! - Readers verify `mimetype` (first entry), compare every ZIP header against
//!   the manifest's `sizes` before extracting anything, cap each part at
//!   [`MAX_PART_SIZE`] (the journal at [`MAX_JOURNAL_SIZE`]) and check blake3
//!   checksums where the manifest lists one. Unknown parts (anything that is
//!   not a reserved name or a well-formed `images/<hash>.<ext>`) are returned
//!   in [`LoadedDocument::extra_parts`] and written back from
//!   [`WriteOptions::extra_parts`], so they survive a round trip unchanged.
//! - A file whose `format_version` is newer than this reader but whose
//!   `min_reader_version` is not is decoded ignoring unknown fields (forward
//!   compatibility, docs/09 §4); only `min_reader_version > FORMAT_VERSION`
//!   is `E1401`.
//!
//! Owner key: `io-native`.

#![warn(unused_variables, dead_code)]

pub mod manifest;
pub mod migrate;

pub use manifest::Manifest;

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipArchive, ZipWriter};

use crate::diagnostics::Warning;
use crate::document::{Document, FORMAT_VERSION, ImageData, ImageFormat, ImageRef, ImageStore};
use crate::io::storage::{Storage, StorageKey};
use crate::io::{IoError, Progress};

/// Value of the `mimetype` entry (docs/09 §1).
pub const MIMETYPE: &str = "application/x-planyrith";

/// Container part names in the mandated write order (docs/09 §1).
pub mod parts {
    /// First entry, stored uncompressed for sniffing.
    pub const MIMETYPE: &str = "mimetype";
    /// Manifest.
    pub const MANIFEST: &str = "manifest.json";
    /// Document body.
    pub const DOCUMENT: &str = "document.msgpack";
    /// Image directory prefix (`images/<blake3-hex>.<ext>`).
    pub const IMAGES_DIR: &str = "images/";
    /// Optional preview.
    pub const THUMBNAIL: &str = "thumbnail.png";
    /// Optional command journal.
    pub const HISTORY: &str = "history.msgpack";
    /// Plugin payload prefix (`ext/<plugin-id>/…`).
    pub const EXT_DIR: &str = "ext/";
}

/// Maximum size of one part (ZIP64 enabled; docs/09 §1).
pub const MAX_PART_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// Default cap of the history journal (docs/09 §3).
pub const MAX_JOURNAL_SIZE: u64 = 64 * 1024 * 1024;

/// Cap of `manifest.json`; it is read before anything else is trusted.
pub const MAX_MANIFEST_SIZE: u64 = 16 * 1024 * 1024;

/// Deflate level used for compressed parts (pinned for byte stability).
const DEFLATE_LEVEL: i64 = 6;

/// What a container holds, by extension (docs/09 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerKind {
    /// `.plyr` — a document.
    Document,
    /// `.plyrc` — one definition plus referenced materials.
    Component,
    /// `.plyrm` — material library.
    MaterialLibrary,
    /// `.plyrs` — style library.
    StyleLibrary,
    /// `.plyrt` — template (a document flagged `is_template`).
    Template,
}

impl ContainerKind {
    /// File extension without the dot.
    pub const fn extension(self) -> &'static str {
        match self {
            ContainerKind::Document => "plyr",
            ContainerKind::Component => "plyrc",
            ContainerKind::MaterialLibrary => "plyrm",
            ContainerKind::StyleLibrary => "plyrs",
            ContainerKind::Template => "plyrt",
        }
    }

    /// Kind from a file extension (case-insensitive).
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "plyr" => ContainerKind::Document,
            "plyrc" => ContainerKind::Component,
            "plyrm" => ContainerKind::MaterialLibrary,
            "plyrs" => ContainerKind::StyleLibrary,
            "plyrt" => ContainerKind::Template,
            _ => return None,
        })
    }
}

/// Writer options (docs/09 §1 rules, §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOptions {
    /// Fixed manifest timestamps and no journal timestamps so identical
    /// documents give identical bytes (`--deterministic`, docs/08 §10). ZIP
    /// metadata is fixed regardless (see the module documentation).
    pub deterministic: bool,
    /// Deflate parts (`false` = Stored). Images and the thumbnail are always
    /// Stored because they are already compressed.
    pub compress: bool,
    /// PNG bytes for `thumbnail.png` (`None` = omit).
    pub thumbnail: Option<Vec<u8>>,
    /// Write `history.msgpack` from the engine journal.
    pub include_history: bool,
    /// `generator` string for the manifest (`planyrith <version>`).
    pub generator: String,
    /// RFC 3339 creation time for the manifest, supplied by the caller (the
    /// core has no clock). Ignored under `deterministic`.
    pub created: String,
    /// RFC 3339 modification time for the manifest, supplied by the caller.
    /// Ignored under `deterministic`.
    pub modified: String,
    /// Flag the manifest as a `.plyrt` template (docs/09 §1).
    pub is_template: bool,
    /// Opaque parts written after the known ones: `ext/<plugin-id>/…`
    /// payloads first, then any other preserved part, each group in name
    /// order. Reserved names are rejected (`E1402`).
    pub extra_parts: BTreeMap<String, Vec<u8>>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            deterministic: false,
            compress: true,
            thumbnail: None,
            include_history: true,
            generator: format!("planyrith {}", env!("CARGO_PKG_VERSION")),
            created: Manifest::DETERMINISTIC_TIMESTAMP.to_string(),
            modified: Manifest::DETERMINISTIC_TIMESTAMP.to_string(),
            is_template: false,
            extra_parts: BTreeMap::new(),
        }
    }
}

impl WriteOptions {
    /// Carries over what a re-save must keep from the file it was loaded from:
    /// `created`, the template flag, the preserved unknown parts and, when no
    /// new thumbnail is set, the previous thumbnail.
    pub fn carry_over(mut self, loaded: &LoadedDocument) -> Self {
        self.created = loaded.manifest.created.clone();
        self.is_template = loaded.manifest.is_template;
        self.extra_parts = loaded.extra_parts.clone();
        if self.thumbnail.is_none() {
            self.thumbnail = loaded.thumbnail.clone();
        }
        self
    }
}

/// A journal row in `history.msgpack` (docs/09 §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Command name.
    pub command: String,
    /// Params as executed.
    pub params: serde_json::Value,
    /// Undo group label, if grouped.
    pub group: Option<String>,
    /// RFC 3339 timestamp, absent under `--deterministic`.
    pub timestamp: Option<String>,
    /// Pids the command created, so a later session can resolve `last`
    /// (docs/08 §2). Absent in older journals.
    #[serde(default)]
    pub created: Vec<crate::ids::Pid>,
}

/// Result of reading a container.
#[derive(Debug)]
pub struct LoadedDocument {
    /// The document (images loaded into `Document::images`).
    pub document: Document,
    /// The manifest.
    pub manifest: Manifest,
    /// Journal rows, if the part exists.
    pub history: Vec<HistoryEntry>,
    /// `thumbnail.png` bytes, if the part exists.
    pub thumbnail: Option<Vec<u8>>,
    /// Parts this reader does not interpret (`ext/*` and unknown names),
    /// preserved verbatim for the next save (docs/09 §1).
    pub extra_parts: BTreeMap<String, Vec<u8>>,
    /// Non-fatal notes (unknown parts, missing planes recomputed, migrations).
    pub warnings: Vec<Warning>,
}

/// Which reserved role a part name plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Mimetype,
    Manifest,
    Document,
    Image(ImageRef, ImageFormat),
    Thumbnail,
    History,
    /// A ZIP directory entry (name ends with `/`).
    Directory,
    /// Anything else: preserved verbatim.
    Other,
}

fn classify(name: &str) -> PartKind {
    match name {
        parts::MIMETYPE => PartKind::Mimetype,
        parts::MANIFEST => PartKind::Manifest,
        parts::DOCUMENT => PartKind::Document,
        parts::THUMBNAIL => PartKind::Thumbnail,
        parts::HISTORY => PartKind::History,
        _ if name.ends_with('/') => PartKind::Directory,
        _ => match parse_image_part_name(name) {
            Some((hash, format)) => PartKind::Image(hash, format),
            None => PartKind::Other,
        },
    }
}

/// `images/<blake3-hex>.<ext>` for an image (docs/09 §1).
fn image_part_name(hash: ImageRef, format: ImageFormat) -> String {
    format!(
        "{}{}.{}",
        parts::IMAGES_DIR,
        hash.to_hex(),
        format.extension()
    )
}

/// Inverse of [`image_part_name`]; `None` for anything that is not a
/// well-formed image part name.
fn parse_image_part_name(name: &str) -> Option<(ImageRef, ImageFormat)> {
    let rest = name.strip_prefix(parts::IMAGES_DIR)?;
    let (hex, ext) = rest.rsplit_once('.')?;
    let format = match ext {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::WebP,
        _ => return None,
    };
    Some((ImageRef(decode_hex_32(hex)?), format))
}

fn decode_hex_32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// Rejects extra-part names that are reserved, unsafe or not plain relative
/// paths.
fn validate_extra_part_name(name: &str) -> Result<(), IoError> {
    let reserved = !matches!(classify(name), PartKind::Other);
    let malformed = name.is_empty()
        || name.contains('\\')
        || name.contains('\0')
        || name
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if reserved || malformed {
        return Err(IoError::corrupt(
            name,
            "invalid or reserved name for an extra part",
        ));
    }
    Ok(())
}

fn zip_error(part: &str, err: &zip::result::ZipError) -> IoError {
    match err {
        zip::result::ZipError::FileNotFound => IoError::corrupt(part, "missing part"),
        other => IoError::corrupt(part, other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Appends one entry with fixed metadata (see the module documentation).
fn put_entry<W: Write + std::io::Seek>(
    writer: &mut ZipWriter<W>,
    name: &str,
    bytes: &[u8],
    method: CompressionMethod,
) -> Result<(), IoError> {
    let level = if method == CompressionMethod::Deflated {
        Some(DEFLATE_LEVEL)
    } else {
        None
    };
    let options = SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(level)
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644)
        .system(System::Unix)
        .large_file(bytes.len() as u64 >= u64::from(u32::MAX));
    writer
        .start_file(name, options)
        .map_err(|e| zip_error(name, &e))?;
    writer
        .write_all(bytes)
        .map_err(|e| IoError::corrupt(name, format!("write failed: {e}")))
}

fn check_part_size(name: &str, len: u64, limit: u64) -> Result<(), IoError> {
    if len > limit {
        return Err(IoError::LimitExceeded(format!(
            "{name}: {len} bytes exceeds the limit of {limit} bytes"
        )));
    }
    Ok(())
}

/// Serializes a document into `.plyr` bytes in the mandated part order with
/// checksums (docs/09 §1).
pub fn write_document(
    doc: &Document,
    history: &[HistoryEntry],
    opts: &WriteOptions,
    progress: &dyn Progress,
) -> Result<Vec<u8>, IoError> {
    progress.check_cancelled()?;
    progress.report(0.0, "serializing document");

    // Writers always write the latest version (docs/09 §4).
    let stamped;
    let doc = if doc.info.format_version == FORMAT_VERSION {
        doc
    } else {
        let mut d = doc.clone();
        d.info.format_version = FORMAT_VERSION;
        stamped = d;
        &stamped
    };

    let payload_method = if opts.compress {
        CompressionMethod::Deflated
    } else {
        CompressionMethod::Stored
    };

    // Payload parts in write order (docs/09 §1): document, images, thumbnail,
    // history, ext/*, then any other preserved part.
    let mut entries: Vec<(String, Vec<u8>, CompressionMethod)> = Vec::new();

    let document_bytes = rmp_serde::to_vec_named(doc)
        .map_err(|e| IoError::corrupt(parts::DOCUMENT, format!("serialization failed: {e}")))?;
    entries.push((parts::DOCUMENT.to_string(), document_bytes, payload_method));

    for (hash, image) in doc.images.iter() {
        entries.push((
            image_part_name(hash, image.format),
            image.bytes.clone(),
            CompressionMethod::Stored,
        ));
    }

    if let Some(thumbnail) = &opts.thumbnail {
        entries.push((
            parts::THUMBNAIL.to_string(),
            thumbnail.clone(),
            CompressionMethod::Stored,
        ));
    }

    if opts.include_history && !history.is_empty() {
        let rows: Vec<HistoryEntry> = history
            .iter()
            .cloned()
            .map(|mut row| {
                if opts.deterministic {
                    row.timestamp = None;
                }
                row
            })
            .collect();
        let bytes = rmp_serde::to_vec_named(&rows)
            .map_err(|e| IoError::corrupt(parts::HISTORY, format!("serialization failed: {e}")))?;
        check_part_size(parts::HISTORY, bytes.len() as u64, MAX_JOURNAL_SIZE)?;
        entries.push((parts::HISTORY.to_string(), bytes, payload_method));
    }

    let (ext, other): (Vec<_>, Vec<_>) = opts
        .extra_parts
        .iter()
        .partition(|(name, _)| name.starts_with(parts::EXT_DIR));
    for (name, bytes) in ext.into_iter().chain(other) {
        validate_extra_part_name(name)?;
        entries.push((name.clone(), bytes.clone(), payload_method));
    }

    for (name, bytes, _) in &entries {
        check_part_size(name, bytes.len() as u64, MAX_PART_SIZE)?;
    }

    let mut manifest = Manifest::new(&opts.generator, &doc.info.title);
    manifest.author = doc.info.author.clone();
    manifest.description = doc.info.description.clone();
    manifest.is_template = opts.is_template;
    if opts.deterministic {
        manifest.created = Manifest::DETERMINISTIC_TIMESTAMP.to_string();
        manifest.modified = Manifest::DETERMINISTIC_TIMESTAMP.to_string();
    } else {
        manifest.created = opts.created.clone();
        manifest.modified = opts.modified.clone();
    }
    manifest.add_unchecked_part(parts::MIMETYPE);
    manifest.add_unchecked_part(parts::MANIFEST);
    for (name, bytes, _) in &entries {
        manifest.add_part(name.clone(), bytes);
    }
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| IoError::corrupt(parts::MANIFEST, format!("serialization failed: {e}")))?;
    manifest_bytes.push(b'\n');

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    put_entry(
        &mut writer,
        parts::MIMETYPE,
        MIMETYPE.as_bytes(),
        CompressionMethod::Stored,
    )?;
    put_entry(
        &mut writer,
        parts::MANIFEST,
        &manifest_bytes,
        payload_method,
    )?;
    let total = entries.len().max(1) as f32;
    for (i, (name, bytes, method)) in entries.iter().enumerate() {
        progress.check_cancelled()?;
        progress.report(0.1 + 0.9 * (i as f32 / total), name);
        put_entry(&mut writer, name, bytes, *method)?;
    }
    let cursor = writer.finish().map_err(|e| zip_error("container", &e))?;
    progress.report(1.0, "done");
    Ok(cursor.into_inner())
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

type Archive<'a> = ZipArchive<Cursor<&'a [u8]>>;

fn open_archive(bytes: &[u8]) -> Result<Archive<'_>, IoError> {
    ZipArchive::new(Cursor::new(bytes)).map_err(|e| zip_error("container", &e))
}

fn entry_name(archive: &Archive<'_>, index: usize) -> Result<String, IoError> {
    match archive.name_for_index(index) {
        Some(Ok(name)) => Ok(name.into_owned()),
        Some(Err(e)) => Err(zip_error("container", &e)),
        None => Err(IoError::corrupt(
            "container",
            format!("entry {index} is missing from the central directory"),
        )),
    }
}

/// Reads one part with its uncompressed size bounded by `limit`: the ZIP
/// header is checked first, then the stream is read through a `take` so that
/// a lying header cannot make the reader allocate or inflate past the limit.
fn read_part(archive: &mut Archive<'_>, name: &str, limit: u64) -> Result<Vec<u8>, IoError> {
    let file = archive.by_name(name).map_err(|e| zip_error(name, &e))?;
    let declared = file.size();
    check_part_size(name, declared, limit)?;
    let capacity = usize::try_from(declared.min(16 * 1024 * 1024)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| IoError::corrupt(name, format!("unreadable part: {e}")))?;
    check_part_size(name, bytes.len() as u64, limit)?;
    if bytes.len() as u64 != declared {
        return Err(IoError::corrupt(
            name,
            format!(
                "ZIP header declares {declared} bytes but the part holds {}",
                bytes.len()
            ),
        ));
    }
    Ok(bytes)
}

/// [`read_part`] plus the manifest checksum (when listed).
fn read_checked_part(
    archive: &mut Archive<'_>,
    manifest: &Manifest,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, IoError> {
    let bytes = read_part(archive, name, limit)?;
    if !manifest.checksum_matches(name, &bytes) {
        return Err(IoError::corrupt(name, "blake3 checksum mismatch"));
    }
    Ok(bytes)
}

fn check_mimetype(archive: &mut Archive<'_>) -> Result<(), IoError> {
    if archive.is_empty() {
        return Err(IoError::corrupt(parts::MIMETYPE, "empty archive"));
    }
    let first = entry_name(archive, 0)?;
    if first != parts::MIMETYPE {
        return Err(IoError::corrupt(
            parts::MIMETYPE,
            format!("first entry is `{first}`, expected `mimetype`"),
        ));
    }
    let bytes = read_part(archive, parts::MIMETYPE, 256)?;
    if bytes != MIMETYPE.as_bytes() {
        return Err(IoError::corrupt(
            parts::MIMETYPE,
            "unexpected media type; not a Planyrith container",
        ));
    }
    Ok(())
}

fn read_manifest_from(archive: &mut Archive<'_>) -> Result<Manifest, IoError> {
    let bytes = read_part(archive, parts::MANIFEST, MAX_MANIFEST_SIZE)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| IoError::corrupt(parts::MANIFEST, format!("invalid JSON: {e}")))
}

/// Compares every ZIP header against the manifest **before** extraction
/// (docs/09 §8): sizes above [`MAX_PART_SIZE`] and sizes that disagree with
/// the manifest are refused.
fn check_sizes(archive: &Archive<'_>, manifest: &Manifest) -> Result<(), IoError> {
    for index in 0..archive.len() {
        let entry = archive
            .by_index_data(index)
            .map_err(|e| zip_error("container", &e))?;
        let name = entry
            .name()
            .map_err(|e| zip_error("container", &e))?
            .into_owned();
        let size = entry.size();
        check_part_size(&name, size, MAX_PART_SIZE)?;
        if let Some(declared) = manifest.declared_size(&name)
            && declared != size
        {
            return Err(IoError::corrupt(
                name,
                format!("ZIP header declares {size} bytes, manifest {declared} bytes"),
            ));
        }
    }
    Ok(())
}

/// Decodes `document.msgpack` written at `format_version` into the current
/// `Document`, migrating through the intermediate tree when needed
/// (docs/09 §4).
fn decode_document(bytes: &[u8], format_version: u32) -> Result<Document, IoError> {
    let undecodable = |e: rmp_serde::decode::Error| {
        IoError::corrupt(parts::DOCUMENT, format!("undecodable document: {e}"))
    };
    let mut document: Document = if format_version >= FORMAT_VERSION {
        rmp_serde::from_slice(bytes).map_err(undecodable)?
    } else {
        let tree: serde_json::Value = rmp_serde::from_slice(bytes).map_err(undecodable)?;
        let tree = migrate::migrate(tree, format_version)?;
        serde_json::from_value(tree).map_err(|e| {
            IoError::corrupt(
                parts::DOCUMENT,
                format!("migrated document is invalid: {e}"),
            )
        })?
    };
    document.info.format_version = FORMAT_VERSION;
    Ok(document)
}

/// Decodes the header of an image part and enforces the pixel cap
/// (`ImageStore::MAX_SIDE_PX`, docs/04 §4, docs/09 §8).
fn image_dimensions(name: &str, bytes: &[u8], format: ImageFormat) -> Result<(u32, u32), IoError> {
    let decoder_format = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::WebP => image::ImageFormat::WebP,
    };
    let mut reader = image::ImageReader::new(Cursor::new(bytes));
    reader.set_format(decoder_format);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| IoError::corrupt(name, format!("undecodable image header: {e}")))?;
    if width > ImageStore::MAX_SIDE_PX || height > ImageStore::MAX_SIDE_PX {
        return Err(IoError::LimitExceeded(format!(
            "{name}: {width}x{height} px exceeds {} px per side",
            ImageStore::MAX_SIDE_PX
        )));
    }
    Ok((width, height))
}

/// Parses a whole `.plyr` from memory: verifies `mimetype`, reads the
/// manifest, size-checks every entry, migrates `document.msgpack` to the
/// current version and loads images, the thumbnail, the journal and any
/// unknown part (docs/09 §1–§2, §4, §8).
pub fn read_document(bytes: &[u8], progress: &dyn Progress) -> Result<LoadedDocument, IoError> {
    progress.check_cancelled()?;
    progress.report(0.0, "opening container");
    let mut archive = open_archive(bytes)?;
    check_mimetype(&mut archive)?;
    let manifest = read_manifest_from(&mut archive)?;
    if !manifest.is_readable() {
        return Err(IoError::UnsupportedVersion {
            found: manifest.min_reader_version,
            max: FORMAT_VERSION,
        });
    }
    if manifest.format_version < migrate::MIN_SUPPORTED_VERSION {
        migrate::check_version(manifest.format_version)?;
    }
    check_sizes(&archive, &manifest)?;

    progress.report(0.2, parts::DOCUMENT);
    let document_bytes =
        read_checked_part(&mut archive, &manifest, parts::DOCUMENT, MAX_PART_SIZE)?;
    let mut document = decode_document(&document_bytes, manifest.format_version)?;

    let names = (0..archive.len())
        .map(|i| entry_name(&archive, i))
        .collect::<Result<Vec<_>, _>>()?;
    let mut history = Vec::new();
    let mut thumbnail = None;
    let mut extra_parts = BTreeMap::new();
    let total = names.len().max(1) as f32;
    for (i, name) in names.iter().enumerate() {
        progress.check_cancelled()?;
        progress.report(0.3 + 0.7 * (i as f32 / total), name);
        match classify(name) {
            PartKind::Mimetype | PartKind::Manifest | PartKind::Document | PartKind::Directory => {}
            PartKind::Image(hash, format) => {
                let bytes = read_checked_part(&mut archive, &manifest, name, MAX_PART_SIZE)?;
                if ImageRef::from_bytes(&bytes) != hash {
                    return Err(IoError::corrupt(
                        name,
                        "content hash differs from the part name",
                    ));
                }
                let (width, height) = image_dimensions(name, &bytes, format)?;
                document.images.insert(ImageData {
                    bytes,
                    format,
                    width,
                    height,
                });
            }
            PartKind::Thumbnail => {
                thumbnail = Some(read_checked_part(
                    &mut archive,
                    &manifest,
                    name,
                    MAX_PART_SIZE,
                )?);
            }
            PartKind::History => {
                let bytes = read_checked_part(&mut archive, &manifest, name, MAX_JOURNAL_SIZE)?;
                history = rmp_serde::from_slice(&bytes).map_err(|e| {
                    IoError::corrupt(parts::HISTORY, format!("undecodable journal: {e}"))
                })?;
            }
            PartKind::Other => {
                let bytes = read_checked_part(&mut archive, &manifest, name, MAX_PART_SIZE)?;
                extra_parts.insert(name.clone(), bytes);
            }
        }
    }
    progress.report(1.0, "done");
    Ok(LoadedDocument {
        document,
        manifest,
        history,
        thumbnail,
        extra_parts,
        warnings: Vec::new(),
    })
}

/// Reads only `mimetype` and `manifest.json` (fast `info`, recent-files
/// panel). Does not check the version: a newer file still reports its
/// manifest.
pub fn read_manifest(bytes: &[u8]) -> Result<Manifest, IoError> {
    let mut archive = open_archive(bytes)?;
    check_mimetype(&mut archive)?;
    read_manifest_from(&mut archive)
}

/// Verifies every part's blake3 against the manifest; returns the names of
/// mismatching or unreadable parts in name order (docs/09 §1 `checksums`,
/// `planyrith validate`).
pub fn verify_checksums(bytes: &[u8]) -> Result<Vec<String>, IoError> {
    let mut archive = open_archive(bytes)?;
    check_mimetype(&mut archive)?;
    let manifest = read_manifest_from(&mut archive)?;
    let mut mismatching = Vec::new();
    for name in manifest.checksums.keys() {
        match read_part(&mut archive, name, MAX_PART_SIZE) {
            Ok(part) if manifest.checksum_matches(name, &part) => {}
            Ok(_) | Err(_) => mismatching.push(name.clone()),
        }
    }
    Ok(mismatching)
}

/// Rebuilds a document from `document.msgpack` alone (`repair --from-parts`,
/// docs/09 §8). The version is taken from `info.format_version` inside the
/// part; images are not available without their parts.
pub fn read_document_part(document_msgpack: &[u8]) -> Result<Document, IoError> {
    #[derive(Deserialize)]
    struct Header {
        info: HeaderInfo,
    }
    #[derive(Deserialize)]
    struct HeaderInfo {
        format_version: u32,
    }
    let header: Header = rmp_serde::from_slice(document_msgpack).map_err(|e| {
        IoError::corrupt(
            parts::DOCUMENT,
            format!("cannot read the document header: {e}"),
        )
    })?;
    let version = header.info.format_version;
    if version > FORMAT_VERSION {
        // Without a manifest there is no `min_reader_version`; be strict.
        migrate::check_version(version)?;
    }
    decode_document(document_msgpack, version)
}

/// Opens a document from storage (docs/11 §1: commands take `&dyn Storage`).
pub fn open(
    storage: &dyn Storage,
    key: &StorageKey,
    progress: &dyn Progress,
) -> Result<LoadedDocument, IoError> {
    let bytes = storage.read(key)?;
    read_document(&bytes, progress)
}

/// Saves a document to storage atomically.
pub fn save(
    storage: &dyn Storage,
    key: &StorageKey,
    doc: &Document,
    history: &[HistoryEntry],
    opts: &WriteOptions,
    progress: &dyn Progress,
) -> Result<(), IoError> {
    let bytes = write_document(doc, history, opts, progress)?;
    storage.write(key, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{ComponentDefinition, Value};
    use crate::io::storage::MemoryStorage;
    use crate::io::{CancelToken, NoProgress};

    fn png_bytes(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
        cursor.into_inner()
    }

    /// A document built through the public API plus the crate-internal
    /// `add_definition` (the only implemented definition mutator so far).
    fn fixture() -> (Document, Vec<HistoryEntry>) {
        let mut doc = Document::new();
        doc.info.title = "Fixture house".to_string();
        doc.info.author = "Planyrith tests".to_string();
        doc.info.description = "Round-trip fixture".to_string();
        doc.attrs
            .set("planyrith", "fixture", Value::Str("yes".to_string()));
        doc.attrs.set("planyrith", "height", Value::Float(2700.5));
        doc.add_definition(ComponentDefinition::new(
            uuid::Uuid::from_u128(0x1234),
            "Door",
        ));
        doc.add_definition(ComponentDefinition::new(
            uuid::Uuid::from_u128(0x5678),
            "Window",
        ));
        let red = png_bytes(2, 2, [255, 0, 0, 255]);
        let first = doc.images.insert(ImageData {
            bytes: red.clone(),
            format: ImageFormat::Png,
            width: 2,
            height: 2,
        });
        // Same bytes again: deduplicated by content hash.
        let again = doc.images.insert(ImageData {
            bytes: red,
            format: ImageFormat::Png,
            width: 2,
            height: 2,
        });
        assert_eq!(first, again);
        doc.images.insert(ImageData {
            bytes: png_bytes(3, 1, [0, 255, 0, 255]),
            format: ImageFormat::Png,
            width: 3,
            height: 1,
        });
        let history = vec![
            HistoryEntry {
                command: "draw-line".to_string(),
                params: serde_json::json!({ "from": [0.0, 0.0, 0.0], "to": [1000.0, 0.0, 0.0] }),
                group: None,
                timestamp: Some("2026-10-20T10:00:00Z".to_string()),
                created: vec![crate::ids::Pid(1)],
            },
            HistoryEntry {
                command: "push-pull".to_string(),
                params: serde_json::json!({ "face": "pid:7", "distance": 2700 }),
                group: Some("extrude wall".to_string()),
                timestamp: Some("2026-10-20T10:01:00Z".to_string()),
                created: Vec::new(),
            },
        ];
        (doc, history)
    }

    fn doc_json(doc: &Document) -> serde_json::Value {
        serde_json::to_value(doc).unwrap()
    }

    fn deterministic_options() -> WriteOptions {
        WriteOptions {
            deterministic: true,
            thumbnail: Some(png_bytes(4, 4, [10, 20, 30, 255])),
            ..WriteOptions::default()
        }
    }

    fn entry_names(bytes: &[u8]) -> Vec<String> {
        let archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..archive.len())
            .map(|i| archive.name_for_index(i).unwrap().unwrap().into_owned())
            .collect()
    }

    /// Re-zips `bytes`, letting `edit` replace (or, with `None`, drop) each
    /// entry's content. Used to forge manifests and tamper with parts.
    fn repack(bytes: &[u8], mut edit: impl FnMut(&str, Vec<u8>) -> Option<Vec<u8>>) -> Vec<u8> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let name = file.name().unwrap().into_owned();
            let method = file.compression();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            drop(file);
            if let Some(data) = edit(&name, data) {
                writer
                    .start_file(
                        &name,
                        SimpleFileOptions::default().compression_method(method),
                    )
                    .unwrap();
                writer.write_all(&data).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn with_manifest(bytes: &[u8], edit: impl Fn(&mut serde_json::Value)) -> Vec<u8> {
        repack(bytes, |name, data| {
            if name == parts::MANIFEST {
                let mut json: serde_json::Value = serde_json::from_slice(&data).unwrap();
                edit(&mut json);
                Some(serde_json::to_vec(&json).unwrap())
            } else {
                Some(data)
            }
        })
    }

    #[test]
    fn container_kinds_by_extension() {
        assert_eq!(
            ContainerKind::from_extension("PLYR"),
            Some(ContainerKind::Document)
        );
        assert_eq!(
            ContainerKind::from_extension("plyrc"),
            Some(ContainerKind::Component)
        );
        assert_eq!(ContainerKind::from_extension("zip"), None);
        assert_eq!(ContainerKind::Template.extension(), "plyrt");
    }

    #[test]
    fn default_write_options_name_the_generator() {
        let o = WriteOptions::default();
        assert!(o.generator.starts_with("planyrith "));
        assert!(o.compress && o.include_history && !o.deterministic);
        assert_eq!(o.created, Manifest::DETERMINISTIC_TIMESTAMP);
        assert!(o.extra_parts.is_empty() && !o.is_template);
    }

    #[test]
    fn image_part_names_round_trip() {
        let hash = ImageRef::from_bytes(b"pixels");
        let name = image_part_name(hash, ImageFormat::Jpeg);
        assert!(name.starts_with("images/") && name.ends_with(".jpg"));
        assert_eq!(
            parse_image_part_name(&name),
            Some((hash, ImageFormat::Jpeg))
        );
        assert_eq!(parse_image_part_name("images/short.png"), None);
        assert_eq!(parse_image_part_name("images/"), None);
        assert_eq!(
            parse_image_part_name(&format!("images/{}.avif", hash.to_hex())),
            None
        );
        assert_eq!(classify("images/notes.txt"), PartKind::Other);
        assert_eq!(classify("ext/plugin/"), PartKind::Directory);
    }

    #[test]
    fn round_trip_preserves_document_images_and_history() {
        let (doc, history) = fixture();
        let opts = deterministic_options();
        let bytes = write_document(&doc, &history, &opts, &NoProgress).unwrap();
        let loaded = read_document(&bytes, &NoProgress).unwrap();

        assert_eq!(doc_json(&loaded.document), doc_json(&doc));
        assert_eq!(loaded.document.images, doc.images);
        assert_eq!(loaded.document.images.len(), 2);
        assert_eq!(loaded.thumbnail, opts.thumbnail);
        assert!(loaded.extra_parts.is_empty());
        assert!(loaded.warnings.is_empty());

        // Deterministic writes strip journal timestamps but keep the rows.
        assert_eq!(loaded.history.len(), 2);
        assert_eq!(loaded.history[0].command, "draw-line");
        assert_eq!(loaded.history[0].params, history[0].params);
        assert_eq!(loaded.history[1].group.as_deref(), Some("extrude wall"));
        assert!(loaded.history.iter().all(|h| h.timestamp.is_none()));

        let m = &loaded.manifest;
        assert_eq!(m.format_version, FORMAT_VERSION);
        assert_eq!(m.min_reader_version, FORMAT_VERSION);
        assert_eq!(m.title, "Fixture house");
        assert_eq!(m.author, "Planyrith tests");
        assert_eq!(m.created, Manifest::DETERMINISTIC_TIMESTAMP);
        assert_eq!(m.parts, entry_names(&bytes));
        assert!(!m.checksums.contains_key(parts::MIMETYPE));
        assert!(!m.checksums.contains_key(parts::MANIFEST));
        assert_eq!(m.checksums.len(), m.parts.len() - 2);
        assert_eq!(m.sizes.len(), m.checksums.len());
        assert!(verify_checksums(&bytes).unwrap().is_empty());
    }

    #[test]
    fn non_deterministic_writes_keep_caller_timestamps() {
        let (doc, history) = fixture();
        let opts = WriteOptions {
            created: "2026-10-19T08:00:00Z".to_string(),
            modified: "2026-10-20T09:30:00Z".to_string(),
            ..WriteOptions::default()
        };
        let bytes = write_document(&doc, &history, &opts, &NoProgress).unwrap();
        let loaded = read_document(&bytes, &NoProgress).unwrap();
        assert_eq!(loaded.manifest.created, "2026-10-19T08:00:00Z");
        assert_eq!(loaded.manifest.modified, "2026-10-20T09:30:00Z");
        assert_eq!(loaded.history[0].timestamp, history[0].timestamp);
    }

    #[test]
    fn parts_follow_the_mandated_order_and_mimetype_is_stored_first() {
        let (doc, history) = fixture();
        let opts = WriteOptions {
            extra_parts: [
                ("zzz/other.bin".to_string(), vec![1, 2, 3]),
                ("ext/com.example.plugin/data.bin".to_string(), vec![9]),
                ("aaa/early.bin".to_string(), vec![4]),
            ]
            .into_iter()
            .collect(),
            ..deterministic_options()
        };
        let bytes = write_document(&doc, &history, &opts, &NoProgress).unwrap();
        let names = entry_names(&bytes);
        assert_eq!(names[0], "mimetype");
        assert_eq!(names[1], "manifest.json");
        assert_eq!(names[2], "document.msgpack");
        assert!(names[3].starts_with("images/") && names[4].starts_with("images/"));
        assert!(names[3] < names[4]);
        assert_eq!(names[5], "thumbnail.png");
        assert_eq!(names[6], "history.msgpack");
        assert_eq!(names[7], "ext/com.example.plugin/data.bin");
        assert_eq!(names[8], "aaa/early.bin");
        assert_eq!(names[9], "zzz/other.bin");
        assert_eq!(names.len(), 10);

        let mut archive = ZipArchive::new(Cursor::new(&bytes[..])).unwrap();
        let first = archive.by_index(0).unwrap();
        assert_eq!(first.compression(), CompressionMethod::Stored);
        assert_eq!(first.size(), MIMETYPE.len() as u64);
        drop(first);
        for i in 0..archive.len() {
            let entry = archive.by_index(i).unwrap();
            let dt = entry.last_modified().unwrap();
            assert_eq!((dt.year(), dt.month(), dt.day()), (1980, 1, 1));
            assert!(
                entry.extra_data_fields().next().is_none(),
                "no extra fields"
            );
            assert_eq!(entry.system(), System::Unix);
        }
        // The sniffable prefix: local header, name, then the media type.
        assert!(bytes.starts_with(b"PK\x03\x04"));
        assert_eq!(&bytes[30..38], b"mimetype");
        assert_eq!(&bytes[38..38 + MIMETYPE.len()], MIMETYPE.as_bytes());
    }

    #[test]
    fn two_saves_of_the_same_document_are_byte_identical() {
        let (doc, history) = fixture();
        let opts = deterministic_options();
        let a = write_document(&doc, &history, &opts, &NoProgress).unwrap();
        let b = write_document(&doc, &history, &opts, &NoProgress).unwrap();
        assert_eq!(a, b);

        // Also through a load/save cycle and with Stored parts.
        let loaded = read_document(&a, &NoProgress).unwrap();
        let again = write_document(
            &loaded.document,
            &loaded.history,
            &opts.clone().carry_over(&loaded),
            &NoProgress,
        )
        .unwrap();
        assert_eq!(a, again);

        let stored = WriteOptions {
            compress: false,
            ..opts
        };
        let c = write_document(&doc, &history, &stored, &NoProgress).unwrap();
        let d = write_document(&doc, &history, &stored, &NoProgress).unwrap();
        assert_eq!(c, d);
        assert_ne!(a, c);
    }

    #[test]
    fn truncated_archives_give_e1402_without_panicking() {
        let (doc, history) = fixture();
        let bytes = write_document(&doc, &history, &deterministic_options(), &NoProgress).unwrap();
        for cut in [
            0,
            1,
            10,
            40,
            bytes.len() / 3,
            bytes.len() / 2,
            bytes.len() - 1,
        ] {
            let err = read_document(&bytes[..cut], &NoProgress).unwrap_err();
            assert_eq!(err.code(), "E1402", "cut at {cut}: {err}");
        }
        assert_eq!(read_manifest(&[]).unwrap_err().code(), "E1402");
        assert_eq!(
            read_document(b"not a zip at all", &NoProgress)
                .unwrap_err()
                .code(),
            "E1402"
        );
        assert_eq!(read_document_part(b"\xc1").unwrap_err().code(), "E1402");
        assert_eq!(read_document_part(&[]).unwrap_err().code(), "E1402");
    }

    #[test]
    fn corrupted_document_bytes_give_e1402() {
        let (doc, history) = fixture();
        let opts = WriteOptions {
            compress: false,
            ..deterministic_options()
        };
        let bytes = write_document(&doc, &history, &opts, &NoProgress).unwrap();
        // Stored parts are verbatim: find the document payload and flip a byte.
        let payload = rmp_serde::to_vec_named(&doc).unwrap();
        let offset = bytes
            .windows(payload.len())
            .position(|w| w == payload.as_slice())
            .unwrap();
        let mut tampered = bytes.clone();
        tampered[offset + payload.len() / 2] ^= 0xFF;
        let err = read_document(&tampered, &NoProgress).unwrap_err();
        assert_eq!(err.code(), "E1402");
        assert_eq!(
            verify_checksums(&tampered).unwrap(),
            vec![parts::DOCUMENT.to_string()]
        );
    }

    #[test]
    fn tampered_parts_are_reported_by_verify_checksums() {
        let (doc, history) = fixture();
        let bytes = write_document(&doc, &history, &deterministic_options(), &NoProgress).unwrap();
        let tampered = repack(&bytes, |name, mut data| {
            if name == parts::THUMBNAIL {
                data[0] ^= 1;
            }
            Some(data)
        });
        assert_eq!(
            verify_checksums(&tampered).unwrap(),
            vec![parts::THUMBNAIL.to_string()]
        );
        let err = read_document(&tampered, &NoProgress).unwrap_err();
        assert_eq!(err.code(), "E1402");
        assert!(matches!(err, IoError::Corrupt { ref part, .. } if part == parts::THUMBNAIL));

        let missing = repack(&bytes, |name, data| {
            (name != parts::DOCUMENT).then_some(data)
        });
        assert_eq!(
            verify_checksums(&missing).unwrap(),
            vec![parts::DOCUMENT.to_string()]
        );
        assert_eq!(
            read_document(&missing, &NoProgress).unwrap_err().code(),
            "E1402"
        );
    }

    #[test]
    fn sizes_are_checked_against_the_manifest_before_extraction() {
        let (doc, history) = fixture();
        let bytes = write_document(&doc, &history, &deterministic_options(), &NoProgress).unwrap();
        let lying = with_manifest(&bytes, |m| {
            m["sizes"][parts::DOCUMENT] = serde_json::json!(1);
        });
        let err = read_document(&lying, &NoProgress).unwrap_err();
        assert_eq!(err.code(), "E1402");
        assert!(matches!(err, IoError::Corrupt { ref part, .. } if part == parts::DOCUMENT));
    }

    #[test]
    fn newer_min_reader_version_gives_e1401() {
        let (doc, history) = fixture();
        let bytes = write_document(&doc, &history, &deterministic_options(), &NoProgress).unwrap();
        let future = with_manifest(&bytes, |m| {
            m["format_version"] = serde_json::json!(FORMAT_VERSION + 1);
            m["min_reader_version"] = serde_json::json!(FORMAT_VERSION + 1);
        });
        let err = read_document(&future, &NoProgress).unwrap_err();
        assert_eq!(err.code(), "E1401");
        assert_eq!(
            err,
            IoError::UnsupportedVersion {
                found: FORMAT_VERSION + 1,
                max: FORMAT_VERSION
            }
        );
        // `info` still works on a newer file.
        assert_eq!(
            read_manifest(&future).unwrap().format_version,
            FORMAT_VERSION + 1
        );

        // Newer writer, compatible reader: loads, ignoring unknown fields.
        let compatible = with_manifest(&bytes, |m| {
            m["format_version"] = serde_json::json!(FORMAT_VERSION + 1);
        });
        let loaded = read_document(&compatible, &NoProgress).unwrap();
        assert_eq!(loaded.document.info.format_version, FORMAT_VERSION);

        // Pre-0.1 versions are corrupt, not "newer".
        let ancient = with_manifest(&bytes, |m| {
            m["format_version"] = serde_json::json!(0);
        });
        assert_eq!(
            read_document(&ancient, &NoProgress).unwrap_err().code(),
            "E1402"
        );
    }

    #[test]
    fn unknown_parts_survive_a_round_trip() {
        let (doc, history) = fixture();
        let extra: BTreeMap<String, Vec<u8>> = [
            (
                "ext/com.example.plugin/state.bin".to_string(),
                b"opaque plugin bytes".to_vec(),
            ),
            ("future/part.msgpack".to_string(), vec![0x90]),
            ("images/readme.txt".to_string(), b"not an image".to_vec()),
        ]
        .into_iter()
        .collect();
        let opts = WriteOptions {
            extra_parts: extra.clone(),
            ..deterministic_options()
        };
        let first = write_document(&doc, &history, &opts, &NoProgress).unwrap();
        let loaded = read_document(&first, &NoProgress).unwrap();
        assert_eq!(loaded.extra_parts, extra);
        assert_eq!(loaded.document.images.len(), 2, "readme is not an image");

        let resave_opts = WriteOptions {
            extra_parts: BTreeMap::new(),
            ..deterministic_options()
        }
        .carry_over(&loaded);
        assert_eq!(resave_opts.extra_parts, extra);
        let second =
            write_document(&loaded.document, &loaded.history, &resave_opts, &NoProgress).unwrap();
        assert_eq!(first, second);
        let reloaded = read_document(&second, &NoProgress).unwrap();
        assert_eq!(reloaded.extra_parts, extra);
    }

    #[test]
    fn reserved_or_unsafe_extra_part_names_are_rejected() {
        let (doc, history) = fixture();
        for bad in [
            parts::DOCUMENT,
            parts::MANIFEST,
            parts::MIMETYPE,
            parts::THUMBNAIL,
            "../escape.bin",
            "ext//double.bin",
            "/absolute.bin",
            "ext/dir/",
            "",
            "back\\slash.bin",
        ] {
            let opts = WriteOptions {
                extra_parts: [(bad.to_string(), vec![1])].into_iter().collect(),
                ..deterministic_options()
            };
            let err = write_document(&doc, &history, &opts, &NoProgress).unwrap_err();
            assert_eq!(err.code(), "E1402", "{bad:?}");
        }
        let hash = ImageRef::from_bytes(b"x");
        let opts = WriteOptions {
            extra_parts: [(image_part_name(hash, ImageFormat::Png), vec![1])]
                .into_iter()
                .collect(),
            ..deterministic_options()
        };
        assert_eq!(
            write_document(&doc, &history, &opts, &NoProgress)
                .unwrap_err()
                .code(),
            "E1402"
        );
    }

    #[test]
    fn mimetype_must_be_the_first_entry_with_the_right_content() {
        let (doc, history) = fixture();
        let bytes = write_document(&doc, &history, &deterministic_options(), &NoProgress).unwrap();

        let no_mimetype = repack(&bytes, |name, data| {
            (name != parts::MIMETYPE).then_some(data)
        });
        let err = read_document(&no_mimetype, &NoProgress).unwrap_err();
        assert_eq!(err.code(), "E1402");
        assert!(matches!(err, IoError::Corrupt { ref part, .. } if part == parts::MIMETYPE));

        let wrong_type = repack(&bytes, |name, data| {
            Some(if name == parts::MIMETYPE {
                b"application/zip".to_vec()
            } else {
                data
            })
        });
        assert_eq!(
            read_document(&wrong_type, &NoProgress).unwrap_err().code(),
            "E1402"
        );

        let mut plain = ZipWriter::new(Cursor::new(Vec::new()));
        plain
            .start_file("hello.txt", SimpleFileOptions::default())
            .unwrap();
        plain.write_all(b"hi").unwrap();
        let plain = plain.finish().unwrap().into_inner();
        assert_eq!(
            read_document(&plain, &NoProgress).unwrap_err().code(),
            "E1402"
        );
    }

    #[test]
    fn document_part_alone_can_be_recovered() {
        let (doc, _) = fixture();
        let payload = rmp_serde::to_vec_named(&doc).unwrap();
        let recovered = read_document_part(&payload).unwrap();
        assert_eq!(doc_json(&recovered), doc_json(&doc));
        assert!(recovered.images.is_empty());

        let mut newer = doc.clone();
        newer.info.format_version = FORMAT_VERSION + 1;
        let payload = rmp_serde::to_vec_named(&newer).unwrap();
        assert_eq!(read_document_part(&payload).unwrap_err().code(), "E1401");
    }

    #[test]
    fn writer_stamps_the_current_format_version() {
        let (mut doc, history) = fixture();
        doc.info.format_version = 0;
        let bytes = write_document(&doc, &history, &deterministic_options(), &NoProgress).unwrap();
        let loaded = read_document(&bytes, &NoProgress).unwrap();
        assert_eq!(loaded.document.info.format_version, FORMAT_VERSION);
        assert_eq!(loaded.manifest.format_version, FORMAT_VERSION);
    }

    #[test]
    fn history_can_be_omitted_and_the_journal_is_capped() {
        let (doc, history) = fixture();
        let opts = WriteOptions {
            include_history: false,
            ..deterministic_options()
        };
        let bytes = write_document(&doc, &history, &opts, &NoProgress).unwrap();
        assert!(!entry_names(&bytes).iter().any(|n| n == parts::HISTORY));
        assert!(
            read_document(&bytes, &NoProgress)
                .unwrap()
                .history
                .is_empty()
        );

        let huge = with_manifest(&bytes, |m| {
            m["sizes"]["history.msgpack"] = serde_json::json!(MAX_JOURNAL_SIZE + 1);
        });
        // The manifest lists a size for a part that does not exist: fine.
        assert!(read_document(&huge, &NoProgress).is_ok());
    }

    #[test]
    fn cancellation_is_reported_as_e1701() {
        let (doc, history) = fixture();
        let token = CancelToken::new();
        token.cancel();
        assert_eq!(
            write_document(&doc, &history, &deterministic_options(), &token)
                .unwrap_err()
                .code(),
            "E1701"
        );
        let bytes = write_document(&doc, &history, &deterministic_options(), &NoProgress).unwrap();
        assert_eq!(read_document(&bytes, &token).unwrap_err().code(), "E1701");
    }

    #[test]
    fn save_and_open_through_storage() {
        let (doc, history) = fixture();
        let storage = MemoryStorage::new();
        let key = StorageKey::from("house.plyr");
        save(
            &storage,
            &key,
            &doc,
            &history,
            &deterministic_options(),
            &NoProgress,
        )
        .unwrap();
        let loaded = open(&storage, &key, &NoProgress).unwrap();
        assert_eq!(doc_json(&loaded.document), doc_json(&doc));
        assert_eq!(
            open(&storage, &StorageKey::from("missing.plyr"), &NoProgress)
                .unwrap_err()
                .code(),
            "E1403"
        );
        let read_only = MemoryStorage::new().read_only();
        assert_eq!(
            save(
                &read_only,
                &key,
                &doc,
                &history,
                &deterministic_options(),
                &NoProgress
            )
            .unwrap_err()
            .code(),
            "E1501"
        );
    }
}
