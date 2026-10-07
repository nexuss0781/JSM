use std::{fmt, str::FromStr};

use nodejs_semver::{Range as NpmRange, Version as NpmVersion};

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

// Keep adversarial comparator/alternative growth bounded before calling the
// semver implementation. These limits are above ordinary published npm ranges.
const MAX_RANGE_ALTERNATIVES: usize = 64;
const MAX_RANGE_COMPARATOR_TOKENS: usize = 64;

fn validate_range(value: &str) -> Result<(), &'static str> {
    if value.len() > 4096 {
        return Err("exceeds the Phase 0 length limit of 4096 bytes");
    }
    if value.contains(',') {
        return Err("commas are not valid npm range separators");
    }
    if value.chars().any(char::is_control) {
        return Err("must not contain control characters");
    }
    let normalized = value.replace('\u{feff}', " ");
    if normalized
        .chars()
        .any(|character| !character.is_ascii() && !character.is_whitespace())
    {
        return Err("must use ASCII semver syntax");
    }
    let value = normalized.as_str();
    let alternatives = value.split("||").collect::<Vec<_>>();
    if alternatives.len() > MAX_RANGE_ALTERNATIVES {
        return Err("contains more than 32 disjunctive alternatives");
    }
    let mut token_count = 0;
    for token in alternatives.iter().flat_map(|arm| arm.split_whitespace()) {
        if token.len() > 256 {
            return Err("contains a token longer than 256 bytes");
        }
        token_count += 1;
        if token_count > MAX_RANGE_COMPARATOR_TOKENS {
            return Err("contains more than 64 comparator tokens");
        }
    }
    for token in value.split(|character: char| character.is_whitespace() || character == ',') {
        let token = token.trim_start_matches(|character| "^~><=".contains(character));
        let token = token.trim_start_matches(['v', 'V']);
        let version = token.split(['-', '+']).next().unwrap_or_default();
        let components = version.split('.').collect::<Vec<_>>();
        if let Some(wildcard) = components
            .iter()
            .position(|part| part.eq_ignore_ascii_case("x") || *part == "*")
            && components[wildcard + 1..]
                .iter()
                .any(|part| !part.eq_ignore_ascii_case("x") && *part != "*")
        {
            return Err("numeric components cannot follow a wildcard");
        }
    }
    let tokens = value.split_whitespace().collect::<Vec<_>>();
    if tokens.contains(&"-") && (tokens.len() != 3 || tokens[1] != "-") {
        return Err("hyphen ranges require a lower and upper bound");
    }
    Ok(())
}

enum RangeAtom {
    Any,
    Empty,
    Parsed(NpmRange),
}

fn parse_npm_range(source: &str) -> Result<(NpmRange, bool), String> {
    let arms = source.split("||").collect::<Vec<_>>();
    if arms.iter().any(|arm| arm.trim().is_empty()) {
        return Ok((NpmRange::any(), false));
    }

    let mut ranges = Vec::new();
    for arm in arms {
        let arm = arm.trim();
        let atoms = if arm.contains(" - ") {
            vec![arm.to_owned()]
        } else {
            let tokens = arm.split_whitespace().collect::<Vec<_>>();
            let mut atoms = Vec::new();
            let mut index = 0;
            while index < tokens.len() {
                let operator = tokens[index];
                if matches!(operator, ">" | ">=" | "<" | "<=" | "=" | "^" | "~" | "~>") {
                    let value = tokens
                        .get(index + 1)
                        .ok_or_else(|| "comparator is missing a version".to_string())?;
                    atoms.push(format!("{operator}{value}"));
                    index += 2;
                } else {
                    atoms.push(operator.to_owned());
                    index += 1;
                }
            }
            atoms
        };

        let mut current: Option<NpmRange> = None;
        let mut unsatisfiable = false;
        for atom in atoms {
            let parsed = match normalize_range_atom(&atom)? {
                RangeAtom::Any => continue,
                RangeAtom::Empty => {
                    unsatisfiable = true;
                    break;
                }
                RangeAtom::Parsed(parsed) => parsed,
            };
            current = match current.take() {
                Some(current) => match current.intersect(&parsed) {
                    Some(intersection) => Some(intersection),
                    None => {
                        unsatisfiable = true;
                        break;
                    }
                },
                None => Some(parsed),
            };
        }
        if !unsatisfiable {
            ranges.push(current.unwrap_or_else(NpmRange::any));
        }
    }

    if ranges.is_empty() {
        return Ok((NpmRange::any(), true));
    }
    let combined_source = ranges
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" || ");
    let combined = NpmRange::parse(&combined_source).map_err(|error| error.to_string())?;
    Ok((combined, false))
}

fn normalize_range_atom(atom: &str) -> Result<RangeAtom, String> {
    let (operator, version) = [">=", "<=", ">", "<", "^", "~>", "~", "="]
        .into_iter()
        .find_map(|operator| {
            atom.strip_prefix(operator)
                .map(|version| (operator, version))
        })
        .unwrap_or(("", atom));

    if matches!(version.to_ascii_lowercase().as_str(), "*" | "x") {
        return Ok(match operator {
            ">" | "<" => RangeAtom::Empty,
            _ => RangeAtom::Any,
        });
    }

    let normalized = match operator {
        "^" if version.starts_with('=') => format!("^{}", &version[1..]),
        "~" if version.starts_with('=') => format!("~{}", &version[1..]),
        "~>" if version.starts_with('=') => format!("~>{}", &version[1..]),
        _ => atom.to_owned(),
    };
    NpmRange::parse(&normalized)
        .map(RangeAtom::Parsed)
        .map_err(|error| error.to_string())
}

/// An npm-compatible semantic-version range. The original source is retained for
/// stable serde and Display output; matching uses node-semver-compatible rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range {
    source: String,
    parsed: NpmRange,
    empty: bool,
}

impl serde::Serialize for Range {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.source)
    }
}
impl<'de> serde::Deserialize<'de> for Range {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let source = String::deserialize(deserializer)?;
        Self::new(source).map_err(serde::de::Error::custom)
    }
}
impl Range {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let source = value.into();
        validate_range(&source)
            .map_err(|error| ValidationError(format!("invalid version range: {error}")))?;
        let normalized = source.replace('\u{feff}', " ");
        let (parsed, empty) = parse_npm_range(&normalized)
            .map_err(|error| ValidationError(format!("invalid version range: {error}")))?;
        Ok(Self {
            source,
            parsed,
            empty,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub fn matches(&self, version: &Version) -> bool {
        !self.empty
            && NpmVersion::parse(version.to_string())
                .is_ok_and(|version| self.parsed.satisfies(&version))
    }

    pub fn intersects(&self, other: &Range) -> bool {
        !self.empty && !other.empty && self.parsed.allows_any(&other.parsed)
    }

    pub fn intersection(&self, other: &Range) -> Option<Range> {
        if self.empty || other.empty {
            return None;
        }
        let parsed = self.parsed.intersect(&other.parsed)?;
        Some(Range {
            source: parsed.to_string(),
            parsed,
            empty: false,
        })
    }

    pub fn is_subset_of(&self, other: &Range) -> bool {
        self.empty
            || (!other.empty
                && (self.parsed == other.parsed || other.parsed.allows_all(&self.parsed)))
    }

    /// Alias used by resolver code for [`Range::intersects`].
    pub fn intersect(&self, other: &Range) -> bool {
        self.intersects(other)
    }

    /// Alias used by resolver code for [`Range::is_subset_of`].
    pub fn subset_of(&self, other: &Range) -> bool {
        self.is_subset_of(other)
    }

    /// Return the parser's normalized representation of this range.
    pub fn simplify(&self) -> Range {
        if self.empty {
            return self.clone();
        }
        let source = self.parsed.to_string();
        if source == self.source {
            self.clone()
        } else {
            Range {
                source,
                parsed: self.parsed.clone(),
                empty: false,
            }
        }
    }
}
impl fmt::Display for Range {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.source)
    }
}
impl AsRef<str> for Range {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl FromStr for Range {
    type Err = ValidationError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Self::new(source)
    }
}

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

    #[test]
    fn npm_range_matching_and_prerelease_rules() {
        let v = |s| Version::parse(s).unwrap();
        assert!(Range::new("^1.2.3").unwrap().matches(&v("1.9.0")));
        assert!(Range::new("^5.15.1").unwrap().matches(&v("5.15.1")));
        assert!(!Range::new("^1.2.3").unwrap().matches(&v("2.0.0")));
        assert!(Range::new("1.2.x").unwrap().matches(&v("1.2.99")));
        assert!(Range::new(">=1.2").unwrap().matches(&v("2.0.0")));
        assert!(!Range::new(">=1.2").unwrap().matches(&v("1.2.0-alpha.1")));
        assert!(
            Range::new(">=1.2.0-alpha.1 <1.3.0")
                .unwrap()
                .matches(&v("1.2.0-alpha.2"))
        );
        assert!(Range::new("1.0.0 || 2.x").unwrap().matches(&v("2.4.0")));
    }

    #[test]
    fn empty_npm_range_means_any_stable_version() {
        let range = Range::new("").unwrap();
        assert!(range.matches(&Version::parse("0.0.0").unwrap()));
        assert!(!range.matches(&Version::parse("0.0.1-alpha.1").unwrap()));
    }

    #[test]
    fn wildcard_upper_bound_overflow_is_reported_without_panicking() {
        assert!(Range::new("18446744073709551615.x").is_err());
        assert!(Range::new("1.18446744073709551615.x").is_err());
    }

    #[test]
    fn pathological_range_tokens_are_rejected_before_semver_parsing() {
        let long_invalid_token = format!("1.{}", "V".repeat(3_000));
        assert!(Range::new(long_invalid_token).is_err());

        let many_alternatives = std::iter::repeat_n("1.0.0", 65)
            .collect::<Vec<_>>()
            .join(" || ");
        assert!(Range::new(many_alternatives).is_err());

        let many_comparators = std::iter::repeat_n(">=1.0.0", 65)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(Range::new(many_comparators).is_err());

        let timeout_regression =
            include_str!("../../../fuzz/corpus/semver_range/timeout_multi_alternatives_20261007");
        assert!(Range::new(timeout_regression).is_err());
    }

    #[test]
    fn many_valid_disjunctions_parse_within_the_supported_range_budget() {
        let alternatives = std::iter::repeat_n("1.0.0", 60)
            .collect::<Vec<_>>()
            .join(" || ");
        assert!(Range::new(alternatives).is_ok());
    }

    #[test]
    fn malformed_unicode_is_rejected_before_semver_parser_and_bom_is_whitespace() {
        let malformed = format!("1.0.0{}", "�".repeat(1_000));
        let error = Range::new(malformed).unwrap_err();
        assert!(error.to_string().contains("ASCII semver syntax"));

        let source = "\u{feff}^1.2.3\u{feff}";
        let range = Range::new(source).unwrap();
        assert_eq!(range.as_str(), source);
        assert!(range.matches(&Version::parse("1.9.0").unwrap()));
    }

    #[test]
    fn npm_partial_comparator_boundaries_and_operator_spacing() {
        let v = |s| Version::parse(s).unwrap();
        assert!(Range::new("<1.2").unwrap().matches(&v("1.1.99")));
        assert!(!Range::new("<1.2").unwrap().matches(&v("1.2.0")));
        assert!(!Range::new("<1.2").unwrap().matches(&v("1.2.99")));
        assert!(Range::new("<=1.2").unwrap().matches(&v("1.2.99")));
        assert!(!Range::new("<=1.2").unwrap().matches(&v("1.3.0")));
        assert!(!Range::new(">1.2").unwrap().matches(&v("1.2.99")));
        assert!(Range::new(">1.2").unwrap().matches(&v("1.3.0")));
        assert!(Range::new(">= 1.2").unwrap().matches(&v("1.2.0")));
        assert!(Range::new(">1.2.x").unwrap().matches(&v("1.3.0")));
        assert!(!Range::new(">1.2.x").unwrap().matches(&v("1.2.99")));
        assert!(Range::new("^0").unwrap().matches(&v("0.9.0")));
        assert!(!Range::new("^0").unwrap().matches(&v("1.0.0")));
        assert!(Range::new("^0.0").unwrap().matches(&v("0.0.99")));
        assert!(!Range::new("^0.0").unwrap().matches(&v("0.1.0")));
        assert!(Range::new("~1").unwrap().matches(&v("1.9.0")));
        assert!(!Range::new("~1").unwrap().matches(&v("2.0.0")));
        assert!(Range::new("~> 1.2.3").unwrap().matches(&v("1.2.99")));
        assert!(!Range::new("~> 1.2.3").unwrap().matches(&v("1.3.0")));
        assert!(Range::new("1.x.3").is_err());
    }

    #[test]
    fn range_set_operations_are_conservative() {
        let a = Range::new(">=1.0.0 <2.0.0").unwrap();
        let b = Range::new("^1.5.0").unwrap();
        assert!(a.intersects(&b));
        assert!(b.is_subset_of(&a));
        assert!(!a.is_subset_of(&Range::new("^2.0.0").unwrap()));
        assert!(a.intersection(&Range::new("^3.0.0").unwrap()).is_none());
    }
}
