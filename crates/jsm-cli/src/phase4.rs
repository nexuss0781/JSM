use super::*;
use std::collections::BTreeSet;
use std::fs;

pub fn update(
    cli: &Cli,
    cwd: &Path,
    packages: Vec<String>,
    _latest: bool,
    review: bool,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    let before = phase3::lockfile_text(cwd);
    let before_lock = if cwd.join("jsm.lock").exists() {
        Lockfile::read(cwd.join("jsm.lock")).ok()
    } else {
        None
    };
    // Force re-resolution by removing the lockfile; install will resolve fresh.
    let lock_path = cwd.join("jsm.lock");
    let _ = fs::remove_file(&lock_path);
    let mut silent = cli.clone();
    silent.json = false;
    silent.quiet = true;
    install(&silent, cwd, false, false, false, cancellation, progress)?;
    let after_lock = Lockfile::read(cwd.join("jsm.lock")).ok();
    phase3::record_history(cwd, "update", before, phase3::lockfile_text(cwd));
    if review {
        let mut changes = Vec::new();
        if let (Some(before), Some(after)) = (before_lock, after_lock) {
            for (id, pkg) in &after.packages {
                if let Some(old) = before.packages.get(id) {
                    if old.version != pkg.version {
                        changes.push(serde_json::json!({"package":pkg.name,"from":old.version,"to":pkg.version}));
                        if !cli.json && !cli.quiet {
                            println!("{}: {} -> {}", pkg.name, old.version, pkg.version);
                        }
                    }
                } else {
                    changes.push(serde_json::json!({"package":pkg.name,"from":null,"to":pkg.version}));
                    if !cli.json && !cli.quiet {
                        println!("{}: added {}", pkg.name, pkg.version);
                    }
                }
            }
        }
        if cli.json {
            emit_json("update", serde_json::json!({"command":"update","review":true,"changes":changes}));
            return Ok(());
        }
    }
    if cli.json {
        emit_json("update", serde_json::json!({"command":"update","packages":packages}));
    } else if !cli.quiet {
        println!("updated dependencies");
    }
    Ok(())
}

pub fn outdated(cli: &Cli, cwd: &Path) -> Result<()> {
    let manifest = read_manifest(cwd)?;
    let lock = if cwd.join("jsm.lock").exists() {
        Some(Lockfile::read(cwd.join("jsm.lock")).into_diagnostic()?)
    } else {
        None
    };
    let config = effective_config(cli, cwd)?;
    let reg = registry(&config, cli, cwd)?;
    let mut rows = Vec::new();
    for field in ["dependencies", "devDependencies"] {
        if let Some(obj) = manifest.get(field).and_then(Value::as_object) {
            for (name, _) in obj {
                let current = lock.as_ref().and_then(|l| {
                    l.packages
                        .values()
                        .find(|p| p.name == *name)
                        .map(|p| p.version.clone())
                });
                let latest = reg
                    .packument(name)
                    .ok()
                    .and_then(|p| p.dist_tags.get("latest").cloned())
                    .unwrap_or_else(|| "unknown".into());
                rows.push(serde_json::json!({"name":name,"current":current,"latest":latest}));
                if !cli.json {
                    println!("{name}: {} -> {latest}", current.unwrap_or_else(|| "-".into()));
                }
            }
        }
    }
    if cli.json {
        emit_json("outdated", serde_json::json!({"command":"outdated","packages":rows}));
    }
    Ok(())
}

fn find_manifest(store: &Store, name: &str, version: &str) -> Result<jsm_store::PackageManifest> {
    let dir = store.root().join("packages").join(name).join(version);
    for entry in fs::read_dir(&dir).into_diagnostic()? {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("manifest") {
            let bytes = fs::read(&path).into_diagnostic()?;
            let m: jsm_store::PackageManifest = serde_json::from_slice(&bytes).into_diagnostic()?;
            return Ok(m);
        }
    }
    Err(miette!("missing manifest for {name}@{version}"))
}

pub fn diff(cli: &Cli, cwd: &Path, left: &str, right: &str) -> Result<()> {
    let store = Store::open_for_project(cwd, cli.store_dir.as_deref()).into_diagnostic()?;
    let (ln, lv) = parse_identity(left)?;
    let (rn, rv) = parse_identity(right)?;
    let lm = find_manifest(&store, &ln, &lv)?;
    let rm = find_manifest(&store, &rn, &rv)?;
    let left_files: BTreeSet<String> = lm.entries.iter().map(|e| e.path.clone()).collect();
    let right_files: BTreeSet<String> = rm.entries.iter().map(|e| e.path.clone()).collect();
    let added: Vec<&String> = right_files.difference(&left_files).collect();
    let removed: Vec<&String> = left_files.difference(&right_files).collect();
    if cli.json {
        emit_json(
            "diff",
            serde_json::json!({
                "command":"diff",
                "left":left,
                "right":right,
                "added":added,
                "removed":removed,
            }),
        );
    } else {
        for f in &added {
            println!("+ {f}");
        }
        for f in &removed {
            println!("- {f}");
        }
    }
    Ok(())
}

pub fn dupes(cli: &Cli, cwd: &Path) -> Result<()> {
    let lock = Lockfile::read(cwd.join("jsm.lock")).into_diagnostic()?;
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in lock.packages.values() {
        by_name
            .entry(p.name.clone())
            .or_default()
            .push(p.version.clone());
    }
    let mut dupes = BTreeMap::new();
    for (name, versions) in &by_name {
        let mut unique: Vec<String> = versions.clone();
        unique.sort();
        unique.dedup();
        if unique.len() > 1 {
            dupes.insert(name.clone(), unique);
        }
    }
    if cli.json {
        emit_json("dupes", serde_json::json!({"command":"dupes","packages":dupes}));
    } else {
        for (name, versions) in &dupes {
            println!("{name}: {}", versions.join(", "));
        }
    }
    Ok(())
}

pub fn unused(cli: &Cli, cwd: &Path) -> Result<()> {
    let manifest = read_manifest(cwd)?;
    let mut deps = Vec::new();
    for field in ["dependencies", "devDependencies"] {
        if let Some(obj) = manifest.get(field).and_then(Value::as_object) {
            for name in obj.keys() {
                deps.push(name.clone());
            }
        }
    }
    // Collect source files
    let mut files = Vec::new();
    collect_files(cwd, &mut files);
    let mut unused = Vec::new();
    for dep in &deps {
        let mut found = false;
        for file in &files {
            if let Ok(content) = fs::read_to_string(file)
                && (content.contains(&format!("require(\"{dep}\")"))
                    || content.contains(&format!("require('{dep}')"))
                    || content.contains(&format!("from \"{dep}\""))
                    || content.contains(&format!("from '{dep}'")))
            {
                found = true;
                break;
            }
        }
        if !found {
            unused.push(dep.clone());
        }
    }
    if cli.json {
        emit_json("unused", serde_json::json!({"command":"unused","packages":unused}));
    } else {
        for dep in &unused {
            println!("{dep}");
        }
    }
    Ok(())
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                collect_files(&path, out);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "js" | "ts" | "mjs" | "cjs" | "jsx" | "tsx"))
            {
                out.push(path);
            }
        }
    }
}

fn parse_identity(raw: &str) -> Result<(String, String)> {
    // name@version, handle scoped names by rsplit_once('@') but ensure name not empty
    let (name, version) = raw
        .rsplit_once('@')
        .filter(|(n, v)| !n.is_empty() && !v.is_empty())
        .ok_or_else(|| miette!("expected package@version"))?;
    Ok((name.to_string(), version.to_string()))
}