use super::*;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SnapshotCommandKind {
    Save,
    Restore,
    List,
    Delete,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
struct HistoryState {
    entries: Vec<HistoryEntry>,
    position: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct HistoryEntry {
    id: usize,
    timestamp: i64,
    command: String,
    before: Option<String>,
    after: Option<String>,
    summary: String,
}

fn history_path(cwd: &Path) -> PathBuf {
    cwd.join(".jsm-history.json")
}

fn load_history(cwd: &Path) -> HistoryState {
    fs::read_to_string(history_path(cwd))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_history(cwd: &Path, state: &HistoryState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state).into_diagnostic()?;
    fs::write(history_path(cwd), bytes).into_diagnostic()?;
    Ok(())
}

pub fn record_history(cwd: &Path, command: &str, before: Option<String>, after: Option<String>) {
    let mut state = load_history(cwd);
    let id = state.entries.len() + 1;
    state.entries.push(HistoryEntry {
        id,
        timestamp: now_seconds(),
        command: command.to_string(),
        before,
        after,
        summary: format!("{command} completed"),
    });
    state.position = state.entries.len();
    let _ = save_history(cwd, &state);
}

pub fn history(cwd: &Path, limit: Option<usize>, json: bool) -> Result<()> {
    let state = load_history(cwd);
    let limit = limit.unwrap_or(20).min(state.entries.len());
    let start = state.entries.len().saturating_sub(limit);
    let slice = &state.entries[start..];
    if json {
        emit_json(
            "history",
            serde_json::json!({
                "command": "history",
                "position": state.position,
                "entries": slice,
            }),
        );
    } else {
        println!(
            "history position {}/{}",
            state.position,
            state.entries.len()
        );
        for entry in slice {
            println!("  #{} {} {}", entry.id, entry.timestamp, entry.command);
        }
    }
    Ok(())
}

pub fn lockfile_text(cwd: &Path) -> Option<String> {
    fs::read_to_string(cwd.join("jsm.lock")).ok()
}

pub fn undo(
    cli: &Cli,
    cwd: &Path,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    let mut state = load_history(cwd);
    if state.position == 0 {
        return Err(miette!("nothing to undo"));
    }
    let entry = state.entries[state.position - 1].clone();
    restore_lockfile(cwd, entry.before.clone())?;
    let mut silent = cli.clone();
    silent.json = false;
    silent.quiet = true;
    install(&silent, cwd, false, false, false, false, cancellation, progress)?;
    state.position -= 1;
    save_history(cwd, &state)?;
    if cli.json {
        emit_json("undo", serde_json::json!({"command":"undo","id":entry.id}));
    } else if !cli.quiet {
        println!("undone {}", entry.command);
    }
    Ok(())
}

pub fn redo(
    cli: &Cli,
    cwd: &Path,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    let mut state = load_history(cwd);
    if state.position >= state.entries.len() {
        return Err(miette!("nothing to redo"));
    }
    let entry = state.entries[state.position].clone();
    restore_lockfile(cwd, entry.after.clone())?;
    let mut silent = cli.clone();
    silent.json = false;
    silent.quiet = true;
    install(&silent, cwd, false, false, false, false, cancellation, progress)?;
    state.position += 1;
    save_history(cwd, &state)?;
    if cli.json {
        emit_json("redo", serde_json::json!({"command":"redo","id":entry.id}));
    } else if !cli.quiet {
        println!("redone {}", entry.command);
    }
    Ok(())
}

fn restore_lockfile(cwd: &Path, content: Option<String>) -> Result<()> {
    let path = cwd.join("jsm.lock");
    match content {
        Some(text) => fs::write(path, text).into_diagnostic()?,
        None => {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

pub fn snapshot(
    cli: &Cli,
    cwd: &Path,
    kind: SnapshotCommandKind,
    name: Option<String>,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    let dir = cwd.join(".jsm").join("snapshots");
    fs::create_dir_all(&dir).into_diagnostic()?;
    match kind {
        SnapshotCommandKind::Save => {
            let name = name.ok_or_else(|| miette!("snapshot name required"))?;
            let content = lockfile_text(cwd).unwrap_or_default();
            fs::write(dir.join(format!("{name}.lock")), content).into_diagnostic()?;
            if cli.json {
                emit_json(
                    "snapshot",
                    serde_json::json!({"command":"snapshot","action":"save","name":name}),
                );
            } else if !cli.quiet {
                println!("saved snapshot {name}");
            }
        }
        SnapshotCommandKind::Restore => {
            let name = name.ok_or_else(|| miette!("snapshot name required"))?;
            let before = lockfile_text(cwd);
            let content = fs::read_to_string(dir.join(format!("{name}.lock"))).into_diagnostic()?;
            restore_lockfile(cwd, Some(content.clone()))?;
            let mut silent = cli.clone();
            silent.json = false;
            silent.quiet = true;
            install(&silent, cwd, false, false, false, false, cancellation, progress)?;
            record_history(
                cwd,
                &format!("snapshot restore {name}"),
                before,
                lockfile_text(cwd),
            );
            if cli.json {
                emit_json(
                    "snapshot",
                    serde_json::json!({"command":"snapshot","action":"restore","name":name}),
                );
            } else if !cli.quiet {
                println!("restored snapshot {name}");
            }
        }
        SnapshotCommandKind::List => {
            let mut names = Vec::new();
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if let Some(name) = entry.file_name().to_str()
                        && let Some(stem) = name.strip_suffix(".lock")
                    {
                        names.push(stem.to_string());
                    }
                }
            }
            names.sort();
            if cli.json {
                emit_json(
                    "snapshot",
                    serde_json::json!({"command":"snapshot","action":"list","names":names}),
                );
            } else {
                for name in names {
                    println!("{name}");
                }
            }
        }
        SnapshotCommandKind::Delete => {
            let name = name.ok_or_else(|| miette!("snapshot name required"))?;
            let _ = fs::remove_file(dir.join(format!("{name}.lock")));
            if cli.json {
                emit_json(
                    "snapshot",
                    serde_json::json!({"command":"snapshot","action":"delete","name":name}),
                );
            } else if !cli.quiet {
                println!("deleted snapshot {name}");
            }
        }
    }
    Ok(())
}

pub fn switch(
    cli: &Cli,
    cwd: &Path,
    package: &str,
    version: &str,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    let before = lockfile_text(cwd);
    let path = manifest_path(cwd);
    let original = fs::read_to_string(&path).into_diagnostic()?;
    let mut manifest =
        Manifest::parse(path.display().to_string(), original.clone()).into_diagnostic()?;
    let kinds = [
        ManifestDependencyType::Dependencies,
        ManifestDependencyType::DevDependencies,
        ManifestDependencyType::PeerDependencies,
        ManifestDependencyType::OptionalDependencies,
    ];
    let mut found = false;
    for kind in kinds {
        if manifest.dependency(kind, package).is_some() {
            manifest
                .add_dependency(kind, package, version, SaveMode::Exact, '^')
                .into_diagnostic()?;
            found = true;
        }
    }
    if !found {
        return Err(miette!("package `{package}` is not a dependency"));
    }
    fs::write(&path, manifest.source()).into_diagnostic()?;
    let mut silent = cli.clone();
    silent.json = false;
    silent.quiet = true;
    if let Err(error) = install(&silent, cwd, false, false, false, false, cancellation, progress) {
        fs::write(&path, original).into_diagnostic()?;
        return Err(error);
    }
    record_history(
        cwd,
        &format!("switch {package} {version}"),
        before,
        lockfile_text(cwd),
    );
    if cli.json {
        emit_json(
            "switch",
            serde_json::json!({"command":"switch","package":package,"version":version}),
        );
    } else if !cli.quiet {
        println!("switched {package} to {version}");
    }
    Ok(())
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

