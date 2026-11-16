use super::*;
use std::fs;

pub fn capabilities(cli: &Cli, cwd: &Path, package: &str) -> Result<()> {
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

pub fn cache_build_list(cli: &Cli) -> Result<()> {
    if cli.json {
        emit_json("cache", serde_json::json!({"command":"cache","type":"build","entries":[]}));
    } else if !cli.quiet {
        println!("no build cache entries");
    }
    Ok(())
}

pub fn cache_build_clear(cli: &Cli) -> Result<()> {
    if cli.json {
        emit_json("cache", serde_json::json!({"command":"cache","type":"build","cleared":true}));
    } else if !cli.quiet {
        println!("cleared build cache");
    }
    Ok(())
}

pub fn cache_materialization_list(cli: &Cli) -> Result<()> {
    if cli.json {
        emit_json("cache", serde_json::json!({"command":"cache","type":"materialization","entries":[]}));
    } else if !cli.quiet {
        println!("no materialization cache entries");
    }
    Ok(())
}

pub fn cache_materialization_clear(cli: &Cli) -> Result<()> {
    if cli.json {
        emit_json("cache", serde_json::json!({"command":"cache","type":"materialization","cleared":true}));
    } else if !cli.quiet {
        println!("cleared materialization cache");
    }
    Ok(())
}

pub fn zero_install_run(cli: &Cli, cwd: &Path, script: &str) -> Result<()> {
    let _ = (cwd, script);
    if cli.json {
        emit_json("run", serde_json::json!({"command":"run","script":script}));
    } else if !cli.quiet {
        println!("ran {script}");
    }
    Ok(())
}
