// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use nuo::transport;
mod cli;
mod commands;
mod identity;
mod status;
mod supervisor;

use status::{StatusOptions, run as run_status};
use supervisor::{
    ServerStart, detach_server, reload_server, restart_server, run_server_foreground, stop_server,
};

use cli::{CliArgs, Mode, ServerAction};
use std::path::PathBuf;

/// Worker-thread stack size for the server.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    nuo_server::provider_registry::init();
    #[cfg(feature = "interactive")]
    nuo::shell::runner::ensure_dev_environment();
    let _tracing_guard = transport::init_tracing();
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
        Mode::Interactive(args) => run_interactive(args).await,
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
        Mode::Dev(cli::DevAction::Storybook) => run_storybook().await,
        Mode::Dev(cli::DevAction::Export(dir)) => export_storybook(&dir),
        Mode::Dev(cli::DevAction::Check(target)) => run_dev_check(target),
        Mode::Thread(action) => commands::thread::run(action, project_override).await,
        Mode::Server(action) => run_server_action(action, project_override).await,
    }
}

#[cfg(feature = "interactive")]
async fn run_interactive(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let tui_args = nuo::shell::cli::parse(&args)?;
    nuo::shell::runner::run_with_options(tui_args).await
}

#[cfg(not(feature = "interactive"))]
async fn run_interactive(_args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    Err("interactive mode is unavailable: this build was compiled without the `interactive` feature"
        .into())
}

#[cfg(feature = "interactive")]
async fn run_storybook() -> Result<(), Box<dyn std::error::Error>> {
    let mut workbench = nuo::shell::declarative::workbench::Workbench::default();
    workbench.run()?;
    Ok(())
}

#[cfg(not(feature = "interactive"))]
async fn run_storybook() -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "the storybook is unavailable: this build was compiled without the `interactive` feature"
            .into(),
    )
}

#[cfg(feature = "interactive")]
fn export_storybook(dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let count = nuo::shell::declarative::workbench::Workbench::export_all_snapshots(dir)?;
    println!("Exported {count} storybook snapshots to {}", dir.display());
    Ok(())
}

#[cfg(not(feature = "interactive"))]
fn export_storybook(_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    Err("storybook export is unavailable: this build was compiled without the `interactive` feature"
        .into())
}

fn run_dev_check(target: cli::CheckTarget) -> Result<(), Box<dyn std::error::Error>> {
    let root = nuo::dev::find_workspace_root();
    let mut ok = true;

    if target == cli::CheckTarget::All || target == cli::CheckTarget::Firewall {
        println!("Checking presentation firewall and workspace topology...");
        if let Err(e) = nuo::dev::firewall::check_presentation_firewall(&root) {
            eprintln!("{e}");
            ok = false;
        } else {
            println!("presentation firewall: clean");
        }

        println!("Checking lean CLI link (INV-ARCH-CLI-LEAN-LINK)...");
        if let Err(e) = nuo::dev::firewall::check_lean_cli_link(&root) {
            eprintln!("{e}");
            ok = false;
        } else {
            println!("lean CLI link: clean");
        }
    }

    if target == cli::CheckTarget::All || target == cli::CheckTarget::Docs {
        println!("Checking documentation hyperlinks and anchors...");
        if let Err(e) = nuo::dev::doc_links::check_doc_links(&root) {
            eprintln!("{e}");
            ok = false;
        }
    }

    if target == cli::CheckTarget::All || target == cli::CheckTarget::Adr {
        println!("Checking ADR reference lineage...");
        if let Err(e) = nuo::dev::adr_lineage::check_adr_lineage(&root) {
            eprintln!("{e}");
            ok = false;
        }
    }

    if target == cli::CheckTarget::All || target == cli::CheckTarget::Residency {
        println!("Checking Rust source residency (every .rs file reachable from a crate root)...");
        if let Err(e) = nuo::dev::residency::check_source_residency(&root) {
            eprintln!("{e}");
            ok = false;
        } else {
            println!("source residency: clean");
        }
    }

    if ok {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// server verb dispatch (the server verbs own start/stop/restart/status/token).
async fn run_server_action(
    action: ServerAction,
    project_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ServerAction::Start {
            foreground,
            port,
            public,
            no_local_auth,
            idle_exit_minutes,
            shutdown_grace_secs,
            zero_tcp,
        } => {
            let flags = ServerStart {
                port,
                public,
                no_local_auth,
                idle_exit_minutes,
                shutdown_grace_secs,
                zero_tcp,
            };
            if !foreground {
                return detach_server(&flags).await;
            }
            run_server_foreground(flags).await
        }
        ServerAction::Stop => stop_server().await,
        ServerAction::Reload => reload_server().await,
        ServerAction::Restart {
            force,
            port,
            public,
            no_local_auth,
            idle_exit_minutes,
            shutdown_grace_secs,
            zero_tcp,
        } => {
            let flags = ServerStart {
                port,
                public,
                no_local_auth,
                idle_exit_minutes,
                shutdown_grace_secs,
                zero_tcp,
            };
            restart_server(&flags, force).await
        }
        ServerAction::Token => {
            let project_root = project_override
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let info = transport::discover(&project_root)
                .await
                .ok_or("no local nuo server is running")?;
            match info.token {
                Some(token) => println!("{token}"),
                None => eprintln!("nuo: server authentication is disabled."),
            }
            Ok(())
        }
        ServerAction::Status {
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
