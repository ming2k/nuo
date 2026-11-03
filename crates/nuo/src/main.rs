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
    let is_server_mode = std::env::args().any(|arg| arg == "server");
    let _tracing_guard = if is_server_mode {
        transport::init_server_tracing()
    } else {
        transport::init_tracing()
    };
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
        Mode::Dev(cli::DevAction::Storybook { watch }) => run_storybook(watch).await,
        Mode::Dev(cli::DevAction::Export(dir)) => export_storybook(&dir),
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
async fn run_storybook(watch: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !watch {
        let mut workbench = nuo::shell::declarative::workbench::Workbench::default();
        workbench.run()?;
        return Ok(());
    }

    run_storybook_watcher().await
}

#[cfg(feature = "interactive")]
async fn run_storybook_watcher() -> Result<(), Box<dyn std::error::Error>> {
    use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;
    use std::time::Duration;

    println!("Nuo Storybook Native Watch Supervisor (ADR-0148 [INV-TUI-WATCH-01])");
    println!("Watching crates/ for changes... (Press Ctrl+C to stop)\n");

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                let is_rs = event
                    .paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|ext| ext == "rs"));
                if is_rs {
                    let _ = tx.send(());
                }
            }
        },
        notify::Config::default(),
    )?;

    for watch_path in [
        "crates/nuo/src",
        "crates/nuo-view/src",
        "crates/nuo-ui-primitives/src",
    ] {
        let p = std::path::Path::new(watch_path);
        if p.exists() {
            let _ = watcher.watch(p, RecursiveMode::Recursive);
        }
    }

    let exe = std::env::current_exe()?;

    loop {
        let mut child = std::process::Command::new(&exe)
            .args(["dev", "storybook"])
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()?;

        let mut child_exited = false;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    child_exited = true;
                    break;
                }
                Ok(None) => {}
                Err(_) => {
                    child_exited = true;
                    break;
                }
            }

            if rx.recv_timeout(Duration::from_millis(200)).is_ok() {
                // Debounce extra file events
                while rx.recv_timeout(Duration::from_millis(300)).is_ok() {}
                break;
            }
        }

        if child_exited {
            nuo::shell::terminal::restore_terminal();
            break;
        }

        // Graceful termination of child process
        #[cfg(unix)]
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = child.kill();

        let mut waited = false;
        for _ in 0..10 {
            if let Ok(Some(_)) = child.try_wait() {
                waited = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !waited {
            let _ = child.kill();
            let _ = child.wait();
        }

        nuo::shell::terminal::restore_terminal();

        println!("\n[Watch] Recompiling storybook after code modification...\n");
        let build_status = std::process::Command::new("cargo")
            .args(["build", "-p", "nuo"])
            .status();

        match build_status {
            Ok(status) if status.success() => {
                // Proceed to spawn recompiled child
            }
            _ => {
                println!("\n[Watch] Compilation failed. Waiting for next code modification...");
                let _ = rx.recv();
                while rx.recv_timeout(Duration::from_millis(300)).is_ok() {}
            }
        }
    }

    nuo::shell::terminal::restore_terminal();
    Ok(())
}

#[cfg(not(feature = "interactive"))]
async fn run_storybook(_watch: bool) -> Result<(), Box<dyn std::error::Error>> {
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
