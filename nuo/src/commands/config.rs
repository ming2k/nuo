use crate::cli::ConfigAction;
use nuo_persistence::config::Config;

pub fn run(action: ConfigAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ConfigAction::Path => {
            println!("Primary Legacy Config:  {}", Config::config_file_path().display());
            println!("Server Config (ADR-0031): {}", Config::server_config_file_path().display());
            println!("Client Config (ADR-0031): {}", Config::client_config_file_path().display());
            println!("Terminal Config (ADR-0031): {}", Config::terminal_config_file_path().display());
            println!("Agent Config (ADR-0031):  {}", Config::agent_config_file_path().display());
            println!("Credentials Store (ADR-0032): {}", nuo_host::paths::get().credentials_file().display());
        }
        ConfigAction::Migrate => {
            println!("Executing ADR-0031 configuration domain separation migration...");
            let config_path = Config::config_file_path();
            if !config_path.exists() {
                println!("No legacy config.toml found at {}. Nothing to migrate.", config_path.display());
                return Ok(());
            }
            let legacy_cfg = Config::load();

            // 1. server.toml
            let server_path = Config::server_config_file_path();
            if !server_path.exists() {
                let toml_str = format!(
                    "[lifecycle]\nshutdown_grace_secs = {}\nidle_exit_minutes = {}\n\n[network]\nlocal_auth = {}\n",
                    legacy_cfg.daemon.shutdown_grace_secs,
                    legacy_cfg.daemon.idle_exit_minutes,
                    legacy_cfg.daemon.local_auth,
                );
                std::fs::write(&server_path, toml_str)?;
                println!("  ✓ Created {}", server_path.display());
            }

            // 2. client.toml
            let client_path = Config::client_config_file_path();
            if !client_path.exists() {
                let toml_str = format!(
                    "[connection]\ndefault_connection = {:?}\ndefault_model = {:?}\n\n[resilience]\nretry_max_attempts = {}\nretry_base_delay_ms = {}\nretry_max_delay_ms = {}\n\n[models]\nfavorites = {:?}\nhidden = {:?}\n",
                    legacy_cfg.default_connection,
                    legacy_cfg.default_model,
                    legacy_cfg.connection_retry_max_attempts,
                    legacy_cfg.connection_retry_base_ms,
                    legacy_cfg.connection_retry_max_ms,
                    legacy_cfg.favorites,
                    legacy_cfg.hidden_models,
                );
                std::fs::write(&client_path, toml_str)?;
                println!("  ✓ Created {}", client_path.display());
            }

            // 3. terminal.toml
            let terminal_path = Config::terminal_config_file_path();
            let tui_path = nuo_host::paths::get().tui_config_file();
            if !terminal_path.exists() {
                if tui_path.exists() {
                    let content = std::fs::read_to_string(&tui_path)?;
                    std::fs::write(&terminal_path, content)?;
                    println!("  ✓ Migrated {} -> {}", tui_path.display(), terminal_path.display());
                } else {
                    std::fs::write(&terminal_path, "[appearance]\ncolor_scheme = \"default\"\n")?;
                    println!("  ✓ Created {}", terminal_path.display());
                }
            }

            // 4. agent.toml
            let agent_path = Config::agent_config_file_path();
            if !agent_path.exists() {
                let mut agent_doc = toml::value::Table::new();
                if let Ok(agent_val) = toml::Value::try_from(&legacy_cfg.agent) {
                    agent_doc.insert("agent".to_string(), agent_val);
                }
                if let Ok(ctx_val) = toml::Value::try_from(&legacy_cfg.context) {
                    agent_doc.insert("context".to_string(), ctx_val);
                }
                if let Ok(perm_val) = toml::Value::try_from(&legacy_cfg.permissions) {
                    agent_doc.insert("permissions".to_string(), perm_val);
                }
                if let Ok(bash_val) = toml::Value::try_from(&legacy_cfg.bash_policy) {
                    agent_doc.insert("bash_policy".to_string(), bash_val);
                }
                if let Ok(web_val) = toml::Value::try_from(&legacy_cfg.web) {
                    agent_doc.insert("web".to_string(), web_val);
                }
                if let Ok(skills_val) = toml::Value::try_from(&legacy_cfg.skills) {
                    agent_doc.insert("skills".to_string(), skills_val);
                }
                if let Ok(mcp_val) = toml::Value::try_from(&legacy_cfg.mcp) {
                    agent_doc.insert("mcp".to_string(), mcp_val);
                }
                let formatted = toml::to_string_pretty(&agent_doc)?;
                std::fs::write(&agent_path, formatted)?;
                println!("  ✓ Created {}", agent_path.display());
            }

            println!("\nMigration completed successfully. Legacy configuration decoupled into domain-separated matrices under ADR-0031.");
            return Ok(());
        }
        ConfigAction::Check => {
            let findings = nuo_persistence::config_check::check_config_file(None);
            if findings.is_empty() {
                println!("config.toml is valid and every key is understood by this version.");
                return Ok(());
            }
            let mut legacy = 0;
            for finding in &findings {
                if finding.is_legacy {
                    legacy += 1;
                }
                println!("  {}: {}", finding.key, finding.message);
            }
            println!(
                "\n{} finding(s), {} legacy key(s). Unknown keys are ignored at \
                 load, so none of these block startup — but a typo silently \
                 falls back to the default.",
                findings.len(),
                legacy
            );
            std::process::exit(1);
        }
        ConfigAction::List => {
            let config = Config::load();
            println!(
                "Configuration file: {}\n",
                Config::config_file_path().display()
            );
            println!(
                "default_connection: {}",
                if config.default_connection.is_empty() {
                    "(none)"
                } else {
                    &config.default_connection
                }
            );
            println!(
                "default_model:      {}",
                config.default_model.as_deref().unwrap_or("(none)")
            );
            println!(
                "retry_max_attempts:  {}",
                config.connection_retry_max_attempts
            );
            println!("retry_base_ms:       {}ms", config.connection_retry_base_ms);
            println!("retry_max_ms:        {}ms", config.connection_retry_max_ms);
            println!(
                "context.schema_version:           {}",
                config.context.schema_version
            );
            println!(
                "context.preferred_recent_rounds:  {}",
                config.context.preferred_recent_rounds
            );
            println!(
                "context.checkpoint_enabled:       {}",
                config.context.checkpoint_enabled
            );
            println!(
                "context.fallback_window_tokens:   {}",
                config.context.fallback_window_tokens
            );
            println!("mcp_servers_count:          {}", config.mcp.len());
            println!(
                "connections_count:          {}",
                nuo_persistence::connections::Connections::load()
                    .connections
                    .len()
            );
        }
        ConfigAction::Get(key) => {
            let config = Config::load();
            match key.as_str() {
                "default_connection" | "default_provider" => {
                    println!("{}", config.default_connection)
                }
                "default_model" => println!("{}", config.default_model.as_deref().unwrap_or("")),
                "connection_retry_max_attempts" | "provider_retry_max_attempts" => {
                    println!("{}", config.connection_retry_max_attempts)
                }
                "connection_retry_base_ms" | "provider_retry_base_ms" => {
                    println!("{}", config.connection_retry_base_ms)
                }
                "connection_retry_max_ms" | "provider_retry_max_ms" => {
                    println!("{}", config.connection_retry_max_ms)
                }
                "context.schema_version" => {
                    println!("{}", config.context.schema_version)
                }
                "context.preferred_recent_rounds" => {
                    println!("{}", config.context.preferred_recent_rounds)
                }
                "context.checkpoint_enabled" => {
                    println!("{}", config.context.checkpoint_enabled)
                }
                "context.fallback_window_tokens" => {
                    println!("{}", config.context.fallback_window_tokens)
                }
                "context.inspect_page_tokens" => {
                    println!("{}", config.context.inspect_page_tokens)
                }
                k if k.starts_with("compaction.") || k.starts_with("compaction_") => {
                    eprintln!("legacy 'compaction.*' configuration is retired under ADR-0280 [INV-POLICY-01]; run `nuo context migrate` to convert to versioned `context.*` policy");
                }
                "agent.hard_stop_turns" => {
                    println!("{}", config.agent.hard_stop_turns)
                }
                "agent.allow_model_stdin" => {
                    println!("{}", config.agent.allow_model_stdin)
                }
                "agent.skip_interactive_input" => {
                    println!("{}", config.agent.skip_interactive_input)
                }
                "agent.trajectory_guard.enabled" => {
                    println!("{}", config.agent.trajectory_guard.enabled)
                }
                "agent.trajectory_guard.window" => {
                    println!("{}", config.agent.trajectory_guard.window)
                }
                "agent.trajectory_guard.threshold" => {
                    println!("{}", config.agent.trajectory_guard.threshold)
                }
                "agent.trajectory_guard.cognitive_review" => {
                    println!("{}", config.agent.trajectory_guard.cognitive_review)
                }
                "daemon.shutdown_grace_secs" => println!("{}", config.daemon.shutdown_grace_secs),
                "daemon.idle_exit_minutes" => println!("{}", config.daemon.idle_exit_minutes),
                "daemon.local_auth" => println!("{}", config.daemon.local_auth),
                "terminal.color_scheme"
                | "terminal.transcript_layout"
                | "terminal.click_outside_dismiss"
                | "terminal.expand_auto_scroll"
                | "tui.color_scheme"
                | "tui.transcript_layout"
                | "tui.click_outside_dismiss"
                | "tui.expand_auto_scroll"
                | "input_history.dedup"
                | "input_history.record_commands" => {
                    return Err(
                        "terminal presentation settings live in $XDG_CONFIG_HOME/nuo/terminal.toml (ADR-0031)"
                            .into(),
                    );
                }
                other => {
                    return Err(format!("unknown configuration key '{other}'").into());
                }
            }
        }
        ConfigAction::Set { key, value } => {
            let mut config = Config::load();
            match key.as_str() {
                "default_connection" | "default_provider" => {
                    config.default_connection = value.clone();
                }
                "default_model" => {
                    config.default_model = Some(value.clone());
                }
                "connection_retry_max_attempts" | "provider_retry_max_attempts" => {
                    config.connection_retry_max_attempts = value
                        .parse()
                        .map_err(|_| "invalid integer for retry_max_attempts")?;
                }
                "connection_retry_base_ms" | "provider_retry_base_ms" => {
                    config.connection_retry_base_ms = value
                        .parse()
                        .map_err(|_| "invalid integer for retry_base_ms")?;
                }
                "connection_retry_max_ms" | "provider_retry_max_ms" => {
                    config.connection_retry_max_ms = value
                        .parse()
                        .map_err(|_| "invalid integer for retry_max_ms")?;
                }
                "context.preferred_recent_rounds" => {
                    config.context.preferred_recent_rounds = value
                        .parse()
                        .map_err(|_| "invalid integer for context.preferred_recent_rounds")?;
                }
                "context.checkpoint_enabled" => {
                    config.context.checkpoint_enabled = value
                        .parse()
                        .map_err(|_| "invalid boolean for context.checkpoint_enabled")?;
                }
                "context.fallback_window_tokens" => {
                    config.context.fallback_window_tokens = value
                        .parse()
                        .map_err(|_| "invalid integer for context.fallback_window_tokens")?;
                }
                "context.inspect_page_tokens" => {
                    config.context.inspect_page_tokens = value
                        .parse()
                        .map_err(|_| "invalid integer for context.inspect_page_tokens")?;
                }
                k if k.starts_with("compaction.") || k.starts_with("compaction_") => {
                    return Err("legacy 'compaction.*' configuration is retired under ADR-0280 [INV-POLICY-01]; run `nuo context migrate` to convert to versioned `context.*` policy".into());
                }
                "agent.hard_stop_turns" => {
                    config.agent.hard_stop_turns = value
                        .parse()
                        .map_err(|_| "invalid integer for agent.hard_stop_turns")?;
                }
                "agent.allow_model_stdin" => {
                    config.agent.allow_model_stdin = value
                        .parse()
                        .map_err(|_| "invalid boolean for agent.allow_model_stdin")?;
                }
                "agent.skip_interactive_input" => {
                    config.agent.skip_interactive_input = value
                        .parse()
                        .map_err(|_| "invalid boolean for agent.skip_interactive_input")?;
                }
                "agent.trajectory_guard.enabled" => {
                    config.agent.trajectory_guard.enabled = value
                        .parse()
                        .map_err(|_| "invalid boolean for agent.trajectory_guard.enabled")?;
                }
                "agent.trajectory_guard.window" => {
                    config.agent.trajectory_guard.window = value
                        .parse()
                        .map_err(|_| "invalid integer for agent.trajectory_guard.window")?;
                }
                "agent.trajectory_guard.threshold" => {
                    config.agent.trajectory_guard.threshold = value
                        .parse()
                        .map_err(|_| "invalid integer for agent.trajectory_guard.threshold")?;
                }
                "agent.trajectory_guard.cognitive_review" => {
                    config.agent.trajectory_guard.cognitive_review = value.parse().map_err(
                        |_| "invalid boolean for agent.trajectory_guard.cognitive_review",
                    )?;
                }
                "daemon.shutdown_grace_secs" => {
                    config.daemon.shutdown_grace_secs = value
                        .parse()
                        .map_err(|_| "invalid integer for daemon.shutdown_grace_secs")?;
                }
                "daemon.idle_exit_minutes" => {
                    config.daemon.idle_exit_minutes = value
                        .parse()
                        .map_err(|_| "invalid integer for daemon.idle_exit_minutes")?;
                }
                "daemon.local_auth" => {
                    config.daemon.local_auth = value
                        .parse()
                        .map_err(|_| "invalid boolean for daemon.local_auth")?;
                }
                "terminal.color_scheme"
                | "terminal.transcript_layout"
                | "terminal.click_outside_dismiss"
                | "terminal.expand_auto_scroll"
                | "tui.color_scheme"
                | "tui.transcript_layout"
                | "tui.click_outside_dismiss"
                | "tui.expand_auto_scroll"
                | "input_history.dedup"
                | "input_history.record_commands" => {
                    return Err(
                        "terminal presentation settings live in $XDG_CONFIG_HOME/nuo/terminal.toml (ADR-0031)"
                            .into(),
                    );
                }
                other => {
                    return Err(
                        format!("unsupported or read-only configuration key '{other}'").into(),
                    );
                }
            }
            config.save()?;
            println!("Updated {} = {}", key, value);
        }
    }
    Ok(())
}
