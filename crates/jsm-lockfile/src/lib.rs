//! Deterministic `jsm.lock` format (SPECS.md §1.10 and Appendix A).
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};
use thiserror::Error;

pub const CRATE_NAME: &str = "jsm-lockfile";
pub const LEGACY_LOCKFILE_VERSION: u64 = 1;
pub const CURRENT_LOCKFILE_VERSION: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Importer {
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub dev_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub optional_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub resolved_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub resolved_dev_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub resolved_optional_dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Package {
    pub resolution: String,
    pub integrity: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// Peer package name to the exact resolved peer instance key.
    #[serde(default)]
    pub peer_context: BTreeMap<String, String>,
    #[serde(default)]
    pub peer_context_hash: String,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub optional_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub peer_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub peer_dependencies_meta: BTreeMap<String, String>,
    #[serde(default)]
    pub engines: BTreeMap<String, String>,
    #[serde(default)]
    pub os: Vec<String>,
    #[serde(default)]
    pub cpu: Vec<String>,
    #[serde(default)]
    pub libc: Vec<String>,
    #[serde(default)]
    pub has_scripts: bool,
    #[serde(default)]
    pub provenance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    pub lockfile_version: u64,
    pub generated_by: String,
    #[serde(default)]
    pub importers: BTreeMap<String, Importer>,
    #[serde(default)]
    pub packages: BTreeMap<String, Package>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self::new("jsm 0.1.0")
    }
}

impl Lockfile {
    pub fn new(generated_by: impl Into<String>) -> Self {
        Self {
            lockfile_version: CURRENT_LOCKFILE_VERSION,
            generated_by: generated_by.into(),
            importers: BTreeMap::new(),
            packages: BTreeMap::new(),
        }
    }
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        self.serialize()
    }
    pub fn serialize(&self) -> String {
        let mut out = format!(
            "lockfile_version = {}\ngenerated_by = {}\n",
            self.lockfile_version,
            quote(&self.generated_by)
        );
        for (path, i) in &self.importers {
            out.push_str(&format!("\n[importers.{}]\n", quote(path)));
            emit_map(&mut out, "dependencies", &i.dependencies);
            emit_map(&mut out, "dev_dependencies", &i.dev_dependencies);
            emit_map(&mut out, "optional_dependencies", &i.optional_dependencies);
            emit_map(&mut out, "resolved_dependencies", &i.resolved_dependencies);
            emit_map(
                &mut out,
                "resolved_dev_dependencies",
                &i.resolved_dev_dependencies,
            );
            emit_map(
                &mut out,
                "resolved_optional_dependencies",
                &i.resolved_optional_dependencies,
            );
        }
        for (id, p) in &self.packages {
            out.push_str(&format!("\n[packages.{}]\n", quote(id)));
            out.push_str(&format!(
                "resolution = {}\nintegrity = {}\n",
                quote(&p.resolution),
                quote(&p.integrity)
            ));
            if self.lockfile_version >= 2 {
                out.push_str(&format!(
                    "name = {}\nversion = {}\n",
                    quote(&p.name),
                    quote(&p.version)
                ));
                emit_map(&mut out, "peer_context", &p.peer_context);
                out.push_str(&format!(
                    "peer_context_hash = {}\n",
                    quote(&p.peer_context_hash)
                ));
            }
            emit_map(&mut out, "dependencies", &p.dependencies);
            emit_map(&mut out, "optional_dependencies", &p.optional_dependencies);
            emit_map(&mut out, "peer_dependencies", &p.peer_dependencies);
            emit_map(
                &mut out,
                "peer_dependencies_meta",
                &p.peer_dependencies_meta,
            );
            emit_map(&mut out, "engines", &p.engines);
            emit_array(&mut out, "os", &p.os);
            emit_array(&mut out, "cpu", &p.cpu);
            emit_array(&mut out, "libc", &p.libc);
            out.push_str(&format!("has_scripts = {}\n", p.has_scripts));
            if let Some(v) = &p.provenance {
                out.push_str(&format!("provenance = {}\n", quote(v)));
            }
        }
        out
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(input: &str) -> Result<Self, LockfileError> {
        parse(input)
    }
    pub fn deserialize(input: &str) -> Result<Self, LockfileError> {
        Self::from_str(input)
    }
    pub fn read(path: impl AsRef<Path>) -> Result<Self, LockfileError> {
        Self::from_str(&fs::read_to_string(path)?)
    }
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), LockfileError> {
        fs::write(path, self.serialize()).map_err(LockfileError::Io)
    }
    pub fn importer_is_stale(&self, path: &str, manifest: &Importer) -> bool {
        self.importers.get(path) != Some(manifest)
    }
    pub fn is_stale(&self, manifests: &BTreeMap<String, Importer>) -> bool {
        self.importers.len() != manifests.len()
            || self.importers.iter().any(|(path, locked)| {
                let Some(manifest) = manifests.get(path) else {
                    return true;
                };
                locked.dependencies != manifest.dependencies
                    || locked.dev_dependencies != manifest.dev_dependencies
                    || locked.optional_dependencies != manifest.optional_dependencies
            })
    }
    pub fn validate_version(&self) -> Result<(), LockfileError> {
        if self.lockfile_version > CURRENT_LOCKFILE_VERSION {
            return Err(LockfileError::NewerMajor {
                found: self.lockfile_version,
                supported: CURRENT_LOCKFILE_VERSION,
            });
        }
        if self.lockfile_version == LEGACY_LOCKFILE_VERSION {
            return Ok(());
        }
        for (key, package) in &self.packages {
            if package.name.is_empty() || package.version.is_empty() {
                return Err(LockfileError::InvalidPackageKey(key.clone()));
            }
            let expected_hash = peer_context_hash(&package.peer_context);
            if package.peer_context_hash != expected_hash
                || package_instance_key(
                    &package.name,
                    &package.version,
                    &package.integrity,
                    &package.peer_context,
                ) != *key
            {
                return Err(LockfileError::InvalidPackageKey(key.clone()));
            }
        }
        Ok(())
    }

    /// Convert an unambiguous peer-free v1 graph to v2 package-instance keys.
    /// The returned value is in-memory only; callers decide whether it is safe
    /// to persist (frozen installs must not rewrite the source lockfile).
    pub fn migrate_peer_free_v1(&self) -> Result<Self, LockfileError> {
        if self.lockfile_version != LEGACY_LOCKFILE_VERSION {
            return Ok(self.clone());
        }
        if self.packages.values().any(|package| {
            !package.peer_dependencies.is_empty()
                || !package.peer_dependencies_meta.is_empty()
                || !package.peer_context.is_empty()
        }) {
            return Err(LockfileError::PeerContextUpgrade);
        }

        let mut migrated = self.clone();
        let mut key_map = BTreeMap::new();
        for (old_key, package) in &self.packages {
            let (name, version) = split_legacy_package_key(old_key)
                .ok_or_else(|| LockfileError::InvalidPackageKey(old_key.clone()))?;
            key_map.insert(
                old_key.clone(),
                package_instance_key(name, version, &package.integrity, &BTreeMap::new()),
            );
        }
        for importer in migrated.importers.values_mut() {
            remap_values(&mut importer.resolved_dependencies, &key_map);
            remap_values(&mut importer.resolved_dev_dependencies, &key_map);
            remap_values(&mut importer.resolved_optional_dependencies, &key_map);
        }
        let mut packages = BTreeMap::new();
        for (old_key, mut package) in std::mem::take(&mut migrated.packages) {
            let (name, version) = split_legacy_package_key(&old_key)
                .ok_or_else(|| LockfileError::InvalidPackageKey(old_key.clone()))?;
            package.name = name.to_owned();
            package.version = version.to_owned();
            package.peer_context.clear();
            package.peer_context_hash = peer_context_hash(&package.peer_context);
            remap_values(&mut package.dependencies, &key_map);
            remap_values(&mut package.optional_dependencies, &key_map);
            let new_key = key_map
                .get(&old_key)
                .cloned()
                .ok_or_else(|| LockfileError::InvalidPackageKey(old_key.clone()))?;
            packages.insert(new_key, package);
        }
        migrated.packages = packages;
        migrated.lockfile_version = CURRENT_LOCKFILE_VERSION;
        migrated.validate_version()?;
        Ok(migrated)
    }
    pub fn load(
        path: impl AsRef<Path>,
        policy: LockfilePolicy,
        manifests: &BTreeMap<String, Importer>,
    ) -> Result<Option<Self>, LockfileError> {
        if policy.no_lockfile {
            return Ok(None);
        }
        let path = path.as_ref();
        if !path.exists() {
            if policy.frozen {
                return Err(LockfileError::FrozenWouldChange {
                    reason: "lockfile is missing".into(),
                });
            }
            return Ok(None);
        }
        let lock = Self::read(path)?;
        if lock.is_stale(manifests) {
            if policy.frozen {
                return Err(LockfileError::FrozenWouldChange {
                    reason: "manifest importer specifications differ".into(),
                });
            }
            return Err(LockfileError::Stale {
                reason: "manifest importer specifications differ".into(),
            });
        }
        Ok(Some(lock))
    }
    pub fn save(
        &self,
        path: impl AsRef<Path>,
        policy: LockfilePolicy,
    ) -> Result<(), LockfileError> {
        if policy.no_lockfile {
            return Ok(());
        }
        if policy.frozen {
            return Err(LockfileError::FrozenWouldChange {
                reason: "writing a lockfile is disabled in frozen mode".into(),
            });
        }
        self.write(path)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockfilePolicy {
    pub frozen: bool,
    pub no_lockfile: bool,
}

#[derive(Debug, Error)]
pub enum LockfileError {
    #[error(
        "JSM_E_LOCKFILE: lockfile format major version {found} is newer than supported {supported}"
    )]
    NewerMajor { found: u64, supported: u64 },
    #[error("JSM_E_LOCKFILE: lockfile is stale: {reason}")]
    Stale { reason: String },
    #[error("JSM_E_LOCKFILE: frozen lockfile would change: {reason}")]
    FrozenWouldChange { reason: String },
    #[error(
        "JSM_E_LOCKFILE: v1 lockfile contains peer metadata; re-resolve it without --frozen-lockfile to migrate peer contexts"
    )]
    PeerContextUpgrade,
    #[error("JSM_E_LOCKFILE: invalid v2 package-instance key `{0}`")]
    InvalidPackageKey(String),
    #[error("JSM_E_LOCKFILE: invalid lockfile: {0}")]
    Parse(String),
    #[error("JSM_E_LOCKFILE: {0}")]
    Io(#[from] std::io::Error),
}

fn quote(v: &str) -> String {
    serde_json::to_string(v).expect("string serialization cannot fail")
}
fn emit_map(out: &mut String, name: &str, map: &BTreeMap<String, String>) {
    out.push_str(name);
    out.push_str(" = {");
    for (n, v) in map {
        out.push_str(&format!(" {} = {},", quote(n), quote(v)));
    }
    if !map.is_empty() {
        out.pop();
    }
    out.push_str(" }\n");
}
fn emit_array(out: &mut String, name: &str, values: &[String]) {
    out.push_str(name);
    out.push_str(" = [");
    for v in values {
        out.push_str(&format!(" {},", quote(v)));
    }
    if !values.is_empty() {
        out.pop();
    }
    out.push_str(" ]\n");
}

/// SHA-256 of compact JSON encoding of a UTF-8-byte-sorted peer binding map.
pub fn peer_context_hash(context: &BTreeMap<String, String>) -> String {
    let canonical = serde_json::to_vec(context).expect("peer context serialization cannot fail");
    format!("{:x}", Sha256::digest(canonical))
}

/// Construct the opaque v2 package-instance key.
pub fn package_instance_key(
    name: &str,
    version: &str,
    integrity: &str,
    peer_context: &BTreeMap<String, String>,
) -> String {
    format!(
        "{name}@{version}#integrity={integrity}#peer={}",
        peer_context_hash(peer_context)
    )
}

fn split_legacy_package_key(key: &str) -> Option<(&str, &str)> {
    let (name, version) = key.rsplit_once('@')?;
    (!name.is_empty() && !version.is_empty()).then_some((name, version))
}

fn remap_values(map: &mut BTreeMap<String, String>, key_map: &BTreeMap<String, String>) {
    for value in map.values_mut() {
        if let Some(new_key) = key_map.get(value) {
            *value = new_key.clone();
        }
    }
}

fn parse(input: &str) -> Result<Lockfile, LockfileError> {
    let mut lock = Lockfile::default();
    let mut section = String::new();
    for raw in input.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                return Err(LockfileError::Parse("unterminated section".into()));
            }
            section = line[1..line.len() - 1].to_string();
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| LockfileError::Parse(format!("expected key = value: {line}")))?;
        let key = key.trim();
        let value = value.trim();
        if section.is_empty() {
            match key {
                "lockfile_version" => {
                    lock.lockfile_version = value
                        .parse()
                        .map_err(|_| LockfileError::Parse("invalid lockfile_version".into()))?
                }
                "generated_by" => lock.generated_by = string(value)?,
                _ => return Err(LockfileError::Parse(format!("unknown top-level key {key}"))),
            }
            continue;
        }
        let (kind, id) = section
            .split_once('.')
            .ok_or_else(|| LockfileError::Parse("invalid section".into()))?;
        let id = string(id)?;
        match kind {
            "importers" => {
                let i = lock.importers.entry(id).or_default();
                parse_importer(i, key, value)?;
            }
            "packages" => {
                let p = lock.packages.entry(id).or_default();
                parse_package(p, key, value)?;
            }
            _ => return Err(LockfileError::Parse(format!("unknown section {kind}"))),
        }
    }
    if lock.lockfile_version == LEGACY_LOCKFILE_VERSION {
        for (key, package) in &mut lock.packages {
            let (name, version) = split_legacy_package_key(key)
                .ok_or_else(|| LockfileError::InvalidPackageKey(key.clone()))?;
            package.name = name.to_owned();
            package.version = version.to_owned();
        }
    }
    lock.validate_version()?;
    Ok(lock)
}
fn string(v: &str) -> Result<String, LockfileError> {
    serde_json::from_str(v)
        .map_err(|_| LockfileError::Parse(format!("expected quoted string: {v}")))
}
fn map(v: &str) -> Result<BTreeMap<String, String>, LockfileError> {
    let v = v.trim();
    if v == "{}" {
        return Ok(BTreeMap::new());
    }
    if !v.starts_with('{') || !v.ends_with('}') {
        return Err(LockfileError::Parse("expected inline map".into()));
    }
    let mut m = BTreeMap::new();
    for part in v[1..v.len() - 1]
        .split(',')
        .filter(|x| !x.trim().is_empty())
    {
        let (k, x) = part
            .split_once('=')
            .ok_or_else(|| LockfileError::Parse("invalid map entry".into()))?;
        m.insert(string(k.trim())?, string(x.trim())?);
    }
    Ok(m)
}
fn array(v: &str) -> Result<Vec<String>, LockfileError> {
    let v = v.trim();
    if !v.starts_with('[') || !v.ends_with(']') {
        return Err(LockfileError::Parse("expected array".into()));
    }
    v[1..v.len() - 1]
        .split(',')
        .filter(|x| !x.trim().is_empty())
        .map(|x| string(x.trim()))
        .collect()
}
fn parse_importer(i: &mut Importer, k: &str, v: &str) -> Result<(), LockfileError> {
    match k {
        "dependencies" => i.dependencies = map(v)?,
        "dev_dependencies" => i.dev_dependencies = map(v)?,
        "optional_dependencies" => i.optional_dependencies = map(v)?,
        "resolved_dependencies" => i.resolved_dependencies = map(v)?,
        "resolved_dev_dependencies" => i.resolved_dev_dependencies = map(v)?,
        "resolved_optional_dependencies" => i.resolved_optional_dependencies = map(v)?,
        _ => return Err(LockfileError::Parse(format!("unknown importer field {k}"))),
    }
    Ok(())
}
fn parse_package(p: &mut Package, k: &str, v: &str) -> Result<(), LockfileError> {
    match k {
        "resolution" => p.resolution = string(v)?,
        "integrity" => p.integrity = string(v)?,
        "name" => p.name = string(v)?,
        "version" => p.version = string(v)?,
        "peer_context" => p.peer_context = map(v)?,
        "peer_context_hash" => p.peer_context_hash = string(v)?,
        "dependencies" => p.dependencies = map(v)?,
        "optional_dependencies" => p.optional_dependencies = map(v)?,
        "peer_dependencies" => p.peer_dependencies = map(v)?,
        "peer_dependencies_meta" => p.peer_dependencies_meta = map(v)?,
        "engines" => p.engines = map(v)?,
        "os" => p.os = array(v)?,
        "cpu" => p.cpu = array(v)?,
        "libc" => p.libc = array(v)?,
        "has_scripts" => p.has_scripts = v == "true",
        "provenance" => p.provenance = Some(string(v)?),
        _ => return Err(LockfileError::Parse(format!("unknown package field {k}"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package(name: &str, version: &str, integrity: &str) -> (String, Package) {
        let peer_context = BTreeMap::new();
        let package = Package {
            resolution: format!("https://x/{name}.tgz"),
            integrity: integrity.into(),
            name: name.into(),
            version: version.into(),
            peer_context_hash: peer_context_hash(&peer_context),
            peer_context,
            ..Default::default()
        };
        let key = package_instance_key(name, version, integrity, &package.peer_context);
        (key, package)
    }
    #[test]
    fn shuffled_input_is_stable() {
        let mut a = Lockfile::new("jsm 1");
        let (z_key, z) = package("z", "1.0.0", "sha512-z");
        let (a_key, a_package) = package("a", "1.0.0", "sha512-a");
        a.packages.insert(z_key.clone(), z.clone());
        a.packages.insert(a_key.clone(), a_package.clone());
        let mut b = Lockfile::new("jsm 1");
        b.packages.insert(a_key, a_package);
        b.packages.insert(z_key, z);
        assert_eq!(a.serialize(), b.serialize());
    }
    #[test]
    fn round_trip() {
        let mut l = Lockfile::new("jsm test");
        let mut i = Importer::default();
        i.dependencies.insert("a".into(), "^1".into());
        l.importers.insert(".".into(), i);
        let (key, package) = package("a", "1.0.0", "sha512-a");
        l.packages.insert(key, package);
        assert_eq!(Lockfile::from_str(&l.serialize()).unwrap(), l);
    }
    #[test]
    fn stale_add_remove_range() {
        let mut l = Lockfile::default();
        let mut i = Importer::default();
        i.dependencies.insert("a".into(), "^1".into());
        l.importers.insert(".".into(), i.clone());
        assert!(!l.importer_is_stale(".", &i));
        i.dependencies.insert("b".into(), "^1".into());
        assert!(l.importer_is_stale(".", &i));
        i.dependencies.remove("a");
        assert!(l.importer_is_stale(".", &i));
        i.dependencies.insert("a".into(), "^2".into());
        assert!(l.importer_is_stale(".", &i));
    }
    #[test]
    fn rejects_newer_major() {
        let l = Lockfile {
            lockfile_version: 1001,
            ..Default::default()
        };
        let e = Lockfile::from_str(&l.serialize()).unwrap_err();
        assert!(matches!(e, LockfileError::NewerMajor { .. }));
    }

    #[test]
    fn resolved_ids_do_not_make_lockfile_stale() {
        let mut lock = Lockfile::default();
        let mut locked = Importer::default();
        locked.dependencies.insert("a".into(), "^1".into());
        locked
            .resolved_dependencies
            .insert("a".into(), "a@1.2.0".into());
        lock.importers.insert(".".into(), locked);

        let mut manifest = Importer::default();
        manifest.dependencies.insert("a".into(), "^1".into());
        let mut manifests = BTreeMap::new();
        manifests.insert(".".into(), manifest.clone());
        assert!(!lock.is_stale(&manifests));

        manifest.dependencies.insert("a".into(), "^2".into());
        manifests.insert(".".into(), manifest);
        assert!(lock.is_stale(&manifests));
    }
}

#[cfg(test)]
mod v2_tests {
    use super::*;

    #[test]
    fn peer_context_hashes_are_stable_and_disambiguate_instances() {
        let first = BTreeMap::from([("host".into(), "host@1.0.0#peer=one".into())]);
        let same = BTreeMap::from([("host".into(), "host@1.0.0#peer=one".into())]);
        let other = BTreeMap::from([("host".into(), "host@2.0.0#peer=two".into())]);
        assert_eq!(peer_context_hash(&first), peer_context_hash(&same));
        assert_ne!(peer_context_hash(&first), peer_context_hash(&other));
        assert_ne!(
            package_instance_key("widget", "1.0.0", "sha512-content", &first),
            package_instance_key("widget", "1.0.0", "sha512-content", &other)
        );
        assert_eq!(
            package_instance_key("widget", "1.0.0", "sha512-content", &BTreeMap::new()),
            package_instance_key("widget", "1.0.0", "sha512-content", &BTreeMap::new())
        );
    }

    #[test]
    fn peer_free_v1_migrates_and_remaps_all_instance_references() {
        let source = r#"
lockfile_version = 1
generated_by = "jsm 0.1.0"

[importers."."]
dependencies = { "a" = "^1" }
resolved_dependencies = { "a" = "a@1.0.0" }

[packages."a@1.0.0"]
resolution = "https://registry.invalid/a.tgz"
integrity = "sha512-a"
dependencies = { "b" = "b@2.0.0" }

[packages."b@2.0.0"]
resolution = "https://registry.invalid/b.tgz"
integrity = "sha512-b"
"#;
        let legacy = Lockfile::from_str(source).unwrap();
        assert_eq!(legacy.lockfile_version, LEGACY_LOCKFILE_VERSION);
        assert_eq!(legacy.packages["a@1.0.0"].name, "a");
        assert_eq!(legacy.packages["a@1.0.0"].version, "1.0.0");

        let migrated = legacy.migrate_peer_free_v1().unwrap();
        let empty = BTreeMap::new();
        let a_key = package_instance_key("a", "1.0.0", "sha512-a", &empty);
        let b_key = package_instance_key("b", "2.0.0", "sha512-b", &empty);
        assert_eq!(migrated.lockfile_version, CURRENT_LOCKFILE_VERSION);
        assert_eq!(migrated.importers["."].resolved_dependencies["a"], a_key);
        assert_eq!(migrated.packages[&a_key].dependencies["b"], b_key);
        assert!(migrated.validate_version().is_ok());
    }

    #[test]
    fn v1_with_peer_metadata_requires_unfrozen_reresolution() {
        let source = r#"
lockfile_version = 1
generated_by = "jsm 0.1.0"

[packages."plugin@1.0.0"]
resolution = "https://registry.invalid/plugin.tgz"
integrity = "sha512-plugin"
peer_dependencies = { "host" = "^1" }
"#;
        let legacy = Lockfile::from_str(source).unwrap();
        assert!(matches!(
            legacy.migrate_peer_free_v1(),
            Err(LockfileError::PeerContextUpgrade)
        ));
    }

    #[test]
    fn v2_parser_rejects_tampered_peer_context_hashes() {
        let peer_context = BTreeMap::new();
        let key = package_instance_key("a", "1.0.0", "sha512-a", &peer_context);
        let mut package = Package {
            resolution: "https://registry.invalid/a.tgz".into(),
            integrity: "sha512-a".into(),
            name: "a".into(),
            version: "1.0.0".into(),
            peer_context,
            peer_context_hash: "tampered".into(),
            ..Default::default()
        };
        package.dependencies.insert("b".into(), "b@2.0.0".into());
        let mut lock = Lockfile::new("jsm test");
        lock.packages.insert(key, package);
        assert!(matches!(
            Lockfile::from_str(&lock.serialize()),
            Err(LockfileError::InvalidPackageKey(_))
        ));
    }
}
