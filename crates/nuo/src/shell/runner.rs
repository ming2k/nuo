#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use crate::shell::cli::{self, CliArgs, Mode};
use crate::shell::client;
use crate::shell::headless;
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
const RELINK_STABILITY_WINDOW: Duration = Duration::from_secs(5);

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

/// Wait until the operator presses `Esc` or `Ctrl+C` (or the input stream
/// closes). Used to keep both backoff sleeps and stalled remote re-dials
/// immediately cancellable (ADR-0224).
async fn wait_for_relink_cancel(input: &mut crate::shell::event_loop::input_reader::InputSource) {
    loop {
        match input.recv().await {
            Some(graticule::Event::Key(key)) => {
                let is_ctrl_c = key.code == graticule::KeyCode::Char('c')
                    && key.modifiers.contains(graticule::KeyModifiers::CONTROL);
                if is_ctrl_c || key.code == graticule::KeyCode::Esc {
                    return;
                }
            }
            None => return,
            _ => {}
        }
    }
}

/// Sleep for `delay` during a relink backoff, returning `true` immediately if
/// the operator presses `Esc` or `Ctrl+C` to abort reconnecting.
async fn sleep_or_relink_cancel(
    delay: Duration,
    input: &mut crate::shell::event_loop::input_reader::InputSource,
) -> bool {
    if delay.is_zero() {
        return false;
    }
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        _ = wait_for_relink_cancel(input) => true,
    }
}

/// Paint a minimal, color-neutral reconnect status line onto the live terminal
/// so the operator sees live re-dial progress instead of a frozen frame while
/// waiting for a remote server to return (ADR-0224).
fn draw_relink_status(
    terminal: &mut graticule::Terminal<std::io::Stdout>,
    endpoint: &str,
    attempt: u32,
    max_attempts: u32,
    delay_ms: Option<u64>,
) {
    let text = match delay_ms {
        Some(ms) if ms > 0 => format!(
            " Reconnecting to {endpoint} (attempt {attempt}/{max_attempts}, retrying in {:.1}s — Esc or Ctrl+C to cancel)",
            (ms as f64) / 1000.0
        ),
        _ => format!(
            " Reconnecting to {endpoint} (attempt {attempt}/{max_attempts} — Esc or Ctrl+C to cancel)"
        ),
    };
    let _ = terminal.draw(|f| {
        let area = f.area();
        if area.height == 0 || area.width == 0 {
            return;
        }
        let bar = graticule::Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(1),
            width: area.width,
            height: 1,
        };
        f.render_widget(
            &mut graticule::widgets::Paragraph::new(graticule::Line::from(text.as_str())),
            bar,
        );
    });
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
    let project_root = host::paths::find_project_root(&raw_root);
    let info = client::ensure_server(&project_root).await?;
    if !client::versions_compatible(&info) {
        return Err(client::incompatibility_error(&info).into());
    }
    let mut rx = client::monitor_stream(
        &info,
        protocol::MonitorAction {
            watch: false,
            include_idle: true,
        },
    )
    .await
    .map_err(|e| format!("could not read the server's session list: {e}"))?;
    let snapshot = match rx.recv().await {
        Some(protocol::MonitorEvent::Snapshot(snap)) => snap,
        Some(_) => return Err("server monitor stream opened without a snapshot".into()),
        None => return Err("server closed the monitor stream".into()),
    };
    drop(rx);
    let carrier = snapshot
        .threads
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

fn relink_max_attempts(target_server: &client::ClientTarget) -> (bool, u32) {
    let is_remote = matches!(target_server, client::ClientTarget::Remote(_));
    let limit = if is_remote {
        REMOTE_RELINK_MAX_ATTEMPTS
    } else {
        LOCAL_RELINK_MAX_ATTEMPTS
    };
    (is_remote, limit)
}

async fn resolve_initial_target(
    remote: Option<String>,
    token: Option<String>,
    project_override: &Option<PathBuf>,
    explicit_port: Option<u16>,
) -> Result<client::ClientTarget, Box<dyn std::error::Error>> {
    match remote {
        Some(addr) => {
            let remote_server = client::RemoteServer::parse(&addr, token)?;
            Ok(client::ClientTarget::Remote(remote_server))
        }
        None => {
            let raw_root = project_override
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let project_root = host::paths::find_project_root(&raw_root);
            // A spawn can take a few seconds; the alternate screen is not up
            // yet, so one honest line beats a silent blank terminal.
            eprintln!("nuo: connecting to the session server…");
            let info = client::ensure_server_explicit(&project_root, explicit_port).await?;
            if !client::versions_compatible(&info) {
                return Err(client::incompatibility_error(&info).into());
            }
            Ok(client::ClientTarget::Local(info))
        }
    }
}

/// Step the relink backoff budget by one attempt, draw the live reconnect
/// status line, wait for the cancellable backoff delay, and (for local targets)
/// re-resolve the daemon endpoint in case the server was replaced or restarted.
#[allow(clippy::too_many_arguments)]
async fn step_relink_backoff(
    target_server: &mut client::ClientTarget,
    project_override: &Option<PathBuf>,
    explicit_port: Option<u16>,
    consecutive_relinks: &mut u32,
    terminal: &mut graticule::Terminal<std::io::Stdout>,
    input: &mut crate::shell::event_loop::input_reader::InputSource,
    session_guard: &mut graticule::TerminalSession,
    failure_context: Option<&client::AttachError>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (is_remote, limit) = relink_max_attempts(target_server);
    *consecutive_relinks += 1;
    let attempt = *consecutive_relinks;
    let Some(delay_ms) = relink_delay_ms(is_remote, attempt, |span| fastrand::u64(0..=span)) else {
        let detail = match failure_context {
            Some(error) => format!(
                "server connection to {} lost; aborting after {limit} re-dial attempts ({error})",
                target_server.endpoint_display()
            ),
            None => format!(
                "server connection to {} repeatedly lost; aborting attach loop after {limit} re-dial attempts",
                target_server.endpoint_display()
            ),
        };
        return Err(detail.into());
    };
    if let Some(error) = failure_context {
        tracing::warn!(
            endpoint = %target_server.endpoint_display(),
            attempt,
            delay_ms,
            %error,
            "attach loop: re-dial failed; backing off before next attempt"
        );
    }
    draw_relink_status(
        terminal,
        &target_server.endpoint_display(),
        attempt,
        limit,
        Some(delay_ms),
    );
    if sleep_or_relink_cancel(Duration::from_millis(delay_ms), input).await {
        session_guard.restore();
        return Err(format!(
            "reconnect to {} cancelled by user",
            target_server.endpoint_display()
        )
        .into());
    }
    if failure_context.is_none() {
        tracing::warn!(
            endpoint = %target_server.endpoint_display(),
            attempt,
            delay_ms,
            "attach loop: re-dialing after link loss"
        );
    }
    if let client::ClientTarget::Local(_) = target_server {
        let raw_root = project_override
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let project_root = host::paths::find_project_root(&raw_root);
        let resolve = tokio::select! {
            res = client::ensure_server_explicit(&project_root, explicit_port) => res,
            _ = wait_for_relink_cancel(input) => {
                session_guard.restore();
                return Err(format!(
                    "reconnect to {} cancelled by user",
                    target_server.endpoint_display()
                )
                .into());
            }
        };
        match resolve {
            Ok(info) if client::versions_compatible(&info) => {
                *target_server = client::ClientTarget::Local(info);
            }
            Ok(info) => {
                return Err(client::incompatibility_error(&info).into());
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    attempt,
                    "local server re-resolve failed during relink; will retry on next dial"
                );
            }
        }
    }
    Ok(())
}

fn print_detach_epilogue(target_server: &client::ClientTarget) {
    match target_server {
        client::ClientTarget::Local(info) => {
            // The `port > 0` gate this replaces silenced the epilogue for a
            // UDS-only server — which is the default shape — so the one line
            // that tells the operator what just happened and how to get back
            // was never printed. The endpoint is whichever one the server
            // actually advertises.
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
    let mut target_server =
        resolve_initial_target(remote, token, &project_override, explicit_port).await?;
    let mut target = session_id.clone();
    let mut fresh_pending = fresh;
    let mut pick_pending = session_id.is_none() && !fresh;
    let mut startup_overlay_pending = initial_overlay;
    let mut preserved_tabs: Option<Vec<crate::shell::surfaces::ClientTab>> = None;
    let mut preserved_tab_history: Option<Vec<crate::shell::surfaces::TabKind>> = None;
    // Disclosures produced *between* TUI runs — a retired stale tab, a session
    // that could not be reopened (ADR-0123 `[INV-ATTACH-02]`). Handed to the next
    // run so the notice log owns them instead of a lost stderr line.
    let mut retire_notice: Option<crate::shell::app::notice_catalog::ClientNotice> = None;
    let init_options = client::ThreadInitOptions::new(unattended_at_start, confined_at_start)
        .with_role(role.clone())
        .with_resume(resume);

    // Initialize terminal once and keep it alive across thread switches to eliminate flicker.
    let profile = crate::shell::terminal::detect_terminal_profile();
    let mut session_guard = crate::shell::terminal::enter_terminal(&profile)?;
    crate::shell::terminal::spawn_signal_guard();
    crate::shell::ensure_panic_hook();

    let stdout = std::io::stdout();
    let backend = graticule::Backend::new(stdout).with_profile(profile);
    let mut terminal = graticule::live_terminal(backend);
    // One input source for every attachment in this process: a cross-session
    // tab switch or tab close detaches and re-attaches *in place*, and an input
    // source owned by a single attachment dropped whatever the operator pressed
    // while the next attachment handshook — a lost first click or chord stroke
    // is indistinguishable from "the gesture needs two attempts".
    let mut input = crate::shell::event_loop::input_reader::InputSource::spawn()?;
    let mut consecutive_relinks: u32 = 0;
    let mut relinking = false;

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
        let connect_result = if relinking {
            let (_, limit) = relink_max_attempts(&target_server);
            draw_relink_status(
                &mut terminal,
                &target_server.endpoint_display(),
                consecutive_relinks.max(1),
                limit,
                None,
            );
            tokio::select! {
                res = target_server.connect(action) => res,
                _ = wait_for_relink_cancel(&mut input) => {
                    session_guard.restore();
                    return Err(format!(
                        "reconnect to {} cancelled by user",
                        target_server.endpoint_display()
                    )
                    .into());
                }
            }
        } else {
            target_server.connect(action).await
        };
        let handshake = match connect_result {
            Ok(handshake) => handshake,
            // The server proved the session does not exist: retire the tab that
            // names it and fall back to the picker. Liveness is settled by server
            // attestation only (`[INV-TAB-14]`), never by a broadcast.
            Err(error) if error.is_session_gone() => {
                relinking = false;
                consecutive_relinks = 0;
                tracing::info!(
                    code = ?error.code(),
                    target = ?target,
                    "attach refused: session no longer exists; retiring tab"
                );
                if let Some(gone) = target.take() {
                    retire_preserved_tab(&mut preserved_tabs, &gone);
                }
                retire_notice = Some(
                    crate::shell::app::notice_catalog::ClientNotice::AttachSessionGone {
                        detail: error.message,
                    },
                );
                pick_pending = true;
                continue;
            }
            // The session exists durably but could not be assembled: keep the tab
            // — its history is real and a retry may work — and report the fault.
            Err(error) if error.is_session_unresumable() => {
                relinking = false;
                consecutive_relinks = 0;
                tracing::warn!(
                    code = ?error.code(),
                    target = ?target,
                    "attach refused: session could not be assembled; tab retained"
                );
                target = None;
                retire_notice = Some(
                    crate::shell::app::notice_catalog::ClientNotice::AttachSessionUnresumable {
                        detail: error.message,
                    },
                );
                pick_pending = true;
                continue;
            }
            // While recovering from a dropped live link (`relinking == true`), a
            // transport dial failure is the outage itself, not a reason to abort
            // on attempt 1 (ADR-0224 `[INV-RESILIENCE-04]`). Explicit server
            // `Wire::Error` refusals (`!is_transport_failure()`) still fail
            // immediately instead of spinning through the retry budget.
            Err(error) if relinking && error.is_transport_failure() => {
                step_relink_backoff(
                    &mut target_server,
                    &project_override,
                    explicit_port,
                    &mut consecutive_relinks,
                    &mut terminal,
                    &mut input,
                    &mut session_guard,
                    Some(&error),
                )
                .await?;
                continue;
            }
            // Initial connect or server refusal with no recovery taxonomy stays
            // fatal so a bad `--remote` address fails loudly instead of hanging.
            Err(error) => return Err(error.into()),
        };
        let handshake = match handshake {
            client::Handshake::Pick(_) => {
                // A picker carrier is a second attach, not a second chance to
                // ignore the refusal taxonomy (ADR-0123 `[INV-ATTACH-02]`).
                match target_server
                    .connect(client::AttachAction::Picker(Some(init_options.clone())))
                    .await
                {
                    Ok(handshake) => handshake,
                    Err(error) if error.is_session_gone() => {
                        relinking = false;
                        consecutive_relinks = 0;
                        if let Some(gone) = target.take() {
                            retire_preserved_tab(&mut preserved_tabs, &gone);
                        }
                        retire_notice = Some(
                            crate::shell::app::notice_catalog::ClientNotice::AttachSessionGone {
                                detail: error.message,
                            },
                        );
                        pick_pending = true;
                        continue;
                    }
                    Err(error) if error.is_session_unresumable() => {
                        relinking = false;
                        consecutive_relinks = 0;
                        target = None;
                        retire_notice = Some(
                            crate::shell::app::notice_catalog::ClientNotice::AttachSessionUnresumable {
                                detail: error.message,
                            },
                        );
                        pick_pending = true;
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            handshake => handshake,
        };
        let (tx, rx, link_lost, hosted_session_id, initial_phase) = match handshake {
            client::Handshake::Attached {
                req_tx,
                resp_rx,
                link_lost,
                session_id,
                phase,
            } => (req_tx, resp_rx, link_lost, session_id, phase),
            client::Handshake::Pick(_) => {
                return Err("the server offered no session to pick from".into());
            }
        };
        // Attachment succeeded; leave relink recovery mode so subsequent normal
        // tab switches or explicit refusals do not re-enter relink backoff.
        relinking = false;
        // The opening prompt rides the launch config and is flushed only on the
        // `Ready` transition; a prompt sent here would race the snapshot.
        let launch_prompt = initial_prompt.take();
        let base_config = crate::shell::config::TuiConfig::load();
        let input_history = Vec::new();
        let tui_config = base_config.clone();
        let input_history_config = base_config.input_history.clone();
        let startup_overlay = std::mem::replace(
            &mut startup_overlay_pending,
            crate::shell::StartupOverlay::None,
        );
        let attached_at = Instant::now();
        let attached_session_id = hosted_session_id.clone();
        let exit_tx = tx.clone();
        let outcome = crate::shell::run_tui_with_input(
            &mut terminal,
            tx,
            rx,
            link_lost,
            crate::shell::TuiLaunchConfig {
                // Hydration arrives as an authoritative `SessionSnapshot` after
                // Welcome (ADR-0133); the handshake carries identity + phase
                // only.
                initial_provider: String::new(),
                initial_model: String::new(),
                input_history,
                initial_messages: Vec::new(),
                initial_commands: Vec::new(),
                initial_round_count: 0,
                command_catalog: tool::CommandCatalog::default(),
                initial_round_interrupts: Vec::new(),
                initial_phase,
                initial_prompt: launch_prompt,
                tui_config,
                input_history_config,
                session: crate::shell::HookStartSource::Remote {
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
        if let Err(error) = exit_tx.send(client::ClientOutbound::Request {
            thread_id: None,
            request: protocol::AgentRequest::RecordInputHistory {
                entries: outcome.history,
                dedup: base_config.input_history.dedup,
            },
        }) {
            tracing::error!(%error, "server link lost before exit history flush");
        }
        if outcome.switch_to_new {
            target = None;
            fresh_pending = true;
            initial_prompt = outcome.switch_to_new_prompt;
            continue;
        }
        // The link died with the session still hosted (ADR-0136, ADR-0224).
        // Re-dial and re-attach in place: the transcript and tabs ride
        // `TuiOutcome`, so the operator keeps their view across the gap instead
        // of losing the window.
        if outcome.relink_requested {
            if attached_at.elapsed() >= RELINK_STABILITY_WINDOW {
                consecutive_relinks = 0;
            }
            if target.is_none() {
                target = Some(attached_session_id);
            }
            relinking = true;
            step_relink_backoff(
                &mut target_server,
                &project_override,
                explicit_port,
                &mut consecutive_relinks,
                &mut terminal,
                &mut input,
                &mut session_guard,
                None,
            )
            .await?;
            continue;
        }
        relinking = false;
        consecutive_relinks = 0;
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
                session_guard.restore();
                print_detach_epilogue(&target_server);
                return Ok(());
            }
        }
    }
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
