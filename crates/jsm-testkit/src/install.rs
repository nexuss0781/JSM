use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use jsm_core::{ErrorCode, Integrity, JsmError, PackageId, PackageName};
use serde::Deserialize;
use tar::{Archive, EntryType};

use crate::validate_relative_archive_path;

#[derive(Debug, Deserialize)]
struct Packument {
    #[serde(rename = "dist-tags")]
    dist_tags: DistTags,
    versions: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct DistTags {
    latest: String,
}

/// Fetch one fixture package from a local fake registry and publish it only after SRI verification.
/// Dependency scripts are never executed. `destination` must not already exist.
pub fn install_fixture_from_registry(
    base_url: &str,
    package_name: &str,
    destination: impl AsRef<Path>,
) -> Result<PackageId, JsmError> {
    if !base_url.starts_with("http://127.0.0.1:") && !base_url.starts_with("http://localhost:") {
        return Err(JsmError::new(
            ErrorCode::Security,
            "fixture installs require a loopback registry",
        ));
    }
    let name = PackageName::new(package_name)
        .map_err(|error| JsmError::new(ErrorCode::Usage, error.to_string()))?;
    let metadata_url = format!("{base_url}/meta/{}", name.as_str());
    let metadata_response = ureq::get(&metadata_url).call().map_err(|error| {
        JsmError::caused_by(
            ErrorCode::Network,
            "failed to fetch fixture metadata",
            &error,
        )
    })?;
    let mut metadata_bytes = Vec::new();
    metadata_response
        .into_reader()
        .read_to_end(&mut metadata_bytes)
        .map_err(|error| {
            JsmError::caused_by(
                ErrorCode::Network,
                "failed to read fixture metadata",
                &error,
            )
        })?;
    let packument: Packument = serde_json::from_slice(&metadata_bytes).map_err(|error| {
        JsmError::caused_by(
            ErrorCode::Network,
            "fake registry returned invalid metadata",
            &error,
        )
    })?;
    let version = packument.dist_tags.latest;
    let dist = packument
        .versions
        .get(&version)
        .and_then(|version| version.get("dist"))
        .ok_or_else(|| {
            JsmError::new(
                ErrorCode::Network,
                "fake registry metadata has no latest dist record",
            )
        })?;
    let tarball_url = dist
        .get("tarball")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            JsmError::new(
                ErrorCode::Network,
                "fake registry metadata has no tarball URL",
            )
        })?;
    if !tarball_url.starts_with(&format!("{base_url}/tarballs/")) {
        return Err(JsmError::new(
            ErrorCode::Security,
            "fake registry returned a non-local tarball URL",
        ));
    }
    let expected_integrity = dist
        .get("integrity")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            JsmError::new(
                ErrorCode::Network,
                "fake registry metadata has no integrity value",
            )
        })?;
    let integrity = Integrity::new(expected_integrity)
        .map_err(|error| JsmError::new(ErrorCode::Integrity, error.to_string()))?;

    let response = ureq::get(tarball_url).call().map_err(|error| {
        JsmError::caused_by(
            ErrorCode::Network,
            "failed to fetch fixture tarball",
            &error,
        )
    })?;
    let mut tarball = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut tarball)
        .map_err(|error| {
            JsmError::caused_by(ErrorCode::Network, "failed to read fixture tarball", &error)
        })?;
    if !integrity.verifies(&tarball) {
        return Err(JsmError::new(
            ErrorCode::Integrity,
            "fixture tarball SHA-512 integrity mismatch",
        ));
    }

    let destination = destination.as_ref();
    if destination.exists() {
        return Err(JsmError::new(
            ErrorCode::Usage,
            "fixture destination already exists",
        ));
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        JsmError::caused_by(
            ErrorCode::Store,
            "failed to create fixture destination parent",
            &error,
        )
    })?;
    let staging = tempfile::Builder::new()
        .prefix("jsm-fixture-stage-")
        .tempdir_in(parent)
        .map_err(|error| {
            JsmError::caused_by(
                ErrorCode::Store,
                "failed to create fixture staging directory",
                &error,
            )
        })?;
    let package_root = staging.path().join("package");
    fs::create_dir(&package_root).map_err(|error| {
        JsmError::caused_by(
            ErrorCode::Store,
            "failed to create fixture package staging directory",
            &error,
        )
    })?;

    let mut archive = Archive::new(GzDecoder::new(Cursor::new(tarball)));
    let entries = archive.entries().map_err(|error| {
        JsmError::caused_by(
            ErrorCode::Integrity,
            "fixture tarball is not a valid archive",
            &error,
        )
    })?;
    let mut seen = HashSet::<PathBuf>::new();
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            JsmError::caused_by(
                ErrorCode::Integrity,
                "failed to read fixture archive entry",
                &error,
            )
        })?;
        let archive_path = entry
            .path()
            .map_err(|error| {
                JsmError::caused_by(
                    ErrorCode::Security,
                    "invalid path in fixture archive",
                    &error,
                )
            })?
            .into_owned();
        let relative = archive_path.strip_prefix("package").map_err(|_| {
            JsmError::new(
                ErrorCode::Security,
                "fixture archive entries must be under package/",
            )
        })?;
        if relative.as_os_str().is_empty() {
            if entry.header().entry_type() == EntryType::Directory {
                continue;
            }
            return Err(JsmError::new(
                ErrorCode::Security,
                "invalid package root entry",
            ));
        }
        validate_relative_archive_path(relative)
            .map_err(|error| JsmError::new(ErrorCode::Security, error.to_string()))?;
        if !seen.insert(relative.to_path_buf()) {
            return Err(JsmError::new(
                ErrorCode::Security,
                "duplicate path in fixture archive",
            ));
        }
        let output_path = package_root.join(relative);
        match entry.header().entry_type() {
            EntryType::Directory => {
                fs::create_dir_all(&output_path).map_err(|error| {
                    JsmError::caused_by(
                        ErrorCode::Store,
                        "failed to create fixture directory",
                        &error,
                    )
                })?;
            }
            EntryType::Regular | EntryType::Continuous => {
                if let Some(parent) = output_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        JsmError::caused_by(
                            ErrorCode::Store,
                            "failed to create fixture file parent",
                            &error,
                        )
                    })?;
                }
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&output_path)
                    .map_err(|error| {
                        JsmError::caused_by(
                            ErrorCode::Store,
                            "failed to create staged fixture file",
                            &error,
                        )
                    })?;
                std::io::copy(&mut entry, &mut file).map_err(|error| {
                    JsmError::caused_by(
                        ErrorCode::Integrity,
                        "failed to extract fixture file",
                        &error,
                    )
                })?;
                file.flush().map_err(|error| {
                    JsmError::caused_by(
                        ErrorCode::Store,
                        "failed to flush staged fixture file",
                        &error,
                    )
                })?;
            }
            _ => {
                return Err(JsmError::new(
                    ErrorCode::Security,
                    "links and special files are forbidden in fixture archives",
                ));
            }
        }
    }
    if !package_root.join("package.json").is_file() {
        return Err(JsmError::new(
            ErrorCode::Integrity,
            "fixture archive has no package.json",
        ));
    }
    fs::rename(&package_root, destination).map_err(|error| {
        JsmError::caused_by(
            ErrorCode::Store,
            "failed to publish verified fixture",
            &error,
        )
    })?;
    let version = jsm_core::Version::parse(&version).map_err(|error| {
        JsmError::caused_by(
            ErrorCode::Network,
            "fake registry returned an invalid version",
            &error,
        )
    })?;
    Ok(PackageId::new(name, version, integrity))
}
