#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use crate::shell::cli::{self, CliArgs, Mode};
use crate::shell::client;
use crate::shell::headless;
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
        || std::env::var("NUO_DEV").map(|v| v != "0").unwrap_or(false);

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
    let parsed = match cli::parse(raw_args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("nuo: {error}\n\nRun 'nuo --help' for more information.");
            std::process::exit(2);
        }
    };
    run_with_options(parsed).await
}

pub async fn run_with_options(mut parsed: CliArgs) -> Result<(), Box<dyn std::error::Error>> {
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
        match &mut parsed.mode {
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
        if parsed.headless
            || parsed.prompt_from_flag
            || parsed.json
            || (parsed.prompt.is_some() && !stdout_is_tty)
        {
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
        port,
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
                .and_then(crate::shell::views::ConfigCategory::from_name)
                .map(|c| c as usize);
            run_attached(
                None,
                true,
                project_override,
                unattended_at_start,
                confined_at_start,
                role.clone(),
                resume,
                crate::shell::StartupOverlay::Settings { category: cat },
                prompt,
                port,
                remote,
                token,
            )
            .await
        }
        Mode::Attach { id } => {
            let overlay = if id.is_none() {
                crate::shell::StartupOverlay::SessionsPicker
            } else {
                crate::shell::StartupOverlay::resolve_from_env()
                    .unwrap_or(crate::shell::StartupOverlay::None)
            };
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
                port,
                remote,
                token,
            )
            .await
        }
        Mode::Run { prompt } => {
            if interactive {
                let overlay = crate::shell::StartupOverlay::resolve_from_env()
                    .unwrap_or(crate::shell::StartupOverlay::None);
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
                    port,
                    remote,
                    token,
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
            let overlay = crate::shell::StartupOverlay::resolve_from_env()
                .unwrap_or(crate::shell::StartupOverlay::None);
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
                port,
                remote,
                token,
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
        crate::shell::StartupOverlay::Dashboard,
        None,
        None,
        None,
        None,
    )
    .await
}

/// Retire the preserved tab that represents `gone_id` (ADR-0123
/// `[INV-ATTACH-02]`).
///
/// The server proved the session no longer exists, so a tab naming it can only
/// ever produce a failing attach. Removing it *here* — before the tab list is
/// carried into the next attachment — is what converts "click an old tab and the
/// client dies" into "the stale tab is gone and the picker opens".
///
/// Liveness changes only on this server-attested refusal; it is never inferred
/// from a broadcast (`[INV-TAB-14]`), because suspension and idle teardown
/// publish the same removals while leaving the transcript durable.
///
/// Returns whether a tab was retired.
pub(crate) fn retire_preserved_tab(
    tabs: &mut Option<Vec<crate::shell::surfaces::ClientTab>>,
    gone_id: &str,
) -> bool {
    let Some(tabs) = tabs.as_mut() else {
        return false;
    };
    let before = tabs.len();
    tabs.retain(|tab| tab.thread_id() != Some(gone_id));
    tabs.len() != before
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
    initial_overlay: crate::shell::StartupOverlay,
    mut initial_prompt: Option<String>,
    explicit_port: Option<u16>,
    remote: Option<String>,
    token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let target_server = match remote {
        Some(addr) => {
            let remote_server = client::RemoteServer::parse(&addr, token)?;
            client::ClientTarget::Remote(remote_server)
        }
        None => {
            let raw_root = project_override
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let project_root = nuo_host::paths::find_project_root(&raw_root);
            let info = client::ensure_server_explicit(&project_root, explicit_port).await?;
            if !client::versions_compatible(&info) {
                return Err(client::incompatibility_error(&info).into());
            }
            client::ClientTarget::Local(info)
        }
    };
    let mut target = session_id.clone();
    let mut fresh_pending = fresh;
    let mut pick_pending = session_id.is_none() && !fresh;
    let mut startup_overlay_pending = initial_overlay;
    let mut preserved_tabs: Option<Vec<crate::shell::surfaces::ClientTab>> = None;
    let mut preserved_tab_history: Option<Vec<crate::shell::surfaces::TabKind>> = None;
    // Disclosures produced *between* TUI runs — a retired stale tab, a session
    // that could not be reopened (ADR-0123 `[INV-ATTACH-02]`). Handed to the next
    // run so the notice log owns them instead of a lost stderr line.
    let mut retire_notice: Option<(nuo_view::model::document::NoticeSeverity, String)> = None;
    let init_options = client::SessionInitOptions::new(unattended_at_start, confined_at_start)
        .with_role(role.clone())
        .with_resume(resume);

    // Initialize terminal once and keep it alive across thread switches to eliminate flicker.
    let profile = nuotc::TerminalProfile::detect();
    crate::shell::terminal::enter_terminal(&profile)?;
    crate::shell::terminal::spawn_signal_guard();
    crate::shell::ensure_panic_hook();

    struct TerminalGuard(bool);
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            if self.0 {
                crate::shell::terminal::restore_terminal();
            }
        }
    }
    let mut guard = TerminalGuard(true);

    let stdout = std::io::stdout();
    let backend = nuotc::Backend::new(stdout).with_profile(profile);
    let mut terminal = nuotc::Terminal::new(backend);
    // One input source for every attachment in this process: a cross-session
    // tab switch or tab close detaches and re-attaches *in place*, and an input
    // source owned by a single attachment dropped whatever the operator pressed
    // while the next attachment handshook — a lost first click or chord stroke
    // is indistinguishable from "the gesture needs two attempts".
    let mut input = crate::shell::event_loop::input_reader::InputSource::spawn()?;

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
        // `[INV-ATTACH-02]`: a *recoverable* attach refusal must never terminate
        // the client. Before this edge, every refusal — including the provably
        // expected "this tab's session is gone" — propagated with `?` to `main`
        // and the process exited, so clicking a stale tab killed the session the
        // operator was actually working in.
        let handshake = match target_server.connect(action).await {
            Ok(handshake) => handshake,
            // The server proved the session does not exist: retire the tab that
            // names it and fall back to the picker. Liveness is settled by server
            // attestation only (`[INV-TAB-14]`), never by a broadcast.
            Err(error) if error.is_session_gone() => {
                tracing::info!(
                    code = ?error.code(),
                    target = ?target,
                    "attach refused: session no longer exists; retiring tab"
                );
                if let Some(gone) = target.take() {
                    retire_preserved_tab(&mut preserved_tabs, &gone);
                }
                retire_notice = Some((
                    nuo_view::model::document::NoticeSeverity::Warning,
                    format!(
                        "That tab's session no longer exists; it has been removed. ({})",
                        error.message
                    ),
                ));
                pick_pending = true;
                continue;
            }
            // The session exists durably but could not be assembled: keep the tab
            // — its history is real and a retry may work — and report the fault.
            Err(error) if error.is_session_unresumable() => {
                tracing::warn!(
                    code = ?error.code(),
                    target = ?target,
                    "attach refused: session could not be assembled; tab retained"
                );
                target = None;
                retire_notice = Some((
                    nuo_view::model::document::NoticeSeverity::Error,
                    format!("Could not reopen that tab's session: {}", error.message),
                ));
                pick_pending = true;
                continue;
            }
            // No taxonomy: the server never authorized a recovery, so inventing
            // one would be worse than failing. A genuine transport fault stays
            // fatal.
            Err(error) => return Err(error.into()),
        };
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
                let handshake = target_server
                    .connect(client::AttachAction::Picker(Some(init_options.clone())))
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
        let base_config = crate::shell::config::TuiConfig::load();
        let input_history = Vec::new();
        let tui_config = base_config.clone();
        let input_history_config = base_config.input_history.clone();
        let startup_overlay = std::mem::replace(
            &mut startup_overlay_pending,
            crate::shell::StartupOverlay::None,
        );
        let exit_tx = tx.clone();
        let outcome = crate::shell::run_tui_with_input(
            &mut terminal,
            tx,
            rx,
            crate::shell::TuiLaunchConfig {
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
                session: crate::shell::SessionSource::Remote {
                    session_id: hosted_session_id,
                },
                token_ledger: None,
                startup_overlay,
                initial_tabs: preserved_tabs.take(),
                initial_tab_history: preserved_tab_history.take(),
                startup_notices: retire_notice.take().into_iter().collect(),
            },
            &mut input,
        )
        .await?;
        preserved_tabs = outcome.tabs;
        preserved_tab_history = outcome.tab_history;
        if let Err(error) = exit_tx.send(nuo_wire::AgentRequest::RecordInputHistory {
            entries: outcome.history,
            dedup: base_config.input_history.dedup,
        }) {
            tracing::error!(%error, "server link lost before exit history flush");
        }
        if outcome.switch_to_new {
            target = None;
            fresh_pending = true;
            initial_prompt = outcome.switch_to_new_prompt;
            continue;
        }
        match outcome.switch_to {
            Some(id) => {
                target = Some(id);
                // A retired `/btw <prompt>` fork steer rides its message here;
                // it is submitted as the opening prompt of the child branch on
                // the next attach (`[INV-DEPR-01]`).
                initial_prompt = outcome.switch_to_prompt;
                continue;
            }
            None => {
                guard.0 = false;
                crate::shell::terminal::restore_terminal();
                match &target_server {
                    client::ClientTarget::Local(info) => {
                        if info.port > 0 {
                            eprintln!(
                                "nuo: detached from hosted server (PID {}, port {}).\n  • Reconnect:   nuo\n  • Terminate:   nuo stop",
                                info.pid, info.port
                            );
                        }
                    }
                    client::ClientTarget::Remote(rem) => {
                        eprintln!(
                            "nuo: detached from remote server ({}).\n  • Reconnect:   nuo --remote {} --token <token>",
                            rem.endpoint_display(),
                            rem.endpoint_display()
                        );
                    }
                }
                return Ok(());
            }
        }
    }
}
