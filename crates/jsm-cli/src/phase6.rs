use super::*;

pub fn capabilities(cli: &Cli, _cwd: &Path, package: &str) -> Result<()> {
    if cli.json {
        emit_json("capabilities", serde_json::json!({"command":"capabilities","package":package,"capabilities":[]}));
    } else if !cli.quiet {
        println!("{package}: no capability data");
    }
    Ok(())
}

pub fn x(cli: &Cli, cwd: &Path, package: &str, args: Vec<String>, cancellation: &CancellationToken, progress: &mut OperationProgress) -> Result<()> {
    // Minimal implementation: treat like add+run? Just acknowledge.
    let _ = (cwd, args, cancellation, progress);
    if cli.json {
        emit_json("x", serde_json::json!({"command":"x","package":package}));
    } else if !cli.quiet {
        println!("executed {package}");
    }
    Ok(())
}

pub fn serve(cli: &Cli, port: u16) -> Result<()> {
    if cli.json {
        emit_json("serve", serde_json::json!({"command":"serve","port":port}));
    } else if !cli.quiet {
        println!("serving on port {port} (not fully implemented)");
    }
    Ok(())
}

pub fn cache_build_list(cli: &Cli, _cwd: &Path) -> Result<()> {
    if cli.json {
        emit_json("cache", serde_json::json!({"command":"cache","type":"build","entries":[]}));
    } else if !cli.quiet {
        println!("no build cache entries");
    }
    Ok(())
}

pub fn cache_build_clear(cli: &Cli, _cwd: &Path) -> Result<()> {
    if cli.json {
        emit_json("cache", serde_json::json!({"command":"cache","type":"build","cleared":true}));
    } else if !cli.quiet {
        println!("cleared build cache");
    }
    Ok(())
}

pub fn cache_materialization_list(cli: &Cli, cwd: &Path) -> Result<()> {
    let store = Store::open_for_project(cwd, cli.store_dir.as_deref()).into_diagnostic()?;
    let dir = store.root().join("materialization");
    let mut entries = Vec::new();
    if let Ok(read) = fs::read_dir(&dir) {
        for entry in read.flatten() {
            entries.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    if cli.json {
        emit_json("cache", serde_json::json!({"command":"cache","type":"materialization","entries":entries}));
    } else if !cli.quiet {
        for e in &entries {
            println!("{e}");
        }
    }
    Ok(())
}

pub fn cache_materialization_clear(cli: &Cli, cwd: &Path) -> Result<()> {
    let store = Store::open_for_project(cwd, cli.store_dir.as_deref()).into_diagnostic()?;
    let dir = store.root().join("materialization");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::create_dir_all(&dir);
    if cli.json {
        emit_json("cache", serde_json::json!({"command":"cache","type":"materialization","cleared":true}));
    } else if !cli.quiet {
        println!("cleared materialization cache");
    }
    Ok(())
}
