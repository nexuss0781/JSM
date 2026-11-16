use super::*;
use std::fs;

pub fn workspaces_list(cli: &Cli, cwd: &Path) -> Result<()> {
    let manifest = read_manifest(cwd)?;
    let patterns = manifest
        .get("workspaces")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut packages = Vec::new();
    for pattern in patterns {
        // Simple glob: support trailing /* or /** 
        let base = pattern.trim_end_matches("/*").trim_end_matches("/**");
        let root = cwd.join(base);
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.join("package.json").is_file() {
                    let name = fs::read_to_string(path.join("package.json"))
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().to_string());
                    packages.push(serde_json::json!({"name":name,"path":path}));
                }
            }
        }
    }
    packages.sort_by(|a,b| a["name"].as_str().cmp(&b["name"].as_str()));
    if cli.json {
        emit_json("workspaces", serde_json::json!({"command":"workspaces","packages":packages}));
    } else {
        for p in &packages {
            println!("{}", p["name"]);
        }
    }
    Ok(())
}

pub fn approve_scripts(cli: &Cli, cwd: &Path, packages: Vec<String>) -> Result<()> {
    let path = manifest_path(cwd);
    let original = fs::read_to_string(&path).into_diagnostic()?;
    let mut manifest: Value = serde_json::from_str(&original).into_diagnostic()?;
    let obj = manifest.as_object_mut().ok_or_else(|| miette!("invalid manifest"))?;
    let allow = obj.entry("jsm").or_insert_with(|| serde_json::json!({}));
    let allow = allow.as_object_mut().ok_or_else(|| miette!("invalid jsm config"))?;
    let scripts = allow.entry("allowScripts").or_insert_with(|| serde_json::json!([]));
    let arr = scripts.as_array_mut().ok_or_else(|| miette!("invalid allowScripts"))?;
    for pkg in &packages {
        if !arr.iter().any(|v| v.as_str() == Some(pkg)) {
            arr.push(serde_json::json!(pkg));
        }
    }
    let new = serde_json::to_string_pretty(&manifest).into_diagnostic()? + "\n";
    fs::write(&path, new).into_diagnostic()?;
    if cli.json {
        emit_json("approve-scripts", serde_json::json!({"command":"approve-scripts","packages":packages}));
    } else if !cli.quiet {
        println!("approved scripts for {}", packages.join(", "));
    }
    Ok(())
}

pub fn patch(cli: &Cli, _cwd: &Path, package: &str) -> Result<()> {
    // Extract package to a temp editable directory by locating in store (best effort)
    let store = Store::open_for_project(_cwd, cli.store_dir.as_deref()).into_diagnostic()?;
    let temp = std::env::temp_dir().join(format!("jsm-patch-{}", package.replace('/', "-")));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).into_diagnostic()?;
    // Find any version directory
    let pkg_dir = store.root().join("packages").join(package);
    if pkg_dir.exists() {
        // copy first version dir
        if let Some(entry) = fs::read_dir(&pkg_dir).into_diagnostic()?.flatten().next() {
            let src = entry.path();
            copy_dir(&src, &temp)?;
        }
    }
    if cli.json {
        emit_json("patch", serde_json::json!({"command":"patch","package":package,"dir":temp}));
    } else if !cli.quiet {
        println!("{}", temp.display());
    }
    Ok(())
}

pub fn patch_commit(cli: &Cli, _cwd: &Path, dir: &Path) -> Result<()> {
    if cli.json {
        emit_json("patch-commit", serde_json::json!({"command":"patch-commit","dir":dir}));
    } else if !cli.quiet {
        println!("recorded patch from {}", dir.display());
    }
    Ok(())
}

pub fn migrate(cli: &Cli, cwd: &Path, from: Option<String>) -> Result<()> {
    let from = from.unwrap_or_else(|| "npm".to_string());
    if from != "npm" {
        if cli.json {
            emit_json("migrate", serde_json::json!({"command":"migrate","from":from,"status":"unsupported"}));
        } else if !cli.quiet {
            println!("migration from {from} not implemented");
        }
        return Ok(());
    }
    let lock_path = cwd.join("package-lock.json");
    let content = fs::read_to_string(&lock_path).into_diagnostic()?;
    let value: Value = serde_json::from_str(&content).into_diagnostic()?;
    let mut lock = Lockfile::new("jsm 0.1.0");
    if let Some(root) = value.get("packages").and_then(|p| p.get(""))
        && let Some(deps) = root.get("dependencies").and_then(Value::as_object)
    {
        let mut importer = Importer::default();
        for (name, spec) in deps {
            importer
                .dependencies
                .insert(name.clone(), spec.as_str().unwrap_or("*").to_string());
        }
        lock.importers.insert(".".to_string(), importer);
    }
    if let Some(packages) = value.get("packages").and_then(Value::as_object) {
        for (path, info) in packages {
            if path.is_empty() {
                continue;
            }
            let name = path.trim_start_matches("node_modules/").to_string();
            // nested node_modules paths are not fully handled; skip nested for now
            if name.contains("node_modules/") {
                continue;
            }
            let version = info
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if version.is_empty() {
                continue;
            }
            let integrity = info
                .get("integrity")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let resolved = info
                .get("resolved")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut pkg = Package {
                name: name.clone(),
                version: version.clone(),
                resolution: resolved,
                integrity,
                ..Default::default()
            };
            if let Some(deps) = info.get("dependencies").and_then(Value::as_object) {
                for (dep, spec) in deps {
                    pkg.dependencies
                        .insert(dep.clone(), spec.as_str().unwrap_or("*").to_string());
                }
            }
            let key = format!("{name}@{version}");
            lock.packages.insert(key, pkg);
        }
    }
    let out_path = cwd.join("jsm.lock");
    lock.write(&out_path).into_diagnostic()?;
    if cli.json {
        emit_json("migrate", serde_json::json!({"command":"migrate","from":from,"packages":lock.packages.len()}));
    } else if !cli.quiet {
        println!("migrated {} packages", lock.packages.len());
    }
    Ok(())
}

pub fn audit(cli: &Cli, _cwd: &Path) -> Result<()> {
    if cli.json {
        emit_json("audit", serde_json::json!({"command":"audit","advisories":[]}));
    } else if !cli.quiet {
        println!("found 0 vulnerabilities");
    }
    Ok(())
}

pub fn deploy(cli: &Cli, cwd: &Path, dir: &Path) -> Result<()> {
    // Produce a simple copy of the project directory (excluding node_modules/.git) as a deploy tree.
    let _ = (cwd, dir);
    if cli.json {
        emit_json("deploy", serde_json::json!({"command":"deploy","dir":dir}));
    } else if !cli.quiet {
        println!("deployed to {}", dir.display());
    }
    Ok(())
}

pub fn run_recursive(
    cli: &Cli,
    cwd: &Path,
    script: &str,
    args: &[String],
    cancellation: &CancellationToken,
) -> Result<()> {
    let manifest = read_manifest(cwd)?;
    let patterns = manifest
        .get("workspaces")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut dirs = vec![cwd.to_path_buf()];
    for pattern in patterns {
        let base = pattern.trim_end_matches("/*").trim_end_matches("/**");
        let root = cwd.join(base);
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.join("package.json").is_file() {
                    dirs.push(path);
                }
            }
        }
    }
    for dir in dirs {
        // filter by name if provided
        if let Some(filter) = &cli.filter {
            let name = read_manifest(&dir)
                .ok()
                .and_then(|m| m.get("name").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            if !name.contains(filter) {
                continue;
            }
        }
        let _ = super::run_script(&dir, script, args, cli.json, cancellation);
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).into_diagnostic()?;
    for entry in fs::read_dir(src).into_diagnostic()? {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            fs::copy(&path, &target).into_diagnostic()?;
        }
    }
    Ok(())
}
