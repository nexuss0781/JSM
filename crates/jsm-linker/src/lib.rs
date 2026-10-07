//! Isolated `node_modules` linker (SPECS.md §1.11–1.12).
//!
//! Package bytes are materialized unchanged: package shebangs are preserved and
//! are never rewritten by the linker.
use jsm_lockfile::{Importer, Lockfile};
use jsm_store::{Store, StoreError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use thiserror::Error;

pub const CRATE_NAME: &str = "jsm-linker";

#[derive(Debug, Clone)]
pub struct LinkOptions {
    pub importer: String,
    pub include_dev: bool,
    pub settings: BTreeMap<String, String>,
}
impl Default for LinkOptions {
    fn default() -> Self {
        Self {
            importer: ".".into(),
            include_dev: true,
            settings: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkState {
    pub lockfile_hash: String,
    pub linker: String,
    pub settings: BTreeMap<String, String>,
    pub packages: Vec<String>,
    #[serde(default)]
    pub direct_dependencies: BTreeMap<String, String>,
}
#[derive(Debug, Error)]
pub enum LinkError {
    #[error("linker I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid lockfile package `{0}`")]
    InvalidPackage(String),
    #[error("missing package `{0}`")]
    MissingPackage(String),
    #[error("unsafe path: {0}")]
    UnsafePath(String),
    #[error("invalid package metadata for `{0}`")]
    Metadata(String),
    #[error("link transaction failed: {0}")]
    Transaction(String),
}

/// Link the selected importer into `project/node_modules`. The operation stages all
/// files before publishing, so a failed install leaves the previous tree untouched.
pub fn link(
    project: impl AsRef<Path>,
    store: &Store,
    lock: &Lockfile,
    options: LinkOptions,
) -> Result<LinkState, LinkError> {
    let project = project.as_ref();
    ensure_safe_dir(project)?;
    let importer = lock
        .importers
        .get(&options.importer)
        .ok_or_else(|| LinkError::MissingPackage(format!("importer {}", options.importer)))?;
    let direct = direct_dependencies(importer, options.include_dev);
    let mut selected = BTreeSet::new();
    let mut queue: Vec<(String, String)> = direct
        .iter()
        .map(|(n, id)| (n.clone(), id.clone()))
        .collect();
    while let Some((name, id)) = queue.pop() {
        let key = package_key(&name, &id)?;
        if !selected.insert(key.clone()) {
            continue;
        }
        let p = lock
            .packages
            .get(&key)
            .or_else(|| lock.packages.get(&id))
            .ok_or_else(|| LinkError::MissingPackage(key.clone()))?;
        for (dep, dep_id) in p.dependencies.iter().chain(p.optional_dependencies.iter()) {
            queue.push((dep.clone(), dep_id.clone()));
        }
    }
    let nm = project.join("node_modules");
    ensure_safe_dir(project)?;
    fs::create_dir_all(&nm)?;
    let jsm = nm.join(".jsm");
    ensure_safe_dir(&jsm)?;
    let old_state = if jsm.join("state.json").is_file() {
        Some(serde_json::from_slice::<LinkState>(&fs::read(
            jsm.join("state.json"),
        )?)?)
    } else {
        None
    };
    let previous_direct = old_state
        .as_ref()
        .map(|state| state.direct_dependencies.clone())
        .unwrap_or_default();
    let mut published_direct = BTreeMap::new();
    for (name, id) in &direct {
        let key = package_key(name, id)?;
        if selected.contains(&key) {
            published_direct.insert(name.clone(), id.clone());
        }
    }
    let txn = project.join(format!(".jsm-link-txn-{}", std::process::id()));
    if txn.exists() {
        fs::remove_dir_all(&txn)?;
    }
    fs::create_dir_all(&txn)?;
    let result = build_tree(
        &txn,
        &jsm,
        store,
        lock,
        &selected,
        &published_direct,
        &options,
    );
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&txn);
        return Err(e);
    }
    let backup = project.join(format!(".jsm-old-{}", std::process::id()));
    let hash = hash_lock(lock);
    let state = LinkState {
        lockfile_hash: hash,
        linker: "isolated".into(),
        settings: options.settings.clone(),
        packages: selected.iter().cloned().collect(),
        direct_dependencies: published_direct.clone(),
    };
    if let Err(error) = atomic_json(&txn.join("state.json"), &state) {
        let _ = fs::remove_dir_all(&txn);
        return Err(error);
    }
    if let Err(error) = validate_top_level(
        &nm,
        &txn,
        lock,
        &previous_direct,
        &published_direct,
        &selected,
    ) {
        let _ = fs::remove_dir_all(&txn);
        return Err(error);
    }
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if jsm.exists() {
        fs::rename(&jsm, &backup)?;
    }
    if let Err(e) = fs::rename(&txn, &jsm) {
        let _ = fs::remove_dir_all(&txn);
        if backup.exists() {
            let _ = fs::rename(&backup, &jsm);
        }
        return Err(e.into());
    }
    let top_result = publish_top_level(
        &nm,
        &jsm,
        &previous_direct,
        &published_direct,
        lock,
        &selected,
    );
    if let Err(e) = top_result {
        let _ = fs::remove_dir_all(&jsm);
        if backup.exists() {
            let _ = fs::rename(&backup, &jsm);
            if let Some(old) = &old_state {
                let old_selected = old.packages.iter().cloned().collect::<BTreeSet<_>>();
                let _ = publish_top_level(
                    &nm,
                    &jsm,
                    &published_direct,
                    &old.direct_dependencies,
                    lock,
                    &old_selected,
                );
            }
        }
        return Err(e);
    }
    // The new tree is committed at this point. Orphan cleanup is deliberately
    // best-effort: a cleanup error must not turn a successful commit into an
    // error (or make the caller believe the old tree was restored).
    if backup.exists() {
        let _ = fs::remove_dir_all(backup);
    }
    Ok(state)
}

fn build_tree(
    root: &Path,
    final_root: &Path,
    store: &Store,
    lock: &Lockfile,
    selected: &BTreeSet<String>,
    direct: &BTreeMap<String, String>,
    options: &LinkOptions,
) -> Result<(), LinkError> {
    for key in selected {
        let p = lock
            .packages
            .get(key)
            .ok_or_else(|| LinkError::MissingPackage(key.clone()))?;
        let (name, version) = package_identity(key, p)?;
        let integrity = &p.integrity;
        let manifest = store
            .get_package_manifest(&name, &version, integrity)?
            .ok_or_else(|| LinkError::MissingPackage(key.clone()))?;
        let dest = instance_path(root, key).join("node_modules").join(&name);
        safe_join(root, &dest)?;
        if instance_path(final_root, key).exists() {
            copy_tree(&instance_path(final_root, key), &instance_path(root, key))?;
        } else {
            fs::create_dir_all(&dest)?;
        }
        let (private_bin_paths, private_bin_package) = match &manifest.package_json {
            Some(package_json) => bin_private_paths(package_json, &name, &manifest)?,
            // If package metadata is unavailable, fail safe: no file in the
            // package may be hardlinked before later bin processing.
            None => (BTreeSet::new(), true),
        };
        materialize_manifest_with_private_paths(
            store,
            &manifest,
            &dest,
            &private_bin_paths,
            private_bin_package,
        )?;
        prune_unlisted_entries(&dest, &manifest)?;
        let deps = p.dependencies.iter().chain(p.optional_dependencies.iter());
        for (dep, id) in deps {
            let dep_key = package_key(dep, id)?;
            if selected.contains(&dep_key) {
                let target = instance_path(final_root, &dep_key)
                    .join("node_modules")
                    .join(dep);
                let edge = dest.join("node_modules").join(dep);
                safe_join(root, &edge)?;
                fs::create_dir_all(edge.parent().unwrap())?;
                remove_existing(&edge)?;
                symlink_dir(&target, &edge)?;
            }
        }
        let _ = (direct, options);
    }
    write_bins(root, final_root, lock, selected, direct)?;
    Ok(())
}
#[cfg(test)]
fn materialize_manifest(
    store: &Store,
    manifest: &jsm_store::PackageManifest,
    dest: &Path,
) -> Result<(), LinkError> {
    materialize_manifest_with_private_paths(store, manifest, dest, &BTreeSet::new(), false)
}

fn materialize_manifest_with_private_paths(
    store: &Store,
    manifest: &jsm_store::PackageManifest,
    dest: &Path,
    private_paths: &BTreeSet<PathBuf>,
    private_package: bool,
) -> Result<(), LinkError> {
    for e in &manifest.entries {
        // Validate the manifest path independently of the destination root. This
        // catches platform-specific separators (notably `\\` on Unix) before
        // any filesystem operation can interpret them.
        validate_relative(&e.path)?;
        let out = safe_join(dest, &dest.join(&e.path))?;
        reject_symlink_ancestors(dest, &out)?;
        if let Some(target) = &e.symlink {
            validate_relative(target)?;
            remove_existing(&out)?;
            symlink_file(Path::new(target), &out)?;
            continue;
        }
        let blob = store
            .get_blob_path(&e.hash)?
            .ok_or_else(|| LinkError::MissingPackage(e.path.clone()))?;
        // A hardlink is safe only after the CAS source has been independently
        // verified and confirmed readonly. This also makes a corrupted or
        // accidentally mutable store fail closed before it becomes visible.
        verify_cas_blob(&blob, &e.hash, e.size)?;
        let private_output = private_package || private_paths.contains(Path::new(&e.path));
        if !private_output && out.is_file() && file_matches_hash(&out, &e.hash, e.size)? {
            continue;
        }
        remove_existing(&out)?;
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        materialize_cas_file_with_hardlink(&blob, &out, e.executable, !private_output)?;
        if e.executable {
            set_exec(&out)?;
        }
    }
    if !dest.join("package.json").exists()
        && let Some(v) = &manifest.package_json
    {
        atomic_json(&dest.join("package.json"), v)?;
    }
    Ok(())
}

fn bin_private_paths(
    package_json: &serde_json::Value,
    package_name: &str,
    manifest: &jsm_store::PackageManifest,
) -> Result<(BTreeSet<PathBuf>, bool), LinkError> {
    let mut paths = BTreeSet::new();
    let mut private_package = false;
    let mut add_path = |path: &str| -> Result<(), LinkError> {
        let normalized = normalize_bin_path(path)?;
        if manifest
            .entries
            .iter()
            .any(|entry| Path::new(&entry.path) == normalized && entry.symlink.is_some())
        {
            // A bin symlink may cause set_exec to follow its target. Make all
            // files private in this uncommon case rather than risk chmod on CAS.
            private_package = true;
        }
        paths.insert(normalized);
        Ok(())
    };

    if let Some(bin) = package_json.get("bin") {
        if let Some(path) = bin.as_str() {
            add_path(path)?;
        } else if let Some(entries) = bin.as_object() {
            for path in entries.values() {
                add_path(
                    path.as_str()
                        .ok_or_else(|| LinkError::Metadata(package_name.to_owned()))?,
                )?;
            }
        } else if !bin.is_null() {
            return Err(LinkError::Metadata(package_name.to_owned()));
        }
    }

    if package_json
        .get("bin")
        .is_none_or(serde_json::Value::is_null)
        && let Some(directory) = package_json
            .get("directories")
            .and_then(|directories| directories.get("bin"))
    {
        let directory = normalize_bin_path(
            directory
                .as_str()
                .ok_or_else(|| LinkError::Metadata(package_name.to_owned()))?,
        )?;
        for entry in &manifest.entries {
            let path = Path::new(&entry.path);
            if path.parent() == Some(directory.as_path()) {
                if entry.symlink.is_some() {
                    private_package = true;
                }
                paths.insert(path.to_path_buf());
            }
        }
    }
    Ok((paths, private_package))
}

/// Remove files left by an older package version. Staging starts from the prior
/// instance to make unchanged entries incremental, so deletion must be explicit.
fn prune_unlisted_entries(
    dest: &Path,
    manifest: &jsm_store::PackageManifest,
) -> Result<(), LinkError> {
    let mut keep = BTreeSet::new();
    for entry in &manifest.entries {
        validate_relative(&entry.path)?;
        keep.insert(PathBuf::from(&entry.path));
    }
    // package.json is synthesized when present in the committed manifest and is
    // always package metadata rather than an arbitrary stale output.
    keep.insert(PathBuf::from("package.json"));
    fn visit(root: &Path, current: &Path, keep: &BTreeSet<PathBuf>) -> Result<(), LinkError> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .map_err(|_| LinkError::UnsafePath(path.display().to_string()))?;
            let metadata = fs::symlink_metadata(&path)?;
            let is_link = is_symlink_or_junction(&path, &metadata)?;
            if metadata.is_dir() && !is_link {
                visit(root, &path, keep)?;
                if fs::read_dir(&path)?.next().is_none() && !keep.iter().any(|k| k.starts_with(rel))
                {
                    fs::remove_dir(&path)?;
                }
            } else if !keep.contains(rel) {
                remove_existing(&path)?;
            }
        }
        Ok(())
    }
    visit(dest, dest, &keep)
}
fn publish_top_level(
    nm: &Path,
    jsm: &Path,
    previous_direct: &BTreeMap<String, String>,
    direct: &BTreeMap<String, String>,
    lock: &Lockfile,
    selected: &BTreeSet<String>,
) -> Result<(), LinkError> {
    for name in previous_direct
        .keys()
        .filter(|name| !direct.contains_key(*name))
    {
        remove_managed_package_link(nm, name, true)?;
    }
    for (name, id) in direct {
        validate_relative(name)?;
        let key = package_key(name, id)?;
        if !selected.contains(&key) {
            continue;
        }
        let package = lock
            .packages
            .get(&key)
            .ok_or_else(|| LinkError::MissingPackage(key.clone()))?;
        let (package_name, _) = package_identity(&key, package)?;
        let target = instance_path(jsm, &key)
            .join("node_modules")
            .join(package_name);
        let out = nm.join(name);
        remove_managed_package_link(nm, name, previous_direct.contains_key(name))?;
        if let Some(p) = out.parent() {
            fs::create_dir_all(p)?;
        }
        symlink_dir(&target, &out)?;
    }
    let bins = nm.join(".bin");
    if let Ok(metadata) = fs::symlink_metadata(&bins) {
        if is_symlink_or_junction(&bins, &metadata)? {
            remove_existing(&bins)?;
        } else {
            return Err(LinkError::Transaction(
                "refusing to replace unmanaged node_modules/.bin".into(),
            ));
        }
    }
    symlink_dir(&jsm.join(".bin"), &bins)?;
    Ok(())
}

fn validate_top_level(
    nm: &Path,
    staged_jsm: &Path,
    lock: &Lockfile,
    previous_direct: &BTreeMap<String, String>,
    direct: &BTreeMap<String, String>,
    selected: &BTreeSet<String>,
) -> Result<(), LinkError> {
    for name in previous_direct.keys().chain(direct.keys()) {
        validate_relative(name)?;
        let path = nm.join(name);
        if let Some(parent) = path.parent() {
            reject_symlink_ancestors(nm, parent)?;
        }
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && !is_symlink_or_junction(&path, &metadata)?
            && !metadata.is_dir()
        {
            return Err(LinkError::Transaction(format!(
                "refusing to replace unmanaged path {}",
                path.display()
            )));
        }
    }
    let bins = nm.join(".bin");
    if let Ok(metadata) = fs::symlink_metadata(&bins)
        && !is_symlink_or_junction(&bins, &metadata)?
    {
        return Err(LinkError::Transaction(
            "refusing to replace unmanaged node_modules/.bin".into(),
        ));
    }
    validate_bins(staged_jsm, lock, selected)?;
    Ok(())
}

fn remove_managed_package_link(
    nm: &Path,
    name: &str,
    previously_owned: bool,
) -> Result<(), LinkError> {
    validate_relative(name)?;
    let path = nm.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if is_symlink_or_junction(&path, &metadata)? {
                let target = read_link_target(&path)?;
                let target = if target.is_absolute() {
                    target
                } else {
                    nm.join(target)
                };
                let points_into_jsm = target.starts_with(nm.join(".jsm"));
                if !previously_owned && !points_into_jsm {
                    return Err(LinkError::Transaction(format!(
                        "refusing to replace unmanaged link {}",
                        path.display()
                    )));
                }
                remove_existing(&path)?;
            } else if previously_owned && metadata.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                return Err(LinkError::Transaction(format!(
                    "refusing to replace unmanaged path {}",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = path.parent()
        && parent != nm
        && parent.starts_with(nm)
        && fs::read_dir(parent)?.next().is_none()
    {
        fs::remove_dir(parent)?;
    }
    Ok(())
}
fn write_bins(
    root: &Path,
    final_root: &Path,
    lock: &Lockfile,
    selected: &BTreeSet<String>,
    direct: &BTreeMap<String, String>,
) -> Result<(), LinkError> {
    let bin_dir = root.join(".bin");
    fs::create_dir_all(&bin_dir)?;
    let mut winners = BTreeMap::<String, (bool, String, PathBuf)>::new();
    let mut conflicts = BTreeSet::<(String, String, String)>::new();
    for key in selected {
        let package = lock
            .packages
            .get(key)
            .ok_or_else(|| LinkError::MissingPackage(key.clone()))?;
        let (name, _) = package_identity(key, package)?;
        let staged_target = instance_path(root, key).join("node_modules").join(&name);
        let target = instance_path(final_root, key)
            .join("node_modules")
            .join(&name);
        let meta = staged_target.join("package.json");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&meta).map_err(|_| LinkError::Metadata(key.clone()))?)
                .map_err(|_| LinkError::Metadata(key.clone()))?;
        let mut bins = bin_entries(&value, &name, &staged_target)?;
        bins.sort();
        for (bin, rel) in bins {
            validate_bin_name(&bin)?;
            let relative = normalize_bin_path(&rel)?;
            if !staged_target.join(&relative).is_file() {
                continue;
            }
            set_exec(&staged_target.join(&relative))?;
            let candidate = (
                direct.contains_key(&name),
                key.clone(),
                target.join(relative),
            );
            let replace = select_bin_winner(&winners, &bin, &candidate);
            if let Some((_, existing, _)) = winners.get(&bin) {
                let winner = if replace { &candidate.1 } else { existing };
                let loser = if replace { existing } else { &candidate.1 };
                conflicts.insert((bin.clone(), winner.clone(), loser.clone()));
            }
            if replace {
                winners.insert(bin, (candidate.0, candidate.1, candidate.2));
            }
        }
    }
    // `selected` and the maps are ordered, and conflicts are emitted only after
    // selection, making diagnostics stable regardless of filesystem ordering.
    for (bin, winner, loser) in conflicts {
        eprintln!("warning: bin `{bin}` conflict: package `{winner}` wins over `{loser}`");
    }
    for (bin, (_, _, target)) in winners {
        let out = bin_dir.join(&bin);
        remove_existing(&out)?;
        create_bin_shims(&bin_dir, &bin, &target)?;
    }
    Ok(())
}

fn validate_bins(
    root: &Path,
    lock: &Lockfile,
    selected: &BTreeSet<String>,
) -> Result<(), LinkError> {
    for key in selected {
        let package_meta = lock
            .packages
            .get(key)
            .ok_or_else(|| LinkError::MissingPackage(key.clone()))?;
        let (name, _) = package_identity(key, package_meta)?;
        let package = instance_path(root, key).join("node_modules").join(&name);
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(package.join("package.json"))
                .map_err(|_| LinkError::Metadata(key.clone()))?,
        )
        .map_err(|_| LinkError::Metadata(key.clone()))?;
        let mut bins = bin_entries(&value, &name, &package)?;
        bins.sort();
        for (bin, rel) in bins {
            validate_bin_name(&bin)?;
            let relative = normalize_bin_path(&rel)?;
            if package.join(&relative).is_file() {
                continue;
            }
        }
    }
    Ok(())
}

fn select_bin_winner(
    winners: &BTreeMap<String, (bool, String, PathBuf)>,
    bin: &str,
    candidate: &(bool, String, PathBuf),
) -> bool {
    match winners.get(bin) {
        None => true,
        Some((direct, key, _)) => {
            (candidate.0 && !*direct) || (candidate.0 == *direct && candidate.1 < *key)
        }
    }
}

fn bin_entries(
    value: &serde_json::Value,
    name: &str,
    package_dir: &Path,
) -> Result<Vec<(String, String)>, LinkError> {
    if let Some(bin) = value.get("bin") {
        if let Some(path) = bin.as_str() {
            return Ok(vec![(default_bin_name(name), path.into())]);
        }
        if let Some(entries) = bin.as_object() {
            return entries
                .iter()
                .map(|(bin_name, path)| {
                    Ok((
                        bin_name.clone(),
                        path.as_str()
                            .ok_or_else(|| LinkError::Metadata(name.to_owned()))?
                            .to_owned(),
                    ))
                })
                .collect();
        }
        if !bin.is_null() {
            return Err(LinkError::Metadata(name.to_owned()));
        }
    }
    if let Some(directory) = value
        .get("directories")
        .and_then(|directories| directories.get("bin"))
    {
        let relative = normalize_bin_path(
            directory
                .as_str()
                .ok_or_else(|| LinkError::Metadata(name.to_owned()))?,
        )?;
        let bin_dir = package_dir.join(&relative);
        if !bin_dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut bins = Vec::new();
        for entry in fs::read_dir(&bin_dir)? {
            let entry = entry?;
            if entry.metadata()?.is_file() {
                let file_name = entry.file_name().to_string_lossy().into_owned();
                let path = relative.join(&file_name);
                bins.push((file_name, path.to_string_lossy().into_owned()));
            }
        }
        return Ok(bins);
    }
    Ok(Vec::new())
}
fn default_bin_name(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).to_owned()
}
fn direct(i: &Importer, dev: bool) -> BTreeMap<String, String> {
    let mut m = if i.resolved_dependencies.is_empty() {
        i.dependencies.clone()
    } else {
        i.resolved_dependencies.clone()
    };
    m.extend(i.resolved_optional_dependencies.clone());
    if dev {
        m.extend(if i.resolved_dev_dependencies.is_empty() {
            i.dev_dependencies.clone()
        } else {
            i.resolved_dev_dependencies.clone()
        });
    }
    m
}
fn direct_dependencies(i: &Importer, dev: bool) -> BTreeMap<String, String> {
    direct(i, dev)
}
fn package_key(name: &str, id: &str) -> Result<String, LinkError> {
    validate_package_name(name)?;
    if id.starts_with(name) && id.get(name.len()..).is_some_and(|x| x.starts_with('@')) {
        return Ok(id.into());
    }
    if id.starts_with('@') {
        return Ok(id.into());
    }
    if id
        .bytes()
        .all(|b| b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'+')
    {
        return Ok(format!("{name}@{id}"));
    }
    Ok(id.into())
}
fn package_identity(
    key: &str,
    package: &jsm_lockfile::Package,
) -> Result<(String, String), LinkError> {
    if !package.name.is_empty() && !package.version.is_empty() {
        validate_package_name(&package.name)?;
        return Ok((package.name.clone(), package.version.clone()));
    }
    let (name, version) = split_key(key).ok_or_else(|| LinkError::InvalidPackage(key.into()))?;
    validate_package_name(name)?;
    Ok((name.to_owned(), version.to_owned()))
}

fn instance_path(root: &Path, logical_key: &str) -> PathBuf {
    let mut hash = Sha256::new();
    hash.update(logical_key.as_bytes());
    root.join("instances")
        .join(format!("{:x}", hash.finalize()))
}
fn split_key(k: &str) -> Option<(&str, &str)> {
    if let Some(rest) = k.strip_prefix('@') {
        let slash = rest.find('/')? + 1;
        let at = k[slash..].find('@')? + slash;
        Some((&k[..at], &k[at + 1..]))
    } else {
        let at = k.rfind('@')?;
        Some((&k[..at], &k[at + 1..]))
    }
}
fn hash_lock(l: &Lockfile) -> String {
    let mut h = Sha256::new();
    h.update(l.serialize().as_bytes());
    format!("{:x}", h.finalize())
}
fn validate_package_name(s: &str) -> Result<(), LinkError> {
    let p = Path::new(s);
    if s.is_empty()
        || p.is_absolute()
        || s.contains('\\')
        || p.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(LinkError::UnsafePath(s.into()));
    }
    Ok(())
}

fn validate_relative(s: &str) -> Result<(), LinkError> {
    let p = Path::new(s);
    if s.is_empty()
        || p.is_absolute()
        || s.contains('\\')
        || p.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(LinkError::UnsafePath(s.into()));
    }
    Ok(())
}
fn normalize_bin_path(s: &str) -> Result<PathBuf, LinkError> {
    let path = Path::new(s);
    if path.is_absolute() || s.contains('\\') {
        return Err(LinkError::UnsafePath(s.into()));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            _ => return Err(LinkError::UnsafePath(s.into())),
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(LinkError::UnsafePath(s.into()));
    }
    Ok(normalized)
}

fn validate_bin_name(name: &str) -> Result<(), LinkError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(LinkError::UnsafePath(name.into()));
    }
    Ok(())
}

#[cfg(windows)]
fn is_junction(path: &Path) -> Result<bool, LinkError> {
    match junction::exists(path) {
        Ok(is_junction) => Ok(is_junction),
        // The crate's FSCTL probe returns ERROR_NOT_A_REPARSE_POINT (4390)
        // for ordinary directories instead of returning `Ok(false)`.
        Err(error) if error.raw_os_error() == Some(4390) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(LinkError::Io(error)),
    }
}
#[cfg(not(windows))]
fn is_junction(_path: &Path) -> Result<bool, LinkError> {
    Ok(false)
}
fn is_symlink_or_junction(path: &Path, metadata: &fs::Metadata) -> Result<bool, LinkError> {
    if metadata.file_type().is_symlink() {
        Ok(true)
    } else {
        is_junction(path)
    }
}
fn read_link_target(path: &Path) -> Result<PathBuf, LinkError> {
    if is_junction(path)? {
        #[cfg(windows)]
        return junction::get_target(path).map_err(LinkError::Io);
    }
    fs::read_link(path).map_err(LinkError::Io)
}

fn ensure_safe_dir(p: &Path) -> Result<(), LinkError> {
    if let Ok(metadata) = fs::symlink_metadata(p)
        && is_symlink_or_junction(p, &metadata)?
    {
        return Err(LinkError::UnsafePath(p.display().to_string()));
    }
    Ok(())
}
fn safe_join(root: &Path, p: &Path) -> Result<PathBuf, LinkError> {
    let rel = p
        .strip_prefix(root)
        .map_err(|_| LinkError::UnsafePath(p.display().to_string()))?;
    for c in rel.components() {
        if !matches!(c, Component::Normal(_)) {
            return Err(LinkError::UnsafePath(p.display().to_string()));
        }
    }
    reject_symlink_ancestors(root, p)?;
    Ok(p.to_path_buf())
}
fn reject_symlink_ancestors(root: &Path, p: &Path) -> Result<(), LinkError> {
    let rel = p
        .strip_prefix(root)
        .map_err(|_| LinkError::UnsafePath(p.display().to_string()))?;
    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component.as_os_str());
        if cur != p
            && let Ok(metadata) = fs::symlink_metadata(&cur)
            && is_symlink_or_junction(&cur, &metadata)?
        {
            return Err(LinkError::UnsafePath(cur.display().to_string()));
        }
    }
    Ok(())
}
fn remove_existing(p: &Path) -> Result<(), LinkError> {
    if let Ok(m) = fs::symlink_metadata(p) {
        if is_junction(p)? {
            #[cfg(windows)]
            {
                // Deleting the reparse tag leaves the empty mount-point
                // directory behind; remove it too so the path can be reused.
                junction::delete(p)?;
                fs::remove_dir(p)?;
            }
        } else if m.file_type().is_dir() && !m.file_type().is_symlink() {
            fs::remove_dir_all(p)?
        } else if m.file_type().is_symlink() && m.is_dir() {
            fs::remove_dir(p)?
        } else {
            fs::remove_file(p)?
        }
    }
    Ok(())
}
fn atomic_json<T: Serialize>(p: &Path, v: &T) -> Result<(), LinkError> {
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    fs::rename(tmp, p)?;
    Ok(())
}
fn file_matches_hash(path: &Path, expected: &str, size: u64) -> Result<bool, LinkError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() != size {
        return Ok(false);
    }
    let bytes = fs::read(path)?;
    let mut hash = Sha512::new();
    hash.update(bytes);
    Ok(format!("{:x}", hash.finalize()) == expected)
}

/// Copy an existing instance into the staging tree while preserving symlinks.
/// Individual regular files are then hash-checked by `materialize_manifest`, so
/// unchanged entries are reused and changed entries are replaced safely.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), LinkError> {
    let metadata = fs::symlink_metadata(src)?;
    if is_junction(src)? {
        let target = read_link_target(src)?;
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        symlink_dir(&target, dst)?;
    } else if metadata.file_type().is_symlink() {
        let target = read_link_target(src)?;
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        symlink_file(&target, dst)?;
    } else if metadata.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        // Existing project-visible staged trees are mutable outputs, so they
        // must never be hardlinked back to their source entries.
        copy_file(src, dst)?;
    }
    Ok(())
}
fn verify_cas_blob(src: &Path, expected: &str, size: u64) -> Result<(), LinkError> {
    let metadata = fs::metadata(src)?;
    if !metadata.permissions().readonly() {
        return Err(LinkError::Transaction(format!(
            "CAS blob is not readonly: {}",
            src.display()
        )));
    }
    if !file_matches_hash(src, expected, size)? {
        return Err(LinkError::Transaction(format!(
            "CAS blob failed integrity verification: {}",
            src.display()
        )));
    }
    Ok(())
}

/// Materialize a verified CAS package file without rewriting its bytes. The
/// order is reflink, hardlink (only for non-executable readonly CAS blobs),
/// then copy. Executable files are always reflinked or copied because changing
/// their mode after a hardlink would mutate the CAS inode.
#[cfg(test)]
fn materialize_cas_file(src: &Path, dst: &Path, executable: bool) -> Result<(), LinkError> {
    materialize_cas_file_with_hardlink(src, dst, executable, true)
}

fn materialize_cas_file_with_hardlink(
    src: &Path,
    dst: &Path,
    executable: bool,
    allow_hardlink: bool,
) -> Result<(), LinkError> {
    if try_reflink(src, dst) {
        return Ok(());
    }
    if allow_hardlink && !executable && is_readonly(src) && try_hardlink(src, dst) {
        return Ok(());
    }
    copy_file(src, dst)
}

fn is_readonly(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.permissions().readonly())
        .unwrap_or(false)
}

/// Copy a project-visible or otherwise mutable source. It deliberately omits
/// hardlinking even when the source happens to be on the same filesystem.
fn copy_file(src: &Path, dst: &Path) -> Result<(), LinkError> {
    if try_reflink(src, dst) {
        return Ok(());
    }
    fs::copy(src, dst)?;
    Ok(())
}

fn try_hardlink(src: &Path, dst: &Path) -> bool {
    match fs::hard_link(src, dst) {
        Ok(()) => true,
        Err(_) => {
            let _ = fs::remove_file(dst);
            false
        }
    }
}

fn reflink_cache() -> &'static Mutex<BTreeMap<(String, String), bool>> {
    static CACHE: OnceLock<Mutex<BTreeMap<(String, String), bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn try_reflink(src: &Path, dst: &Path) -> bool {
    let existed = fs::symlink_metadata(dst).is_ok();
    let key = reflink_key(src, dst);
    if let Some(key) = &key
        && let Ok(cache) = reflink_cache().lock()
        && cache.get(key) == Some(&false)
    {
        return false;
    }
    let success = reflink_copy::reflink(src, dst).is_ok();
    if let Some(key) = key
        && let Ok(mut cache) = reflink_cache().lock()
    {
        cache.insert(key, success);
    }
    if !success && !existed {
        // Some platform APIs may leave a partial destination on failure. The
        // stage is private, and the next hardlink/copy fallback needs a clean path.
        let _ = fs::remove_file(dst);
    }
    success
}

fn reflink_key(src: &Path, dst: &Path) -> Option<(String, String)> {
    let source_volume = volume_key(src)?;
    let target_volume = volume_key(dst.parent()?)?;
    Some((source_volume, target_volume))
}

#[cfg(unix)]
fn volume_key(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.dev().to_string())
}

#[cfg(windows)]
fn volume_key(path: &Path) -> Option<String> {
    path.components().find_map(|component| match component {
        Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().to_lowercase()),
        _ => None,
    })
}

#[cfg(not(any(unix, windows)))]
fn volume_key(path: &Path) -> Option<String> {
    path.ancestors()
        .last()
        .map(|root| root.to_string_lossy().into_owned())
}
#[cfg(unix)]
fn set_exec(p: &Path) -> Result<(), LinkError> {
    use std::os::unix::fs::PermissionsExt;
    let mut x = fs::metadata(p)?.permissions();
    x.set_mode(x.mode() | 0o111);
    fs::set_permissions(p, x)?;
    Ok(())
}
#[cfg(not(unix))]
fn set_exec(_p: &Path) -> Result<(), LinkError> {
    Ok(())
}
#[cfg(unix)]
fn symlink_dir(src: &Path, dst: &Path) -> Result<(), LinkError> {
    std::os::unix::fs::symlink(src, dst)?;
    Ok(())
}
#[cfg(not(unix))]
fn symlink_dir(src: &Path, dst: &Path) -> Result<(), LinkError> {
    junction::create(src, dst).map_err(LinkError::Io)
}
#[cfg(unix)]
fn create_bin_shims(dir: &Path, name: &str, target: &Path) -> Result<(), LinkError> {
    let out = dir.join(name);
    symlink_file(target, &out)
}
#[cfg(windows)]
fn create_bin_shims(dir: &Path, name: &str, target: &Path) -> Result<(), LinkError> {
    let target = target.display().to_string();
    let powershell_target = target.replace("'", "''");
    fs::write(
        dir.join(format!("{name}.cmd")),
        format!("@echo off\r\nnode \"{target}\" %*\r\n"),
    )?;
    fs::write(
        dir.join(format!("{name}.ps1")),
        format!(
            "$target = '{powershell_target}'\r\n& node -- $target @args\r\nexit $LASTEXITCODE\r\n"
        ),
    )?;
    Ok(())
}
#[cfg(unix)]
fn symlink_file(src: &Path, dst: &Path) -> Result<(), LinkError> {
    std::os::unix::fs::symlink(src, dst)?;
    Ok(())
}
#[cfg(not(unix))]
fn symlink_file(src: &Path, dst: &Path) -> Result<(), LinkError> {
    std::os::windows::fs::symlink_file(src, dst).map_err(LinkError::Io)
}

/// Compatibility name for callers that prefer an explicit operation verb.
pub fn link_project(
    project: impl AsRef<Path>,
    store: &Store,
    lock: &Lockfile,
    options: LinkOptions,
) -> Result<LinkState, LinkError> {
    link(project, store, lock, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Output};
    #[test]
    fn package_keys() {
        assert_eq!(split_key("@a/b@1.0.0"), Some(("@a/b", "1.0.0")));
        assert_eq!(split_key("x@1"), Some(("x", "1")));
    }

    #[test]
    fn v2_keys_are_opaque_and_use_explicit_identity() {
        let key = "pkg@1.2.3#integrity=sha512-a/b+c==#peer=0123456789abcdef";
        let package = jsm_lockfile::Package {
            name: "pkg".into(),
            version: "1.2.3".into(),
            ..Default::default()
        };
        assert_eq!(
            package_identity(key, &package).unwrap(),
            ("pkg".to_owned(), "1.2.3".to_owned())
        );
        let physical = instance_path(Path::new(".jsm/instances"), key);
        assert_eq!(physical.components().count(), 4);
        assert_eq!(physical.file_name().unwrap().to_string_lossy().len(), 64);
        assert!(!physical.to_string_lossy().contains("/b+c=="));
    }

    #[test]
    fn v1_keys_fall_back_to_name_and_version() {
        let package = jsm_lockfile::Package::default();
        assert_eq!(
            package_identity("@scope/pkg@4.5.6", &package).unwrap(),
            ("@scope/pkg".to_owned(), "4.5.6".to_owned())
        );
    }
    #[test]
    fn rejects_paths() {
        assert!(validate_relative("../x").is_err());
        assert!(validate_relative("/x").is_err());
        assert!(validate_relative("dir\\file").is_err());
        assert!(validate_relative("").is_err());
    }
    #[test]
    fn validates_package_bin_paths_and_names() {
        assert_eq!(
            normalize_bin_path("./bin/tool").unwrap(),
            PathBuf::from("bin/tool")
        );
        assert!(normalize_bin_path("../../escape").is_err());
        assert!(normalize_bin_path("/absolute/tool").is_err());
        assert!(validate_bin_name("../../escape").is_err());
        assert_eq!(default_bin_name("@scope/package"), "package");
    }

    #[test]
    fn directories_bin_registers_every_immediate_file_by_filename() {
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-directories-bin",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin/nested")).unwrap();
        fs::write(root.join("bin/runner.js"), b"runner").unwrap();
        fs::write(root.join("bin/helper"), b"helper").unwrap();
        fs::write(root.join("bin/nested/ignored"), b"nested").unwrap();
        let value = serde_json::json!({"directories": {"bin": "bin"}});
        let mut bins = bin_entries(&value, "fixture", &root).unwrap();
        bins.sort();
        assert_eq!(
            bins,
            vec![
                ("helper".to_owned(), "bin/helper".to_owned()),
                ("runner.js".to_owned(), "bin/runner.js".to_owned()),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bin_conflicts_are_direct_then_lexical() {
        let mut winners = BTreeMap::new();
        let indirect = (false, "z@1.0.0".to_owned(), PathBuf::from("z"));
        assert!(select_bin_winner(&winners, "tool", &indirect));
        winners.insert("tool".to_owned(), indirect);
        let direct = (true, "z@1.0.0".to_owned(), PathBuf::from("z"));
        assert!(select_bin_winner(&winners, "tool", &direct));
        winners.insert("tool".to_owned(), direct);
        assert!(!select_bin_winner(
            &winners,
            "tool",
            &(true, "zz@1.0.0".to_owned(), PathBuf::from("zz"))
        ));
    }

    #[test]
    fn materialization_preserves_shebang_and_detects_changes() {
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-materialize",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let src = root.join("source");
        let dst = root.join("destination");
        let bytes = b"#!/usr/bin/env node\r\nconsole.log('ok');\n";
        fs::write(&src, bytes).unwrap();
        copy_file(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), bytes);
        let mut hash = Sha512::new();
        hash.update(bytes);
        assert!(
            file_matches_hash(&dst, &format!("{:x}", hash.finalize()), bytes.len() as u64).unwrap()
        );
        assert!(!file_matches_hash(&dst, &format!("{:x}", Sha512::digest(b"changed")), 7).unwrap());
        remove_existing(&dst).unwrap();
        fs::write(&src, b"changed").unwrap();
        copy_file(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"changed");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn mutable_outputs_are_not_hardlinks_to_sources() {
        use std::os::unix::fs::MetadataExt;
        let root =
            std::env::temp_dir().join(format!("jsm-linker-test-{}-isolation", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let src = root.join("source");
        let dst = root.join("destination");
        fs::write(&src, b"mutable").unwrap();
        copy_file(&src, &dst).unwrap();
        assert_ne!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&dst).unwrap().ino()
        );
        fs::write(&dst, b"changed").unwrap();
        assert_eq!(fs::read(&src).unwrap(), b"mutable");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn readonly_cas_non_executable_uses_safe_materialization() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root =
            std::env::temp_dir().join(format!("jsm-linker-test-{}-hardlink", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let src = root.join("blob");
        let dst = root.join("output");
        fs::write(&src, b"cas bytes").unwrap();
        let mut permissions = fs::metadata(&src).unwrap().permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(&src, permissions).unwrap();
        materialize_cas_file(&src, &dst, false).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"cas bytes");
        // Reflink is allowed to win for the public strategy. Exercise the
        // hardlink operation itself deterministically on the same safe source.
        let hardlink_dst = root.join("hardlink-output");
        assert!(try_hardlink(&src, &hardlink_dst));
        assert_eq!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&hardlink_dst).unwrap().ino()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn executable_cas_output_never_hardlinks_when_mode_changes() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-exec-output",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let src = root.join("blob");
        let dst = root.join("output");
        fs::write(&src, b"#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(&src).unwrap().permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(&src, permissions).unwrap();
        materialize_cas_file(&src, &dst, true).unwrap();
        set_exec(&dst).unwrap();
        assert_ne!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&dst).unwrap().ino()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn chmodding_bin_output_never_mutates_readonly_cas_blob() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-bin-cas-safety",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let store = Store::new(root.join("store")).unwrap();
        let bytes = b"#!/usr/bin/env node\n";
        let hash = store.put_blob(bytes).unwrap();
        let blob = store.get_blob_path(&hash).unwrap().unwrap();
        let before = fs::metadata(&blob).unwrap().permissions().mode() & 0o777;
        let package_json = serde_json::json!({"name": "bin-safety", "bin": {"tool": "bin/tool"}});
        let manifest = jsm_store::PackageManifest {
            name: "bin-safety".into(),
            version: "1.0.0".into(),
            integrity: "a".repeat(128),
            entries: vec![jsm_store::ManifestEntry {
                path: "bin/tool".into(),
                hash,
                size: bytes.len() as u64,
                executable: false,
                symlink: None,
            }],
            package_json: Some(package_json.clone()),
        };
        let (private_paths, private_package) =
            bin_private_paths(&package_json, "bin-safety", &manifest).unwrap();
        let destination = root.join("stage");
        fs::create_dir_all(&destination).unwrap();
        materialize_manifest_with_private_paths(
            &store,
            &manifest,
            &destination,
            &private_paths,
            private_package,
        )
        .unwrap();
        let output = destination.join("bin/tool");
        set_exec(&output).unwrap();

        let after = fs::metadata(&blob).unwrap();
        assert_eq!(after.permissions().mode() & 0o777, before);
        assert_eq!(after.permissions().mode() & 0o111, 0);
        assert_ne!(after.ino(), fs::metadata(&output).unwrap().ino());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writable_source_falls_back_to_copy_without_hardlink() {
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-copy-fallback",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let src = root.join("source");
        let dst = root.join("output");
        fs::write(&src, b"mutable source").unwrap();
        materialize_cas_file(&src, &dst, false).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"mutable source");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fallback_capability_is_platform_safe() {
        assert!(!try_reflink(Path::new("missing"), Path::new("missing")));
    }

    #[cfg(unix)]
    #[test]
    fn executable_bin_shim_invokes_target() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        let root =
            std::env::temp_dir().join(format!("jsm-linker-test-{}-exec", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target");
        fs::write(&target, b"#!/bin/sh\nprintf invoked\n").unwrap();
        let mut permissions = fs::metadata(&target).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&target, permissions).unwrap();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        create_bin_shims(&bin, "tool", &target).unwrap();
        let output = Command::new(bin.join("tool")).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"invoked");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    fn invoke_test_bin(bin_dir: &Path, name: &str, argument: &str) -> Vec<Output> {
        vec![
            Command::new(bin_dir.join(name))
                .arg(argument)
                .output()
                .unwrap(),
        ]
    }

    #[cfg(windows)]
    fn invoke_test_bin(bin_dir: &Path, name: &str, argument: &str) -> Vec<Output> {
        let cmd = Command::new("cmd.exe")
            .args(["/D", "/C", "call"])
            .arg(bin_dir.join(format!("{name}.cmd")))
            .arg(argument)
            .output()
            .unwrap();
        let powershell = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(bin_dir.join(format!("{name}.ps1")))
            .arg(argument)
            .output()
            .unwrap();
        vec![cmd, powershell]
    }

    #[test]
    fn phase1_bin_conflict_executes_the_direct_winner() {
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-phase1-bin-conflict space",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut lock = Lockfile::default();
        let mut selected = BTreeSet::new();
        for name in ["a-indirect", "z-direct"] {
            let key = format!("{name}@1.0.0");
            let package_dir = instance_path(&root, &key).join("node_modules").join(name);
            fs::create_dir_all(&package_dir).unwrap();
            fs::write(
                package_dir.join("package.json"),
                serde_json::json!({
                    "name": name,
                    "version": "1.0.0",
                    "bin": {"phase1-tool": "bin.js"}
                })
                .to_string(),
            )
            .unwrap();
            fs::write(
                package_dir.join("bin.js"),
                format!(
                    "#!/usr/bin/env node\nprocess.stdout.write('winner={name}:' + (process.argv[2] || ''));\n"
                ),
            )
            .unwrap();
            lock.packages.insert(
                key.clone(),
                jsm_lockfile::Package {
                    name: name.to_owned(),
                    version: "1.0.0".to_owned(),
                    ..Default::default()
                },
            );
            selected.insert(key);
        }

        // The direct package sorts after the indirect package, so success proves
        // direct-dependency priority wins before the lexical tie-breaker.
        let direct = BTreeMap::from([("z-direct".to_owned(), "z-direct@1.0.0".to_owned())]);
        write_bins(&root, &root, &lock, &selected, &direct).unwrap();
        let outputs = invoke_test_bin(&root.join(".bin"), "phase1-tool", "verified-arg");
        assert!(!outputs.is_empty());
        for output in outputs {
            assert!(
                output.status.success(),
                "bin launcher failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.stdout, b"winner=z-direct:verified-arg");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_bin_shims_generate_cmd_and_powershell_launchers() {
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-windows-shim",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target.js");
        fs::write(&target, b"console.log('ok');\n").unwrap();
        create_bin_shims(&root, "tool", &target).unwrap();
        let cmd = fs::read_to_string(root.join("tool.cmd")).unwrap();
        let ps1 = fs::read_to_string(root.join("tool.ps1")).unwrap();
        assert!(cmd.contains("node "));
        assert!(cmd.contains("%*"));
        assert!(ps1.contains("node "));
        assert!(ps1.contains("@args"));
        assert!(ps1.contains("exit $LASTEXITCODE"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_directory_links_use_junctions_and_cleanup_preserves_target() {
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-junction-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = root.join("target");
        let link = root.join("link");
        fs::create_dir_all(&target).unwrap();
        assert!(!is_junction(&root).unwrap());
        assert!(!is_junction(&target).unwrap());
        fs::write(target.join("sentinel"), b"target survives").unwrap();

        symlink_dir(&target, &link).unwrap();
        assert!(junction::exists(&link).unwrap());
        assert_eq!(fs::read(link.join("sentinel")).unwrap(), b"target survives");
        assert_eq!(read_link_target(&link).unwrap(), target);

        remove_existing(&link).unwrap();
        assert!(target.join("sentinel").is_file());
        assert!(fs::symlink_metadata(&link).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publish_preflight_preserves_existing_tree_on_failure() {
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-{}",
            std::process::id(),
            "preflight"
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        fs::write(root.join("node_modules/.bin/tool"), b"existing").unwrap();
        let nm = root.join("node_modules");
        let err = validate_top_level(
            &nm,
            &root.join("staged"),
            &Lockfile::default(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        )
        .unwrap_err();
        assert!(matches!(err, LinkError::Transaction(_)));
        assert_eq!(
            fs::read(root.join("node_modules/.bin/tool")).unwrap(),
            b"existing"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_update_replaces_changed_file_and_prunes_only_staged_orphans() {
        let root = std::env::temp_dir().join(format!(
            "jsm-linker-test-{}-incremental",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let previous = root.join("previous");
        let staged = root.join("staged");
        fs::create_dir_all(&previous).unwrap();
        fs::write(previous.join("index.js"), b"old content").unwrap();
        fs::write(previous.join("obsolete.js"), b"stale content").unwrap();
        copy_tree(&previous, &staged).unwrap();

        let store = Store::new(root.join("store")).unwrap();
        let changed = b"new content";
        let changed_hash = store.put_blob(changed).unwrap();
        let manifest = jsm_store::PackageManifest {
            name: "incremental-fixture".into(),
            version: "1.0.0".into(),
            integrity: "a".repeat(128),
            entries: vec![jsm_store::ManifestEntry {
                path: "index.js".into(),
                hash: changed_hash,
                size: changed.len() as u64,
                executable: false,
                symlink: None,
            }],
            package_json: None,
        };
        materialize_manifest(&store, &manifest, &staged).unwrap();
        prune_unlisted_entries(&staged, &manifest).unwrap();

        assert_eq!(fs::read(staged.join("index.js")).unwrap(), changed);
        assert!(!staged.join("obsolete.js").exists());
        assert_eq!(fs::read(previous.join("index.js")).unwrap(), b"old content");
        assert!(previous.join("obsolete.js").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
