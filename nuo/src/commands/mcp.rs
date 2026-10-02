use crate::cli::McpAction;
use nuo_contracts::mcp::McpServerConfig;
use nuo_persistence::config::Config;

/// `muta mcp …` — read-only discovery and inspection for MCP servers (ADR-0252).
///
/// Imperative CLI mutations (add, rm, enable, disable, import) have been
/// permanently retired in ADR-0252. Configuration is declarative-only in
/// `~/.config/nuo/config.toml` or `<repo>/.nuo/config.toml`.
pub fn run(action: McpAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        McpAction::List => list(),
        McpAction::Get { name } => {
            let config = Config::load();
            let Some(server) = config.mcp.get(&name) else {
                return Err(format!("no MCP server named '{name}' in config.toml").into());
            };
            print_server(&name, server);
            Ok(())
        }
        McpAction::Probe { .. } => unreachable!("probe is async; dispatched in main"),
    }
}

fn list() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    if config.mcp.is_empty() {
        println!("No MCP servers configured in config.toml.");
        println!("Tip: configure MCP servers declaratively in config.toml under [mcp.<name>].");
        return Ok(());
    }

    println!(
        "{:<18} {:<10} {:<24} Arguments",
        "Server Name", "Status", "Command"
    );
    println!("{:-<18} {:-<10} {:-<24} {:-<20}", "", "", "", "");

    for (name, server) in &config.mcp {
        let status = if server.enabled {
            "Enabled"
        } else {
            "Disabled"
        };
        print_row(name, server, status);
    }
    Ok(())
}

fn print_row(name: &str, server: &McpServerConfig, status: &str) {
    let (command, args) = match &server.url {
        Some(url) => ("http", url.clone()),
        None => (
            server.command.first().map(String::as_str).unwrap_or(""),
            server
                .command
                .iter()
                .skip(1)
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
        ),
    };
    println!("{:<18} {:<10} {:<24} {}", name, status, command, args);
}

fn print_server(name: &str, server: &McpServerConfig) {
    println!("[mcp.{name}]");
    if let Some(url) = &server.url {
        println!("url = {url:?}");
    }
    if !server.command.is_empty() {
        let pretty: Vec<String> = server.command.iter().map(|c| format!("{c:?}")).collect();
        println!("command = [{}]", pretty.join(", "));
    }
    if !server.environment.is_empty() {
        let mut env: Vec<_> = server.environment.iter().collect();
        env.sort();
        let pairs: Vec<String> = env.iter().map(|(k, v)| format!("{k:?} = {v:?}")).collect();
        println!("environment = {{ {} }}", pairs.join(", "));
    }
    println!("enabled = {}", server.enabled);
    println!("read_only = {}", server.read_only);
    if !server.allow_tools.is_empty() {
        let tools: Vec<String> = server
            .allow_tools
            .iter()
            .map(|t| format!("{t:?}"))
            .collect();
        println!("allow_tools = [{}]", tools.join(", "));
    }
    if !server.deny_tools.is_empty() {
        let tools: Vec<String> = server.deny_tools.iter().map(|t| format!("{t:?}")).collect();
        println!("deny_tools = [{}]", tools.join(", "));
    }
}

/// `muta mcp probe <name>` — connect to one configured server, list the tools
/// it advertises, then drop the connection. Async because the MCP client is
/// tokio-based; dispatched directly from `main`.
pub async fn probe(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    let Some(server) = config.mcp.get(name) else {
        return Err(format!("no MCP server named '{name}' in config.toml").into());
    };
    if !server.enabled {
        println!("(server '{name}' is disabled — probing anyway)");
    }
    println!("Connecting to [mcp.{name}] …");
    match nuo::mcp::connect_server(name, server).await {
        Ok((_handle, tools)) => {
            println!(
                "Connected. {} tool(s) advertised (names as published to the agent):",
                tools.len()
            );
            for tool in &tools {
                println!("  {:<32} {}", tool.name(), first_line(tool.description()));
            }
            drop(_handle);
            Ok(())
        }
        Err(error) => Err(format!("could not connect to '{name}': {error}").into()),
    }
}

fn first_line(description: &str) -> String {
    description
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(72)
        .collect()
}
