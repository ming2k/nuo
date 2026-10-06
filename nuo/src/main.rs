#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use nuo_client as client;
use nuo_persistence::session;
mod cli;
mod commands;
mod identity;
mod status;
mod supervisor;

use status::{StatusOptions, run as run_status};
use supervisor::{DaemonStart, detach_daemon, run_daemon_foreground, stop_daemon};

use cli::{CliArgs, DaemonAction, McpAction, Mode};
use std::path::PathBuf;

/// Worker-thread stack size for the daemon.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    nuo_server::provider_registry::init();
    nuo_tui::runner::ensure_dev_environment();
    let _tracing_guard = nuo_client::init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(WORKER_STACK_BYTES)
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_background();
    result
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = match cli::parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("nuo: {error}\n\nRun 'nuo --help' for more information.");
            std::process::exit(2);
        }
    };

    let CliArgs {
        mode,
        project: project_override,
    } = parsed;

    match mode {
        Mode::Interactive(raw_args) => nuo_tui::runner::run_cli_args(&raw_args).await,
        Mode::Version => {
            println!("nuo {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Mode::Help(topic) => {
            if let Some(text) = cli::help_text(topic.as_deref()) {
                print!("{text}");
            }
            Ok(())
        }
        Mode::Completions(shell) => {
            print!("{}", cli::completion_script(shell));
            Ok(())
        }
        Mode::Doctor => session::run_doctor(project_override.as_deref())
            .await
            .map_err(Into::into),
        Mode::Config(action) => commands::config::run(action),
        Mode::Context(action) => commands::context::run(action),
        Mode::Auth(action) => commands::auth::run(action),
        Mode::Mcp(McpAction::Probe { name }) => commands::mcp::probe(&name).await,
        Mode::Mcp(action) => commands::mcp::run(action),
        Mode::Skill(action) => commands::skill::run(action).await,
        Mode::Session(action) => commands::session::run(action, project_override).await,
        Mode::Daemon(action) => run_daemon_action(action, project_override).await,
    }
}

/// daemon verb dispatch (ADR-0116: the daemon verbs own start/stop/status/token).
async fn run_daemon_action(
    action: DaemonAction,
    project_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DaemonAction::Start {
            foreground,
            port,
            public,
            no_local_auth,
            idle_exit_minutes,
            shutdown_grace_secs,
        } => {
            let flags = DaemonStart {
                port,
                public,
                no_local_auth,
                idle_exit_minutes,
                shutdown_grace_secs,
            };
            if !foreground {
                return detach_daemon(&flags);
            }
            run_daemon_foreground(flags).await
        }
        DaemonAction::Stop => stop_daemon().await,
        DaemonAction::Token => {
            let project_root = project_override
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let info = client::discover(&project_root).ok_or("no local nuo daemon is running")?;
            match info.token {
                Some(token) => println!("{token}"),
                None => eprintln!("nuo: daemon authentication is disabled."),
            }
            Ok(())
        }
        DaemonAction::Status {
            watch,
            json,
            include_idle,
            diagnostic,
        } => {
            let project_root = project_override
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            run_status(
                &project_root,
                StatusOptions {
                    watch,
                    json,
                    include_idle,
                    diagnostic,
                },
            )
            .await
        }
    }
}
