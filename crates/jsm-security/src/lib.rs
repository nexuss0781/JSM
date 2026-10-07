//! Safety and integrity primitives shared by the store and fetch pipeline.
use sha2::{Digest, Sha512};
use std::collections::HashSet;
use std::path::{Component, Path};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SafetyError {
    #[error("unsafe archive path: {0}")]
    UnsafePath(String),
    #[error("symlink escapes package root: {0}")]
    SymlinkEscape(String),
    #[error("duplicate or case-colliding archive path: {0}")]
    DuplicatePath(String),
    #[error("device files are not allowed")]
    DeviceFile,
    #[error("invalid sha512 integrity value")]
    InvalidIntegrity,
    #[error("integrity mismatch: expected {expected}, got {actual}")]
    IntegrityMismatch { expected: String, actual: String },
}

/// Validate a tar entry path and return its normalized slash-separated form.
pub fn validate_path(path: &str) -> Result<String, SafetyError> {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') {
        return Err(SafetyError::UnsafePath(path.into()));
    }
    let p = Path::new(path);
    let mut out = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(v) => {
                let s = v
                    .to_str()
                    .ok_or_else(|| SafetyError::UnsafePath(path.into()))?;
                if s.is_empty() {
                    return Err(SafetyError::UnsafePath(path.into()));
                }
                out.push(s);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(SafetyError::UnsafePath(path.into()));
            }
        }
    }
    if out.is_empty() {
        return Err(SafetyError::UnsafePath(path.into()));
    }
    Ok(out.join("/"))
}

/// Validate a symlink target relative to the containing entry.
pub fn validate_symlink(path: &str, target: &str) -> Result<String, SafetyError> {
    if target.starts_with('/') || target.starts_with('\\') {
        return Err(SafetyError::SymlinkEscape(target.into()));
    }
    let base = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    let joined = base.join(target);
    validate_path(&joined.to_string_lossy()).map_err(|_| SafetyError::SymlinkEscape(target.into()))
}

/// Reject duplicate paths, including case collisions (safe on all targets).
pub fn validate_unique_path(seen: &mut HashSet<String>, path: &str) -> Result<(), SafetyError> {
    let normalized = validate_path(path)?;
    let key = normalized.to_lowercase();
    if !seen.insert(key) {
        return Err(SafetyError::DuplicatePath(normalized));
    }
    Ok(())
}

/// SHA-512 digest as lowercase hexadecimal.
pub fn sha512_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha512::digest(bytes))
}

/// Compute an SRI sha512 value (`sha512-<base64>`).
pub fn sha512_sri(bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
    )
}

/// Verify bytes against hexadecimal or SRI sha512 integrity.
pub fn verify_sha512(bytes: &[u8], expected: &str) -> Result<(), SafetyError> {
    let digest = Sha512::digest(bytes);
    verify_sha512_digest(&digest, expected)
}

/// Verify an already-computed 64-byte SHA-512 digest against a hex or SRI value.
pub fn verify_sha512_digest(digest: &[u8], expected: &str) -> Result<(), SafetyError> {
    if digest.len() != 64 {
        return Err(SafetyError::InvalidIntegrity);
    }
    let actual = hex_encode(digest);
    let wanted = expected.strip_prefix("sha512-").unwrap_or(expected);
    let wanted_hex = if wanted.len() == 128 && wanted.bytes().all(|b| b.is_ascii_hexdigit()) {
        wanted.to_ascii_lowercase()
    } else {
        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(wanted)
            .map_err(|_| SafetyError::InvalidIntegrity)?;
        if raw.len() != 64 {
            return Err(SafetyError::InvalidIntegrity);
        }
        hex_encode(&raw)
    };
    if actual != wanted_hex {
        return Err(SafetyError::IntegrityMismatch {
            expected: expected.into(),
            actual,
        });
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
