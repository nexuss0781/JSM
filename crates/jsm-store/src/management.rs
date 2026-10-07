use super::{
    ManifestEntry, PackageManifest, PackageReference, Store, StoreError,
    reference::ReferenceRegistry,
};
use fs4::FileExt;
use jsm_security::{validate_path, validate_symlink, validate_unique_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VerificationReport {
    pub packages_checked: u64,
    pub blobs_checked: u64,
    pub logical_bytes_checked: u64,
    pub findings: Vec<VerificationFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationFinding {
    pub code: String,
    pub message: String,
    pub path: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub integrity: Option<String>,
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReclaimedBytes {
    pub logical: u64,
    pub physical: u64,
    pub manifests: u64,
    pub blobs: u64,
}

impl Store {
    pub fn blob_totals(&self) -> Result<(u64, u64), StoreError> {
        let root = self.root.join("files/sha512");
        let mut count = 0u64;
        let mut physical = 0u64;
        for path in collect_regular_files(&root)? {
            let file = File::open(&path)?;
            let metadata = file.metadata()?;
            physical =
                physical.saturating_add(FileExt::allocated_size(&file).unwrap_or(metadata.len()));
            count = count.saturating_add(1);
        }
        Ok((count, physical))
    }

    /// Enumerate committed package manifests without verifying their payloads.
    pub fn list_package_manifests(&self) -> Result<Vec<PackageManifest>, StoreError> {
        let (manifests, findings) = self.scan_manifests()?;
        if let Some(finding) = findings.first() {
            return Err(StoreError::Invalid(format!(
                "cannot enumerate package manifests: {} ({})",
                finding.message, finding.path
            )));
        }
        Ok(manifests)
    }

    /// Produce a size-aware registry entry for one committed package.
    pub fn package_reference(
        &self,
        manifest: &PackageManifest,
    ) -> Result<PackageReference, StoreError> {
        let mut seen = HashSet::new();
        let mut logical_size = 0u64;
        let mut physical_size = 0u64;
        let mut file_count = 0u64;
        for entry in &manifest.entries {
            if entry.symlink.is_some() {
                continue;
            }
            logical_size = logical_size
                .checked_add(entry.size)
                .ok_or_else(|| StoreError::Invalid("package logical size overflow".into()))?;
            file_count = file_count
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("package file count overflow".into()))?;
            if seen.insert(entry.hash.clone()) {
                let path = self.blob_path(&entry.hash)?;
                if let Ok(file) = File::open(&path) {
                    physical_size = physical_size
                        .checked_add(FileExt::allocated_size(&file).unwrap_or(entry.size))
                        .ok_or_else(|| {
                            StoreError::Invalid("package physical size overflow".into())
                        })?;
                }
            }
        }
        Ok(PackageReference {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            integrity: manifest.integrity.clone(),
            logical_size,
            physical_size,
            file_count,
        })
    }

    /// Remove a package's committed manifest. Caller must hold the store-wide
    /// exclusive maintenance lease when doing destructive maintenance.
    pub fn remove_package_manifest(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<bool, StoreError> {
        let path = self.manifest_path(name, version, integrity)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Verify all package manifests and referenced blobs. Metadata mode checks
    /// paths, types, and lengths; full mode additionally hashes every unique blob.
    pub fn verify(&self, full: bool) -> Result<VerificationReport, StoreError> {
        let (manifests, mut findings) = self.scan_manifests()?;
        let mut report = VerificationReport {
            packages_checked: manifests.len() as u64,
            findings: std::mem::take(&mut findings),
            ..Default::default()
        };
        let mut checked = HashSet::new();
        for manifest in &manifests {
            let mut paths = HashSet::new();
            for entry in &manifest.entries {
                report.logical_bytes_checked =
                    report.logical_bytes_checked.saturating_add(entry.size);
                if let Err(error) = verify_entry_path(entry, &mut paths) {
                    report.findings.push(finding(
                        "invalid_entry",
                        error.to_string(),
                        "",
                        manifest,
                        None,
                    ));
                    continue;
                }
                if entry.symlink.is_some() {
                    continue;
                }
                let blob_path = match self.blob_path(&entry.hash) {
                    Ok(path) => path,
                    Err(error) => {
                        report.findings.push(finding(
                            "invalid_blob_hash",
                            error.to_string(),
                            "",
                            manifest,
                            Some(&entry.hash),
                        ));
                        continue;
                    }
                };
                match fs::symlink_metadata(&blob_path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        report.findings.push(finding(
                            "missing_blob",
                            format!("manifest references missing blob {}", entry.hash),
                            &blob_path.display().to_string(),
                            manifest,
                            Some(&entry.hash),
                        ));
                    }
                    Err(error) => return Err(error.into()),
                    Ok(metadata) if !metadata.file_type().is_file() => {
                        report.findings.push(finding(
                            "invalid_blob_type",
                            format!("blob path is not a regular file: {}", entry.hash),
                            &blob_path.display().to_string(),
                            manifest,
                            Some(&entry.hash),
                        ));
                    }
                    Ok(metadata) => {
                        if metadata.len() != entry.size {
                            report.findings.push(finding(
                                "blob_size_mismatch",
                                format!(
                                    "blob {} has size {}, expected {}",
                                    entry.hash,
                                    metadata.len(),
                                    entry.size
                                ),
                                &blob_path.display().to_string(),
                                manifest,
                                Some(&entry.hash),
                            ));
                        }
                        if full && checked.insert(entry.hash.clone()) {
                            report.blobs_checked += 1;
                            if !file_has_hash(&blob_path, &entry.hash)? {
                                report.findings.push(finding(
                                    "blob_hash_mismatch",
                                    format!("blob contents do not match SHA-512 {}", entry.hash),
                                    &blob_path.display().to_string(),
                                    manifest,
                                    Some(&entry.hash),
                                ));
                            }
                        }
                    }
                }
            }
        }
        report.findings.sort_by(|left, right| {
            (&left.path, &left.code, &left.name, &left.version).cmp(&(
                &right.path,
                &right.code,
                &right.name,
                &right.version,
            ))
        });
        Ok(report)
    }

    /// Quarantine corrupt blobs/manifests and mark package versions for lazy
    /// re-fetch. The caller must hold an exclusive maintenance lease.
    pub fn repair_from_report(
        &self,
        report: &VerificationReport,
    ) -> Result<VerificationReport, StoreError> {
        let mut bad_blobs = BTreeSet::new();
        let mut bad_manifests = BTreeSet::new();
        let mut bad_manifest_paths = BTreeSet::new();
        for finding in &report.findings {
            if let Some(hash) = &finding.hash {
                bad_blobs.insert(hash.clone());
            }
            if finding.code == "invalid_manifest" {
                bad_manifest_paths.insert(PathBuf::from(&finding.path));
            }
            if let (Some(name), Some(version), Some(integrity)) =
                (&finding.name, &finding.version, &finding.integrity)
            {
                bad_manifests.insert((name.clone(), version.clone(), integrity.clone()));
            }
        }
        let quarantine = self.root.join("quarantine");
        fs::create_dir_all(&quarantine)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        for hash in bad_blobs {
            if let Ok(path) = self.blob_path(&hash)
                && path.exists()
            {
                let target = quarantine.join(format!("blob-{hash}-{stamp}"));
                match fs::rename(path, target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let registry = ReferenceRegistry::open(&self.root)?;
        for path in bad_manifest_paths {
            if path.starts_with(self.root.join("packages")) && path.is_file() {
                let target = quarantine.join(format!(
                    "invalid-manifest-{}-{stamp}",
                    safe_component(
                        path.file_name()
                            .and_then(|part| part.to_str())
                            .unwrap_or("unknown")
                    )
                ));
                fs::rename(path, target)?;
            }
        }
        for (name, version, integrity) in bad_manifests {
            let path = self.manifest_path(&name, &version, &integrity)?;
            if path.exists() {
                let target = quarantine.join(format!(
                    "manifest-{}-{}-{}-{stamp}",
                    safe_component(&name),
                    safe_component(&version),
                    &safe_component(&integrity)[..safe_component(&integrity).len().min(24)]
                ));
                match fs::rename(path, target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            registry.mark_corrupt(&name, &version, &integrity)?;
        }
        self.verify(false)
    }

    /// Delete orphaned blobs after manifests have been removed under an exclusive
    /// maintenance lease. Only regular files in the validated hash tree are swept.
    pub fn sweep_unreferenced_blobs(&self) -> Result<(u64, u64), StoreError> {
        let manifests = self.list_package_manifests()?;
        let mut live = HashSet::new();
        for manifest in manifests {
            for entry in manifest.entries {
                if entry.symlink.is_none() {
                    live.insert(entry.hash);
                }
            }
        }
        let blob_root = self.root.join("files/sha512");
        let mut removed_count = 0u64;
        let mut removed_bytes = 0u64;
        for path in collect_regular_files(&blob_root)? {
            let Ok(relative) = path.strip_prefix(&blob_root) else {
                continue;
            };
            let hash = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<String>();
            if hash.len() != 128 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            if !live.contains(&hash) {
                let metadata = fs::metadata(&path)?;
                removed_bytes = removed_bytes.saturating_add(
                    FileExt::allocated_size(&File::open(&path)?).unwrap_or(metadata.len()),
                );
                fs::remove_file(path)?;
                removed_count += 1;
            }
        }
        Ok((removed_count, removed_bytes))
    }

    /// Remove old temporary files, preserving fresh or currently active work.
    pub fn clean_orphan_temps(&self, older_than: std::time::Duration) -> Result<u64, StoreError> {
        let now = SystemTime::now();
        let tmp = self.root.join("tmp");
        let mut removed = 0;
        for path in collect_regular_files(&tmp)? {
            let metadata = fs::metadata(&path)?;
            let old = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= older_than);
            if old {
                fs::remove_file(path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

impl Store {
    fn scan_manifests(
        &self,
    ) -> Result<(Vec<PackageManifest>, Vec<VerificationFinding>), StoreError> {
        let root = self.root.join("packages");
        let mut manifests = Vec::new();
        let mut findings = Vec::new();
        for path in collect_regular_files(&root)? {
            if path.extension().and_then(|extension| extension.to_str()) != Some("manifest") {
                findings.push(VerificationFinding {
                    code: "extra_store_file".into(),
                    message: "unexpected regular file under the package-manifest tree".into(),
                    path: path.display().to_string(),
                    name: None,
                    version: None,
                    integrity: None,
                    hash: None,
                });
                continue;
            }
            let manifest = match fs::read(&path).map_err(StoreError::from).and_then(|bytes| {
                serde_json::from_slice::<PackageManifest>(&bytes).map_err(StoreError::from)
            }) {
                Ok(manifest) => manifest,
                Err(error) => {
                    findings.push(VerificationFinding {
                        code: "invalid_manifest".into(),
                        message: error.to_string(),
                        path: path.display().to_string(),
                        name: None,
                        version: None,
                        integrity: None,
                        hash: None,
                    });
                    continue;
                }
            };
            let canonical =
                match self.manifest_path(&manifest.name, &manifest.version, &manifest.integrity) {
                    Ok(canonical) => canonical,
                    Err(error) => {
                        findings.push(finding(
                            "invalid_manifest",
                            error.to_string(),
                            &path.display().to_string(),
                            &manifest,
                            None,
                        ));
                        continue;
                    }
                };
            if canonical != path {
                findings.push(finding(
                    "invalid_manifest",
                    "manifest identity does not match its on-disk path".into(),
                    &path.display().to_string(),
                    &manifest,
                    None,
                ));
                continue;
            }
            manifests.push(manifest);
        }
        manifests.sort_by(|left, right| {
            (&left.name, &left.version, &left.integrity).cmp(&(
                &right.name,
                &right.version,
                &right.integrity,
            ))
        });
        findings.sort_by(|left, right| (&left.path, &left.code).cmp(&(&right.path, &right.code)));
        Ok((manifests, findings))
    }
}

fn verify_entry_path(entry: &ManifestEntry, seen: &mut HashSet<String>) -> Result<(), StoreError> {
    let normalized = validate_path(&entry.path)?;
    validate_unique_path(seen, &normalized)?;
    if let Some(target) = &entry.symlink {
        if !entry.hash.is_empty() || entry.size != 0 {
            return Err(StoreError::Invalid("symlink has file metadata".into()));
        }
        validate_symlink(&normalized, target)?;
    } else if entry.hash.len() != 128 || !entry.hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::Invalid("invalid SHA-512 blob hash".into()));
    }
    Ok(())
}

fn finding(
    code: &str,
    message: String,
    path: &str,
    manifest: &PackageManifest,
    hash: Option<&str>,
) -> VerificationFinding {
    VerificationFinding {
        code: code.into(),
        message,
        path: path.into(),
        name: Some(manifest.name.clone()),
        version: Some(manifest.version.clone()),
        integrity: Some(manifest.integrity.clone()),
        hash: hash.map(str::to_owned),
    }
}

fn file_has_hash(path: &Path, expected: &str) -> Result<bool, StoreError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha512::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()) == expected.to_ascii_lowercase())
}

fn collect_regular_files(root: &Path) -> Result<Vec<PathBuf>, StoreError> {
    let mut result = Vec::new();
    if !root.exists() {
        return Ok(result);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                result.push(entry.path());
            }
        }
    }
    result.sort();
    Ok(result)
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ManifestEntry, ReferenceRegistry};

    fn committed_store() -> (tempfile::TempDir, Store, PackageManifest) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("store")).unwrap();
        let bytes = b"package source bytes";
        let hash = store.put_blob(bytes).unwrap();
        let manifest = PackageManifest {
            name: "check-pkg".into(),
            version: "1.0.0".into(),
            integrity: format!("sha512-{}", "a".repeat(128)),
            entries: vec![ManifestEntry {
                path: "index.js".into(),
                hash,
                size: bytes.len() as u64,
                executable: false,
                symlink: None,
            }],
            package_json: None,
        };
        store.put_package_manifest(&manifest).unwrap();
        (root, store, manifest)
    }

    #[test]
    fn full_verification_detects_bit_flips_and_fix_quarantines_for_refetch() {
        let (_root, store, manifest) = committed_store();
        let blob = store
            .get_blob_path(&manifest.entries[0].hash)
            .unwrap()
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&blob, fs::Permissions::from_mode(0o644)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut permissions = fs::metadata(&blob).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&blob, permissions).unwrap();
        }
        fs::write(&blob, b"changed bytes here").unwrap();
        let report = store.verify(true).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "blob_hash_mismatch")
        );
        let repaired = store.repair_from_report(&report).unwrap();
        assert!(
            !repaired
                .findings
                .iter()
                .any(|finding| finding.code == "blob_hash_mismatch")
        );
        assert!(!store.has_package(&manifest.name, &manifest.version, &manifest.integrity));
        assert_eq!(
            fs::read_dir(store.root().join("quarantine"))
                .unwrap()
                .count(),
            2
        );
        assert!(
            ReferenceRegistry::open(store.root())
                .unwrap()
                .is_corrupt(&manifest.name, &manifest.version, &manifest.integrity)
                .unwrap()
        );
    }

    #[test]
    fn metadata_verification_detects_missing_blob_and_sweep_preserves_shared_content() {
        let (_root, store, manifest) = committed_store();
        let reference = store.package_reference(&manifest).unwrap();
        assert_eq!(reference.logical_size, manifest.entries[0].size);
        assert_eq!(reference.file_count, 1);
        let (removed, _) = store.sweep_unreferenced_blobs().unwrap();
        assert_eq!(removed, 0);
        fs::remove_file(
            store
                .get_blob_path(&manifest.entries[0].hash)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let report = store.verify(false).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "missing_blob")
        );
    }

    #[test]
    fn full_verification_detects_truncation_and_quarantines_for_refetch() {
        let (_root, store, manifest) = committed_store();
        let blob = store
            .get_blob_path(&manifest.entries[0].hash)
            .unwrap()
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&blob, fs::Permissions::from_mode(0o644)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut permissions = fs::metadata(&blob).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&blob, permissions).unwrap();
        }
        fs::write(&blob, b"short").unwrap();
        let report = store.verify(true).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "blob_size_mismatch")
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "blob_hash_mismatch")
        );
        let repaired = store.repair_from_report(&report).unwrap();
        assert!(
            !repaired
                .findings
                .iter()
                .any(|finding| finding.code == "blob_size_mismatch")
        );
        assert!(!store.has_package(&manifest.name, &manifest.version, &manifest.integrity));
        assert!(
            ReferenceRegistry::open(store.root())
                .unwrap()
                .is_corrupt(&manifest.name, &manifest.version, &manifest.integrity)
                .unwrap()
        );
    }
}
