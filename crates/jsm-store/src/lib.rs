//! Atomic, content-addressed package storage.
use jsm_security::{SafetyError, validate_path, validate_symlink, validate_unique_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const CRATE_NAME: &str = "jsm-store";
const FORMAT_VERSION: &str = "2\n";
const FORMAT_VERSION_NUMBER: u32 = 2;
const LEGACY_FORMAT_VERSION_NUMBER: u32 = 1;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid store or package data: {0}")]
    Invalid(String),
    #[error("integrity error: {0}")]
    Integrity(#[from] SafetyError),
}

/// A file in a package manifest. Blob hashes are lowercase SHA-512 hex.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    pub path: String,
    pub hash: String,
    pub size: u64,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub symlink: Option<String>,
}

/// Durable mapping from a package identity to its files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    pub integrity: String,
    pub entries: Vec<ManifestEntry>,
    #[serde(default)]
    pub package_json: Option<serde_json::Value>,
}

/// A fully hashed package-file blob that is not visible in the CAS until
/// explicitly published. Dropping it removes the private temporary file.
#[derive(Debug)]
pub struct StagedBlob {
    pub hash: String,
    pub size: u64,
    temp_path: PathBuf,
}

impl Drop for StagedBlob {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temp_path);
    }
}

/// Shared content-addressed store rooted at `root`.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Return the platform default store path.
    ///
    /// `JSM_STORE_DIR` is an explicit environment override and takes
    /// precedence over platform defaults (CLI/configuration should take
    /// precedence over this API when they are available).
    pub fn default_path() -> Result<PathBuf, StoreError> {
        if let Ok(path) = std::env::var("JSM_STORE_DIR")
            && !path.trim().is_empty()
        {
            return Ok(PathBuf::from(path));
        }
        Self::default_path_without_override()
    }

    fn default_path_without_override() -> Result<PathBuf, StoreError> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        #[cfg(target_os = "windows")]
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or(home.clone())
            .ok_or_else(|| StoreError::Invalid("cannot determine home directory".into()))?;
        #[cfg(target_os = "macos")]
        let base = home
            .map(|p| p.join("Library").join("Caches"))
            .ok_or_else(|| StoreError::Invalid("cannot determine home directory".into()))?;
        #[cfg(all(unix, not(target_os = "macos")))]
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|p| p.join(".cache")))
            .ok_or_else(|| StoreError::Invalid("cannot determine cache directory".into()))?;
        Ok(base.join("jsm").join("store"))
    }

    /// Open the platform default store, honoring `JSM_STORE_DIR`.
    pub fn open_default() -> Result<Self, StoreError> {
        Self::new(Self::default_path()?)
    }

    /// Open a store for a project, resolving a relative override from that
    /// project root. Callers can apply CLI/config precedence before passing it.
    pub fn open_for_project(
        project_root: impl AsRef<Path>,
        override_path: Option<impl AsRef<Path>>,
    ) -> Result<Self, StoreError> {
        let project_root = project_root.as_ref();
        let explicit = override_path.is_some()
            || std::env::var("JSM_STORE_DIR")
                .map(|path| !path.trim().is_empty())
                .unwrap_or(false);
        let root = match override_path {
            Some(path) => resolve_override(project_root, path.as_ref()),
            None => match std::env::var("JSM_STORE_DIR") {
                Ok(path) if !path.trim().is_empty() => {
                    resolve_override(project_root, Path::new(&path))
                }
                _ => select_store_path(project_root, &Self::default_path_without_override()?),
            },
        };
        match Self::new(&root) {
            Ok(store) => Ok(store),
            Err(error) if !explicit && is_permission_error(&error) => {
                Self::new(project_root.join(".jsm-store"))
            }
            Err(error) => Err(error),
        }
    }

    /// Create (or open) a store and initialize its format version.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let store = Self {
            root: root.as_ref().to_path_buf(),
        };
        for dir in ["files/sha512", "packages", "tmp"] {
            let path = store.root.join(dir);
            fs::create_dir_all(&path)?;
            if !path.is_dir() {
                return Err(StoreError::Invalid(format!(
                    "store layout entry is not a directory: {dir}"
                )));
            }
        }
        let version = store.root.join("VERSION");
        if version.exists() {
            let got = fs::read_to_string(&version)?;
            match got.trim().parse::<u32>().ok() {
                Some(FORMAT_VERSION_NUMBER) => {}
                Some(LEGACY_FORMAT_VERSION_NUMBER) => {
                    // Version 1 accidentally keyed blobs by SHA-512(SHA-512(bytes)).
                    // Version 2 uses SHA-512(bytes); old package manifests are
                    // lazily re-fetched when their blob hashes fail validation.
                    atomic_write(&version, FORMAT_VERSION.as_bytes())?;
                }
                _ => return Err(StoreError::Invalid("unsupported store VERSION".into())),
            }
        } else {
            atomic_write(&version, FORMAT_VERSION.as_bytes())?;
        }
        Ok(store)
    }
    /// Store root path.
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn blob_path(&self, hash: &str) -> Result<PathBuf, StoreError> {
        let h = hash
            .strip_prefix("sha512-")
            .unwrap_or(hash)
            .to_ascii_lowercase();
        if h.len() != 128 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(StoreError::Invalid("invalid blob hash".into()));
        }
        Ok(self.root.join("files/sha512").join(&h[..2]).join(&h[2..]))
    }
    /// Return whether a blob exists.
    pub fn has_blob(&self, hash: &str) -> bool {
        self.blob_path(hash).map(|p| p.is_file()).unwrap_or(false)
    }
    /// Write a stream after hashing it; publication is atomic and idempotent.
    pub fn put_blob_stream<R: Read>(&self, mut input: R) -> Result<String, StoreError> {
        self.put_blob_stream_inner(None, &mut input)
    }

    /// Hash a stream into private store temp space without publishing a CAS blob.
    pub fn stage_blob_stream<R: Read>(&self, mut input: R) -> Result<StagedBlob, StoreError> {
        self.stage_blob_stream_inner(None, &mut input)
    }

    /// Write a blob only when it matches the caller-supplied SHA-512 digest.
    pub fn put_verified_blob_stream<R: Read>(
        &self,
        expected_hash: &str,
        mut input: R,
    ) -> Result<String, StoreError> {
        let expected = normalize_hash(expected_hash)?;
        self.put_blob_stream_inner(Some(&expected), &mut input)
    }

    /// Stage a stream only when it matches the caller-supplied SHA-512 digest.
    pub fn stage_verified_blob_stream<R: Read>(
        &self,
        expected_hash: &str,
        mut input: R,
    ) -> Result<StagedBlob, StoreError> {
        let expected = normalize_hash(expected_hash)?;
        self.stage_blob_stream_inner(Some(&expected), &mut input)
    }

    /// Publish a previously staged blob atomically. The staged file is rehashed
    /// before publication to guard against mutation between staging and commit.
    pub fn publish_staged_blob(&self, staged: StagedBlob) -> Result<String, StoreError> {
        let temp_root = self.root.join("tmp");
        if !staged.temp_path.starts_with(&temp_root) {
            return Err(StoreError::Invalid(
                "staged blob belongs to a different store".into(),
            ));
        }
        if !file_has_hash_and_size(&staged.temp_path, &staged.hash, staged.size)? {
            return Err(StoreError::Invalid(
                "staged blob failed integrity verification".into(),
            ));
        }
        let dest = self.blob_path(&staged.hash)?;
        match fs::symlink_metadata(&dest) {
            Ok(metadata) if metadata.file_type().is_file() => {
                match file_has_hash(&dest, &staged.hash) {
                    Ok(true) => {
                        set_readonly(&dest)?;
                        return Ok(staged.hash.clone());
                    }
                    Ok(false) => {}
                    Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                // The replacement has already been staged and verified, so a
                // corrupt object can be removed without exposing partial bytes.
                match fs::remove_file(&dest) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(_) => fs::remove_file(&dest)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::create_dir_all(dest.parent().unwrap())?;
        match fs::rename(&staged.temp_path, &dest) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if file_has_hash(&dest, &staged.hash)? {
                    set_readonly(&dest)?;
                    return Ok(staged.hash.clone());
                }
                fs::remove_file(&dest)?;
                fs::rename(&staged.temp_path, &dest)?;
            }
            Err(error) => return Err(error.into()),
        }
        set_readonly(&dest)?;
        Ok(staged.hash.clone())
    }

    fn put_blob_stream_inner<R: Read>(
        &self,
        expected_hash: Option<&str>,
        input: &mut R,
    ) -> Result<String, StoreError> {
        let staged = self.stage_blob_stream_inner(expected_hash, input)?;
        self.publish_staged_blob(staged)
    }

    fn stage_blob_stream_inner<R: Read>(
        &self,
        expected_hash: Option<&str>,
        input: &mut R,
    ) -> Result<StagedBlob, StoreError> {
        let tmp =
            self.root
                .join("tmp")
                .join(format!("blob-{}-{}", std::process::id(), unique_suffix()));
        let result = (|| {
            let mut f = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
            let mut hasher = Sha512::new();
            let mut buf = [0u8; 64 * 1024];
            let mut size = 0u64;
            loop {
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                f.write_all(&buf[..n])?;
                hasher.update(&buf[..n]);
                size = size
                    .checked_add(n as u64)
                    .ok_or_else(|| StoreError::Invalid("blob size overflow".into()))?;
            }
            f.sync_all()?;
            drop(f);
            let hash = format!("{:x}", hasher.finalize());
            if let Some(expected) = expected_hash
                && expected != hash
            {
                return Err(StoreError::Invalid(
                    "blob hash does not match expected hash".into(),
                ));
            }
            let mut perms = fs::metadata(&tmp)?.permissions();
            perms.set_readonly(true);
            fs::set_permissions(&tmp, perms)?;
            Ok(StagedBlob {
                hash,
                size,
                temp_path: tmp.clone(),
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
    /// Write bytes as a verified blob.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<String, StoreError> {
        self.put_blob_stream(io::Cursor::new(bytes))
    }
    /// Resolve a blob path, if it exists.
    pub fn get_blob_path(&self, hash: &str) -> Result<Option<PathBuf>, StoreError> {
        let p = self.blob_path(hash)?;
        Ok(p.is_file().then_some(p))
    }
    fn manifest_path(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<PathBuf, StoreError> {
        let name =
            validate_path(name).map_err(|_| StoreError::Invalid("invalid package name".into()))?;
        if version.is_empty()
            || version.contains('/')
            || version.contains('\\')
            || version.contains("..")
        {
            return Err(StoreError::Invalid("invalid package version".into()));
        }
        let i = integrity.strip_prefix("sha512-").unwrap_or(integrity);
        let valid_hex = i.len() == 128 && i.bytes().all(|b| b.is_ascii_hexdigit());
        let valid_base64 = i.len() == 88
            && i.ends_with("==")
            && i.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=');
        if !(valid_hex || valid_base64) {
            return Err(StoreError::Invalid("invalid integrity".into()));
        }
        // Base64 SRI may contain `/`; keep it in one encoded filename component.
        // Without this, PathBuf::join treats a leading slash as an absolute path.
        let safe_integrity = i.replace('/', "%2F");
        Ok(self
            .root
            .join("packages")
            .join(name)
            .join(version)
            .join(format!("{safe_integrity}.manifest")))
    }
    /// Check whether a committed package manifest exists.
    pub fn has_package(&self, name: &str, version: &str, integrity: &str) -> bool {
        self.get_package_manifest(name, version, integrity)
            .map(|manifest| manifest.is_some())
            .unwrap_or(false)
    }
    /// Validate and atomically commit a package manifest (the package commit point).
    pub fn put_package_manifest(&self, manifest: &PackageManifest) -> Result<(), StoreError> {
        let path = self.manifest_path(&manifest.name, &manifest.version, &manifest.integrity)?;
        let mut seen = HashSet::new();
        if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
            return Err(StoreError::Invalid(
                "manifest identity is incomplete".into(),
            ));
        }
        for e in &manifest.entries {
            let normalized = validate_path(&e.path)?;
            validate_unique_path(&mut seen, &normalized)?;
            if let Some(target) = &e.symlink {
                if !e.hash.is_empty() || e.size != 0 {
                    return Err(StoreError::Invalid(
                        "symlink entry has file metadata".into(),
                    ));
                }
                validate_symlink(&normalized, target)?;
                continue;
            }
            if e.hash.len() != 128 || !e.hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(StoreError::Invalid("invalid entry hash".into()));
            }
            if !self.has_blob(&e.hash) {
                return Err(StoreError::Invalid(format!("missing blob {}", e.hash)));
            }
            let blob = self.blob_path(&e.hash)?;
            if fs::metadata(&blob)?.len() != e.size {
                return Err(StoreError::Invalid(
                    "manifest size does not match blob".into(),
                ));
            }
            if !file_has_hash(&blob, &e.hash)? {
                return Err(StoreError::Invalid(
                    "manifest hash does not match blob contents".into(),
                ));
            }
        }
        let data = serde_json::to_vec(manifest)?;
        if path.exists() {
            let existing = fs::read(&path)?;
            if existing == data {
                return Ok(());
            }
            let existing_is_valid = serde_json::from_slice::<PackageManifest>(&existing)
                .ok()
                .is_some_and(|previous| {
                    previous.name == manifest.name
                        && previous.version == manifest.version
                        && previous.integrity == manifest.integrity
                        && self.validate_manifest(&previous).is_ok()
                });
            if existing_is_valid {
                return Err(StoreError::Invalid(
                    "package manifest already committed with different contents".into(),
                ));
            }
        }
        atomic_write(&path, &data)
    }
    /// Read the compact persisted bytes without deserializing a large manifest.
    pub fn get_package_manifest_bytes(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.manifest_path(name, version, integrity)?;
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(fs::read(path)?))
    }

    /// Read a committed package manifest.
    pub fn get_package_manifest(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<Option<PackageManifest>, StoreError> {
        let path = self.manifest_path(name, version, integrity)?;
        if !path.is_file() {
            return Ok(None);
        }
        let m: PackageManifest = serde_json::from_slice(&fs::read(path)?)?;
        if m.name != name || m.version != version || m.integrity != integrity {
            return Err(StoreError::Invalid(
                "manifest identity does not match its path".into(),
            ));
        }
        self.validate_manifest(&m)?;
        Ok(Some(m))
    }
    fn validate_manifest(&self, m: &PackageManifest) -> Result<(), StoreError> {
        let mut seen = HashSet::new();
        for e in &m.entries {
            let normalized = validate_path(&e.path)?;
            validate_unique_path(&mut seen, &normalized)?;
            if let Some(target) = &e.symlink {
                if !e.hash.is_empty() || e.size != 0 {
                    return Err(StoreError::Invalid(
                        "symlink entry has file metadata".into(),
                    ));
                }
                validate_symlink(&normalized, target)?;
                continue;
            }
            if e.hash.len() != 128 || !e.hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(StoreError::Invalid(
                    "manifest contains invalid blob hash".into(),
                ));
            }
            if !self.has_blob(&e.hash) {
                return Err(StoreError::Invalid(
                    "manifest references missing blob".into(),
                ));
            }
            if fs::metadata(self.blob_path(&e.hash)?)?.len() != e.size {
                return Err(StoreError::Invalid(
                    "manifest size does not match blob".into(),
                ));
            }
            if !file_has_hash(&self.blob_path(&e.hash)?, &e.hash)? {
                return Err(StoreError::Invalid(
                    "manifest hash does not match blob contents".into(),
                ));
            }
        }
        Ok(())
    }
}

fn resolve_override(project_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

fn select_store_path(project_root: &Path, shared_default: &Path) -> PathBuf {
    let Some(project_volume) = volume_id(project_root) else {
        return shared_default.to_path_buf();
    };
    let Some(shared_volume) = volume_id(shared_default) else {
        return shared_default.to_path_buf();
    };
    select_store_path_for_volumes(
        project_volume,
        shared_volume,
        volume_root(project_root),
        shared_default,
    )
}

fn select_store_path_for_volumes(
    project_volume: u128,
    shared_volume: u128,
    project_volume_root: Option<PathBuf>,
    shared_default: &Path,
) -> PathBuf {
    if project_volume == shared_volume {
        return shared_default.to_path_buf();
    }
    let Some(volume_root) = project_volume_root else {
        return shared_default.to_path_buf();
    };
    volume_root.join(per_user_cache_suffix(shared_default))
}

fn per_user_cache_suffix(shared_default: &Path) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(relative) = shared_default.strip_prefix(home)
    {
        return relative.to_path_buf();
    }
    platform_cache_suffix()
}

#[cfg(target_os = "windows")]
fn platform_cache_suffix() -> PathBuf {
    PathBuf::from("AppData")
        .join("Local")
        .join("jsm")
        .join("store")
}

#[cfg(target_os = "macos")]
fn platform_cache_suffix() -> PathBuf {
    PathBuf::from("Library")
        .join("Caches")
        .join("jsm")
        .join("store")
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_cache_suffix() -> PathBuf {
    PathBuf::from(".cache").join("jsm").join("store")
}

#[cfg(unix)]
fn volume_id(path: &Path) -> Option<u128> {
    use std::os::unix::fs::MetadataExt;
    existing_metadata(path)
        .ok()
        .map(|metadata| metadata.dev() as u128)
}

#[cfg(target_os = "windows")]
fn volume_id(path: &Path) -> Option<u128> {
    use std::hash::{Hash, Hasher};

    let prefix = existing_path(path)?
        .components()
        .find_map(|component| match component {
            std::path::Component::Prefix(prefix) => {
                Some(prefix.as_os_str().to_string_lossy().to_ascii_lowercase())
            }
            _ => None,
        })?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    prefix.hash(&mut hasher);
    Some(u128::from(hasher.finish()))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn volume_id(_path: &Path) -> Option<u128> {
    None
}

fn volume_root(path: &Path) -> Option<PathBuf> {
    let mut current = existing_path(path)?;
    let id = volume_id(&current)?;
    while let Some(parent) = current.parent() {
        if volume_id(parent) != Some(id) {
            break;
        }
        current = parent.to_path_buf();
    }
    Some(current)
}

fn existing_path(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    loop {
        if current.exists() {
            return fs::canonicalize(current).ok();
        }
        current = current.parent()?;
    }
}

#[cfg(unix)]
fn existing_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let existing = existing_path(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no existing ancestor"))?;
    fs::metadata(existing)
}

fn is_permission_error(error: &StoreError) -> bool {
    matches!(error, StoreError::Io(error) if matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    ))
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), StoreError> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", unique_suffix()));
    let mut f = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    match fs::rename(&tmp, path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp);
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
    }
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}
fn normalize_hash(hash: &str) -> Result<String, StoreError> {
    let h = hash
        .strip_prefix("sha512-")
        .unwrap_or(hash)
        .to_ascii_lowercase();
    if h.len() != 128 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(StoreError::Invalid("invalid blob hash".into()));
    }
    Ok(h)
}

fn file_has_hash(path: &Path, expected: &str) -> Result<bool, StoreError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha512::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()) == expected)
}

fn file_has_hash_and_size(path: &Path, expected: &str, size: u64) -> Result<bool, StoreError> {
    if fs::metadata(path)?.len() != size {
        return Ok(false);
    }
    file_has_hash(path, expected)
}

fn set_readonly(path: &Path) -> Result<(), StoreError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingReader {
        emitted: bool,
    }

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.emitted {
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "reader died"))
            } else {
                self.emitted = true;
                buffer[..4].copy_from_slice(b"dead");
                Ok(4)
            }
        }
    }

    struct BlockingReader {
        emitted: bool,
    }

    impl Read for BlockingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.emitted {
                let partial = b"partial";
                buffer[..partial.len()].copy_from_slice(partial);
                self.emitted = true;
                return Ok(partial.len());
            }
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }

    #[test]
    fn killed_writer_child() {
        let Some(root) = std::env::var_os("JSM_TEST_KILLED_WRITER_ROOT") else {
            return;
        };
        let store = Store::new(root).unwrap();
        let _ = store.put_blob_stream(BlockingReader { emitted: false });
    }

    #[test]
    fn same_volume_selection_keeps_shared_default() {
        let shared = PathBuf::from("shared-cache/store");
        assert_eq!(
            select_store_path_for_volumes(1, 1, Some(PathBuf::from("volume-root")), &shared),
            shared
        );
    }

    #[test]
    fn different_volume_selection_uses_volume_cache_root() {
        let shared = PathBuf::from("shared-cache/store");
        let volume_root = PathBuf::from("volume-root");
        let expected = volume_root.join(platform_cache_suffix());
        assert_eq!(
            select_store_path_for_volumes(2, 1, Some(volume_root), &shared),
            expected
        );
    }

    #[test]
    fn explicit_override_wins_over_volume_selection() {
        let project = PathBuf::from("/project");
        assert_eq!(
            resolve_override(&project, Path::new(".store")),
            PathBuf::from("/project/.store")
        );
        assert_eq!(
            resolve_override(&project, Path::new("/explicit/store")),
            PathBuf::from("/explicit/store")
        );
    }

    #[test]
    fn dying_reader_never_publishes_partial_blob() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let result = store.put_blob_stream(FailingReader { emitted: false });
        assert!(
            matches!(result, Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
        assert!(
            fs::read_dir(root.join("files/sha512"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(fs::read_dir(root.join("tmp")).unwrap().next().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn killed_writer_never_publishes_and_store_recovers() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let root = std::env::temp_dir().join(format!("jsm-store-killed-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::killed_writer_child"])
            .env("JSM_TEST_KILLED_WRITER_ROOT", &root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut partial_temp_seen = false;
        while Instant::now() < deadline {
            partial_temp_seen = fs::read_dir(root.join("tmp"))
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.file_name().to_string_lossy().starts_with("blob-")
                        && entry
                            .metadata()
                            .map(|metadata| metadata.len() > 0)
                            .unwrap_or(false)
                });
            if partial_temp_seen {
                break;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("child writer exited before creating its partial temp: {status}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !partial_temp_seen {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child writer did not create a partial temporary blob");
        }
        child.kill().unwrap();
        let _ = child.wait().unwrap();

        let partial_hash = format!("{:x}", Sha512::digest(b"partial"));
        assert!(!store.has_blob(&partial_hash));
        let complete = b"complete replacement";
        let complete_hash = store.put_blob(complete).unwrap();
        assert_eq!(complete_hash, format!("{:x}", Sha512::digest(complete)));
        assert_eq!(
            fs::read(store.get_blob_path(&complete_hash).unwrap().unwrap()).unwrap(),
            complete
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn base64_integrity_slash_cannot_escape_package_store() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let path = store
            .manifest_path(
                "@scope/package",
                "1.2.3",
                "sha512-/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            )
            .unwrap();
        assert!(path.starts_with(&root));
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "1.2.3");
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("%2F")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_writers_publish_one_complete_blob() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let bytes = b"same bytes from every writer".to_vec();
        let mut writers = Vec::new();
        for _ in 0..8 {
            let s = store.clone();
            let b = bytes.clone();
            writers.push(std::thread::spawn(move || s.put_blob(&b).unwrap()));
        }
        let hashes: Vec<_> = writers.into_iter().map(|w| w.join().unwrap()).collect();
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
        let path = store.get_blob_path(&hashes[0]).unwrap().unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(fs::read_dir(root.join("tmp")).unwrap().next().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_expected_hash_is_rejected_without_publication() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let err = store
            .put_verified_blob_stream(&"0".repeat(128), io::Cursor::new(b"bytes"))
            .unwrap_err();
        assert!(err.to_string().contains("does not match expected"));
        assert!(
            fs::read_dir(root.join("files/sha512"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(fs::read_dir(root.join("tmp")).unwrap().next().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_blobs_remain_private_until_publish_and_drop_cleans_temp() {
        let root = std::env::temp_dir().join(format!("jsm-store-stage-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let bytes = b"staged package file";
        let staged = store.stage_blob_stream(io::Cursor::new(bytes)).unwrap();
        let hash = staged.hash.clone();
        assert_eq!(staged.size, bytes.len() as u64);
        assert!(!store.has_blob(&hash));
        assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 1);
        drop(staged);
        assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 0);

        let staged = store.stage_blob_stream(io::Cursor::new(bytes)).unwrap();
        assert_eq!(store.publish_staged_blob(staged).unwrap(), hash);
        assert!(store.has_blob(&hash));
        assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_relative_override_is_resolved_before_store_creation() {
        let project = std::env::temp_dir().join(format!("jsm-project-{}", unique_suffix()));
        let store = Store::open_for_project(&project, Some(Path::new(".jsm-store"))).unwrap();
        assert_eq!(store.root(), project.join(".jsm-store"));
        assert!(store.root().join("VERSION").is_file());
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn manifest_is_commit_point_and_compact_bytes_are_readable() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let bytes = b"package json";
        let hash = store.put_blob(bytes).unwrap();
        let integrity = format!("sha512-{}==", "A".repeat(86));
        let manifest = PackageManifest {
            name: "pkg".into(),
            version: "1.0.0".into(),
            integrity: integrity.clone(),
            entries: vec![ManifestEntry {
                path: "package.json".into(),
                hash,
                size: bytes.len() as u64,
                executable: false,
                symlink: None,
            }],
            package_json: Some(serde_json::json!({"name":"pkg","version":"1.0.0"})),
        };
        assert!(!store.has_package("pkg", "1.0.0", &integrity));
        store.put_package_manifest(&manifest).unwrap();
        let compact = store
            .get_package_manifest_bytes("pkg", "1.0.0", &integrity)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<PackageManifest>(&compact).unwrap(),
            manifest
        );
        assert!(store.has_package("pkg", "1.0.0", &integrity));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_blob_is_not_replaced_and_tmp_recovery_is_harmless() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let bytes = b"already committed";
        let hash = store.put_blob(bytes).unwrap();
        let path = store.get_blob_path(&hash).unwrap().unwrap();
        let mode = fs::metadata(&path).unwrap().permissions();
        assert!(mode.readonly());
        fs::write(store.root.join("tmp").join("orphan-writer"), b"partial").unwrap();
        assert_eq!(store.put_blob(bytes).unwrap(), hash);
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(store.has_blob(&hash));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blob_key_is_sha512_of_raw_content() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let bytes = b"single hash, not a hash of the digest";
        let hash = store.put_blob(bytes).unwrap();
        assert_eq!(hash, format!("{:x}", Sha512::digest(bytes)));
        fs::remove_dir_all(root).unwrap();
    }

    // The non-Unix branch clears the readonly attribute on a temporary test file
    // so this test can simulate corruption; Unix uses an explicit safe mode.
    #[allow(clippy::permissions_set_readonly_false)]
    #[test]
    fn corrupt_existing_blob_is_repaired_from_verified_replacement_bytes() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let bytes = b"canonical bytes";
        let hash = store.put_blob(bytes).unwrap();
        let path = store.get_blob_path(&hash).unwrap().unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o644);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        fs::write(&path, b"corrupted cache bytes").unwrap();

        assert_eq!(store.put_blob(bytes).unwrap(), hash);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn version_one_double_hash_manifest_is_refetched_and_replaced() {
        let root = std::env::temp_dir().join(format!("jsm-store-test-{}", unique_suffix()));
        let store = Store::new(&root).unwrap();
        let bytes = b"legacy content";
        let raw_digest = Sha512::digest(bytes);
        let raw_hash = format!("{:x}", raw_digest);
        let legacy_hash = format!("{:x}", Sha512::digest(raw_digest));
        let legacy_blob = store.blob_path(&legacy_hash).unwrap();
        fs::create_dir_all(legacy_blob.parent().unwrap()).unwrap();
        fs::write(&legacy_blob, bytes).unwrap();

        let integrity = format!("sha512-{}==", "A".repeat(86));
        let mut manifest = PackageManifest {
            name: "legacy-pkg".into(),
            version: "1.0.0".into(),
            integrity: integrity.clone(),
            entries: vec![ManifestEntry {
                path: "index.js".into(),
                hash: legacy_hash,
                size: bytes.len() as u64,
                executable: false,
                symlink: None,
            }],
            package_json: None,
        };
        let manifest_path = store
            .manifest_path("legacy-pkg", "1.0.0", &integrity)
            .unwrap();
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        fs::write(store.root.join("VERSION"), b"1\n").unwrap();

        let upgraded = Store::new(&root).unwrap();
        assert_eq!(
            fs::read_to_string(upgraded.root.join("VERSION")).unwrap(),
            "2\n"
        );
        assert!(!upgraded.has_package("legacy-pkg", "1.0.0", &integrity));

        let refreshed_hash = upgraded.put_blob(bytes).unwrap();
        assert_eq!(refreshed_hash, raw_hash);
        manifest.entries[0].hash = refreshed_hash;
        upgraded.put_package_manifest(&manifest).unwrap();
        assert!(upgraded.has_package("legacy-pkg", "1.0.0", &integrity));
        fs::remove_dir_all(root).unwrap();
    }
}
