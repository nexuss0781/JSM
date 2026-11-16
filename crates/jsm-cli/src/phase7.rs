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
