#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use nuo_client as client;
use crate::start_tui;
use crate::cli::{self, CliArgs, Mode};
use crate::headless;
use std::path::PathBuf;

pub fn run_main() -> Result<(), Box<dyn std::error::Error>> {
    ensure_dev_environment();
    let _tracing_guard = client::init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run_cli_args(&std::env::args().skip(1).collect::<Vec<_>>()));
    runtime.shutdown_background();
    result
}

pub fn ensure_dev_environment() {
    let is_debug_build = cfg!(debug_assertions);
    let dev_opt_out = std::env::var("NUO_NO_DEV")
        .map(|v| v != "0")
        .unwrap_or(false);
    let dev_mode = (is_debug_build && !dev_opt_out)
        || std::env::var("NUO_DEV").map(|v| v != "0").unwrap_or(false)
        || std::env::var("NUO_DEV_TOAST").is_ok()
        || std::env::var("NUO_TOAST").is_ok();

    if !dev_mode {
        return;
    }

    // 1. Isolate home so it never conflicts with the host-installed nuo.
    if std::env::var_os("NUO_HOME").is_none() {
        let dev_home = if let Ok(current) = std::env::current_exe() {
            if let Some(target) = current.parent().and_then(|p| p.parent()) {
                target.join("nuo-dev")
            } else {
                std::env::temp_dir().join("nuo-dev")
            }
        } else {
            std::env::temp_dir().join("nuo-dev")
        };

        let _ = std::fs::create_dir_all(&dev_home);
        unsafe {
            std::env::set_var("NUO_HOME", &dev_home);
        }
        let _ = nuo_host::paths::set_default(nuo_host::paths::Dirs::system());
    }

    // 2. Point NUO_BIN at the local source-built nuo, building it if not yet present.
    if std::env::var_os("NUO_BIN").is_none()
        && let Ok(current) = std::env::current_exe()
    {
        let sibling = current.with_file_name(format!("nuo{}", std::env::consts::EXE_SUFFIX));
        if !sibling.is_file() {
            eprintln!(
                "[nuo-dev] Local nuo binary not found at {}. Compiling via cargo...",
                sibling.display()
            );
            let status = std::process::Command::new("cargo")
                .args(["build", "-p", "nuo"])
                .status();
            if let Ok(status) = status
                && !status.success()
            {
                eprintln!("[nuo-dev] Warning: Failed to build local nuo from source.");
            }
        }
        if sibling.is_file() {
            unsafe {
                std::env::set_var("NUO_BIN", &sibling);
            }
        }
    }
}

pub async fn run_cli_args(raw_args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut parsed = match cli::parse(raw_args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("nuo: {error}\n\nRun 'nuo --help' for more information.");
            std::process::exit(2);
        }
    };

    // Stdin pipeline detection: piped input becomes (or joins) the prompt.
    // Only the prompt-bearing modes take it — piping into a non-prompt
    // command is a shell mistake, not a headless run.
    use std::io::{self, IsTerminal, Read};
    let stdin_input = if !io::stdin().is_terminal() {
        let mut buffer = String::new();
        if io::stdin().read_to_string(&mut buffer).is_ok() {
            let trimmed = buffer.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        } else {
            None
        }
    } else {
        None
    };
    if let Some(piped) = stdin_input {
        match &parsed.mode {
            Mode::Fresh | Mode::Run { .. } => {
                let joined = match parsed.prompt.take() {
                    Some(existing) => format!("{existing}\n\n--- Standard Input ---\n{piped}"),
                    None => piped,
                };
                parsed.prompt = Some(joined);
            }
            _ => {}
        }
    }

    // Resolve the Fresh/Run/headless intent once, here, where the terminal
    // shape is known: an explicit `-p` or a non-terminal stdout means
    // headless; `-i` forces the TUI; `run` is headless by definition.
    if matches!(parsed.mode, Mode::Fresh) && !parsed.interactive {
        let stdout_is_tty = io::stdout().is_terminal();
        if parsed.prompt_from_flag || parsed.json || (parsed.prompt.is_some() && !stdout_is_tty) {
            parsed.mode = Mode::Run {
                prompt: parsed.prompt.clone().unwrap_or_default(),
            };
        }
    }

    let CliArgs {
        mode,
        project: project_override,
        unattended: unattended_at_start,
        role,
        resume,
        no_confinement,
        interactive,
        prompt,
        json: _,
        remote,
        token,
        ..
    } = parsed;
    let confined_at_start = !no_confinement;

    match mode {
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
        Mode::Dashboard => {
            run_dashboard(project_override, unattended_at_start, confined_at_start).await
        }
        Mode::Settings { category } => {
            let cat_str = category.or_else(|| {
                std::env::var("NUO_SETTINGS_NAV")
                    .or_else(|_| std::env::var("NUO_SETTINGS_CATEGORY"))
                    .ok()
            });
            let cat = cat_str
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(crate::views::ConfigCategory::from_name)
                .map(|c| c as usize);
            run_attached(
                None,
                true,
                project_override,
                unattended_at_start,
                confined_at_start,
                role.clone(),
                resume,
                crate::StartupOverlay::Settings { category: cat },
                prompt,
            )
            .await
        }
        Mode::Attach { id } => {
            let overlay =
                crate::StartupOverlay::resolve_from_env().unwrap_or(crate::StartupOverlay::None);
            run_attached(
                id,
                false,
                project_override,
                unattended_at_start,
                confined_at_start,
                role.clone(),
                resume,
                overlay,
                prompt,
            )
            .await
        }
        Mode::Run { prompt } => {
            if interactive {
                let overlay = crate::StartupOverlay::resolve_from_env()
                    .unwrap_or(crate::StartupOverlay::None);
                run_attached(
                    None,
                    true,
                    project_override,
                    unattended_at_start,
                    confined_at_start,
                    role.clone(),
                    resume,
                    overlay,
                    Some(prompt),
                )
                .await
            } else {
                headless::run_headless(
                    prompt,
                    parsed.json,
                    project_override,
                    unattended_at_start,
                    confined_at_start,
                    role.clone(),
                    resume,
                    remote,
                    token,
                )
                .await
            }
        }
        Mode::Fresh => {
            let overlay =
                crate::StartupOverlay::resolve_from_env().unwrap_or(crate::StartupOverlay::None);
            run_attached(
                None,
                true,
                project_override,
                unattended_at_start,
                confined_at_start,
                role.clone(),
                resume,
                overlay,
                prompt,
            )
            .await
        }
    }
}

pub async fn run_dashboard(
    project_override: Option<PathBuf>,
    unattended_at_start: bool,
    confined_at_start: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let project_root = nuo_host::paths::find_project_root(&raw_root);
    let info = client::ensure_server(&project_root).await?;
    if !client::versions_compatible(&info) {
        return Err(client::incompatibility_error(&info).into());
    }
    let mut rx = client::monitor_stream(
        &info,
        nuo_wire::MonitorAction {
            watch: false,
            include_idle: true,
        },
    )
    .await
    .map_err(|e| format!("could not read the server's session list: {e}"))?;
    let snapshot = match rx.recv().await {
        Some(nuo_wire::MonitorEvent::Snapshot(snap)) => snap,
        Some(_) => return Err("server monitor stream opened without a snapshot".into()),
        None => return Err("server closed the monitor stream".into()),
    };
    drop(rx);
    let carrier = snapshot
        .sessions
        .iter()
        .max_by_key(|s| s.updated_at)
        .map(|s| s.id.clone())
        .ok_or_else(|| {
            "the server hosts no sessions yet. Start one with bare `nuo`, \
             then re-run `nuo dashboard`."
                .to_string()
        })?;
    run_attached(
        Some(carrier),
        false,
        project_override,
        unattended_at_start,
        confined_at_start,
        None,
        false,
        crate::StartupOverlay::Dashboard,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_attached(
    session_id: Option<String>,
    fresh: bool,
    project_override: Option<PathBuf>,
    unattended_at_start: bool,
    confined_at_start: bool,
    role: Option<String>,
    resume: bool,
    initial_overlay: crate::StartupOverlay,
    mut initial_prompt: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let project_root = nuo_host::paths::find_project_root(&raw_root);
    let info = client::ensure_server(&project_root).await?;
    if !client::versions_compatible(&info) {
        return Err(client::incompatibility_error(&info).into());
    }
    let mut target = session_id.clone();
    let mut fresh_pending = fresh;
    let mut pick_pending = session_id.is_none() && !fresh;
    let mut startup_overlay_pending = initial_overlay;
    let init_options =
        nuo_client::SessionInitOptions::new(unattended_at_start, confined_at_start)
            .with_role(role.clone())
            .with_resume(resume);
    loop {
        let action = match &target {
            Some(id) => client::AttachAction::Attach(Some(id.clone())),
            None if fresh_pending => client::AttachAction::New(if init_options.is_default() {
                None
            } else {
                Some(init_options.clone())
            }),
            None if pick_pending => client::AttachAction::Picker(Some(init_options.clone())),
            None => client::AttachAction::Attach(None),
        };
        fresh_pending = false;
        pick_pending = false;
        let handshake = client::connect(&info, action).await?;
        let (
            tx,
            rx,
            hosted_session_id,
            round_counter,
            transcript,
            round_interrupts,
            retry_resolutions,
            provider,
            model,
            command_catalog,
        ) = match handshake {
            client::Handshake::Attached {
                req_tx,
                resp_rx,
                session_id,
                round_counter,
                history,
                round_interrupts,
                retry_resolutions,
                provider,
                model,
                command_catalog,
            } => (
                req_tx,
                resp_rx,
                session_id,
                round_counter,
                history,
                round_interrupts,
                retry_resolutions,
                provider,
                model,
                command_catalog,
            ),
            client::Handshake::Pick(_) => {
                let handshake = client::connect(
                    &info,
                    client::AttachAction::Picker(Some(init_options.clone())),
                )
                .await?;
                match handshake {
                    client::Handshake::Attached {
                        req_tx,
                        resp_rx,
                        session_id,
                        round_counter,
                        history,
                        round_interrupts,
                        retry_resolutions,
                        provider,
                        model,
                        command_catalog,
                    } => (
                        req_tx,
                        resp_rx,
                        session_id,
                        round_counter,
                        history,
                        round_interrupts,
                        retry_resolutions,
                        provider,
                        model,
                        command_catalog,
                    ),
                    client::Handshake::Pick(_) => {
                        return Err("the server offered no session to pick from".into());
                    }
                }
            }
        };
        if let Some(prompt) = initial_prompt.take() {
            tx.send(nuo_wire::AgentRequest::Prompt {
                text: prompt,
                images: Vec::new(),
                sent_at_ms: None,
            })
            .map_err(|error| format!("server link lost before initial prompt: {error}"))?;
        }
        let base_config = crate::config::TuiConfig::load();
        let input_history = Vec::new();
        let tui_config = base_config.clone();
        let input_history_config = base_config.input_history.clone();
        let startup_overlay =
            std::mem::replace(&mut startup_overlay_pending, crate::StartupOverlay::None);
        let exit_tx = tx.clone();
        let outcome = start_tui(
            tx,
            rx,
            crate::TuiLaunchConfig {
                initial_provider: provider,
                initial_model: model,
                input_history,
                initial_messages: transcript,
                initial_commands: Vec::new(),
                initial_round_count: round_counter,
                command_catalog,
                initial_round_interrupts: round_interrupts,
                initial_retry_resolutions: retry_resolutions,
                tui_config,
                input_history_config,
                session: crate::SessionSource::Remote {
                    session_id: hosted_session_id,
                },
                token_ledger: None,
                startup_overlay,
            },
        )
        .await?;
        if let Err(error) = exit_tx.send(nuo_wire::AgentRequest::RecordInputHistory {
            entries: outcome.history,
            dedup: base_config.input_history.dedup,
        }) {
            tracing::error!(%error, "server link lost before exit history flush");
        }
        match outcome.switch_to {
            Some(id) => {
                target = Some(id);
                continue;
            }
            None => return Ok(()),
        }
    }
}
