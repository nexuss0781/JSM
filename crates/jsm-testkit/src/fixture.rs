use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

use flate2::{Compression, write::GzEncoder};
use jsm_core::{Integrity, PackageId, PackageName, Version};
use serde_json::{Map, Value, json};
use tar::{Builder, Header};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error("invalid fixture value: {0}")]
    Invalid(String),
    #[error("failed to encode fixture: {0}")]
    Io(#[from] std::io::Error),
    #[error("fixture JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// An editable package fixture with stable file ordering and archive metadata.
#[derive(Debug, Clone)]
pub struct FixturePackage {
    name: PackageName,
    version: Version,
    files: BTreeMap<String, Vec<u8>>,
    dependencies: BTreeMap<String, String>,
    scripts: BTreeMap<String, String>,
}

impl FixturePackage {
    pub fn new(name: impl Into<String>, version: &str) -> Result<Self, FixtureError> {
        let name =
            PackageName::new(name).map_err(|error| FixtureError::Invalid(error.to_string()))?;
        let version =
            Version::parse(version).map_err(|error| FixtureError::Invalid(error.to_string()))?;
        let mut files = BTreeMap::new();
        files.insert("index.js".into(), b"module.exports = 'fixture';\n".to_vec());
        Ok(Self {
            name,
            version,
            files,
            dependencies: BTreeMap::new(),
            scripts: BTreeMap::new(),
        })
    }

    pub fn file(
        mut self,
        path: impl Into<String>,
        content: impl Into<Vec<u8>>,
    ) -> Result<Self, FixtureError> {
        let path = path.into();
        validate_relative_archive_path(Path::new(&path))?;
        self.files.insert(path, content.into());
        Ok(self)
    }

    /// Add an intentionally unsafe path for archive-security tests only.
    pub fn malicious_file(mut self, path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        self.files.insert(path.into(), content.into());
        self
    }

    pub fn dependency(
        mut self,
        name: impl Into<String>,
        spec: impl Into<String>,
    ) -> Result<Self, FixtureError> {
        let name = name.into();
        PackageName::new(name.clone()).map_err(|error| FixtureError::Invalid(error.to_string()))?;
        self.dependencies.insert(name, spec.into());
        Ok(self)
    }

    pub fn script(mut self, name: impl Into<String>, command: impl Into<String>) -> Self {
        self.scripts.insert(name.into(), command.into());
        self
    }

    pub fn name(&self) -> &PackageName {
        &self.name
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn build(self) -> Result<FixtureArtifact, FixtureError> {
        let mut manifest = Map::new();
        manifest.insert("name".into(), json!(self.name.as_str()));
        manifest.insert("version".into(), json!(self.version.to_string()));
        manifest.insert("main".into(), json!("index.js"));
        if !self.dependencies.is_empty() {
            manifest.insert("dependencies".into(), json!(self.dependencies));
        }
        if !self.scripts.is_empty() {
            manifest.insert("scripts".into(), json!(self.scripts));
        }
        let manifest_bytes = serde_json::to_vec_pretty(&Value::Object(manifest))?;

        let mut entries = self.files;
        entries.insert("package.json".into(), manifest_bytes);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut archive = Builder::new(&mut encoder);
            for (path, bytes) in entries {
                let archive_path = format!("package/{path}");
                let mut header = Header::new_gnu();
                if validate_relative_archive_path(Path::new(&path)).is_err() {
                    if archive_path.len() > 100 || archive_path.as_bytes().contains(&0) {
                        return Err(FixtureError::Invalid(
                            "malicious fixture path must fit the tar header name field".into(),
                        ));
                    }
                    header.set_path("package/placeholder")?;
                    let name = &mut header.as_mut_bytes()[..100];
                    name.fill(0);
                    name[..archive_path.len()].copy_from_slice(archive_path.as_bytes());
                } else {
                    header.set_path(archive_path)?;
                }
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                archive.append(&header, bytes.as_slice())?;
            }
            archive.finish()?;
        }
        let tarball = encoder.finish()?;
        let integrity = Integrity::sha512(&tarball);
        let package_id = PackageId::new(self.name, self.version, integrity.clone());
        Ok(FixtureArtifact {
            package_id,
            integrity,
            tarball,
        })
    }
}

/// Generated tarball and content identity used by the fake registry.
#[derive(Debug, Clone)]
pub struct FixtureArtifact {
    pub package_id: PackageId,
    pub integrity: Integrity,
    pub tarball: Vec<u8>,
}

/// Reject absolute paths, parent traversal, prefixes, and empty paths.
pub fn validate_relative_archive_path(path: &Path) -> Result<(), FixtureError> {
    if path.to_string_lossy().contains('\\') {
        return Err(FixtureError::Invalid(
            "archive paths may not contain backslashes".into(),
        ));
    }
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(FixtureError::Invalid(format!(
            "archive path must be relative: {}",
            path.display()
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {
                return Err(FixtureError::Invalid(
                    "archive path may not contain `.` components".into(),
                ));
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(FixtureError::Invalid(format!(
                    "unsafe archive path: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_tarball_has_reproducible_content_and_integrity() {
        let first = FixturePackage::new("fixture-pkg", "1.2.3")
            .unwrap()
            .build()
            .unwrap();
        let second = FixturePackage::new("fixture-pkg", "1.2.3")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(first.tarball, second.tarball);
        assert!(first.integrity.verifies(&first.tarball));
    }

    #[test]
    fn path_validation_rejects_escape_cases() {
        for path in ["../escape", "/absolute", "a/../../b", ""] {
            assert!(
                validate_relative_archive_path(Path::new(path)).is_err(),
                "accepted {path:?}"
            );
        }
        assert!(validate_relative_archive_path(Path::new("nested/file.js")).is_ok());
    }
}
