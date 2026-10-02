use crate::cli::ConfigAction;
use nuo_persistence::config::Config;

pub fn run(action: ConfigAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ConfigAction::Path => {
            println!("{}", Config::config_file_path().display());
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
                    eprintln!("legacy 'compaction.*' configuration is retired under ADR-0280 [INV-POLICY-01]; run `muta context migrate` to convert to versioned `context.*` policy");
                }
                "agent.hard_stop_turns" | "master.hard_stop_turns" => {
                    println!("{}", config.agent.hard_stop_turns)
                }
                "agent.allow_model_stdin" | "master.allow_model_stdin" => {
                    println!("{}", config.agent.allow_model_stdin)
                }
                "agent.skip_interactive_input" | "master.skip_interactive_input" => {
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
                "tui.color_scheme"
                | "tui.transcript_layout"
                | "tui.click_outside_dismiss"
                | "tui.expand_auto_scroll"
                | "input_history.dedup"
                | "input_history.record_commands" => {
                    return Err(
                        "TUI presentation settings have been decoupled to $XDG_CONFIG_HOME/mutx/config.toml (ADR-0136)"
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
                    return Err("legacy 'compaction.*' configuration is retired under ADR-0280 [INV-POLICY-01]; run `muta context migrate` to convert to versioned `context.*` policy".into());
                }
                "agent.hard_stop_turns" | "master.hard_stop_turns" => {
                    config.agent.hard_stop_turns = value
                        .parse()
                        .map_err(|_| "invalid integer for agent.hard_stop_turns")?;
                }
                "agent.allow_model_stdin" | "master.allow_model_stdin" => {
                    config.agent.allow_model_stdin = value
                        .parse()
                        .map_err(|_| "invalid boolean for agent.allow_model_stdin")?;
                }
                "agent.skip_interactive_input" | "master.skip_interactive_input" => {
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
                "tui.color_scheme"
                | "tui.transcript_layout"
                | "tui.click_outside_dismiss"
                | "tui.expand_auto_scroll"
                | "input_history.dedup"
                | "input_history.record_commands" => {
                    return Err(
                        "TUI presentation settings have been decoupled to $XDG_CONFIG_HOME/mutx/config.toml (ADR-0136)"
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
