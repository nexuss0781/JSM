use super::*;
use clap::CommandFactory;
use clap_complete::{Shell, generate};
use fs4::FileExt;
use jsm_lockfile::Importer;
use jsm_store::{ReferenceRegistry, StoredPackage};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
struct DoctorCheck {
    name: String,
    status: String,
    explanation: String,
    remedy: String,
}

#[derive(Debug, Clone)]
struct RemovalPlan {
    packages: Vec<StoredPackage>,
    logical_bytes: u64,
    physical_bytes: u64,
}

pub(super) fn store_command(
    cli: &Cli,
    cwd: &Path,
    command: StoreCommand,
    cancellation: &CancellationToken,
) -> Result<()> {
    ensure_not_cancelled(cancellation)?;
    let store = open_store(cli, cwd)?;
    let registry = store.reference_registry().into_diagnostic()?;
    match command {
        StoreCommand::Path => {
            if cli.json {
                emit_json(
                    "store.path",
                    json!({"command":"store path", "path":store.root()}),
                );
            } else {
                println!("{}", store.root().display());
            }
            Ok(())
        }
        StoreCommand::Status => {
            let _lease = store.maintenance_lease(true).into_diagnostic()?;
            let records = registry.packages().into_diagnostic()?;
            let manifests = store.list_package_manifests().into_diagnostic()?;
            let (blob_count, physical_bytes) = store.blob_totals().into_diagnostic()?;
            let logical_bytes = manifests
                .iter()
                .flat_map(|manifest| &manifest.entries)
                .filter(|entry| entry.symlink.is_none())
                .fold(0u64, |total, entry| total.saturating_add(entry.size));
            let package_count = manifests
                .iter()
                .map(|manifest| manifest.name.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            let referenced = records
                .iter()
                .map(|package| package.reference_count)
                .sum::<u64>();
            let dedupe_ratio = if physical_bytes == 0 {
                1.0
            } else {
                logical_bytes as f64 / physical_bytes as f64
            };
            let unindexed = manifests
                .iter()
                .filter(|manifest| {
                    !records.iter().any(|record| {
                        record.name == manifest.name
                            && record.version == manifest.version
                            && record.integrity == manifest.integrity
                    })
                })
                .count();
            let value = json!({"command":"store status", "path":store.root(), "total_size":physical_bytes,
                "logical_size":logical_bytes, "package_count":package_count, "version_count":manifests.len(),
                "blob_count":blob_count, "dedupe_ratio":dedupe_ratio, "reference_count":referenced,
                "unindexed_legacy_versions":unindexed});
            if cli.json {
                emit_json("store.status", value);
            } else {
                println!(
                    "{} packages, {} versions, {} blobs; {} physical / {} logical; dedupe {:.2}x; {} references{}",
                    package_count,
                    manifests.len(),
                    blob_count,
                    format_bytes(physical_bytes),
                    format_bytes(logical_bytes),
                    dedupe_ratio,
                    referenced,
                    if unindexed > 0 {
                        format!(
                            "; {unindexed} legacy version(s) have unknown reference history and are protected from automatic prune"
                        )
                    } else {
                        String::new()
                    }
                );
            }
            Ok(())
        }
        StoreCommand::List { sort, filter } => {
            let _lease = store.maintenance_lease(true).into_diagnostic()?;
            let records = registry.packages().into_diagnostic()?;
            let manifests = store.list_package_manifests().into_diagnostic()?;
            let mut values = manifests.iter().map(|manifest| {
                let record = records.iter().find(|row|row.name==manifest.name&&row.version==manifest.version&&row.integrity==manifest.integrity);
                let size = store.package_reference(manifest).map(|reference| reference.logical_size).unwrap_or_default();
                let used = record.map(|row| row.last_used_at).unwrap_or_default();
                let refs = record.map(|row| row.reference_count);
                json!({"name":manifest.name,"version":manifest.version,"integrity":manifest.integrity,
                    "size":size,"references":refs,"pinned":record.is_some_and(|row| row.pinned),
                    "last_used_at":if used == 0 { Value::Null } else { json!(used) },
                    "reference_state":if refs.is_some() {"known"} else {"unknown-legacy"}})
            }).collect::<Vec<_>>();
            if let Some(pattern) = filter.as_deref() {
                values.retain(|value| {
                    glob_match(pattern, value["name"].as_str().unwrap_or_default())
                        || glob_match(
                            pattern,
                            &format!(
                                "{}@{}",
                                value["name"].as_str().unwrap_or_default(),
                                value["version"].as_str().unwrap_or_default()
                            ),
                        )
                });
            }
            match sort {
                StoreSort::Name => values.sort_by(|a, b| {
                    (a["name"].as_str(), a["version"].as_str())
                        .cmp(&(b["name"].as_str(), b["version"].as_str()))
                }),
                StoreSort::Size => values.sort_by(|a, b| {
                    b["size"]
                        .as_u64()
                        .cmp(&a["size"].as_u64())
                        .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
                }),
                StoreSort::Used => values.sort_by(|a, b| {
                    b["last_used_at"]
                        .as_i64()
                        .cmp(&a["last_used_at"].as_i64())
                        .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
                }),
            }
            if cli.json {
                emit_json(
                    "store.list",
                    json!({"command":"store list","packages":values}),
                );
            } else {
                for row in values {
                    println!(
                        "{}@{}\t{}\t{} ref(s)",
                        row["name"].as_str().unwrap_or("?"),
                        row["version"].as_str().unwrap_or("?"),
                        format_bytes(row["size"].as_u64().unwrap_or(0)),
                        row["references"]
                            .as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown".into())
                    );
                }
            }
            Ok(())
        }
        StoreCommand::Versions { package } => {
            let _lease = store.maintenance_lease(true).into_diagnostic()?;
            let name = PackageName::new(package).into_diagnostic()?;
            let records = registry.packages().into_diagnostic()?;
            let manifests = store.list_package_manifests().into_diagnostic()?;
            let mut versions = manifests.into_iter().filter(|manifest| manifest.name == name.as_str()).map(|manifest| {
                let record = records.iter().find(|row|row.name==manifest.name&&row.version==manifest.version&&row.integrity==manifest.integrity);
                json!({"version":manifest.version,"integrity":manifest.integrity,"size":store.package_reference(&manifest).map(|r|r.logical_size).unwrap_or(0),
                    "references":record.map(|r|r.reference_count),"pinned":record.is_some_and(|r|r.pinned)})
            }).collect::<Vec<_>>();
            versions.sort_by(|a, b| version_cmp(a["version"].as_str(), b["version"].as_str()));
            if cli.json {
                emit_json(
                    "store.versions",
                    json!({"command":"store versions","package":name.as_str(),"versions":versions}),
                );
            } else {
                for row in versions {
                    println!(
                        "{}\t{}\t{} ref(s)",
                        row["version"].as_str().unwrap_or("?"),
                        format_bytes(row["size"].as_u64().unwrap_or(0)),
                        row["references"]
                            .as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown".into())
                    );
                }
            }
            Ok(())
        }
        StoreCommand::Info { package } => {
            let _lease = store.maintenance_lease(true).into_diagnostic()?;
            let (name, version) = parse_identity(&package)?;
            let records = registry.packages().into_diagnostic()?;
            let manifests = store.list_package_manifests().into_diagnostic()?;
            let selected = manifests
                .into_iter()
                .filter(|manifest| {
                    manifest.name == name
                        && version
                            .as_ref()
                            .is_none_or(|wanted| wanted == &manifest.version)
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(miette!(
                    "package is not present in the local store: {package}"
                ));
            }
            let values = selected.iter().map(|manifest| {
                let record = records.iter().find(|row|row.name==manifest.name&&row.version==manifest.version&&row.integrity==manifest.integrity);
                json!({"name":manifest.name,"version":manifest.version,"integrity":manifest.integrity,
                    "resolution_metadata":manifest.package_json,"file_count":manifest.entries.len(),
                    "logical_size":manifest.entries.iter().filter(|e|e.symlink.is_none()).map(|e|e.size).sum::<u64>(),
                    "references":record.map(|r|r.reference_count),"build_variants":[],"pinned":record.is_some_and(|r|r.pinned),
                    "first_stored_at":record.map(|r|r.first_stored_at),"last_used_at":record.map(|r|r.last_used_at)})
            }).collect::<Vec<_>>();
            if cli.json {
                emit_json(
                    "store.info",
                    json!({"command":"store info","packages":values}),
                );
            } else {
                for item in values {
                    println!(
                        "{}@{}\n  integrity: {}\n  files: {}\n  size: {}\n  references: {}\n  pinned: {}",
                        item["name"].as_str().unwrap_or("?"),
                        item["version"].as_str().unwrap_or("?"),
                        item["integrity"].as_str().unwrap_or("?"),
                        item["file_count"],
                        format_bytes(item["logical_size"].as_u64().unwrap_or(0)),
                        item["references"]
                            .as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown".into()),
                        item["pinned"]
                    );
                }
            }
            Ok(())
        }
        StoreCommand::Usage { package } => {
            let _lease = store.maintenance_lease(true).into_diagnostic()?;
            let (name, version) = parse_identity(&package)?;
            let records = registry
                .packages()
                .into_diagnostic()?
                .into_iter()
                .filter(|record| {
                    record.name == name
                        && version
                            .as_ref()
                            .is_none_or(|value| value == &record.version)
                })
                .collect::<Vec<_>>();
            let mut projects = Vec::new();
            for record in &records {
                for project in registry
                    .usage(&record.name, &record.version, &record.integrity)
                    .into_diagnostic()?
                {
                    projects.push(json!({"name":record.name,"version":record.version,"path":project.path,
                        "project_id":project.project_id,"lockfile_hash":project.lockfile_hash,"last_install_at":project.last_install_at,"stale":project.stale}));
                }
            }
            if cli.json {
                emit_json(
                    "store.usage",
                    json!({"command":"store usage","package":package,"projects":projects}),
                );
            } else {
                for project in projects {
                    println!(
                        "{}@{}\t{}{}",
                        project["name"].as_str().unwrap_or("?"),
                        project["version"].as_str().unwrap_or("?"),
                        project["path"].as_str().unwrap_or("?"),
                        if project["stale"] == true {
                            " (stale)"
                        } else {
                            ""
                        }
                    );
                }
            }
            Ok(())
        }
        StoreCommand::ForgetProject {
            project,
            dry_run,
            yes,
        } => {
            let _lease = store.maintenance_lease(false).into_diagnostic()?;
            let requested_path =
                fs::canonicalize(&project).unwrap_or_else(|_| PathBuf::from(&project));
            let target = registry.projects().into_diagnostic()?.into_iter().find(|row|
                row.project_id == project || row.path == requested_path
            ).ok_or_else(||miette!("no registered project matches {project}; use `jsm store usage <package>` to inspect references"))?;
            let packages = registry
                .packages_for_project(&target.project_id)
                .into_diagnostic()?;
            if !dry_run
                && !confirm_destructive(
                    cli,
                    yes,
                    &format!(
                        "Forget project {} at {} and release {} package reference(s)?",
                        target.project_id,
                        target.path.display(),
                        packages.len()
                    ),
                )?
            {
                return Ok(());
            }
            if !dry_run {
                registry
                    .delete_project(&target.project_id)
                    .into_diagnostic()?;
            }
            if cli.json {
                emit_json(
                    "store.forget-project",
                    json!({"command":"store forget-project", "project_id":target.project_id,
                    "path":target.path,"dry_run":dry_run,"released_package_references":packages.len()}),
                );
            } else {
                println!(
                    "{} {} reference(s) for project {}",
                    if dry_run { "would release" } else { "released" },
                    packages.len(),
                    target.path.display()
                );
            }
            Ok(())
        }
        StoreCommand::Add {
            specs,
            from_lockfile,
        } => {
            if from_lockfile && !specs.is_empty() {
                return Err(miette!(
                    "--from-lockfile cannot be combined with package specs"
                ));
            }
            let config = effective_config(cli, cwd)?;
            let remote = super::registry(&config, cli, cwd)?;
            let lock = if from_lockfile {
                Lockfile::read(cwd.join("jsm.lock")).into_diagnostic()?
            } else {
                if specs.is_empty() {
                    return Err(miette!(
                        "store add requires package specs or --from-lockfile"
                    ));
                }
                let mut importer = Importer::default();
                let mut roots = Vec::new();
                for raw in specs {
                    let (name, range) = raw
                        .rsplit_once('@')
                        .filter(|(name, range)| !name.is_empty() && !range.is_empty())
                        .ok_or_else(|| miette!("expected a package spec like pkg@^1.2.3: {raw}"))?;
                    let package = PackageName::new(name.to_owned()).into_diagnostic()?;
                    let spec = parse_spec(range)?;
                    importer
                        .dependencies
                        .insert(name.to_owned(), range.to_owned());
                    roots.push(DependencySpec::new(package, spec));
                }
                super::resolve_lock(&remote, &importer, &roots)?
            };
            let _lease = store.maintenance_lease(true).into_diagnostic()?;
            prefetch_lock(&remote, &store, &registry, &lock, cancellation)?;
            if cli.json {
                emit_json(
                    "store.add",
                    json!({"command":"store add","packages":lock.packages.len(),"stored":true}),
                );
            } else {
                println!("stored {} package version(s)", lock.packages.len());
            }
            Ok(())
        }
        StoreCommand::Remove {
            package,
            all,
            force,
            dry_run,
            yes,
        } => {
            let _lease = store.maintenance_lease(false).into_diagnostic()?;
            let (name, version) = parse_identity(&package)?;
            if version.is_none() && !all {
                return Err(miette!("store remove requires pkg@version or --all"));
            }
            let plan = removal_plan(&store, &registry, |record| {
                record.name == name && (all || version.as_ref() == Some(&record.version))
            })?;
            if plan.packages.is_empty() {
                return Err(miette!("no indexed local versions match {package}"));
            }
            if !force {
                if let Some(record) = plan
                    .packages
                    .iter()
                    .find(|record| record.reference_count > 0)
                {
                    return Err(miette!(
                        "cannot remove {}@{}: referenced by {} project(s); use `store usage` to inspect them, or pass --force to accept breaking those installs",
                        record.name,
                        record.version,
                        record.reference_count
                    ));
                }
                if let Some(record) = plan.packages.iter().find(|record| record.pinned) {
                    return Err(miette!(
                        "cannot remove pinned package {}@{}; unpin it first or use --force",
                        record.name,
                        record.version
                    ));
                }
            }
            if cli.json && dry_run {
                emit_json(
                    "store.remove",
                    json!({"command":"store remove","dry_run":true,"packages":plan.packages,"reclaimed_logical_bytes":plan.logical_bytes,"reclaimed_physical_bytes":plan.physical_bytes}),
                );
                return Ok(());
            }
            if !dry_run
                && !confirm_destructive(
                    cli,
                    yes,
                    &format!(
                        "Remove {} package version(s){}?",
                        plan.packages.len(),
                        if force { " and their references" } else { "" }
                    ),
                )?
            {
                return Ok(());
            }
            let result = if dry_run {
                (0, plan.logical_bytes, plan.physical_bytes)
            } else {
                execute_removal(&store, &registry, &plan.packages, force)?
            };
            if cli.json {
                emit_json(
                    "store.remove",
                    json!({"command":"store remove","dry_run":dry_run,"packages_removed":result.0,"reclaimed_logical_bytes":result.1,"reclaimed_physical_bytes":result.2}),
                );
            } else {
                println!(
                    "removed {} version(s); reclaimed {} logical / {} physical",
                    result.0,
                    format_bytes(result.1),
                    format_bytes(result.2)
                );
            }
            Ok(())
        }
        StoreCommand::Prune { dry_run, yes } => {
            let _lease = store.maintenance_lease(false).into_diagnostic()?;
            let _ = registry.detect_stale_projects().into_diagnostic()?;
            let candidates = registry.unreferenced_unpinned().into_diagnostic()?;
            let plan = removal_plan_from_records(&store, &registry, candidates)?;
            if cli.json && dry_run {
                emit_json(
                    "store.prune",
                    json!({"command":"store prune","dry_run":true,"packages":plan.packages,"reclaimed_logical_bytes":plan.logical_bytes,"reclaimed_physical_bytes":plan.physical_bytes}),
                );
                return Ok(());
            }
            if !dry_run
                && !confirm_destructive(
                    cli,
                    yes,
                    &format!(
                        "Prune {} unreferenced package version(s)?",
                        plan.packages.len()
                    ),
                )?
            {
                return Ok(());
            }
            let result = if dry_run {
                (0, plan.logical_bytes, plan.physical_bytes)
            } else {
                execute_removal(&store, &registry, &plan.packages, false)?
            };
            if cli.json {
                emit_json(
                    "store.prune",
                    json!({"command":"store prune","dry_run":dry_run,"packages_removed":result.0,"reclaimed_logical_bytes":result.1,"reclaimed_physical_bytes":result.2}),
                );
            } else {
                println!(
                    "{} package version(s){}; would reclaim {} logical / {} physical",
                    if dry_run {
                        plan.packages.len()
                    } else {
                        result.0 as usize
                    },
                    if dry_run {
                        " would be pruned"
                    } else {
                        " pruned"
                    },
                    format_bytes(result.1),
                    format_bytes(result.2)
                );
            }
            Ok(())
        }
        StoreCommand::Gc {
            older_than,
            max_size,
            dry_run,
            yes,
        } => {
            let _lease = store.maintenance_lease(false).into_diagnostic()?;
            let cutoff = older_than
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .map(|duration| {
                    now_seconds().saturating_sub(duration.as_secs().min(i64::MAX as u64) as i64)
                });
            let target = max_size.as_deref().map(parse_bytes).transpose()?;
            let mut eligible = registry.unreferenced_unpinned().into_diagnostic()?;
            eligible.retain(|record| cutoff.is_none_or(|cutoff| record.last_used_at <= cutoff));
            eligible.sort_by_key(|record| {
                (
                    record.last_used_at,
                    record.name.clone(),
                    record.version.clone(),
                )
            });
            if let Some(target) = target {
                let (_, mut current_size) = store.blob_totals().into_diagnostic()?;
                let mut chosen = Vec::new();
                for record in eligible {
                    if current_size <= target {
                        break;
                    }
                    let mut next = chosen.clone();
                    next.push(record.clone());
                    let plan = removal_plan_from_records(&store, &registry, next.clone())?;
                    current_size = current_size.saturating_sub(plan.physical_bytes);
                    chosen.push(record);
                }
                eligible = chosen;
            }
            let plan = removal_plan_from_records(&store, &registry, eligible)?;
            if cli.json && dry_run {
                emit_json(
                    "store.gc",
                    json!({"command":"store gc","dry_run":true,"packages":plan.packages,"reclaimed_logical_bytes":plan.logical_bytes,"reclaimed_physical_bytes":plan.physical_bytes}),
                );
                return Ok(());
            }
            if !dry_run
                && !confirm_destructive(
                    cli,
                    yes,
                    &format!(
                        "Garbage-collect {} package version(s)?",
                        plan.packages.len()
                    ),
                )?
            {
                return Ok(());
            }
            let result = if dry_run {
                (0, plan.logical_bytes, plan.physical_bytes)
            } else {
                execute_removal(&store, &registry, &plan.packages, false)?
            };
            if cli.json {
                emit_json(
                    "store.gc",
                    json!({"command":"store gc","dry_run":dry_run,"packages_removed":result.0,"reclaimed_logical_bytes":result.1,"reclaimed_physical_bytes":result.2}),
                );
            } else {
                println!(
                    "{} package version(s){}; reclaimed {} logical / {} physical",
                    if dry_run {
                        plan.packages.len()
                    } else {
                        result.0 as usize
                    },
                    if dry_run {
                        " would be evicted"
                    } else {
                        " evicted"
                    },
                    format_bytes(result.1),
                    format_bytes(result.2)
                );
            }
            Ok(())
        }
        StoreCommand::Pin { package } => {
            let _lease = store.maintenance_lease(false).into_diagnostic()?;
            set_pin(cli, &store, &registry, &package, true)
        }
        StoreCommand::Unpin { package } => {
            let _lease = store.maintenance_lease(false).into_diagnostic()?;
            set_pin(cli, &store, &registry, &package, false)
        }
        StoreCommand::Pinned => {
            let _lease = store.maintenance_lease(true).into_diagnostic()?;
            let pinned = registry
                .packages()
                .into_diagnostic()?
                .into_iter()
                .filter(|package| package.pinned)
                .collect::<Vec<_>>();
            if cli.json {
                emit_json(
                    "store.pinned",
                    json!({"command":"store pinned","packages":pinned}),
                );
            } else {
                for package in pinned {
                    println!("{}@{}", package.name, package.version);
                }
            }
            Ok(())
        }
        StoreCommand::Verify { package, full, fix } => {
            let _lease = store.maintenance_lease(fix).into_diagnostic()?;
            let mut report = store.verify(full).into_diagnostic()?;
            if let Some(filter) = package.as_deref() {
                report.findings.retain(|finding| {
                    finding
                        .name
                        .as_deref()
                        .is_some_and(|name| glob_match(filter, name))
                });
            }
            if fix {
                report = store.repair_from_report(&report).into_diagnostic()?;
            }
            let value = json!({"command":"store verify","mode":if full {"full"} else {"metadata"},"fixed":fix,
                "packages_checked":report.packages_checked,"blobs_checked":report.blobs_checked,
                "logical_bytes_checked":report.logical_bytes_checked,"findings":report.findings});
            if cli.json {
                emit_json("store.verify", value);
            } else if report.findings.is_empty() {
                println!(
                    "store verification passed ({} packages, {} blobs)",
                    report.packages_checked, report.blobs_checked
                );
            } else {
                for finding in &report.findings {
                    eprintln!("{}: {} ({})", finding.code, finding.message, finding.path);
                }
            }
            if !fix && !report.findings.is_empty() {
                return Err(CommandExit {code:1,message:"store verification found corruption; rerun with `store verify --fix` to quarantine damaged content".into()}.into());
            }
            Ok(())
        }
    }
}

pub(super) fn remote_versions(cli: &Cli, cwd: &Path, package: &str) -> Result<()> {
    let name = PackageName::new(package.to_owned()).into_diagnostic()?;
    let config = effective_config(cli, cwd)?;
    let remote = registry(&config, cli, cwd)?;
    let packument = remote.packument(name.as_str()).into_diagnostic()?;
    super::validate_packument_identity(&name, &packument).into_diagnostic()?;
    let store = open_store(cli, cwd)?;
    let _lease = store.maintenance_lease(true).into_diagnostic()?;
    let local = store
        .list_package_manifests()
        .into_diagnostic()?
        .into_iter()
        .filter(|manifest| manifest.name == name.as_str())
        .map(|manifest| manifest.version)
        .collect::<HashSet<_>>();
    let mut versions = packument.versions.into_iter().map(|(key, metadata)| {
        let version = metadata.version.unwrap_or(key);
        json!({"version":version,"local":local.contains(&version),"deprecated":metadata.deprecated,"integrity":metadata.dist.integrity})
    }).collect::<Vec<_>>();
    versions.sort_by(|a, b| version_cmp(a["version"].as_str(), b["version"].as_str()));
    if cli.json {
        emit_json(
            "versions",
            json!({"command":"versions","package":name.as_str(),"versions":versions}),
        );
    } else {
        for value in versions {
            println!(
                "{}{}",
                value["version"].as_str().unwrap_or("?"),
                if value["local"] == true {
                    " (local)"
                } else {
                    ""
                }
            );
        }
    }
    Ok(())
}

pub(super) fn completion(shell: CompletionShell) -> Result<()> {
    let mut command = Cli::command();
    let clap_shell = match shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
        CompletionShell::Fish => Shell::Fish,
        CompletionShell::PowerShell => Shell::PowerShell,
        CompletionShell::Elvish => Shell::Elvish,
    };
    generate(clap_shell, &mut command, "jsm", &mut io::stdout());
    let dynamic = match shell {
        CompletionShell::Bash => {
            r#"
_jsm_phase2_dynamic() {
  _jsm
  local kind="" sub="${COMP_WORDS[1]}" prev="${COMP_WORDS[COMP_CWORD-1]}" prefix="${COMP_WORDS[COMP_CWORD]}"
  case "$sub:$prev" in
    store:remove|store:versions|store:info|store:usage|store:pin|store:unpin) kind=packages ;;
    store:forget-project) kind=projects ;;
    why:*|remove:*|versions:*|"":*) kind=packages ;;
  esac
  if [[ -n "$kind" ]]; then
    local -a dynamic
    mapfile -t dynamic < <(command jsm __complete "$kind" "$prefix" 2>/dev/null)
    COMPREPLY+=("${dynamic[@]}")
  fi
}
complete -o default -F _jsm_phase2_dynamic jsm
"#
        }
        CompletionShell::Zsh => {
            r#"
if (( $+functions[_jsm] )); then
  functions[_jsm_phase2_static]=$functions[_jsm]
  _jsm() {
    _jsm_phase2_static "$@"
    local kind="" sub="${words[2]}" prev="${words[CURRENT-1]}"
    if [[ "$sub:$prev" == store:forget-project ]]; then kind=projects
    elif [[ "$sub:$prev" == store:remove || "$sub:$prev" == store:versions || "$sub:$prev" == store:info || "$sub:$prev" == store:usage || "$sub:$prev" == store:pin || "$sub:$prev" == store:unpin || "$sub" == why || "$sub" == remove || "$sub" == versions ]]; then kind=packages; fi
    if [[ -n "$kind" ]]; then compadd -- "${(@f)$(command jsm __complete "$kind" "$PREFIX" 2>/dev/null)}"; fi
  }
fi
"#
        }
        CompletionShell::Fish => {
            r#"
complete -c jsm -n '__fish_seen_subcommand_from remove why versions; or __fish_seen_subcommand_from store; and __fish_seen_subcommand_from remove versions info usage pin unpin' -a '(command jsm __complete packages (commandline -ct) 2>/dev/null)'
complete -c jsm -n '__fish_seen_subcommand_from forget-project' -a '(command jsm __complete projects (commandline -ct) 2>/dev/null)'
"#
        }
        CompletionShell::PowerShell => {
            r#"
Register-ArgumentCompleter -Native -CommandName jsm -ScriptBlock {
  param($wordToComplete, $commandAst, $cursorPosition)
  $tokens = @($commandAst.CommandElements | ForEach-Object { $_.ToString() })
  $kind = $null
  if ($tokens -contains 'forget-project') { $kind = 'projects' }
  elseif ($tokens | Where-Object { $_ -in @('why','remove','versions','info','usage','pin','unpin') }) { $kind = 'packages' }
  if ($kind) { jsm __complete $kind $wordToComplete 2>$null | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object { [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) } }
}
"#
        }
        CompletionShell::Elvish => {
            r#"
var jsm-static-completer = $edit:completion:arg-completer[jsm]
set edit:completion:arg-completer[jsm] = {|@args|
  $jsm-static-completer $@args
  var n = (count $args)
  var kind = ''
  if (>= $n 3) {
    if (and (eq $args[1] 'store') (eq $args[2] 'forget-project')) {
      set kind = 'projects'
    } elif (and (eq $args[1] 'store') (or (eq $args[2] 'remove') (eq $args[2] 'versions') (eq $args[2] 'info') (eq $args[2] 'usage') (eq $args[2] 'pin') (eq $args[2] 'unpin'))) {
      set kind = 'packages'
    } elif (or (eq $args[1] 'remove') (eq $args[1] 'why') (eq $args[1] 'versions')) {
      set kind = 'packages'
    }
  }
  if (eq $kind 'packages') {
    jsm __complete packages $args[-1]
  } elif (eq $kind 'projects') {
    jsm __complete projects $args[-1]
  }
}
"#
        }
    };
    io::stdout()
        .write_all(dynamic.as_bytes())
        .into_diagnostic()?;
    Ok(())
}

pub(super) fn dynamic_completion(
    cli: &Cli,
    cwd: &Path,
    kind: CompletionKind,
    prefix: &str,
) -> Result<()> {
    let store = open_store(cli, cwd)?;
    let _lease = store.maintenance_lease(true).into_diagnostic()?;
    let mut candidates = BTreeSet::new();
    match kind {
        CompletionKind::Packages => {
            for manifest in store.list_package_manifests().into_diagnostic()? {
                candidates.insert(manifest.name.clone());
                candidates.insert(format!("{}@{}", manifest.name, manifest.version));
            }
        }
        CompletionKind::Projects => {
            for project in store
                .reference_registry()
                .into_diagnostic()?
                .projects()
                .into_diagnostic()?
            {
                candidates.insert(project.project_id);
                candidates.insert(project.path.to_string_lossy().into_owned());
            }
        }
    }
    for candidate in candidates
        .into_iter()
        .filter(|candidate| candidate.starts_with(prefix))
    {
        println!("{candidate}");
    }
    Ok(())
}

pub(super) fn topic_help(topic: &str) -> Result<()> {
    let text = match topic {
        "store" => {
            "# Store management\n\n`jsm store path|status|list|versions|info|add|remove|usage|forget-project|prune|gc|pin|unpin|pinned|verify` inspect and manage the shared content-addressed store. Destructive operations accept `--dry-run`, ask before changing data, and accept `--yes` to run non-interactively. Missing project paths remain stale and keep references until explicitly released with `store forget-project <path-or-id>`. `store verify --full` hashes every unique blob; `--fix` quarantines damaged content so install fetches it again.\n"
        }
        "lockfile" => {
            "# Lockfile\n\n`jsm lock verify` checks the lockfile against the current manifest. `jsm lock merge %O %A %B` is a parser-based three-way merge driver. `jsm lock install-merge-driver` installs repository-local Git configuration and a `.gitattributes` rule.\n"
        }
        "config" => {
            "# Configuration\n\n`jsm config get|set|list|delete` manages layered configuration. Project credentials and registry auth values are redacted from diagnostic output.\n"
        }
        "scripts" => {
            "# Scripts\n\nPhase 1 runs only explicitly requested root project scripts. Dependency lifecycle scripts remain disabled until the later script-policy phase.\n"
        }
        _ => {
            return Err(miette!(
                "unknown help topic `{topic}`; available topics: store, lockfile, config, scripts"
            ));
        }
    };
    print!("{text}");
    Ok(())
}

fn open_store(cli: &Cli, cwd: &Path) -> Result<Store> {
    let config = effective_config(cli, cwd)?;
    let configured = cli
        .store_dir
        .clone()
        .or_else(|| config.store_dir.as_deref().map(PathBuf::from));
    Store::open_for_project(cwd, configured.as_deref()).into_diagnostic()
}

fn set_pin(
    cli: &Cli,
    store: &Store,
    registry: &ReferenceRegistry,
    package: &str,
    pin: bool,
) -> Result<()> {
    let (name, version) = parse_identity(package)?;
    let version = version.ok_or_else(|| miette!("pin and unpin require pkg@version"))?;
    let manifests = store.list_package_manifests().into_diagnostic()?;
    let manifest = manifests
        .iter()
        .find(|manifest| manifest.name == name && manifest.version == version)
        .ok_or_else(|| miette!("package version is not in the local store: {package}"))?;
    let reference = store.package_reference(manifest).into_diagnostic()?;
    registry.record_package(&reference).into_diagnostic()?;
    registry
        .set_pinned(&name, &version, &manifest.integrity, pin)
        .into_diagnostic()?;
    if cli.json {
        emit_json(
            if pin { "store.pin" } else { "store.unpin" },
            json!({"command":if pin {"store pin"} else {"store unpin"},"package":format!("{name}@{version}"),"pinned":pin}),
        );
    } else {
        println!(
            "{} {}@{}",
            if pin { "pinned" } else { "unpinned" },
            name,
            version
        );
    }
    Ok(())
}

fn prefetch_lock(
    remote: &Registry,
    store: &Store,
    registry: &ReferenceRegistry,
    lock: &Lockfile,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for package in lock.packages.values() {
        ensure_not_cancelled(cancellation)?;
        let identity = format!("{}@{}@{}", package.name, package.version, package.integrity);
        if !seen.insert(identity.clone()) {
            continue;
        }
        let _package_lease = store.package_lease(&identity).into_diagnostic()?;
        if !store.has_package(&package.name, &package.version, &package.integrity) {
            let body = remote
                .download_tarball_resumable(&package.name, &package.resolution)
                .into_diagnostic()?;
            extract_tarball_with_options(
                store,
                &package.name,
                &package.version,
                &package.integrity,
                body,
                jsm_fetch::ExtractOptions {
                    limits: Default::default(),
                    cancellation,
                    progress: None,
                },
            )
            .map_err(|error| {
                miette!(
                    "failed to prefetch {}@{}: {error}",
                    package.name,
                    package.version
                )
            })?;
        }
        let manifest = store
            .get_package_manifest(&package.name, &package.version, &package.integrity)
            .into_diagnostic()?
            .ok_or_else(|| {
                miette!(
                    "verified package {}@{} is missing from the store",
                    package.name,
                    package.version
                )
            })?;
        registry
            .record_package(&store.package_reference(&manifest).into_diagnostic()?)
            .into_diagnostic()?;
        registry
            .clear_corrupt(&package.name, &package.version, &package.integrity)
            .into_diagnostic()?;
    }
    Ok(())
}

fn removal_plan(
    store: &Store,
    registry: &ReferenceRegistry,
    predicate: impl Fn(&StoredPackage) -> bool,
) -> Result<RemovalPlan> {
    let indexed = registry.packages().into_diagnostic()?;
    let manifests = store.list_package_manifests().into_diagnostic()?;
    let mut selected = Vec::new();
    for manifest in manifests {
        let record = indexed
            .iter()
            .find(|record| {
                record.name == manifest.name
                    && record.version == manifest.version
                    && record.integrity == manifest.integrity
            })
            .cloned()
            .unwrap_or_else(|| {
                let (logical_size, physical_size, file_count) = store
                    .package_reference(&manifest)
                    .map(|reference| {
                        (
                            reference.logical_size,
                            reference.physical_size,
                            reference.file_count,
                        )
                    })
                    .unwrap_or_default();
                // Unknown reference history is deliberately treated as referenced; only an
                // explicit --force may remove a legacy package not indexed by Phase 2.
                StoredPackage {
                    name: manifest.name.clone(),
                    version: manifest.version.clone(),
                    integrity: manifest.integrity.clone(),
                    first_stored_at: 0,
                    last_used_at: 0,
                    logical_size,
                    physical_size,
                    file_count,
                    reference_count: 1,
                    pinned: false,
                }
            });
        if predicate(&record) {
            selected.push(record);
        }
    }
    removal_plan_from_records(store, registry, selected)
}

fn removal_plan_from_records(
    store: &Store,
    _registry: &ReferenceRegistry,
    packages: Vec<StoredPackage>,
) -> Result<RemovalPlan> {
    let target = packages
        .iter()
        .map(|record| {
            (
                record.name.clone(),
                record.version.clone(),
                record.integrity.clone(),
            )
        })
        .collect::<HashSet<_>>();
    let manifests = store.list_package_manifests().into_diagnostic()?;
    let mut remaining_blobs = HashSet::new();
    let mut logical_bytes = 0u64;
    for manifest in &manifests {
        let selected = target.contains(&(
            manifest.name.clone(),
            manifest.version.clone(),
            manifest.integrity.clone(),
        ));
        for entry in &manifest.entries {
            if entry.symlink.is_none() {
                if selected {
                    logical_bytes = logical_bytes.saturating_add(entry.size);
                } else {
                    remaining_blobs.insert(entry.hash.clone());
                }
            }
        }
    }
    let mut candidate_blobs = HashSet::new();
    for manifest in &manifests {
        if target.contains(&(
            manifest.name.clone(),
            manifest.version.clone(),
            manifest.integrity.clone(),
        )) {
            for entry in &manifest.entries {
                if entry.symlink.is_none() {
                    candidate_blobs.insert(entry.hash.clone());
                }
            }
        }
    }
    let mut physical_bytes = 0u64;
    for hash in candidate_blobs.difference(&remaining_blobs) {
        if let Some(path) = store.get_blob_path(hash).into_diagnostic()? {
            let file = File::open(path).into_diagnostic()?;
            physical_bytes = physical_bytes.saturating_add(
                FileExt::allocated_size(&file).unwrap_or(file.metadata().into_diagnostic()?.len()),
            );
        }
    }
    Ok(RemovalPlan {
        packages,
        logical_bytes,
        physical_bytes,
    })
}

fn execute_removal(
    store: &Store,
    registry: &ReferenceRegistry,
    packages: &[StoredPackage],
    force: bool,
) -> Result<(u64, u64, u64)> {
    // Validate all candidates before the first filesystem mutation.
    for record in packages {
        if record.reference_count > 0 && !force {
            return Err(miette!(
                "{}@{} is still referenced",
                record.name,
                record.version
            ));
        }
        if record.pinned && !force {
            return Err(miette!("{}@{} is pinned", record.name, record.version));
        }
    }
    let plan = removal_plan_from_records(store, registry, packages.to_vec())?;
    for record in packages {
        store
            .remove_package_manifest(&record.name, &record.version, &record.integrity)
            .into_diagnostic()?;
        registry
            .remove_package_record(&record.name, &record.version, &record.integrity, force)
            .into_diagnostic()?;
    }
    let _ = (store.sweep_unreferenced_blobs()).into_diagnostic()?;
    Ok((
        packages.len() as u64,
        plan.logical_bytes,
        plan.physical_bytes,
    ))
}

fn confirm_destructive(cli: &Cli, yes: bool, prompt: &str) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if cli.non_interactive || ci_enabled() || !io::stdin().is_terminal() {
        return Err(miette!(
            "{prompt} re-run with --yes to confirm in a non-interactive environment"
        ));
    }
    eprint!("{prompt} [y/N] ");
    io::stderr().flush().ok();
    let mut response = String::new();
    io::stdin().read_line(&mut response).into_diagnostic()?;
    Ok(matches!(
        response.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn parse_identity(raw: &str) -> Result<(String, Option<String>)> {
    if let Some((name, version)) = raw
        .rsplit_once('@')
        .filter(|(name, version)| !name.is_empty() && !version.is_empty())
    {
        let name = PackageName::new(name.to_owned()).into_diagnostic()?;
        Ok((name.as_str().to_owned(), Some(version.to_owned())))
    } else {
        let name = PackageName::new(raw.to_owned()).into_diagnostic()?;
        Ok((name.as_str().to_owned(), None))
    }
}

fn parse_duration(raw: &str) -> Result<Duration> {
    let raw = raw.trim();
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (digits, unit) = raw.split_at(split);
    let value: u64 = digits
        .parse()
        .map_err(|_| miette!("invalid duration `{raw}`"))?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        "" => return Err(miette!("duration unit required; use s, m, h, d, or w")),
        _ => return Err(miette!("unsupported duration unit in `{raw}`")),
    };
    Ok(Duration::from_secs(value.saturating_mul(multiplier)))
}

fn parse_bytes(raw: &str) -> Result<u64> {
    let raw = raw.trim();
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (digits, unit) = raw.split_at(split);
    let value: u64 = digits
        .parse()
        .map_err(|_| miette!("invalid size `{raw}`"))?;
    let factor = match unit.to_ascii_lowercase().as_str() {
        "b" | "" => 1,
        "kb" => 1000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        _ => return Err(miette!("unsupported size unit in `{raw}`")),
    };
    value
        .checked_mul(factor)
        .ok_or_else(|| miette!("size `{raw}` overflows"))
}

fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            Some(b'?') => !t.is_empty() && go(&p[1..], &t[1..]),
            Some(ch) => t.first() == Some(ch) && go(&p[1..], &t[1..]),
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

fn version_cmp(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    let a = a.and_then(|v| semver::Version::parse(v).ok());
    let b = b.and_then(|v| semver::Version::parse(v).ok());
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

pub(super) fn doctor(cli: &Cli, cwd: &Path, fix: bool, report: bool) -> Result<()> {
    let store = open_store(cli, cwd)?;
    let _maintenance = store.maintenance_lease(!fix).into_diagnostic()?;
    let registry = store.reference_registry().into_diagnostic()?;
    let mut checks = Vec::new();
    let verify = store.verify(false);
    checks.push(DoctorCheck{name:"store-integrity".into(),status:if verify.as_ref().is_ok_and(|r|r.findings.is_empty()){"ok"}else{"error"}.into(),
        explanation:verify.as_ref().map(|r|format!("{} package manifests checked; {} finding(s)",r.packages_checked,r.findings.len())).unwrap_or_else(|e|e.to_string()),remedy:"Run `jsm store verify --full`, then `jsm store verify --fix` to quarantine damaged content.".into()});
    let projects = registry.projects().into_diagnostic()?;
    let stale_projects = projects.iter().filter(|project| project.stale).count();
    checks.push(DoctorCheck{name:"project-references".into(),status:if stale_projects==0{"ok"}else{"warning"}.into(),
        explanation:format!("{} registered projects; {} marked stale",projects.len(),stale_projects),remedy:"Run install in moved projects to re-register them; remove projects you no longer use before pruning.".into()});
    checks.push(link_check());
    checks.push(cross_device_check(cwd, &store));
    checks.push(long_path_check());
    checks.push(case_sensitivity_check());
    checks.push(lock_check(store.root()));
    let config = effective_config(cli, cwd)?;
    let proxy_configured = config.proxy.is_some()
        || std::env::var_os("HTTPS_PROXY").is_some()
        || std::env::var_os("HTTP_PROXY").is_some();
    checks.push(DoctorCheck{name:"proxy-ca".into(),status:if config.strict_ssl.unwrap_or(true)||config.cafile.is_some(){"ok"}else{"warning"}.into(),
        explanation:format!("strict TLS {}; custom CA {}{}",config.strict_ssl.unwrap_or(true),if config.cafile.is_some(){"configured"}else{"not configured"},if proxy_configured {"; proxy configured"}else{"; direct network"}),remedy:"Set strict-ssl=true and configure cafile for a trusted enterprise CA; avoid embedding credentials in proxy URLs.".into()});
    let remote = super::registry(&config, cli, cwd)
        .and_then(|client| client.packument("jsm").map(|_| ()).into_diagnostic());
    checks.push(DoctorCheck{name:"registry-reachability".into(),status:if cli.offline {"skipped"}else if remote.is_ok(){"ok"}else{"warning"}.into(),
        explanation:if cli.offline{"Skipped because --offline was requested.".into()}else{remote.err().map(|e|e.to_string()).unwrap_or_else(||"registry metadata request succeeded".into())},remedy:"Check registry configuration, DNS, proxy, TLS CA, and network connectivity; retry with `--offline` only when cached metadata is sufficient.".into()});
    checks.push(DoctorCheck {
        name: "clock".into(),
        status: if now_seconds() > 1_577_836_800 {
            "ok"
        } else {
            "error"
        }
        .into(),
        explanation: format!("system Unix time is {}", now_seconds()),
        remedy: "Synchronize the system clock with the operating system's trusted time service."
            .into(),
    });
    let disk = fs4::available_space(store.root());
    checks.push(DoctorCheck {
        name: "disk-space".into(),
        status: match disk {
            Ok(n) if n > 100 * 1024 * 1024 => "ok",
            Ok(_) => "warning",
            Err(_) => "unknown",
        }
        .into(),
        explanation: disk
            .map(|n| format!("{} available on the store volume", format_bytes(n)))
            .unwrap_or_else(|e| format!("available space could not be queried: {e}")),
        remedy: "Free space on the store volume before fetching or repairing packages.".into(),
    });
    checks.push(runtime_check("node", "--version"));
    checks.push(runtime_check("jsm", "--version"));
    #[cfg(target_os = "windows")]
    {
        checks.push(DoctorCheck{name:"windows-defender-dev-drive".into(),status:"unknown".into(),explanation:"Defender and Dev Drive status require platform APIs not currently available in this build.".into(),remedy:"Review Windows Security and consider a Dev Drive for large stores.".into()});
    }
    #[cfg(not(target_os = "windows"))]
    {
        checks.push(DoctorCheck {
            name: "windows-defender-dev-drive".into(),
            status: "not_applicable".into(),
            explanation: "Windows-only environment checks were skipped.".into(),
            remedy: "No action required on this platform.".into(),
        });
    }
    if fix {
        let removed = store
            .clean_orphan_temps(Duration::from_secs(24 * 3600))
            .into_diagnostic()?;
        // Opening performs any supported index migration/recovery without discarding rows.
        let _ = ReferenceRegistry::open(store.root()).into_diagnostic()?;
        checks.push(DoctorCheck {
            name: "safe-fixes".into(),
            status: "ok".into(),
            explanation: format!(
                "removed {removed} temp file(s) older than 24 hours and checked registry schema"
            ),
            remedy: "Review other reported checks and apply their suggested remedies.".into(),
        });
    }
    let check_values=checks.iter().map(|check|json!({"name":check.name,"status":check.status,"explanation":check.explanation,"remedy":check.remedy})).collect::<Vec<_>>();
    let mut value = json!({"command":"doctor","checks":check_values,"report":report});
    if report {
        let mut text = redact(&value.to_string());
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            let home = home.to_string_lossy();
            if !home.is_empty() {
                text = text.replace(home.as_ref(), "~");
            }
        }
        value = serde_json::from_str(&text)
            .unwrap_or_else(|_| json!({"command":"doctor","redaction_error":true}));
    }
    if cli.json || report {
        emit_json("doctor", value);
    } else {
        for check in checks {
            println!(
                "[{:<13}] {} — {}\n               remedy: {}",
                check.status, check.name, check.explanation, check.remedy
            );
        }
    }
    Ok(())
}

fn link_check() -> DoctorCheck {
    let root = std::env::temp_dir().join(format!(
        "jsm-doctor-link-{}-{}",
        std::process::id(),
        now_seconds()
    ));
    let result = (|| -> io::Result<(bool, bool, bool)> {
        fs::create_dir_all(&root)?;
        let src = root.join("source");
        fs::write(&src, b"probe")?;
        let hard = root.join("hard");
        let hard_ok = fs::hard_link(&src, &hard).is_ok();
        let reflink = root.join("reflink");
        let reflink_ok = jsm_linker::probe_reflink(&src, &reflink);
        let sym = root.join("symlink");
        #[cfg(unix)]
        let sym_ok = { std::os::unix::fs::symlink(&src, &sym).is_ok() };
        #[cfg(windows)]
        let sym_ok = { std::os::windows::fs::symlink_file(&src, &sym).is_ok() };
        #[cfg(not(any(unix, windows)))]
        let sym_ok = false;
        Ok((hard_ok, reflink_ok, sym_ok))
    })();
    let _ = fs::remove_dir_all(&root);
    match result {Ok((hard,reflink,sym))=>DoctorCheck{name:"link-capabilities".into(),status:if hard||reflink||sym{"ok"}else{"warning"}.into(),explanation:format!("hardlink={hard}, reflink={reflink}, symlink={sym}"),remedy:"Keep the project and store on the same volume where hard links are desired; JSM will use portable copy fallback.".into()},Err(e)=>DoctorCheck{name:"link-capabilities".into(),status:"warning".into(),explanation:e.to_string(),remedy:"Check write permissions and available filesystem link capabilities.".into()}}
}

fn cross_device_check(cwd: &Path, store: &Store) -> DoctorCheck {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let left = fs::metadata(cwd).map(|m| m.dev()).ok();
        let right = fs::metadata(store.root()).map(|m| m.dev()).ok();
        DoctorCheck{name:"volume-layout".into(),status:if left.is_some()&&left==right{"ok"}else{"warning"}.into(),explanation:format!("project and store device IDs: {left:?} / {right:?}"),remedy:"Cross-device operation uses reflink or copy fallback; set --store-dir on the project volume for hardlink placement.".into()}
    }
    #[cfg(not(unix))]
    {
        let _ = (cwd, store);
        DoctorCheck {
            name: "volume-layout".into(),
            status: "info".into(),
            explanation: "Cross-volume capability is handled by the active platform linker.".into(),
            remedy: "No action required unless linking reports a cross-device error.".into(),
        }
    }
}

fn long_path_check() -> DoctorCheck {
    #[cfg(windows)]
    {
        DoctorCheck {
            name: "long-paths".into(),
            status: "unknown".into(),
            explanation:
                "Windows long-path policy cannot be reliably read without platform policy APIs."
                    .into(),
            remedy: "Enable Win32 long paths in system policy and keep the store path short."
                .into(),
        }
    }
    #[cfg(not(windows))]
    {
        DoctorCheck {
            name: "long-paths".into(),
            status: "ok".into(),
            explanation: "The current platform does not use the Windows MAX_PATH policy.".into(),
            remedy: "No action required.".into(),
        }
    }
}

fn case_sensitivity_check() -> DoctorCheck {
    let root = std::env::temp_dir().join(format!(
        "jsm-doctor-case-{}-{}",
        std::process::id(),
        now_seconds()
    ));
    let result = (|| -> io::Result<bool> {
        fs::create_dir_all(&root)?;
        fs::write(root.join("CaseProbe"), b"1")?;
        Ok(!root.join("caseprobe").exists())
    })();
    let _ = fs::remove_dir_all(&root);
    let (status, explanation) = match result {
        Ok(true) => ("ok", "filesystem distinguishes case in file names".into()),
        Ok(false) => ("warning", "filesystem appears case-insensitive".into()),
        Err(e) => ("unknown", e.to_string()),
    };
    DoctorCheck{name:"case-sensitivity".into(),status:status.into(),explanation,remedy:"Avoid package file paths that differ only by case when targeting case-insensitive filesystems.".into()}
}

fn lock_check(store_root: &Path) -> DoctorCheck {
    let path = store_root.join("locks").join(format!(
        "doctor-{}-{}.lock",
        std::process::id(),
        now_seconds()
    ));
    let result = (|| -> io::Result<bool> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match FileExt::try_lock(&file) {
            Ok(()) => {
                let second = OpenOptions::new().read(true).write(true).open(&path)?;
                let contention = matches!(
                    FileExt::try_lock(&second),
                    Err(fs4::TryLockError::WouldBlock)
                );
                let _ = FileExt::unlock(&file);
                Ok(contention)
            }
            Err(fs4::TryLockError::WouldBlock) => Ok(false),
            Err(fs4::TryLockError::Error(e)) => Err(e),
        }
    })();
    let _ = fs::remove_file(path);
    let (status, explanation) = match result {
        Ok(true) => (
            "ok",
            "exclusive lock and contention detection succeeded".into(),
        ),
        Ok(false) => (
            "error",
            "exclusive lock did not exclude a second process/file handle".into(),
        ),
        Err(e) => ("warning", format!("lock probe failed: {e}")),
    };
    DoctorCheck{name:"file-locking".into(),status:status.into(),explanation,remedy:"Use a local filesystem with working advisory file locks; network filesystems may not provide reliable cross-process guarantees.".into()}
}

fn runtime_check(program: &str, arg: &str) -> DoctorCheck {
    match ProcessCommand::new(program).arg(arg).output() {
        Ok(output) if output.status.success() => DoctorCheck {
            name: format!("{program}-version"),
            status: "ok".into(),
            explanation: String::from_utf8_lossy(&output.stdout).trim().into(),
            remedy: "No action required.".into(),
        },
        _ => DoctorCheck {
            name: format!("{program}-version"),
            status: "warning".into(),
            explanation: format!("{program} is not available on PATH"),
            remedy: format!(
                "Install {program} or update PATH if this environment is expected to run JavaScript projects."
            ),
        },
    }
}

pub(super) fn lock_command(cli: &Cli, cwd: &Path, command: LockCommand) -> Result<()> {
    match command {
        LockCommand::Verify => {
            let lock = Lockfile::read(cwd.join("jsm.lock")).map_err(|e| CommandExit {
                code: 6,
                message: e.to_string(),
            })?;
            let manifest = super::read_manifest(cwd)?;
            let (importer, _) = super::manifest_importer(&manifest, false)?;
            let expected = BTreeMap::from([(String::from("."), importer)]);
            let stale = lock.is_stale(&expected);
            let missing = jsm_lockfile::verify_lockfile(&lock, &expected)
                .err()
                .map(|error| vec![error.to_string()])
                .unwrap_or_default();
            let valid = !stale && missing.is_empty();
            let value = json!({"command":"lock verify","valid":valid,"stale":stale,"missing_edges":missing,"lockfile_version":lock.lockfile_version,"packages":lock.packages.len()});
            if cli.json {
                emit_json("lock.verify", value);
            } else {
                println!(
                    "{}",
                    if valid {
                        "lockfile verified"
                    } else {
                        "lockfile is stale or has invalid edges"
                    }
                );
            }
            if !valid {
                return Err(CommandExit{code:6,message:"lockfile verification failed; run install or resolve the reported graph errors".into()}.into());
            }
            Ok(())
        }
        LockCommand::Merge { base, ours, theirs } => {
            let base = Lockfile::read(base).into_diagnostic()?;
            let ours_path = ours.clone();
            let ours_lock = Lockfile::read(&ours).into_diagnostic()?;
            let theirs = Lockfile::read(theirs).into_diagnostic()?;
            let merged = jsm_lockfile::merge_lockfiles(&base, &ours_lock, &theirs)
                .map_err(|error| miette!("lockfile merge conflict: {error}"))?;
            let manifest = super::read_manifest(cwd)?;
            let (importer, _) = super::manifest_importer(&manifest, false)?;
            let expected = BTreeMap::from([(String::from("."), importer)]);
            jsm_lockfile::verify_lockfile(&merged, &expected).map_err(|error| {
                miette!("merged lockfile does not match the current manifest: {error}")
            })?;
            let bytes = merged.serialize();
            atomic_write(&ours_path, bytes.as_bytes())?;
            if cli.json {
                emit_json(
                    "lock.merge",
                    json!({"command":"lock merge","merged":true,"packages":merged.packages.len()}),
                );
            }
            Ok(())
        }
        LockCommand::InstallMergeDriver => {
            let path = cwd.join(".gitattributes");
            let mut content = fs::read_to_string(&path).unwrap_or_default();
            let rule = "jsm.lock merge=jsm-lock";
            if !content.lines().any(|line| line.trim() == rule) {
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(rule);
                content.push('\n');
                atomic_write(&path, content.as_bytes())?;
            }
            let name = ProcessCommand::new("git")
                .current_dir(cwd)
                .args([
                    "config",
                    "--local",
                    "merge.jsm-lock.name",
                    "JSM lockfile merge driver",
                ])
                .status()
                .into_diagnostic()?;
            if !name.success() {
                return Err(miette!(
                    "git config could not set the repository-local merge driver"
                ));
            }
            let driver = ProcessCommand::new("git")
                .current_dir(cwd)
                .args([
                    "config",
                    "--local",
                    "merge.jsm-lock.driver",
                    "jsm lock merge %O %A %B",
                ])
                .status()
                .into_diagnostic()?;
            if !driver.success() {
                return Err(miette!(
                    "git config could not set the repository-local merge command"
                ));
            }
            if cli.json {
                emit_json(
                    "lock.install-merge-driver",
                    json!({"command":"lock install-merge-driver","configured":true,"attributes":rule}),
                );
            } else {
                println!("configured repository-local merge driver for jsm.lock");
            }
            Ok(())
        }
    }
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).into_diagnostic()?;
    }
    let temp = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_seconds()));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .into_diagnostic()?;
        file.write_all(bytes).into_diagnostic()?;
        file.sync_all().into_diagnostic()?;
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error).into_diagnostic();
    }
    Ok(())
}
