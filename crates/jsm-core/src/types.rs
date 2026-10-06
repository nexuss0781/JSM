use std::{fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha512};
use thiserror::Error;

/// A validation failure for a user-provided core value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct ValidationError(String);

macro_rules! string_type {
    ($name:ident, $validate:ident, $description:literal) => {
        #[doc = concat!("Validated ", $description, " string value.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Validate and construct this value.
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                $validate(&value).map_err(|reason| {
                    ValidationError(format!("invalid {}: {reason}", $description))
                })?;
                Ok(Self(value))
            }

            /// Return the canonical text supplied at construction.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }

        impl FromStr for $name {
            type Err = ValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
    };
}

fn validate_package_name(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 214 {
        return Err("must contain between 1 and 214 bytes");
    }
    if value.bytes().any(|byte| !byte.is_ascii()) {
        return Err("must be ASCII");
    }
    let (scope, name) = if let Some(scoped) = value.strip_prefix('@') {
        let (scope, name) = scoped
            .split_once('/')
            .ok_or("scoped names need @scope/name")?;
        if scope.is_empty() || name.is_empty() || name.contains('/') {
            return Err("scoped names need one non-empty scope and name");
        }
        (Some(scope), name)
    } else {
        if value.contains('/') {
            return Err("unscoped names cannot contain a slash");
        }
        (None, value)
    };
    for part in scope.into_iter().chain(std::iter::once(name)) {
        if part == "." || part == ".." || part.starts_with(['.', '_']) {
            return Err("segments cannot be . or .. or start with . or _");
        }
        if !part.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        }) {
            return Err("contains a character outside lowercase letters, digits, '-', '_' or '.'");
        }
    }
    Ok(())
}

string_type!(PackageName, validate_package_name, "package name");

fn validate_range(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() {
        return Err("must not be empty");
    }
    if value.len() > 4096 {
        return Err("exceeds the Phase 0 length limit of 4096 bytes");
    }
    if value.chars().any(char::is_control) {
        return Err("must not contain control characters");
    }
    Ok(())
}

string_type!(Range, validate_range, "version range");

fn validate_dist_tag(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 214 {
        return Err("must contain between 1 and 214 bytes");
    }
    if value.starts_with(['.', '_']) {
        return Err("must not start with . or _");
    }
    if value.chars().any(|character| {
        character.is_whitespace()
            || character.is_control()
            || matches!(character, '/' | '\\' | '@' | '#')
    }) {
        return Err("contains whitespace or a prohibited character");
    }
    Ok(())
}

string_type!(DistTag, validate_dist_tag, "distribution tag");

fn validate_integrity(value: &str) -> Result<(), &'static str> {
    let digest = value
        .strip_prefix("sha512-")
        .ok_or("expected a sha512 SRI value")?;
    let bytes = STANDARD
        .decode(digest)
        .map_err(|_| "digest is not valid base64")?;
    if bytes.len() != 64 {
        return Err("sha512 digest must contain exactly 64 bytes");
    }
    if STANDARD.encode(bytes) != digest {
        return Err("digest must use canonical padded base64");
    }
    Ok(())
}

string_type!(Integrity, validate_integrity, "integrity value");

impl Integrity {
    /// Compute SHA-512 SRI for bytes.
    pub fn sha512(bytes: &[u8]) -> Self {
        let digest = Sha512::digest(bytes);
        Self(format!("sha512-{}", STANDARD.encode(digest)))
    }

    /// Verify bytes against this integrity value.
    pub fn verifies(&self, bytes: &[u8]) -> bool {
        Self::sha512(bytes) == *self
    }
}

/// An exact semantic version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(semver::Version);

impl Version {
    /// Parse an exact SemVer version.
    pub fn parse(value: &str) -> Result<Self, semver::Error> {
        semver::Version::parse(value).map(Self)
    }

    /// Borrow the parsed SemVer value.
    pub fn as_semver(&self) -> &semver::Version {
        &self.0
    }
}

impl FromStr for Version {
    type Err = semver::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Package identity keyed by name, exact version, and tarball integrity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageId {
    name: PackageName,
    version: Version,
    integrity: Integrity,
}

impl PackageId {
    /// Construct a package identity from validated components.
    pub fn new(name: PackageName, version: Version, integrity: Integrity) -> Self {
        Self {
            name,
            version,
            integrity,
        }
    }

    pub fn name(&self) -> &PackageName {
        &self.name
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn integrity(&self) -> &Integrity {
        &self.integrity
    }
}

/// A dependency target independent of a specific manifest format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Spec {
    Registry(Range),
    Tag(DistTag),
    TarballUrl(String),
    Git {
        url: String,
        reference: Option<String>,
    },
    File {
        path: String,
    },
    Workspace(Option<Range>),
    Alias {
        name: PackageName,
        spec: Box<Spec>,
    },
}

/// A dependency alias and its requested source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencySpec {
    name: PackageName,
    spec: Spec,
}

impl DependencySpec {
    pub fn new(name: PackageName, spec: Spec) -> Self {
        Self { name, spec }
    }

    pub fn name(&self) -> &PackageName {
        &self.name
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }
}

fn validate_platform_token(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 64 {
        return Err("must contain between 1 and 64 bytes");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
    {
        return Err("must contain only lowercase ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

string_type!(PlatformToken, validate_platform_token, "platform token");

/// Typed OS, CPU, and optional libc selectors. Values remain open for new targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Platform {
    os: PlatformToken,
    cpu: PlatformToken,
    libc: Option<PlatformToken>,
}

impl Platform {
    pub fn new(
        os: impl Into<String>,
        cpu: impl Into<String>,
        libc: Option<String>,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            os: PlatformToken::new(os)?,
            cpu: PlatformToken::new(cpu)?,
            libc: libc.map(PlatformToken::new).transpose()?,
        })
    }

    pub fn os(&self) -> &PlatformToken {
        &self.os
    }

    pub fn cpu(&self) -> &PlatformToken {
        &self.cpu
    }

    pub fn libc(&self) -> Option<&PlatformToken> {
        self.libc.as_ref()
    }
}

impl<'de> Deserialize<'de> for Platform {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawPlatform {
            os: String,
            cpu: String,
            libc: Option<String>,
        }
        let raw = RawPlatform::deserialize(deserializer)?;
        Self::new(raw.os, raw.cpu, raw.libc).map_err(de::Error::custom)
    }
}

/// Node's native module ABI number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct NodeAbi(u32);

impl<'de> Deserialize<'de> for NodeAbi {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl NodeAbi {
    pub fn new(value: u32) -> Result<Self, ValidationError> {
        if value == 0 {
            return Err(ValidationError("Node ABI must be greater than zero".into()));
        }
        Ok(Self(value))
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

/// A currently known libc family. Platform uses an open token to remain forward compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Libc {
    Glibc,
    Musl,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample_integrity() -> Integrity {
        Integrity::sha512(b"fixture bytes")
    }

    #[test]
    fn validates_scoped_names_and_rejects_paths() {
        assert_eq!(
            PackageName::new("@scope/pkg").unwrap().as_str(),
            "@scope/pkg"
        );
        for invalid in [
            "",
            "../pkg",
            "pkg/name",
            "Upper",
            "@scope",
            "@/pkg",
            "@scope/../x",
        ] {
            assert!(PackageName::new(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(serde_json::from_str::<PackageName>("\"../escape\"").is_err());
    }

    #[test]
    fn validates_integrity_and_checks_bytes() {
        let integrity = sample_integrity();
        assert!(integrity.verifies(b"fixture bytes"));
        assert!(!integrity.verifies(b"different"));
        assert!(Integrity::new("sha512-AAAA").is_err());
    }

    #[test]
    fn all_persisted_core_values_round_trip() {
        let name = PackageName::new("@scope/pkg").unwrap();
        let version = Version::parse("1.2.3-beta.1+build.8").unwrap();
        let integrity = sample_integrity();
        let values = vec![
            serde_json::to_value(&name).unwrap(),
            serde_json::to_value(&version).unwrap(),
            serde_json::to_value(Range::new("^1.2.0 || ~2.0").unwrap()).unwrap(),
            serde_json::to_value(&integrity).unwrap(),
            serde_json::to_value(PackageId::new(
                name.clone(),
                version.clone(),
                integrity.clone(),
            ))
            .unwrap(),
            serde_json::to_value(DistTag::new("latest").unwrap()).unwrap(),
            serde_json::to_value(DependencySpec::new(
                name,
                Spec::Registry(Range::new("^1").unwrap()),
            ))
            .unwrap(),
            serde_json::to_value(Platform::new("linux", "x64", Some("glibc".into())).unwrap())
                .unwrap(),
            serde_json::to_value(NodeAbi::new(115).unwrap()).unwrap(),
        ];
        let decoded = (
            serde_json::from_value::<PackageName>(values[0].clone()).unwrap(),
            serde_json::from_value::<Version>(values[1].clone()).unwrap(),
            serde_json::from_value::<Range>(values[2].clone()).unwrap(),
            serde_json::from_value::<Integrity>(values[3].clone()).unwrap(),
            serde_json::from_value::<PackageId>(values[4].clone()).unwrap(),
            serde_json::from_value::<DistTag>(values[5].clone()).unwrap(),
            serde_json::from_value::<DependencySpec>(values[6].clone()).unwrap(),
            serde_json::from_value::<Platform>(values[7].clone()).unwrap(),
            serde_json::from_value::<NodeAbi>(values[8].clone()).unwrap(),
        );
        assert_eq!(serde_json::to_value(decoded.0).unwrap(), values[0]);
        assert_eq!(serde_json::to_value(decoded.1).unwrap(), values[1]);
        assert_eq!(serde_json::to_value(decoded.2).unwrap(), values[2]);
        assert_eq!(serde_json::to_value(decoded.3).unwrap(), values[3]);
        assert_eq!(serde_json::to_value(decoded.4).unwrap(), values[4]);
        assert_eq!(serde_json::to_value(decoded.5).unwrap(), values[5]);
        assert_eq!(serde_json::to_value(decoded.6).unwrap(), values[6]);
        assert_eq!(serde_json::to_value(decoded.7).unwrap(), values[7]);
        assert_eq!(serde_json::to_value(decoded.8).unwrap(), values[8]);
    }

    #[test]
    fn version_is_exact_semver_and_platform_values_are_validated() {
        assert!(Version::parse("1.2").is_err());
        assert!(Platform::new("Linux", "x64", None).is_err());
        assert!(serde_json::from_str::<NodeAbi>("0").is_err());
        assert!(
            serde_json::from_str::<Platform>(r#"{"os":"linux/../../tmp","cpu":"x64","libc":null}"#)
                .is_err()
        );
    }

    proptest! {
        #[test]
        fn package_name_parser_never_panics(value in any::<String>()) {
            let _ = PackageName::new(value);
        }
    }
}
