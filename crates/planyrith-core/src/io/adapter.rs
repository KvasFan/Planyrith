//! Implements docs/09 §6: byte-stream protocol for optional format converters.
//! No converter implementation is linked into the core.

use super::IoError;
use serde::{Deserialize, Serialize};

/// Largest input/output of a converter process (512 MiB).
pub const MAX_BYTES: usize = 512 * 1024 * 1024;
/// Versioned response prefix; followed by a little-endian metadata length.
pub const MAGIC: &[u8] = b"PLYR-ADAPTER-1\n";

/// Converted bytes and explicit notices about data that could not be represented.
#[derive(Debug, Clone)]
pub struct Converted {
    /// Output file contents.
    pub bytes: Vec<u8>,
    /// Compatibility or data-loss notices.
    pub warnings: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Metadata {
    warnings: Vec<String>,
}

/// Encodes a converter's response, including warnings before the file bytes.
pub fn encode(result: &Converted) -> Result<Vec<u8>, IoError> {
    let metadata = serde_json::to_vec(&Metadata {
        warnings: result.warnings.clone(),
    })
    .map_err(|e| IoError::corrupt("adapter metadata", e.to_string()))?;
    if metadata.len() > 1024 * 1024 || result.bytes.len() > MAX_BYTES {
        return Err(IoError::LimitExceeded("converter response".into()));
    }
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&metadata);
    bytes.extend_from_slice(&result.bytes);
    Ok(bytes)
}

/// Checks the protocol framing before exposing a converter's output.
pub fn decode(bytes: &[u8]) -> Result<Converted, IoError> {
    let bad = || IoError::corrupt("adapter", "invalid or truncated response");
    let rest = bytes.strip_prefix(MAGIC).ok_or_else(bad)?;
    let length = u32::from_le_bytes(
        rest.get(..4)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?,
    ) as usize;
    if length > 1024 * 1024 {
        return Err(bad());
    }
    let metadata: Metadata =
        serde_json::from_slice(rest.get(4..4 + length).ok_or_else(bad)?).map_err(|_| bad())?;
    let payload = rest.get(4 + length..).ok_or_else(bad)?;
    if payload.len() > MAX_BYTES {
        return Err(IoError::LimitExceeded("converter output".into()));
    }
    Ok(Converted {
        bytes: payload.to_vec(),
        warnings: metadata.warnings,
    })
}

/// Error used by backends without the optional converter.
pub fn unavailable() -> IoError {
    IoError::UnsupportedFormat("SKP requires the optional planyrith-skp converter next to the application (cargo build -p planyrith-skp), or an absolute PLANYRITH_SKP_ADAPTER path; browser conversion is unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_framing_rejects_damaged_inputs() {
        let converted = Converted {
            bytes: b"data".to_vec(),
            warnings: vec!["notice".into()],
        };
        let bytes = encode(&converted).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.bytes, converted.bytes);
        assert_eq!(decoded.warnings, converted.warnings);
        for cut in 0..bytes.len() - converted.bytes.len() {
            assert!(decode(&bytes[..cut]).is_err());
        }
        assert!(decode(b"not an adapter response").is_err());
    }
}
