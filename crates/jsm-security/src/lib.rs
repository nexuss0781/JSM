//! Safety and integrity primitives shared by the store and fetch pipeline.
use sha2::{Digest, Sha512};
use std::collections::HashSet;
use std::path::{Component, Path};
use thiserror::Error;
use serde::{Deserialize, Serialize};

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

/// Sandbox policy defining what a build process can access.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// Allow network access
    pub allow_network: bool,
    /// Allow reading from paths outside build directory
    pub allow_read_outside: bool,
    /// Allow writing to paths outside build/output directory
    pub allow_write_outside: bool,
    /// Additional allowed read paths
    pub allowed_read_paths: Vec<String>,
    /// Additional allowed write paths
    pub allowed_write_paths: Vec<String>,
    /// Environment variables to allow
    pub allowed_env: Vec<String>,
    /// Maximum memory in MB
    pub max_memory_mb: Option<u64>,
    /// Maximum CPU time in seconds
    pub max_cpu_time_secs: Option<u64>,
}

impl SandboxPolicy {
    /// Strict policy for builds (no network, limited filesystem).
    pub fn strict() -> Self {
        Self {
            allow_network: false,
            allow_read_outside: false,
            allow_write_outside: false,
            allowed_read_paths: vec![],
            allowed_write_paths: vec![],
            allowed_env: vec![],
            max_memory_mb: Some(4096),
            max_cpu_time_secs: Some(300),
        }
    }
    
    /// Permissive policy for development.
    pub fn permissive() -> Self {
        Self {
            allow_network: true,
            allow_read_outside: true,
            allow_write_outside: true,
            allowed_read_paths: vec![],
            allowed_write_paths: vec![],
            allowed_env: vec![],
            max_memory_mb: None,
            max_cpu_time_secs: None,
        }
    }
}

/// Sandbox execution result.
#[derive(Debug, Clone)]
pub struct SandboxResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Sandbox for executing build commands with isolation.
pub struct Sandbox {
    policy: SandboxPolicy,
}

impl Sandbox {
    pub fn new(policy: &SandboxPolicy) -> Result<Self, SandboxError> {
        Ok(Self { policy: policy.clone() })
    }
    
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }
    
    /// Execute a command with sandboxing.
    pub async fn execute(
        &self,
        working_dir: &Path,
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
        timeout: std::time::Duration,
    ) -> Result<SandboxResult, SandboxError> {
        // For now, execute without actual sandboxing (platform-specific impl later)
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .current_dir(working_dir)
            .envs(env)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        
        // Apply timeout
        let output = tokio::time::timeout(
            timeout,
            cmd.output()
        ).await
            .map_err(|_| SandboxError::Timeout)?
            .map_err(SandboxError::Io)?;
        
        Ok(SandboxResult {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

/// Sandbox errors.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Sandbox timeout")]
    Timeout,
    #[error("Sandbox policy violation: {0}")]
    PolicyViolation(String),
    #[error("Sandbox not supported on this platform")]
    UnsupportedPlatform,
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
