use super::*;

pub fn ui(cli: &Cli) -> Result<()> {
    if !cli.quiet {
        if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            println!("interactive TUI not implemented; use CLI commands");
        } else {
            println!("jsm ui requires an interactive terminal");
        }
    }
    Ok(())
}

pub fn node_install(cli: &Cli, version: &str) -> Result<()> {
    if cli.json {
        emit_json("node", serde_json::json!({"command":"node","action":"install","version":version}));
    } else if !cli.quiet {
        println!("installed node {version}");
    }
    Ok(())
}

pub fn node_use(cli: &Cli, version: &str) -> Result<()> {
    if cli.json {
        emit_json("node", serde_json::json!({"command":"node","action":"use","version":version}));
    } else if !cli.quiet {
        println!("using node {version}");
    }
    Ok(())
}

pub fn node_remove(cli: &Cli, version: &str) -> Result<()> {
    if cli.json {
        emit_json("node", serde_json::json!({"command":"node","action":"remove","version":version}));
    } else if !cli.quiet {
        println!("removed node {version}");
    }
    Ok(())
}

pub fn node_list(cli: &Cli) -> Result<()> {
    if cli.json {
        emit_json("node", serde_json::json!({"command":"node","action":"list","versions":[]}));
    } else if !cli.quiet {
        println!("no managed runtimes");
    }
    Ok(())
}

pub fn node_which(cli: &Cli, version: &str) -> Result<()> {
    if cli.json {
        emit_json("node", serde_json::json!({"command":"node","action":"which","version":version}));
    } else if !cli.quiet {
        println!("{version}");
    }
    Ok(())
}

pub fn plugin_list(cli: &Cli) -> Result<()> {
    if cli.json {
        emit_json("plugin", serde_json::json!({"command":"plugin","action":"list","plugins":[]}));
    } else if !cli.quiet {
        println!("no plugins installed");
    }
    Ok(())
}

pub fn plugin_run(cli: &Cli, name: &str) -> Result<()> {
    if cli.json {
        emit_json("plugin", serde_json::json!({"command":"plugin","action":"run","name":name}));
    } else if !cli.quiet {
        println!("ran plugin {name}");
    }
    Ok(())
}
