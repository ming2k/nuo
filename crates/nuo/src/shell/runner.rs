#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use crate::shell::cli::{self, TuiCliArgs, TuiMode};
use crate::shell::client;
use crate::shell::headless;
use std::path::PathBuf;
use std::time::Duration;

/// Local UDS/loopback daemon restarts settle quickly: 10 short attempts
/// (~2.5 s total) catch an in-place server replacement without stalling on a
/// dead host (ADR-0224 `[INV-RESILIENCE-04]`).
const LOCAL_RELINK_MAX_ATTEMPTS: u32 = 10;

/// Remote WebSocket links ride public/wireless networks where a Wi-Fi handoff
/// or brief carrier drop easily lasts 10–60 s. 20 stepped-backoff attempts
/// (`0.5s -> 1s -> 2.5s -> 5s -> 10s...` with equal jitter, ~2.5 min total)
/// survive transient outages while remaining cancellable via `Esc`/`Ctrl+C`
/// (ADR-0224 `[INV-RESILIENCE-04]`).
const REMOTE_RELINK_MAX_ATTEMPTS: u32 = 20;
const REMOTE_RELINK_BASE_MS: u64 = 500;
const REMOTE_RELINK_MAX_MS: u64 = 10_000;

/// Staying attached for at least this long proves the previous relink
/// converged, so a later link drop starts from a fresh relink budget instead of
/// accumulating across an hours-long session.
pub(crate) const RELINK_STABILITY_WINDOW: Duration = Duration::from_secs(5);

/// Compute the backoff delay (in milliseconds) before relink attempt `attempt`
/// (1-based), or `None` once the budget is exhausted (ADR-0224
/// `[INV-RESILIENCE-04]`).
pub(crate) fn relink_delay_ms(
    is_remote: bool,
    attempt: u32,
    roll: impl Fn(u64) -> u64,
) -> Option<u64> {
    if is_remote {
        if attempt == 0 || attempt > REMOTE_RELINK_MAX_ATTEMPTS {
            return None;
        }
        let base = engine::runtime::retry::retry_delay_ms(
            attempt as usize,
            None,
            REMOTE_RELINK_BASE_MS,
            REMOTE_RELINK_MAX_MS,
        );
        Some(engine::runtime::retry::apply_jitter_ms(base, roll))
    } else {
        if attempt == 0 || attempt > LOCAL_RELINK_MAX_ATTEMPTS {
            return None;
        }
        if attempt == 1 {
            Some(0)
        } else {
            Some(((attempt as u64) * 50).min(500))
        }
    }
}

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

#[allow(unsafe_code)]
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
        // SAFETY: Called once at the very start of `main()` before the tokio
        // runtime or background threads are spawned.
        unsafe {
            std::env::set_var("NUO_HOME", &dev_home);
        }
        let _ = host::paths::set_default(host::paths::Dirs::system());
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
            // SAFETY: Called once at startup before worker threads are spawned.
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

pub async fn run_with_options(mut parsed: TuiCliArgs) -> Result<(), Box<dyn std::error::Error>> {
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
            TuiMode::Fresh | TuiMode::Run { .. } => {
                let joined = match parsed.prompt.take() {
                    Some(existing) => format!("{existing}\n\n--- Standard Input ---\n{piped}"),
                    None => piped,
                };
                parsed.prompt = Some(joined);
            }
            _ => {}
        }
    }

    if matches!(parsed.mode, TuiMode::Fresh) && !parsed.interactive {
        let stdout_is_tty = io::stdout().is_terminal();
        if parsed.headless
            || parsed.prompt_from_flag
            || parsed.json
            || (parsed.prompt.is_some() && !stdout_is_tty)
        {
            parsed.mode = TuiMode::Run {
                prompt: parsed.prompt.clone().unwrap_or_default(),
            };
        }
    }

    let TuiCliArgs {
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
        TuiMode::Version => {
            println!("nuo {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        TuiMode::Help(topic) => {
            if let Some(text) = cli::help_text(topic.as_deref()) {
                print!("{text}");
            }
            Ok(())
        }
        TuiMode::Completions(shell) => {
            print!("{}", cli::completion_script(shell));
            Ok(())
        }
        TuiMode::Dashboard => {
            run_dashboard(project_override, unattended_at_start, confined_at_start).await
        }
        TuiMode::Settings { category } => {
            let cat_str = category.or_else(|| {
                std::env::var("NUO_SETTINGS_NAV")
                    .or_else(|_| std::env::var("NUO_SETTINGS_CATEGORY"))
                    .ok()
            });
            let cat = cat_str
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(view::views::ConfigCategory::from_name)
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
        TuiMode::Attach { id } => {
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
        TuiMode::Run { prompt } => {
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
        TuiMode::Fresh => {
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
    // ADR-0260 `[INV-SRVPHASE-02]`: the session-free Hello handshake allows
    // opening the Dashboard directly without forcing a dummy carrier thread.
    run_attached(
        None,
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

/// Retire the preserved tab that represents `gone_id` (ADR-0123 `[INV-ATTACH-02]`).
#[cfg(test)]
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

pub(crate) fn relink_max_attempts(target_server: &client::ClientTarget) -> (bool, u32) {
    let is_remote = matches!(target_server, client::ClientTarget::Remote(_));
    let limit = if is_remote {
        REMOTE_RELINK_MAX_ATTEMPTS
    } else {
        LOCAL_RELINK_MAX_ATTEMPTS
    };
    (is_remote, limit)
}

fn print_detach_epilogue(target_server: &client::ClientTarget) {
    match target_server {
        client::ClientTarget::Local(info) => {
            let endpoint = info
                .effective_local_endpoint()
                .map(|endpoint| endpoint.to_string())
                .unwrap_or_else(|| format!("port {}", info.port));
            eprintln!(
                "nuo: detached from hosted server (PID {}, {}).\n  • Reconnect:   nuo\n  • Terminate:   nuo server stop",
                info.pid, endpoint
            );
        }
        client::ClientTarget::Remote(rem) => {
            eprintln!(
                "nuo: detached from remote server ({}).\n  • Reconnect:   nuo --remote {} --token <token>",
                rem.endpoint_display(),
                rem.endpoint_display()
            );
        }
    }
}

/// Launch the interactive TUI with a single `App` lifetime owned by
/// `supervisor::spawn_connection_supervisor` (ADR-0260):
/// discovery, server startup, `Hello -> ServerHello`, live `ServerPhase`
/// observation, thread subscription, and link-drop reconnection all happen
/// in-place without tearing down `App` or smuggling tab state across loops.
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
    initial_prompt: Option<String>,
    explicit_port: Option<u16>,
    remote: Option<String>,
    token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let init_options = client::ThreadInitOptions::new(unattended_at_start, confined_at_start)
        .with_role(role)
        .with_resume(resume);
    let is_remote = remote.is_some();
    let project_scope = if is_remote {
        None
    } else {
        project_override
            .clone()
            .or_else(|| std::env::current_dir().ok())
    };
    let dashboard_only_launch = matches!(initial_overlay, crate::shell::StartupOverlay::Dashboard)
        && session_id.is_none()
        && !fresh;
    let initial_subscribe = if dashboard_only_launch {
        None
    } else {
        let target = match session_id {
            Some(id) => protocol::ThreadTarget::Attach { id: Some(id) },
            None if fresh => protocol::ThreadTarget::New {
                options: (!init_options.is_default()).then_some(init_options.clone()),
            },
            None => protocol::ThreadTarget::Picker {
                options: Some(init_options.clone()),
            },
        };
        Some((target, project_scope.clone()))
    };

    let profile = crate::shell::terminal::detect_terminal_profile();
    let mut session_guard = crate::shell::terminal::enter_terminal(&profile)?;
    crate::shell::terminal::spawn_signal_guard();
    crate::shell::ensure_panic_hook();

    let stdout = std::io::stdout();
    let backend = graticule::Backend::new(stdout).with_profile(profile);
    let mut terminal = graticule::live_terminal(backend);
    let mut input = crate::shell::event_loop::input_reader::InputSource::spawn()?;
    let tui_config = crate::shell::config::load_tui_config();
    let history_dedup = tui_config.input_history.dedup;

    let (outcome, resolved_target) = crate::shell::run_supervised_tui(
        &mut terminal,
        &mut input,
        crate::shell::SupervisedTuiLaunch {
            remote,
            token,
            project_override,
            explicit_port,
            initial_subscribe,
            initial_init_options: init_options,
            project_scope,
            startup_overlay: initial_overlay,
            initial_prompt,
            tui_config,
        },
    )
    .await?;

    if !outcome.active_session_id.is_empty()
        && let Some(exit_tx) = outcome.exit_tx
        && let Err(error) = exit_tx.send(client::ClientOutbound::Request {
            thread_id: Some(outcome.active_session_id),
            request: protocol::AgentRequest::RecordInputHistory {
                entries: outcome.history,
                dedup: history_dedup,
            },
        })
    {
        tracing::error!(%error, "server link lost before exit history flush");
    }

    session_guard.restore();
    if let Some(target_server) = resolved_target {
        print_detach_epilogue(&target_server);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relink_backoff_separates_local_and_remote_policies() {
        // Local daemon restarts: immediate attempt 1, short 50..500ms cap, 10 attempts.
        assert_eq!(relink_delay_ms(false, 0, |_| 0), None);
        assert_eq!(relink_delay_ms(false, 1, |_| 0), Some(0));
        assert_eq!(relink_delay_ms(false, 2, |_| 0), Some(100));
        assert_eq!(relink_delay_ms(false, 10, |_| 0), Some(500));
        assert_eq!(relink_delay_ms(false, 11, |_| 0), None);

        // Remote WebSocket links: 20 stepped-backoff attempts with equal jitter in [base/2, base].
        assert_eq!(relink_delay_ms(true, 0, |_| 0), None);
        // Attempt 1 -> base 500ms -> equal jitter [250, 500].
        assert_eq!(relink_delay_ms(true, 1, |_| 0), Some(250));
        assert_eq!(relink_delay_ms(true, 1, |span| span), Some(500));
        // Attempt 4+ -> base 5000ms (step multiplier 10 * 500ms) -> [2500, 5000].
        assert_eq!(relink_delay_ms(true, 4, |_| 0), Some(2_500));
        assert_eq!(relink_delay_ms(true, 4, |span| span), Some(5_000));
        assert!(relink_delay_ms(true, REMOTE_RELINK_MAX_ATTEMPTS, |_| 0).is_some());
        assert_eq!(
            relink_delay_ms(true, REMOTE_RELINK_MAX_ATTEMPTS + 1, |_| 0),
            None
        );
    }
}
