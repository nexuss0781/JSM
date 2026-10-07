//! Bounded, integrity-checked tarball extraction for Phase 1.
//!
//! The compressed response and expanded archive are streamed. File contents are
//! copied into the content-addressed store as they are parsed, but the package
//! manifest (the package commit point) is published only after the complete
//! compressed stream has passed SRI verification.
use flate2::read::MultiGzDecoder;
use jsm_security::{SafetyError, validate_path, validate_symlink, verify_sha512_digest};
use jsm_store::{ManifestEntry, PackageManifest, StagedBlob, Store, StoreError};
use sha2::{Digest, Sha512};
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tar::Archive;
use thiserror::Error;

pub const CRATE_NAME: &str = "jsm-fetch";
/// Maximum extractor working memory for one in-flight package.
///
/// Extraction uses a 32 KiB input buffer, tar's bounded parser buffers, and a
/// 64 KiB blob-copy buffer. It never retains file contents or a tarball; the
/// manifest is the only package-sized allocation. `MAX_IN_FLIGHT_MEMORY_BYTES`
/// documents the fixed I/O working-set ceiling (the manifest itself is bounded
/// by `max_entries` and `max_path_bytes`).
pub const MAX_IN_FLIGHT_MEMORY_BYTES: usize = 128 * 1024;
/// Maximum bytes requested from the compressed input in one read buffer.
pub const INPUT_BUFFER_BYTES: usize = 32 * 1024;
/// The store's blob writer uses this fixed-size copy buffer for each entry.
pub const BLOB_BUFFER_BYTES: usize = 64 * 1024;
const HARD_MAX_ENTRIES: usize = 20_000;
const HARD_MAX_COMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
const HARD_MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
const HARD_MAX_PATH_BYTES: usize = 4096;
const HARD_MAX_COMPRESSION_RATIO: u64 = 200;

/// Cooperative cancellation shared by the fetch pipeline and its caller.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Deterministic per-package extraction progress. Events are emitted after
/// each completed archive entry, in archive order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressEvent {
    pub entries: usize,
    pub uncompressed_bytes: u64,
}
pub type ProgressCallback<'a> = &'a mut dyn FnMut(ProgressEvent);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractLimits {
    pub max_entries: usize,
    pub max_compressed_bytes: u64,
    pub max_uncompressed_bytes: u64,
    pub max_entry_bytes: u64,
    pub max_path_bytes: usize,
    pub max_compression_ratio: u64,
}
impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_entries: HARD_MAX_ENTRIES,
            max_compressed_bytes: HARD_MAX_COMPRESSED_BYTES,
            max_uncompressed_bytes: HARD_MAX_UNCOMPRESSED_BYTES,
            max_entry_bytes: HARD_MAX_ENTRY_BYTES,
            max_path_bytes: HARD_MAX_PATH_BYTES,
            max_compression_ratio: HARD_MAX_COMPRESSION_RATIO,
        }
    }
}

impl ExtractLimits {
    fn capped(self) -> Self {
        Self {
            max_entries: self.max_entries.min(HARD_MAX_ENTRIES),
            max_compressed_bytes: self.max_compressed_bytes.min(HARD_MAX_COMPRESSED_BYTES),
            max_uncompressed_bytes: self.max_uncompressed_bytes.min(HARD_MAX_UNCOMPRESSED_BYTES),
            max_entry_bytes: self.max_entry_bytes.min(HARD_MAX_ENTRY_BYTES),
            max_path_bytes: self.max_path_bytes.min(HARD_MAX_PATH_BYTES),
            max_compression_ratio: self.max_compression_ratio.min(HARD_MAX_COMPRESSION_RATIO),
        }
    }
}

pub struct ExtractOptions<'a> {
    pub limits: ExtractLimits,
    pub cancellation: &'a CancellationToken,
    pub progress: Option<ProgressCallback<'a>>,
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("archive safety error: {0}")]
    Safety(#[from] SafetyError),
    #[error("archive limit exceeded: {0}")]
    Limit(String),
    #[error("invalid or empty package archive")]
    InvalidArchive,
    #[error("package extraction cancelled")]
    Cancelled,
}

/// Verify a compressed tarball and install it into `store`; manifest publication is last.
pub fn extract_tarball<R: Read>(
    store: &Store,
    name: &str,
    version: &str,
    expected_integrity: &str,
    input: R,
    limits: ExtractLimits,
) -> Result<PackageManifest, FetchError> {
    let token = CancellationToken::new();
    extract_tarball_with_options(
        store,
        name,
        version,
        expected_integrity,
        input,
        ExtractOptions {
            limits,
            cancellation: &token,
            progress: None,
        },
    )
}

/// Extract a package with cooperative cancellation and deterministic progress.
/// Cancellation is checked before every entry, during byte reads, and before
/// the manifest commit; a cancelled operation never publishes its manifest.
pub fn extract_tarball_with_options<R: Read>(
    store: &Store,
    name: &str,
    version: &str,
    expected_integrity: &str,
    input: R,
    options: ExtractOptions<'_>,
) -> Result<PackageManifest, FetchError> {
    let limits = options.limits;
    let cancellation = options.cancellation;
    let mut progress = options.progress;
    let limits = limits.capped();
    if cancellation.is_cancelled() {
        return Err(FetchError::Cancelled);
    }
    let hashing = HashingReader::new(input, limits.max_compressed_bytes, (*cancellation).clone());
    // Keep the network/decompression side bounded independently of archive
    // size. The store's put_blob_stream has its own fixed 64 KiB buffer.
    let mut probe = BufReader::with_capacity(INPUT_BUFFER_BYTES, hashing);
    let is_gzip = probe.fill_buf()?.starts_with(&[0x1f, 0x8b]);
    let decoded = if is_gzip {
        Decoded::Gzip(MultiGzDecoder::new(probe))
    } else {
        Decoded::Raw(probe)
    };
    let mut bounded = BoundedReader::new(decoded, limits.max_uncompressed_bytes);
    let mut archive = Archive::new(&mut bounded);
    let mut entries = Vec::new();
    let mut staged_blobs = Vec::<StagedBlob>::new();
    let mut seen = HashSet::new();
    let mut total_file_bytes = 0u64;
    let mut entry_count = 0usize;
    let mut has_package_json = false;

    for item in archive.entries()? {
        if cancellation.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        let mut entry = item?;
        entry_count += 1;
        if entry_count > limits.max_entries {
            return Err(FetchError::Limit("entry count".into()));
        }
        let archive_path = entry.path()?.into_owned();
        let raw = archive_path
            .to_str()
            .ok_or_else(|| FetchError::Limit("non-UTF-8 entry path".into()))?
            .replace('\\', "/");
        if raw.len() > limits.max_path_bytes {
            return Err(FetchError::Limit("path length".into()));
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() && is_package_root_dir(&raw, name) {
            continue;
        }
        let path = normalize_tar_path(&raw, name)?;
        let key = path.to_lowercase();
        if !seen.insert(key) {
            return Err(FetchError::Safety(SafetyError::DuplicatePath(path)));
        }
        if kind.is_dir() {
            if let Some(callback) = progress.as_deref_mut() {
                callback(ProgressEvent {
                    entries: entry_count,
                    uncompressed_bytes: total_file_bytes,
                });
            }
            continue;
        }
        if kind.is_block_special() || kind.is_character_special() || kind.is_fifo() {
            return Err(FetchError::Safety(SafetyError::DeviceFile));
        }
        if kind.is_symlink() || kind.is_hard_link() {
            let target = entry
                .link_name()?
                .ok_or_else(|| FetchError::Limit("missing link target".into()))?
                .to_string_lossy()
                .into_owned();
            validate_symlink(&path, &target)?;
            entries.push(ManifestEntry {
                path,
                hash: String::new(),
                size: 0,
                executable: false,
                symlink: Some(target),
            });
            if let Some(callback) = progress.as_deref_mut() {
                callback(ProgressEvent {
                    entries: entry_count,
                    uncompressed_bytes: total_file_bytes,
                });
            }
            continue;
        }
        if !kind.is_file() {
            return Err(FetchError::Limit("unsupported tar entry".into()));
        }
        let declared = entry.header().size()?;
        if declared > limits.max_entry_bytes {
            return Err(FetchError::Limit("entry size".into()));
        }
        total_file_bytes = total_file_bytes
            .checked_add(declared)
            .ok_or_else(|| FetchError::Limit("total file size".into()))?;
        if total_file_bytes > limits.max_uncompressed_bytes {
            return Err(FetchError::Limit("total file size".into()));
        }
        let mut counted = CountedReader::new(&mut entry);
        let staged = match store.stage_blob_stream(&mut counted) {
            Ok(staged) => staged,
            Err(_error) if cancellation.is_cancelled() => return Err(FetchError::Cancelled),
            Err(error) => return Err(error.into()),
        };
        let hash = staged.hash.clone();
        if counted.bytes != declared {
            return Err(FetchError::Limit(
                "truncated or oversized archive entry".into(),
            ));
        }
        if path == "package.json" {
            has_package_json = true;
        }
        let mode = entry.header().mode().unwrap_or(0);
        entries.push(ManifestEntry {
            path,
            hash,
            size: declared,
            executable: mode & 0o111 != 0,
            symlink: None,
        });
        staged_blobs.push(staged);
        if let Some(callback) = progress.as_deref_mut() {
            callback(ProgressEvent {
                entries: entry_count,
                uncompressed_bytes: total_file_bytes,
            });
        }
    }

    if entries.is_empty() || !has_package_json {
        return Err(FetchError::InvalidArchive);
    }

    // The tar parser can stop at the end-of-archive markers before the gzip
    // footer. Drain through the decoder so checksums, size caps, and SRI cover
    // the complete response rather than only the bytes tar happened to request.
    let drained = io::copy(&mut bounded, &mut io::sink());
    if cancellation.is_cancelled() {
        return Err(FetchError::Cancelled);
    }
    drained?;
    let uncompressed_bytes = bounded.bytes;
    let decoded = bounded.into_inner();
    let hashing = decoded.into_hashing()?;
    let (compressed_bytes, digest) = hashing.finish();
    if uncompressed_bytes > 0
        && compressed_bytes.saturating_mul(limits.max_compression_ratio) < uncompressed_bytes
    {
        return Err(FetchError::Limit("compression ratio".into()));
    }
    verify_sha512_digest(&digest, expected_integrity)?;

    if cancellation.is_cancelled() {
        return Err(FetchError::Cancelled);
    }
    for staged in staged_blobs {
        store.publish_staged_blob(staged)?;
    }
    let manifest = PackageManifest {
        name: name.into(),
        version: version.into(),
        integrity: expected_integrity.into(),
        entries,
        package_json: None,
    };
    store.put_package_manifest(&manifest)?;
    Ok(manifest)
}

/// Alias retained for callers that name the operation after its pipeline stage.
pub fn fetch_and_extract<R: Read>(
    store: &Store,
    name: &str,
    version: &str,
    expected_integrity: &str,
    input: R,
    limits: ExtractLimits,
) -> Result<PackageManifest, FetchError> {
    extract_tarball(store, name, version, expected_integrity, input, limits)
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha512,
    bytes: u64,
    max_bytes: u64,
    cancellation: CancellationToken,
}
impl<R> HashingReader<R> {
    fn new(inner: R, max_bytes: u64, cancellation: CancellationToken) -> Self {
        Self {
            inner,
            hasher: Sha512::new(),
            bytes: 0,
            max_bytes,
            cancellation,
        }
    }
    fn finish(self) -> (u64, Vec<u8>) {
        (self.bytes, self.hasher.finalize().to_vec())
    }
}
impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "package extraction cancelled",
            ));
        }
        let remaining = self.max_bytes.saturating_sub(self.bytes);
        let allowed = usize::try_from(remaining.saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let count = self.inner.read(&mut buffer[..allowed])?;
        self.bytes = self.bytes.saturating_add(count as u64);
        if self.bytes > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "compressed download size limit exceeded",
            ));
        }
        self.hasher.update(&buffer[..count]);
        Ok(count)
    }
}

enum Decoded<R: Read> {
    Gzip(MultiGzDecoder<BufReader<HashingReader<R>>>),
    Raw(BufReader<HashingReader<R>>),
}
impl<R: Read> Read for Decoded<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Gzip(reader) => reader.read(buffer),
            Self::Raw(reader) => reader.read(buffer),
        }
    }
}
impl<R: Read> Decoded<R> {
    fn into_hashing(self) -> io::Result<HashingReader<R>> {
        match self {
            Self::Gzip(reader) => {
                let mut buffered = reader.into_inner();
                io::copy(&mut buffered, &mut io::sink())?;
                Ok(buffered.into_inner())
            }
            Self::Raw(mut buffered) => {
                io::copy(&mut buffered, &mut io::sink())?;
                Ok(buffered.into_inner())
            }
        }
    }
}

struct BoundedReader<R> {
    inner: R,
    bytes: u64,
    max_bytes: u64,
}
impl<R> BoundedReader<R> {
    fn new(inner: R, max_bytes: u64) -> Self {
        Self {
            inner,
            bytes: 0,
            max_bytes,
        }
    }
    fn into_inner(self) -> R {
        self.inner
    }
}
impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.max_bytes.saturating_sub(self.bytes);
        let allowed = usize::try_from(remaining.saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let count = self.inner.read(&mut buffer[..allowed])?;
        self.bytes = self.bytes.saturating_add(count as u64);
        if self.bytes > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "uncompressed archive size limit exceeded",
            ));
        }
        Ok(count)
    }
}

struct CountedReader<R> {
    inner: R,
    bytes: u64,
}
impl<R> CountedReader<R> {
    fn new(inner: R) -> Self {
        Self { inner, bytes: 0 }
    }
}
impl<R: Read> Read for CountedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.bytes = self.bytes.saturating_add(count as u64);
        Ok(count)
    }
}

fn is_package_root_dir(raw: &str, package_name: &str) -> bool {
    let package_root = package_name.rsplit('/').next().unwrap_or(package_name);
    raw == "package"
        || raw == "package/"
        || raw == package_root
        || raw == format!("{package_root}/")
}

fn normalize_tar_path(raw: &str, package_name: &str) -> Result<String, FetchError> {
    let package_root = package_name.rsplit('/').next().unwrap_or(package_name);
    let named_root = format!("{package_root}/");
    let stripped = raw
        .strip_prefix("package/")
        .or_else(|| raw.strip_prefix(&named_root))
        .unwrap_or(raw);
    Ok(validate_path(stripped)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use jsm_testkit::FixturePackage;
    use std::io::Cursor;
    use tar::{Builder, EntryType, Header};

    fn new_store() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path().join("store")).unwrap();
        (temp, store)
    }

    fn custom_archive(entries: &[(&str, EntryType, &[u8])]) -> Vec<u8> {
        let mut encoded = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut archive = Builder::new(&mut encoded);
            for &(path, kind, data) in entries {
                let mut header = Header::new_gnu();
                if path.starts_with('/') {
                    header.set_path("placeholder").unwrap();
                    let name = &mut header.as_mut_bytes()[..100];
                    name.fill(0);
                    name[..path.len()].copy_from_slice(path.as_bytes());
                } else {
                    header.set_path(path).unwrap();
                }
                header.set_entry_type(kind);
                if kind.is_file() {
                    header.set_size(data.len() as u64);
                } else if kind.is_symlink() || kind.is_hard_link() {
                    header
                        .set_link_name(std::str::from_utf8(data).unwrap())
                        .unwrap();
                }
                header.set_mode(0o644);
                header.set_cksum();
                archive.append(&header, data).unwrap();
            }
            archive.finish().unwrap();
        }
        encoded.finish().unwrap()
    }

    fn custom_integrity(bytes: &[u8]) -> String {
        jsm_security::sha512_sri(bytes)
    }

    #[test]
    fn strips_package_name_root_from_npm_archive_paths() {
        assert!(is_package_root_dir("estree/", "@types/estree"));
        assert!(!is_package_root_dir("estree//", "@types/estree"));
        assert_eq!(
            normalize_tar_path("estree/package.json", "@types/estree").unwrap(),
            "package.json"
        );
        assert!(normalize_tar_path("estree/../escape", "@types/estree").is_err());
    }

    #[test]
    fn streams_and_commits_only_after_integrity_verification() {
        let artifact = FixturePackage::new("stream-fixture", "1.2.3")
            .unwrap()
            .build()
            .unwrap();
        let expected = artifact.integrity.to_string();
        let (_temp, store) = new_store();
        let manifest = extract_tarball(
            &store,
            "stream-fixture",
            "1.2.3",
            &expected,
            Cursor::new(artifact.tarball.clone()),
            ExtractLimits::default(),
        )
        .unwrap();
        assert!(
            manifest
                .entries
                .iter()
                .any(|entry| entry.path == "package.json")
        );
        assert!(store.has_package("stream-fixture", "1.2.3", &expected));

        let (_temp, rejected_store) = new_store();
        assert!(extract_tarball(
            &rejected_store,
            "stream-fixture",
            "1.2.3",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            Cursor::new(artifact.tarball),
            ExtractLimits::default(),
        )
        .is_err());
        assert!(!rejected_store.has_package("stream-fixture", "1.2.3", &expected));
        assert!(!rejected_store.has_blob(&manifest.entries[0].hash));
        assert_eq!(
            std::fs::read_dir(rejected_store.root().join("tmp"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn enforces_uncompressed_size_limit_without_committing_package() {
        let artifact = FixturePackage::new("limited-fixture", "1.0.0")
            .unwrap()
            .build()
            .unwrap();
        let expected = artifact.integrity.to_string();
        let (_temp, store) = new_store();
        let limits = ExtractLimits {
            max_uncompressed_bytes: 32,
            ..ExtractLimits::default()
        };
        assert!(
            extract_tarball(
                &store,
                "limited-fixture",
                "1.0.0",
                &expected,
                Cursor::new(artifact.tarball),
                limits,
            )
            .is_err()
        );
        assert!(!store.has_package("limited-fixture", "1.0.0", &expected));
    }

    #[test]
    fn rejects_archive_path_traversal_before_package_commit() {
        let artifact = FixturePackage::new("unsafe-fixture", "1.0.0")
            .unwrap()
            .malicious_file("../escape", b"must not reach project".to_vec())
            .build()
            .unwrap();
        let expected = artifact.integrity.to_string();
        let (temp, store) = new_store();
        assert!(
            extract_tarball(
                &store,
                "unsafe-fixture",
                "1.0.0",
                &expected,
                Cursor::new(artifact.tarball),
                ExtractLimits::default(),
            )
            .is_err()
        );
        assert!(!store.has_package("unsafe-fixture", "1.0.0", &expected));
        assert!(!temp.path().join("escape").exists());
    }

    #[test]
    fn rejects_absolute_paths_symlink_escape_and_devices() {
        for (path, kind, data) in [
            ("/absolute", EntryType::Regular, b"x".as_slice()),
            (
                "package/link",
                EntryType::Symlink,
                b"../../escape".as_slice(),
            ),
            ("package/fifo", EntryType::Fifo, b"".as_slice()),
        ] {
            let tarball = custom_archive(&[
                ("package/package.json", EntryType::Regular, b"{}"),
                (path, kind, data),
            ]);
            let (_temp, store) = new_store();
            assert!(
                extract_tarball(
                    &store,
                    "edge-fixture",
                    "1.0.0",
                    &custom_integrity(&tarball),
                    Cursor::new(tarball),
                    ExtractLimits::default(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn rejects_duplicate_and_case_colliding_entries() {
        let tarball = custom_archive(&[
            ("package/package.json", EntryType::Regular, b"{}"),
            ("package/README", EntryType::Regular, b"a"),
            ("package/readme", EntryType::Regular, b"b"),
        ]);
        let (_temp, store) = new_store();
        assert!(
            extract_tarball(
                &store,
                "duplicate-fixture",
                "1.0.0",
                &custom_integrity(&tarball),
                Cursor::new(tarball),
                ExtractLimits::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn enforces_entry_path_and_compressed_limits() {
        let tarball = custom_archive(&[("package/package.json", EntryType::Regular, b"{}")]);
        let (_temp, store) = new_store();
        for limits in [
            ExtractLimits {
                max_entries: 0,
                ..ExtractLimits::default()
            },
            ExtractLimits {
                max_path_bytes: 4,
                ..ExtractLimits::default()
            },
            ExtractLimits {
                max_compressed_bytes: 1,
                ..ExtractLimits::default()
            },
        ] {
            assert!(
                extract_tarball(
                    &store,
                    "limits",
                    "1.0.0",
                    &custom_integrity(&tarball),
                    Cursor::new(tarball.clone()),
                    limits,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn enforces_entry_count_and_per_entry_limits_without_commit() {
        let tarball = custom_archive(&[
            ("package/package.json", EntryType::Regular, b"{}"),
            ("package/large", EntryType::Regular, b"123456"),
        ]);
        for limits in [
            ExtractLimits {
                max_entries: 1,
                ..ExtractLimits::default()
            },
            ExtractLimits {
                max_entry_bytes: 5,
                ..ExtractLimits::default()
            },
        ] {
            let (_temp, store) = new_store();
            assert!(matches!(
                extract_tarball(
                    &store,
                    "entry-limits",
                    "1.0.0",
                    &custom_integrity(&tarball),
                    Cursor::new(tarball.clone()),
                    limits,
                ),
                Err(FetchError::Limit(_))
            ));
            assert!(!store.has_package("entry-limits", "1.0.0", &custom_integrity(&tarball)));
        }
    }

    #[test]
    fn emits_progress_for_each_completed_entry() {
        let artifact = FixturePackage::new("progress-fixture", "1.0.0")
            .unwrap()
            .build()
            .unwrap();
        let (_temp, store) = new_store();
        let mut events = Vec::new();
        let token = CancellationToken::new();
        let mut progress = |event: ProgressEvent| events.push(event);
        let manifest = extract_tarball_with_options(
            &store,
            "progress-fixture",
            "1.0.0",
            artifact.integrity.as_ref(),
            Cursor::new(artifact.tarball),
            ExtractOptions {
                limits: ExtractLimits::default(),
                cancellation: &token,
                progress: Some(&mut progress),
            },
        )
        .unwrap();
        assert_eq!(events.len(), manifest.entries.len());
        assert!(events.windows(2).all(|pair| {
            pair[0].entries < pair[1].entries
                && pair[0].uncompressed_bytes <= pair[1].uncompressed_bytes
        }));
        assert_eq!(events.last().unwrap().entries, manifest.entries.len());
    }

    #[test]
    fn compression_ratio_limit_fails_closed_without_commit() {
        let tarball = custom_archive(&[(
            "package/package.json",
            EntryType::Regular,
            vec![b'x'; 32 * 1024].as_slice(),
        )]);
        let (_temp, store) = new_store();
        let integrity = custom_integrity(&tarball);
        let result = extract_tarball(
            &store,
            "ratio-limit",
            "1.0.0",
            &integrity,
            Cursor::new(tarball),
            ExtractLimits {
                max_compression_ratio: 1,
                ..ExtractLimits::default()
            },
        );
        assert!(
            matches!(result, Err(FetchError::Limit(message)) if message == "compression ratio")
        );
        assert!(!store.has_package("ratio-limit", "1.0.0", &integrity));
    }

    #[test]
    fn rejects_compression_bomb_and_truncated_streams() {
        let large = vec![0; 2 * 1024 * 1024];
        let bomb = custom_archive(&[
            ("package/package.json", EntryType::Regular, b"{}"),
            ("package/large", EntryType::Regular, large.as_slice()),
        ]);
        let (_temp, store) = new_store();
        assert!(
            extract_tarball(
                &store,
                "bomb",
                "1.0.0",
                &custom_integrity(&bomb),
                Cursor::new(bomb),
                ExtractLimits::default(),
            )
            .is_err()
        );

        let artifact = FixturePackage::new("truncated", "1.0.0")
            .unwrap()
            .build()
            .unwrap();
        for truncated in [
            artifact.tarball[..artifact.tarball.len() - 1].to_vec(),
            artifact.tarball[..artifact.tarball.len() / 2].to_vec(),
        ] {
            assert!(
                extract_tarball(
                    &store,
                    "truncated",
                    "1.0.0",
                    &custom_integrity(&truncated),
                    Cursor::new(truncated),
                    ExtractLimits::default(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn cancellation_during_stream_never_publishes_manifest_and_files_report_progress() {
        let artifact = FixturePackage::new("cancel-fixture", "1.0.0")
            .unwrap()
            .build()
            .unwrap();
        let (_temp, store) = new_store();
        let token = CancellationToken::new();
        let mut events = Vec::new();
        let mut progress = |event: ProgressEvent| {
            events.push(event);
            token.cancel();
        };
        let result = extract_tarball_with_options(
            &store,
            "cancel-fixture",
            "1.0.0",
            artifact.integrity.as_ref(),
            Cursor::new(artifact.tarball.clone()),
            ExtractOptions {
                limits: ExtractLimits::default(),
                cancellation: &token,
                progress: Some(&mut progress),
            },
        );
        assert!(matches!(result, Err(FetchError::Cancelled)));
        assert!(!store.has_package("cancel-fixture", "1.0.0", artifact.integrity.as_ref()));
        assert_eq!(events.len(), 1);
        assert!(events[0].uncompressed_bytes > 0);
    }

    #[test]
    fn configured_limits_are_capped_at_documented_hard_limits() {
        let limits = ExtractLimits {
            max_entries: usize::MAX,
            max_compressed_bytes: u64::MAX,
            max_uncompressed_bytes: u64::MAX,
            max_entry_bytes: u64::MAX,
            max_path_bytes: usize::MAX,
            max_compression_ratio: u64::MAX,
        }
        .capped();
        assert_eq!(limits, ExtractLimits::default());
    }
}
