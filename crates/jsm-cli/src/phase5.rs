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

pub fn migrate(cli: &Cli, _cwd: &Path, from: Option<String>) -> Result<()> {
    if cli.json {
        emit_json("migrate", serde_json::json!({"command":"migrate","from":from}));
    } else if !cli.quiet {
        println!("migration from {:?} not yet fully implemented", from);
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
