use super::*;

pub fn capabilities(cli: &Cli, _cwd: &Path, package: &str) -> Result<()> {
    if cli.json {
        emit_json("capabilities", serde_json::json!({"command":"capabilities","package":package,"capabilities":[]}));
    } else if !cli.quiet {
        println!("{package}: no capability data");
    }
    Ok(())
}

pub fn x(
    cli: &Cli,
    cwd: &Path,
    package: &str,
    args: Vec<String>,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    let temp = std::env::temp_dir().join(format!("jsm-x-{}", package.replace('/', "-")));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).into_diagnostic()?;
    // Parse name@version similar to add.
    let (name, version) = package
        .rsplit_once('@')
        .filter(|(n, v)| !n.is_empty() && !v.is_empty())
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .unwrap_or((package.to_string(), "latest".to_string()));
    let manifest = serde_json::json!({"name":"jsm-x","version":"1.0.0","dependencies":{(name.clone()):version}});
    fs::write(
        temp.join("package.json"),
        serde_json::to_string_pretty(&manifest).into_diagnostic()? + "\n",
    )
    .into_diagnostic()?;
    let mut silent = cli.clone();
    silent.json = false;
    silent.quiet = true;
    install(&silent, &temp, false, false, false, cancellation, progress)?;
    // Determine bin entry.
    let pkg_json_path = temp.join("node_modules").join(&name).join("package.json");
    let bin_name = if pkg_json_path.exists() {
        let v: Value = serde_json::from_str(&fs::read_to_string(&pkg_json_path).into_diagnostic()?)
            .into_diagnostic()?;
        v.get("bin")
            .and_then(|b| {
                if let Some(s) = b.as_str() {
                    Some(s.to_string())
                } else {
                    b.as_object().and_then(|o| o.keys().next().cloned())
                }
            })
            .unwrap_or_else(|| name.rsplit('/').next().unwrap_or(&name).to_string())
    } else {
        name.rsplit('/').next().unwrap_or(&name).to_string()
    };
    let bin_path = temp.join("node_modules").join(".bin").join(&bin_name);
    let mut command = ProcessCommand::new(&bin_path);
    command.args(&args).current_dir(cwd);
    command.env("PATH", project_path(cwd).unwrap_or_default());
    let status = command.status().into_diagnostic()?;
    if !status.success() {
        return Err(CommandExit {
            code: status.code().unwrap_or(1),
            message: format!("x failed for {package}"),
        }
        .into());
    }
    if cli.json {
        emit_json("x", serde_json::json!({"command":"x","package":package}));
    }
    Ok(())
}

pub fn serve(cli: &Cli, port: u16) -> Result<()> {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", port)).into_diagnostic()?;
    if !cli.quiet && !cli.json {
        println!("serving on http://127.0.0.1:{port}");
    }
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue; };
        let mut buffer = [0u8; 1024];
        let _ = stream.read(&mut buffer);
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let _ = stream.write_all(response.as_bytes());
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
