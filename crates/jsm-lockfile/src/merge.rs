//! Verification and deterministic three-way merging for `jsm.lock` (SPECS.md §2.9).
//!
//! This module is intentionally separate from `lib.rs`: the parent module can expose it with
//! `mod merge; pub use merge::{merge_lockfiles, verify_lockfile};`.

use super::{Importer, Lockfile, LockfileError, Package, package_instance_key};
use std::collections::{BTreeMap, BTreeSet};

/// Validate lockfile structure and, when supplied, its importer specifications.
pub fn verify_lockfile(
    lock: &Lockfile,
    manifests: &BTreeMap<String, Importer>,
) -> Result<(), LockfileError> {
    lock.validate_version()?;

    for (key, package) in &lock.packages {
        if package.integrity.trim().is_empty() || package.resolution.trim().is_empty() {
            return Err(invalid(format!(
                "package `{key}` has an empty resolution or integrity"
            )));
        }
        if lock.lockfile_version >= 2
            && package_instance_key(
                &package.name,
                &package.version,
                &package.integrity,
                &package.peer_context,
            ) != *key
        {
            return Err(LockfileError::InvalidPackageKey(key.clone()));
        }
        for (field, refs) in [
            ("dependencies", &package.dependencies),
            ("optional_dependencies", &package.optional_dependencies),
        ] {
            for (name, target) in refs {
                if !lock.packages.contains_key(target) {
                    return Err(invalid(format!(
                        "package `{key}` {field} entry `{name}` references missing package `{target}`"
                    )));
                }
            }
        }
    }
    for (path, importer) in &lock.importers {
        for (field, refs) in [
            ("resolved_dependencies", &importer.resolved_dependencies),
            (
                "resolved_dev_dependencies",
                &importer.resolved_dev_dependencies,
            ),
            (
                "resolved_optional_dependencies",
                &importer.resolved_optional_dependencies,
            ),
        ] {
            for (name, target) in refs {
                if !lock.packages.contains_key(target) {
                    return Err(invalid(format!(
                        "importer `{path}` {field} entry `{name}` references missing package `{target}`"
                    )));
                }
            }
        }
    }
    if lock.is_stale(manifests) {
        return Err(LockfileError::Stale {
            reason: "importer specifications differ from manifests".into(),
        });
    }
    Ok(())
}

/// Deterministically merge two lockfiles against their common ancestor.
///
/// Importer and package records are merged by key. Within a record, map fields are merged by
/// entry, allowing independent dependency additions; a change to the same scalar or map entry
/// on both sides is a conflict. Deletion versus any edit is never silently discarded.
pub fn merge_lockfiles(
    base: &Lockfile,
    ours: &Lockfile,
    theirs: &Lockfile,
) -> Result<Lockfile, LockfileError> {
    if ours.lockfile_version != theirs.lockfile_version
        && ours.lockfile_version != base.lockfile_version
        && theirs.lockfile_version != base.lockfile_version
    {
        return Err(conflict("lockfile_version"));
    }
    let lockfile_version = merge_scalar(
        "lockfile_version",
        &base.lockfile_version,
        &ours.lockfile_version,
        &theirs.lockfile_version,
    )?;
    let generated_by = merge_scalar::<String>(
        "generated_by",
        &base.generated_by,
        &ours.generated_by,
        &theirs.generated_by,
    )?;
    let importers = merge_importer_records(&base.importers, &ours.importers, &theirs.importers)?;
    let packages = merge_records(
        &base.packages,
        &ours.packages,
        &theirs.packages,
        "package",
        merge_package,
    )?;

    let merged = Lockfile {
        lockfile_version,
        generated_by,
        importers,
        packages,
    };
    // This catches malformed identities and ensures the result is safe to serialize. Manifest
    // staleness cannot be checked here because this API deliberately has no manifest argument.
    merged.validate_version()?;
    verify_structure_only(&merged)?;
    Ok(merged)
}

fn merge_importer_records(
    base: &BTreeMap<String, Importer>,
    ours: &BTreeMap<String, Importer>,
    theirs: &BTreeMap<String, Importer>,
) -> Result<BTreeMap<String, Importer>, LockfileError> {
    let keys: BTreeSet<_> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .cloned()
        .collect();
    let empty = Importer::default();
    let mut result = BTreeMap::new();
    for key in keys {
        match (base.get(&key), ours.get(&key), theirs.get(&key)) {
            (Some(b), Some(o), Some(t)) => {
                result.insert(key.clone(), merge_importer(&key, b, o, t)?);
            }
            (Some(b), Some(o), None) if o != b => {
                return Err(conflict(format!(
                    "importer `{key}` deleted on theirs but edited on ours"
                )));
            }
            (Some(b), None, Some(t)) if t != b => {
                return Err(conflict(format!(
                    "importer `{key}` deleted on ours but edited on theirs"
                )));
            }
            (None, Some(o), Some(t)) => {
                result.insert(key.clone(), merge_importer(&key, &empty, o, t)?);
            }
            (None, Some(o), None) => {
                result.insert(key.clone(), o.clone());
            }
            (None, None, Some(t)) => {
                result.insert(key.clone(), t.clone());
            }
            _ => {}
        }
    }
    Ok(result)
}

fn merge_records<T: Clone + PartialEq>(
    base: &BTreeMap<String, T>,
    ours: &BTreeMap<String, T>,
    theirs: &BTreeMap<String, T>,
    kind: &str,
    merge: impl Fn(&str, &T, &T, &T) -> Result<T, LockfileError>,
) -> Result<BTreeMap<String, T>, LockfileError> {
    let keys: BTreeSet<_> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .cloned()
        .collect();
    let mut result = BTreeMap::new();
    for key in keys {
        match (base.get(&key), ours.get(&key), theirs.get(&key)) {
            (Some(b), Some(o), Some(t)) => {
                result.insert(key.clone(), merge(&key, b, o, t)?);
            }
            (Some(b), Some(o), None) => {
                if o != b {
                    return Err(conflict(format!(
                        "{kind} `{key}` deleted on theirs but edited on ours"
                    )));
                }
            }
            (Some(b), None, Some(t)) => {
                if t != b {
                    return Err(conflict(format!(
                        "{kind} `{key}` deleted on ours but edited on theirs"
                    )));
                }
            }
            (None, Some(o), Some(t)) => {
                if o == t {
                    result.insert(key.clone(), o.clone());
                } else {
                    return Err(conflict(format!("different additions for {kind} `{key}`")));
                }
            }
            (None, Some(o), None) => {
                result.insert(key.clone(), o.clone());
            }
            (None, None, Some(t)) => {
                result.insert(key.clone(), t.clone());
            }
            (None, None, None) | (Some(_), None, None) => {}
        }
    }
    Ok(result)
}

fn merge_importer(
    path: &str,
    b: &Importer,
    o: &Importer,
    t: &Importer,
) -> Result<Importer, LockfileError> {
    Ok(Importer {
        dependencies: merge_map(
            &format!("importer `{path}` dependencies"),
            &b.dependencies,
            &o.dependencies,
            &t.dependencies,
        )?,
        dev_dependencies: merge_map(
            &format!("importer `{path}` dev_dependencies"),
            &b.dev_dependencies,
            &o.dev_dependencies,
            &t.dev_dependencies,
        )?,
        optional_dependencies: merge_map(
            &format!("importer `{path}` optional_dependencies"),
            &b.optional_dependencies,
            &o.optional_dependencies,
            &t.optional_dependencies,
        )?,
        resolved_dependencies: merge_map(
            &format!("importer `{path}` resolved_dependencies"),
            &b.resolved_dependencies,
            &o.resolved_dependencies,
            &t.resolved_dependencies,
        )?,
        resolved_dev_dependencies: merge_map(
            &format!("importer `{path}` resolved_dev_dependencies"),
            &b.resolved_dev_dependencies,
            &o.resolved_dev_dependencies,
            &t.resolved_dev_dependencies,
        )?,
        resolved_optional_dependencies: merge_map(
            &format!("importer `{path}` resolved_optional_dependencies"),
            &b.resolved_optional_dependencies,
            &o.resolved_optional_dependencies,
            &t.resolved_optional_dependencies,
        )?,
    })
}

fn merge_package(
    key: &str,
    b: &Package,
    o: &Package,
    t: &Package,
) -> Result<Package, LockfileError> {
    let result = Package {
        resolution: merge_scalar(
            &format!("package `{key}` resolution"),
            &b.resolution,
            &o.resolution,
            &t.resolution,
        )?,
        integrity: merge_scalar(
            &format!("package `{key}` integrity"),
            &b.integrity,
            &o.integrity,
            &t.integrity,
        )?,
        name: merge_scalar(&format!("package `{key}` name"), &b.name, &o.name, &t.name)?,
        version: merge_scalar(
            &format!("package `{key}` version"),
            &b.version,
            &o.version,
            &t.version,
        )?,
        peer_context: merge_map(
            &format!("package `{key}` peer_context"),
            &b.peer_context,
            &o.peer_context,
            &t.peer_context,
        )?,
        peer_context_hash: merge_scalar(
            &format!("package `{key}` peer_context_hash"),
            &b.peer_context_hash,
            &o.peer_context_hash,
            &t.peer_context_hash,
        )?,
        dependencies: merge_map(
            &format!("package `{key}` dependencies"),
            &b.dependencies,
            &o.dependencies,
            &t.dependencies,
        )?,
        optional_dependencies: merge_map(
            &format!("package `{key}` optional_dependencies"),
            &b.optional_dependencies,
            &o.optional_dependencies,
            &t.optional_dependencies,
        )?,
        peer_dependencies: merge_map(
            &format!("package `{key}` peer_dependencies"),
            &b.peer_dependencies,
            &o.peer_dependencies,
            &t.peer_dependencies,
        )?,
        peer_dependencies_meta: merge_map(
            &format!("package `{key}` peer_dependencies_meta"),
            &b.peer_dependencies_meta,
            &o.peer_dependencies_meta,
            &t.peer_dependencies_meta,
        )?,
        engines: merge_map(
            &format!("package `{key}` engines"),
            &b.engines,
            &o.engines,
            &t.engines,
        )?,
        os: merge_scalar(&format!("package `{key}` os"), &b.os, &o.os, &t.os)?,
        cpu: merge_scalar(&format!("package `{key}` cpu"), &b.cpu, &o.cpu, &t.cpu)?,
        libc: merge_scalar(&format!("package `{key}` libc"), &b.libc, &o.libc, &t.libc)?,
        has_scripts: merge_scalar(
            &format!("package `{key}` has_scripts"),
            &b.has_scripts,
            &o.has_scripts,
            &t.has_scripts,
        )?,
        provenance: merge_scalar(
            &format!("package `{key}` provenance"),
            &b.provenance,
            &o.provenance,
            &t.provenance,
        )?,
    };
    // A v2 key is an identity, not merely a map key: reject an attempted identity rewrite.
    if !result.name.is_empty()
        && package_instance_key(
            &result.name,
            &result.version,
            &result.integrity,
            &result.peer_context,
        ) != key
    {
        return Err(conflict(format!(
            "conflicting same package identity `{key}`"
        )));
    }
    Ok(result)
}

fn merge_map(
    path: &str,
    base: &BTreeMap<String, String>,
    ours: &BTreeMap<String, String>,
    theirs: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, LockfileError> {
    let keys: BTreeSet<_> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .cloned()
        .collect();
    let mut out = BTreeMap::new();
    for key in keys {
        match (base.get(&key), ours.get(&key), theirs.get(&key)) {
            (Some(b), Some(o), Some(t)) => {
                out.insert(
                    key.clone(),
                    merge_scalar(&format!("{path} `{key}`"), b, o, t)?,
                );
            }
            (Some(b), Some(o), None) if o != b => {
                return Err(conflict(format!(
                    "{path} `{key}` deleted on theirs but edited on ours"
                )));
            }
            (Some(b), None, Some(t)) if t != b => {
                return Err(conflict(format!(
                    "{path} `{key}` deleted on ours but edited on theirs"
                )));
            }
            (Some(_), Some(_), None) | (Some(_), None, Some(_)) | (Some(_), None, None) => {}
            (None, Some(o), Some(t)) if o == t => {
                out.insert(key.clone(), o.clone());
            }
            (None, Some(_), Some(_)) => {
                return Err(conflict(format!("conflicting additions at {path} `{key}`")));
            }
            (None, Some(o), None) => {
                out.insert(key.clone(), o.clone());
            }
            (None, None, Some(t)) => {
                out.insert(key.clone(), t.clone());
            }
            (None, None, None) => {}
        }
    }
    Ok(out)
}

fn merge_scalar<T: Clone + PartialEq>(
    path: &str,
    base: &T,
    ours: &T,
    theirs: &T,
) -> Result<T, LockfileError> {
    if ours == theirs {
        Ok(ours.clone())
    } else if ours == base {
        Ok(theirs.clone())
    } else if theirs == base {
        Ok(ours.clone())
    } else {
        Err(conflict(path))
    }
}

fn verify_structure_only(lock: &Lockfile) -> Result<(), LockfileError> {
    for (key, package) in &lock.packages {
        if package.integrity.trim().is_empty() || package.resolution.trim().is_empty() {
            return Err(invalid(format!(
                "package `{key}` has an empty resolution or integrity"
            )));
        }
        for target in package
            .dependencies
            .values()
            .chain(package.optional_dependencies.values())
        {
            if !lock.packages.contains_key(target) {
                return Err(invalid(format!(
                    "package `{key}` references missing package `{target}`"
                )));
            }
        }
    }
    Ok(())
}

fn conflict(path: impl Into<String>) -> LockfileError {
    LockfileError::Parse(format!("merge conflict at {}", path.into()))
}
fn invalid(reason: impl Into<String>) -> LockfileError {
    LockfileError::Parse(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock() -> Lockfile {
        Lockfile::new("test")
    }
    fn package(name: &str, version: &str) -> (String, Package) {
        let mut p = Package {
            resolution: format!("https://registry.invalid/{name}-{version}.tgz"),
            integrity: format!("sha512-{name}-{version}"),
            name: name.into(),
            version: version.into(),
            ..Default::default()
        };
        p.peer_context_hash = super::super::peer_context_hash(&p.peer_context);
        let key = package_instance_key(name, version, &p.integrity, &p.peer_context);
        (key, p)
    }

    #[test]
    fn parallel_independent_additions_union() {
        let base = lock();
        let mut ours = base.clone();
        let mut theirs = base.clone();
        let (ka, pa) = package("a", "1.0.0");
        let (kb, pb) = package("b", "1.0.0");
        ours.packages.insert(ka, pa);
        theirs.packages.insert(kb, pb);
        let merged = merge_lockfiles(&base, &ours, &theirs).unwrap();
        assert_eq!(merged.packages.len(), 2);
    }

    #[test]
    fn same_importer_disjoint_dependency_additions_merge() {
        let base = lock();
        let mut ours = base.clone();
        let mut theirs = base.clone();
        ours.importers
            .entry(".".into())
            .or_default()
            .dependencies
            .insert("a".into(), "^1".into());
        theirs
            .importers
            .entry(".".into())
            .or_default()
            .dev_dependencies
            .insert("b".into(), "^1".into());
        let merged = merge_lockfiles(&base, &ours, &theirs).unwrap();
        assert_eq!(merged.importers["."].dependencies["a"], "^1");
        assert_eq!(merged.importers["."].dev_dependencies["b"], "^1");
    }

    #[test]
    fn identical_changes_are_accepted() {
        let base = lock();
        let mut ours = base.clone();
        let mut theirs = base.clone();
        ours.generated_by = "same".into();
        theirs.generated_by = "same".into();
        assert_eq!(merge_lockfiles(&base, &ours, &theirs).unwrap(), ours);
    }

    #[test]
    fn delete_vs_edit_is_a_conflict() {
        let mut base = lock();
        base.importers.insert(".".into(), Importer::default());
        let mut ours = base.clone();
        ours.importers
            .get_mut(".")
            .unwrap()
            .dependencies
            .insert("a".into(), "^2".into());
        let theirs = lock();
        assert!(merge_lockfiles(&base, &ours, &theirs).is_err());
    }

    #[test]
    fn conflicting_same_package_identity_is_rejected() {
        let (key, p) = package("a", "1.0.0");
        let mut base = lock();
        base.packages.insert(key.clone(), p);
        let mut ours = base.clone();
        let mut theirs = base.clone();
        ours.packages.get_mut(&key).unwrap().resolution = "ours".into();
        theirs.packages.get_mut(&key).unwrap().resolution = "theirs".into();
        let err = merge_lockfiles(&base, &ours, &theirs)
            .unwrap_err()
            .to_string();
        assert!(err.contains(&key));
    }
}
