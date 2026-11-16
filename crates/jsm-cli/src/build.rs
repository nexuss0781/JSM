use super::*;
use jsm_build::{
    BuildCache, BuildKey, BuildPlatform, BuildScheduler, BuildTask, BuildScriptConfig,
};
use jsm_core::{Integrity, PackageName};
use jsm_lockfile::{Lockfile, Package};
use jsm_security::SandboxPolicy;
use jsm_store::Store;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

pub fn build_command(
    cli: &Cli,
    cwd: &Path,
    package: Option<String>,
    all: bool,
    dry_run: bool,
    _cancellation: &CancellationToken,
    _progress: &mut OperationProgress,
) -> Result<()> {
    let store = Store::open_for_project(cwd, cli.store_dir.as_deref()).into_diagnostic()?;
    let _reference_registry = store.reference_registry().into_diagnostic()?;
    let lock_path = cwd.join("jsm.lock");
    
    if !lock_path.exists() {
        return Err(miette!("no lockfile found; run `jsm install` first"));
    }
    
    let lock = Lockfile::read(&lock_path).into_diagnostic()?;
    
    // Determine which packages to build
    let packages_to_build = if all {
        collect_all_native_packages(&store, &lock)?
    } else if let Some(pkg_name) = package {
        collect_named_package(&store, &lock, &pkg_name)?
    } else {
        // Default: build packages that have native code in the current project
        collect_project_native_packages(&store, &lock, cwd)?
    };
    
    if packages_to_build.is_empty() {
        if cli.json {
            emit_json("build", json!({"command":"build","built":0,"message":"no native packages to build"}));
        } else if !cli.quiet {
            println!("no native packages to build");
        }
        return Ok(());
    }
    
    if dry_run {
        if cli.json {
            emit_json("build", json!({"command":"build","dry_run":true,"packages":packages_to_build.iter().map(|t| t.name.to_string()).collect::<Vec<_>>()}));
        } else if !cli.quiet {
            println!("Would build {} native package(s):", packages_to_build.len());
            for task in &packages_to_build {
                println!("  {}@{}", task.name, task.version);
            }
        }
        return Ok(());
    }
    
    // Create build cache
    let build_cache = BuildCache::new(store.clone()).into_diagnostic()?;
    let cache = Arc::new(build_cache);
    
    // Create scheduler
    let concurrency = cli.network_concurrency.unwrap_or(4).max(1);
    let scheduler = BuildScheduler::new(concurrency, cache.clone());
    
    // Execute builds
    let results = scheduler.build_all(packages_to_build);
    
    // Wait for results
    let rt = tokio::runtime::Runtime::new().into_diagnostic()?;
    let results = rt.block_on(results).into_diagnostic()?;
    
    // Report results
    let built: Vec<_> = results.iter().filter(|r| r.success).collect();
    let failed: Vec<_> = results.iter().filter(|r| !r.success).collect();
    
    if cli.json {
        emit_json("build", json!({
            "command": "build",
            "built": built.len(),
            "failed": failed.len(),
            "packages": results.iter().map(|r| json!({
                "package": r.build_key.package_integrity,
                "success": r.success,
                "duration_ms": r.duration_ms,
                "exit_code": r.exit_code,
            })).collect::<Vec<_>>()
        }));
    } else if !cli.quiet {
        println!("Built {} native package(s), {} failed", built.len(), failed.len());
        for r in &failed {
            eprintln!("  FAILED: {} (exit code: {:?})", r.build_key.package_integrity, r.exit_code);
            eprintln!("    stderr: {}", r.stderr);
        }
    }
    
    if !failed.is_empty() {
        return Err(miette!("{} package(s) failed to build", failed.len()));
    }
    
    Ok(())
}

fn collect_all_native_packages(store: &Store, lock: &Lockfile) -> Result<Vec<BuildTask>> {
    let mut tasks = Vec::new();
    for pkg in lock.packages.values() {
        if has_native_code(store, pkg) {
            tasks.push(create_build_task(store, pkg)?);
        }
    }
    Ok(tasks)
}

fn collect_named_package(store: &Store, lock: &Lockfile, name: &str) -> Result<Vec<BuildTask>> {
    let mut tasks = Vec::new();
    for pkg in lock.packages.values() {
        if pkg.name == name && has_native_code(store, pkg) {
            tasks.push(create_build_task(store, pkg)?);
        }
    }
    if tasks.is_empty() {
        return Err(miette!("package {} not found or has no native code", name));
    }
    Ok(tasks)
}

fn collect_project_native_packages(store: &Store, lock: &Lockfile, cwd: &Path) -> Result<Vec<BuildTask>> {
    let manifest_path = cwd.join("package.json");
    let manifest = std::fs::read_to_string(&manifest_path).into_diagnostic()?;
    let manifest_json: serde_json::Value = serde_json::from_str(&manifest).into_diagnostic()?;
    
    let mut dep_names = BTreeSet::new();
    for field in ["dependencies", "devDependencies", "optionalDependencies"] {
        if let Some(obj) = manifest_json.get(field).and_then(|v| v.as_object()) {
            for name in obj.keys() {
                dep_names.insert(name.clone());
            }
        }
    }
    
    let mut tasks = Vec::new();
    for name in dep_names {
        if let Some(pkg) = lock.packages.values().find(|p| p.name == name)
            && has_native_code(store, pkg)
        {
            tasks.push(create_build_task(store, pkg)?);
        }
    }
    Ok(tasks)
}

fn has_native_code(store: &Store, pkg: &Package) -> bool {
    // Get package manifest to check for native build scripts
    let Some(manifest) = store.get_package_manifest(&pkg.name, &pkg.version, &pkg.integrity).ok().flatten() else {
        return false;
    };
    let Some(pkg_json) = &manifest.package_json else {
        return false;
    };
    if let Some(scripts) = pkg_json.get("scripts").and_then(|s| s.as_object()) {
        for name in scripts.keys() {
            if name == "install" || name == "preinstall" || name == "build" || name == "prepare" {
                return true;
            }
        }
        // Check for gypfile or binding.gyp
        if pkg_json.get("gypfile").is_some() || pkg_json.get("binding").is_some() {
            return true;
        }
    }
    false
}

fn create_build_task(store: &Store, pkg: &Package) -> Result<BuildTask> {
    let name = PackageName::new(pkg.name.clone()).into_diagnostic()?;
    let version = Version::parse(&pkg.version).into_diagnostic()?;
    let integrity = Integrity::new(pkg.integrity.clone()).into_diagnostic()?;
    
    // Compute build key components
    let script_hash = compute_script_hash_from_lockfile(store, pkg);
    let platform = BuildPlatform {
        os: std::env::consts::OS.to_string(),
        cpu: std::env::consts::ARCH.to_string(),
        libc: if cfg!(target_env = "musl") { "musl" } else { "glibc" }.to_string(),
    };
    let toolchain_fp = jsm_build::compute_toolchain_fingerprint();
    let node_abi = jsm_build::detect_node_abi();
    let sandbox_policy = SandboxPolicy::strict();
    
    let build_key = BuildKey::compute(
        &integrity,
        &script_hash,
        &node_abi,
        platform.clone(),
        &toolchain_fp,
        BTreeMap::new(),
        BTreeSet::new(),
        &SandboxPolicy::strict(),
    );
    
    let build_dir = std::env::temp_dir().join(format!("jsm-build-{}-{}", pkg.name, pkg.version));
    let output_dir = build_dir.join("output");
    
    Ok(BuildTask {
        name,
        version,
        integrity,
        build_key,
        build_dir,
        output_dir,
        script: BuildScriptConfig {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "build".to_string()],
            working_dir: None,
            env: BTreeMap::new(),
        },
        sandbox_policy,
        timeout: Duration::from_secs(300),
        env: BTreeMap::new(),
        dependencies: vec![], // Would resolve from lockfile
    })
}

fn compute_script_hash_from_lockfile(store: &Store, pkg: &Package) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    
    if let Ok(Some(manifest)) = store.get_package_manifest(&pkg.name, &pkg.version, &pkg.integrity)
    && let Some(pkg_json) = &manifest.package_json
    && let Some(scripts) = pkg_json.get("scripts").and_then(|s| s.as_object())
{
    let mut names: Vec<_> = scripts.keys().collect();
    names.sort();
    for name in names {
        if let Some(script) = scripts.get(name).and_then(|s| s.as_str()) {
            hasher.update(name.as_bytes());
            hasher.update(b":");
            hasher.update(script.as_bytes());
            hasher.update(b"\n");
        }
    }
}
    hex::encode(hasher.finalize())
}