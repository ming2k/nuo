//! Terminal user interface and interactive application runtime for Nuo.
//!
//! Subsystem architecture ratified under ADR-0223 (Graticule Terminal Substrate),
//! ADR-0185 (Single Render Path), and ADR-0107 (Two-Tier Presentation Purification).
//!
//! Layer structure:
//! - The terminal substrate (`graticule`) — retained cell grid (`graticule-canvas`)
//!   with dirty tracking and back/front diff, plus physical TTY input (`graticule-tty`).
//! - The pure zero-IO presentation crates (`ui-primitives` for generic tokens/components,
//!   `view` for agent-domain transcript models, tool presenters, layout, chrome, and views).
//! - This interactive shell runtime: application state ([`app`]), input mapping ([`input`]),
//!   surface routing ([`surfaces`]), overlays ([`overlays`]), and the event/render loop
//!   (`event_loop`). [`runner::run_with_options`] is the entry point wired by the `nuo` binary.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// Link the composition root in the test build so every `providers/*` crate's
// `inventory` baseline registration is present in the UI test binary: the
// registry-lookup tests (model search, vision gate) resolve baselines through
// it (`[INV-PROV-09]` linkage).
#[cfg(test)]
extern crate server;

pub mod action_routes;
pub mod app;
pub mod browser;
pub mod chrome;
pub mod cli;
pub mod client;
pub mod clipboard;
pub mod clipboard_ops;
pub mod completion;
pub mod composer;
pub mod composer_extension;
pub mod composer_grid;
pub mod composer_hints;
pub mod config;
pub mod copy;
pub mod draft_store;
pub mod event_loop;
pub mod focus;
pub mod headless;
pub mod input;
pub mod interaction;
pub mod keymap;
pub mod modal_keys;
pub mod overlays;
pub(crate) mod palette;
pub mod pre_attach;
pub mod question_model;
pub mod render;
pub mod runner;
pub mod session;
pub mod sheet;
pub mod step_interaction;
pub mod supervisor;
pub mod surfaces;
pub mod terminal;
pub mod transcript;
pub mod ui;

pub use app::{App, CaretOwner, ProviderDeleteChoice, SelectionEdge};
pub use completion::CompletionKind;
pub use focus::{Focus, FocusPlane, TranscriptFocus};
pub use pre_attach::{PreAttachDecision, PreAttachSignal, PreAttachState};
pub use ui_primitives::design::*;
pub use ui_primitives::{
    Theme, chord, design, elevation, fuzzy, key, keyvocab, primitives, syntax, theme, time,
};
pub use view::phase::Phase;
pub use view::providers::{CustomField, PROVIDER_PRESETS, provider_label_for};
pub use view::{
    disclosure_config, empty_state, footer_stack, layout, markdown_table, message_body, model,
    notice, phase, providers, text_layout, tools, trust_gate,
};

/// Test support: flatten a rendered grid into one string per row.
#[cfg(test)]
pub(crate) fn grid_rows(grid: &graticule::CellGrid) -> Vec<String> {
    let (w, h) = grid.size();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| grid.get(x, y).unwrap().symbol.as_str())
                .collect::<String>()
        })
        .collect()
}

use crate::shell::app::state::{
    ComposerState, HumanState, LinkState, ProviderEditorState, SessionState, SurfacesState,
    TelemetryState, Viewport,
};
use protocol::{AgentRequest, AgentResponse, ProviderPickerSnapshot, RoundEvent};

use tool::{Message, Role};

use graticule::{Backend, Terminal};
use std::{collections::HashMap, error::Error, io, sync::Arc, sync::atomic::AtomicBool};
use tokio::sync::{Mutex, mpsc};

pub use transcript::{finalize_streaming_reasoning, rebase_transcript_rounds};
use transcript::{
    merge_timeline_rows, transcript_commands_from_ledger, transcript_interrupts_from_records,
    transcript_messages_from_core,
};
use view::model::document::{DeliveryStatus, MessageKind, TranscriptMessage};
#[cfg(test)]
use view::model::document::{NoticeSeverity, UserMessageOrigin};
use view::model::selection::SelectionDrag;

/// Where the session this TUI drives lives. All sessions in the unified
/// server model are remote (server-hosted).
#[derive(Debug, Clone)]
pub enum HookStartSource {
    Remote {
        /// The hosted session's id, learned from the WS handshake.
        session_id: String,
    },
}

impl HookStartSource {
    /// The primary session id.
    pub(crate) async fn session_id(&self) -> String {
        match self {
            HookStartSource::Remote { session_id } => session_id.clone(),
        }
    }
}

/// Which full-screen overlay (if any) the TUI opens straight into at startup
/// instead of a thread view. In that mode the overlay is not a transient
/// modal — there is no thread the user asked for behind it — so closing
/// it quits the program rather than dropping into an empty chat (mirrors how
/// `nuo attach`'s picker behaves). Distinct from `None`, where the TUI
/// opens directly onto a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupOverlay {
    /// Ordinary startup: land on the thread.
    None,
    /// `nuo attach` (no id): open the sessions picker to choose a session.
    SessionsPicker,
    /// `nuo dashboard`: open the session dashboard over the carrier session.
    Dashboard,
    /// `nuo settings`: open the settings view directly (optional category index).
    Settings { category: Option<usize> },
}

impl StartupOverlay {
    /// Resolve startup overlay intent from acceptance / test / launch environment variables:
    /// - `NUO_STARTUP_VIEW`: e.g. `settings`, `settings:web`, `settings:3`, `dashboard`, `sessions`.
    /// - `NUO_SETTINGS_NAV` / `NUO_SETTINGS_CATEGORY`: e.g. `appearance`, `components`, `search`, `web`, `system`, `0..4`.
    pub fn resolve_from_env() -> Option<Self> {
        let view_val = std::env::var("NUO_STARTUP_VIEW")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let nav_val = std::env::var("NUO_SETTINGS_NAV")
            .or_else(|_| std::env::var("NUO_SETTINGS_CATEGORY"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(view) = view_val {
            let lower = view.to_ascii_lowercase();
            if lower == "dashboard" {
                return Some(StartupOverlay::Dashboard);
            }
            if lower == "sessions" || lower == "attach" || lower == "picker" {
                return Some(StartupOverlay::SessionsPicker);
            }
            if lower == "settings"
                || lower.starts_with("settings:")
                || lower.starts_with("settings/")
                || lower.starts_with("settings.")
                || lower == "config"
                || lower.starts_with("config:")
                || lower.starts_with("config/")
            {
                let suffix_cat = lower.split_once([':', '/', '.']).map(|(_, s)| s);
                let cat = suffix_cat
                    .or(nav_val.as_deref())
                    .and_then(view::views::ConfigCategory::from_name)
                    .map(|c| c as usize);
                return Some(StartupOverlay::Settings { category: cat });
            }
        } else if let Some(nav) = nav_val {
            let cat = view::views::ConfigCategory::from_name(&nav).map(|c| c as usize);
            return Some(StartupOverlay::Settings { category: cat });
        }

        None
    }
}

/// Whether an inbound response is a high-frequency visual update that can wait
/// for the active 10fps render heartbeat. The listener still applies it and
/// marks the UI dirty immediately; it merely avoids waking the event loop for
/// every token. Starts, ends, errors, permissions, and tool lifecycle changes
/// Initial `App::pre_attach` value resolved from the acceptance
/// environment. Returns the PreAttach acceptance fixture when
/// `NUO_FORCE_PRE_ATTACH` is set to a truthy value (`1`, `true`,
/// `yes`, `on` — case-insensitive); `None` otherwise, so the
/// per-frame sync owns the production mounting path through the
/// `pre_attach_signal` cell.
///
/// Documented alongside the other `NUO_*` acceptance toggles per
fn pre_attach_initial() -> Option<PreAttachState> {
    let raw = std::env::var("NUO_FORCE_PRE_ATTACH")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let truthy = |v: &str| matches!(v, "1" | "true" | "yes" | "on");
    if raw.as_deref().is_some_and(truthy) {
        tracing::info!("nuo: NUO_FORCE_PRE_ATTACH set — mounting PreAttach acceptance fixture");
        Some(PreAttachState::acceptance_fixture())
    } else {
        None
    }
}

/// The minimum interval between content frames while a model response is
/// streaming. Animation keeps its own 100ms heartbeat; text is allowed to
/// repaint at up to 60Hz without turning every provider delta into a frame.
pub(crate) const STREAM_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// remain immediate so the UI never feels unresponsive at a state boundary.
pub(crate) fn is_coalescible_stream_update(response: &AgentResponse) -> bool {
    matches!(
        response,
        AgentResponse::Round { scope, .. }
            if matches!(
                scope.event(),
                RoundEvent::StreamDelta(_)
                    | RoundEvent::StreamReasoningDelta(_)
                    | RoundEvent::ToolStream { .. }
                    | RoundEvent::ToolInputProgress { .. }
                    | RoundEvent::SubagentStep {
                        event: tool::SubagentEvent::StreamDelta(_)
                            | tool::SubagentEvent::StreamReasoningDelta(_),
                        ..
                    }
            )
    )
}

pub struct TuiLaunchConfig {
    pub initial_provider: String,
    pub initial_model: String,
    pub input_history: Vec<protocol::HistoryEntry>,
    pub initial_messages: Vec<Message>,
    pub initial_commands: Vec<protocol::CommandRecord>,
    pub initial_round_count: u64,
    pub command_catalog: tool::CommandCatalog,
    pub initial_round_interrupts: Vec<protocol::RoundInterrupt>,
    pub tui_config: view::tui_config::TuiConfig,
    pub input_history_config: view::tui_config::InputHistoryConfig,
    pub session: HookStartSource,
    pub token_ledger: Option<Arc<protocol::TokenSourceLedger>>,
    pub startup_overlay: StartupOverlay,
    pub initial_tabs: Option<Vec<crate::shell::surfaces::ClientTab>>,
    pub initial_tab_history: Option<Vec<crate::shell::surfaces::TabKind>>,
    /// Disclosures the attach loop accumulated *before* this run existed — a
    /// stale tab it had to retire, a session it could not reopen
    /// (ADR-0123 `[INV-ATTACH-02]`).
    ///
    /// They ride the launch config because the client had no `App` yet when the
    /// refusal arrived, and the disposal must be *retained* rather than printed
    /// to a stderr the alternate screen is about to paint over. Each one is
    /// replayed through the canonical notice funnel
    /// ([ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md)),
    /// so the reason a tab vanished is answerable minutes later.
    /// Server-attested readiness at attach time (ADR-0133). `Ready` is only
    /// authoritative when the snapshot has also been applied; the client gates
    /// input on the transition, not on this initial value.
    pub initial_phase: protocol::ThreadPhase,
    /// Server-attested global lifecycle phase from `Wire::ServerHello` (ADR-0260).
    pub initial_server_phase: protocol::ServerPhase,
    /// Optional thread subscription deferred until `ServerPhase::Ready` arrives
    /// (ADR-0260 `[INV-SRVPHASE-02]`).
    pub pending_initial_subscribe: Option<(protocol::ThreadTarget, Option<std::path::PathBuf>)>,
    /// Opening prompt (`-p`) dispatched on the `Ready` transition rather than
    /// at handshake time (ADR-0133).
    pub initial_prompt: Option<String>,
    /// Pre-mount disclosures from the attach loop (ADR-0123
    /// `[INV-ATTACH-02]`): a retired/unresumable tab is *retained* in the notice
    /// log rather than merely flashed and lost. Carried as catalog kinds
    /// (ADR-0138) so the wording and topic are owned in one place, not formatted
    /// by the attach loop.
    pub startup_notices: Vec<crate::shell::app::notice_catalog::ClientNotice>,
}

/// Install the panic hook so abnormal main-thread termination restores the terminal.
pub fn ensure_panic_hook() {
    static PANIC_HOOK: std::sync::Once = std::sync::Once::new();
    PANIC_HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let main_thread = std::thread::current()
                .name()
                .is_some_and(|name| name == "main");
            if main_thread {
                terminal::restore_terminal();
            }
            default_hook(info);
        }));
    });
}

pub async fn run_tui(
    tx: mpsc::UnboundedSender<client::ClientOutbound>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    monitor_rx: mpsc::UnboundedReceiver<protocol::MonitorEvent>,
    link_lost: crate::shell::client::LinkLostMarker,
    config: TuiLaunchConfig,
) -> Result<TuiOutcome, Box<dyn Error>> {
    let profile = terminal::detect_terminal_profile();
    let mut session_guard = terminal::enter_terminal(&profile)?;
    let stdout = io::stdout();
    let backend = Backend::new(stdout).with_profile(profile);
    let mut terminal = graticule::live_terminal(backend);
    terminal::spawn_signal_guard();
    ensure_panic_hook();
    let outcome = run_tui_with_terminal(&mut terminal, tx, rx, monitor_rx, link_lost, config).await;
    session_guard.restore();
    outcome
}

pub async fn run_tui_with_terminal(
    terminal: &mut Terminal<io::Stdout>,
    tx: mpsc::UnboundedSender<client::ClientOutbound>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    monitor_rx: mpsc::UnboundedReceiver<protocol::MonitorEvent>,
    link_lost: crate::shell::client::LinkLostMarker,
    config: TuiLaunchConfig,
) -> Result<TuiOutcome, Box<dyn Error>> {
    let mut input = event_loop::input_reader::InputSource::spawn()?;
    run_tui_with_input(terminal, tx, rx, monitor_rx, link_lost, config, &mut input).await
}

/// One attachment's TUI run against a caller-owned [`InputSource`].
///
/// The attach loop (`runner::run_attached`) owns the input source so terminal
/// events that arrive while the next session handshakes are still delivered
/// instead of being dropped with the previous loop — see
/// [`event_loop::input_reader::InputSource`].
/// Translate a wire turn ordinal into a transcript turn ordinal.
///
/// `RoundEvent::TurnStarted` and `RoundEvent::StreamDiscard` carry the
/// **0-indexed** model-request position. Transcript rows and UI labels are
/// **1-indexed** (`turn 0` is shown as `turn 1`). One helper owns that
/// conversion so a new position-carrying event cannot invent a different
/// convention.
pub(crate) fn transcript_turn(wire_turn: u64) -> u64 {
    wire_turn.saturating_add(1)
}

/// The one translation from a wire discard to the shell's transcript edit.
pub(crate) fn stream_discard_edit(
    round: u64,
    wire_turn: u64,
) -> event_loop::mutations::TranscriptEdit {
    event_loop::mutations::TranscriptEdit::StreamDiscard {
        round,
        turn: transcript_turn(wire_turn),
    }
}

fn pending_prompts(initial_prompt: Option<String>) -> Vec<protocol::AgentRequest> {
    initial_prompt
        .map(|text| {
            vec![protocol::AgentRequest::Prompt {
                text,
                images: Vec::new(),
                sent_at_ms: None,
            }]
        })
        .unwrap_or_default()
}

fn restore_transcript(
    initial_messages: Vec<Message>,
    initial_commands: Vec<protocol::CommandRecord>,
    initial_round_interrupts: Vec<protocol::RoundInterrupt>,
    initial_round_count: u64,
    tui_config: &view::tui_config::TuiConfig,
) -> Vec<view::model::document::TranscriptMessage> {
    let mut restored = transcript_messages_from_core(initial_messages, tui_config);
    restored = merge_timeline_rows(
        restored,
        transcript_commands_from_ledger(initial_commands),
        transcript_interrupts_from_records(initial_round_interrupts),
    );
    rebase_transcript_rounds(&mut restored, initial_round_count);
    restored
}

#[allow(clippy::too_many_arguments)]
async fn start_ui_runtime(
    tui_config: view::tui_config::TuiConfig,
    session: &crate::shell::HookStartSource,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    monitor_rx: mpsc::UnboundedReceiver<protocol::MonitorEvent>,
    link_lost: crate::shell::client::LinkLostMarker,
    initial_provider: &str,
    initial_model: &str,
    initial_round_count: u64,
) -> (
    Arc<view::tui_config::TuiConfig>,
    event_loop::UiRuntime,
    String,
    Arc<AtomicBool>,
    tokio::sync::mpsc::Receiver<event_loop::AppMutation>,
) {
    let tui_config = Arc::new(tui_config);
    // M1: the response translator and the server-event translator own **no**
    // application state. They consume wire frames from the single unified
    // connection (ADR-0260 `[INV-SRVPHASE-04]`) and produce typed
    // `AppMutation`s onto a bounded channel; the event loop — the sole `App`
    // writer — drains and applies them each iteration.
    let (mutation_tx, mutation_rx) = tokio::sync::mpsc::channel::<event_loop::AppMutation>(1024);
    let mutations = event_loop::mutations::MutationSink::new(mutation_tx);

    // Stage 3 redraw signal + Stage 4 wakeup: translators flip/notify so the
    // loop's `select!` wakes immediately on a response; high-frequency
    // stream deltas deliberately rely on the loop's 10fps heartbeat to
    // coalesce into a smooth stream.
    let dirty = Arc::new(AtomicBool::new(true));
    let dirty_notify = Arc::new(tokio::sync::Notify::new());
    let should_quit = Arc::new(AtomicBool::new(false));

    // Loop → translator coordination facts (see `event_loop::runtime`).
    let is_responding = Arc::new(AtomicBool::new(false));
    let trust_gate_dismissed = Arc::new(AtomicBool::new(false));
    let awaiting_oauth_add = Arc::new(AtomicBool::new(false));
    let viewed_session_id = Arc::new(Mutex::new(None::<String>));

    let live_session_init = session.session_id().await;

    let runtime = event_loop::UiRuntime {
        dirty,
        dirty_notify,
        is_responding,
        awaiting_oauth_add,
        trust_gate_dismissed,
        viewed_session_id,
        mutations,
    };

    event_loop::translators::spawn_monitor_translator(&runtime, monitor_rx);
    event_loop::translators::spawn_response_translator(
        event_loop::translators::ResponseTranslatorConfig {
            rx,
            runtime: runtime.clone(),
            link_lost,
            tui_config: tui_config.clone(),
            initial_provider: initial_provider.to_string(),
            initial_model: initial_model.to_string(),
            initial_round_count,
            live_session: live_session_init.clone(),
        },
    );

    (
        tui_config,
        runtime,
        live_session_init,
        should_quit,
        mutation_rx,
    )
}

/// Every captured launch input the initial [`App`] literal reads.
///
/// Grouped so [`build_app`] stays a pure constructor: the orchestrator decides
/// what the boot *is*, the helper decides how it is wired into sub-states.
pub(crate) struct AppBoot<'a> {
    pub(crate) live_session_init: &'a String,
    pub(crate) initial_provider: String,
    pub(crate) initial_model: String,
    pub(crate) session_phase_init: protocol::ThreadPhase,
    pub(crate) server_phase_init: protocol::ServerPhase,
    pub(crate) pending_initial_subscribe:
        Option<(protocol::ThreadTarget, Option<std::path::PathBuf>)>,
    pub(crate) restored: Vec<TranscriptMessage>,
    pub(crate) tui_config: Arc<view::tui_config::TuiConfig>,
    pub(crate) command_catalog: tool::CommandCatalog,
    pub(crate) initial_pending_prompts: Vec<protocol::AgentRequest>,
    pub(crate) input_history: Vec<protocol::HistoryEntry>,
    pub(crate) input_history_config: view::tui_config::InputHistoryConfig,
    pub(crate) startup_overlay: StartupOverlay,
    pub(crate) profile: graticule::TerminalProfile,
    pub(crate) token_ledger: Option<Arc<protocol::TokenSourceLedger>>,
    pub(crate) tx: mpsc::UnboundedSender<client::ClientOutbound>,
    pub(crate) should_quit: Arc<AtomicBool>,
}

/// Build the initial [`App`] from a captured [`AppBoot`].
///
/// The literal is kept whole (rather than spread across per-sub-state
/// constructors) so the initial values and their order stay auditable in one
/// place; [`run_tui_with_input`] remains startup/teardown orchestration.
pub(crate) fn build_app(boot: AppBoot<'_>) -> App {
    let mut initial_thread = crate::shell::app::workspaces::ThreadInstance::new(
        boot.live_session_init.clone(),
        1,
        boot.session_phase_init.clone(),
    );
    initial_thread.messages = boot.restored;
    initial_thread.current_provider = boot.initial_provider;
    initial_thread.current_model = boot.initial_model;

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let theme_files = crate::shell::config::load_all_theme_files(Some(&cwd));

    App {
        session: SessionState {
            queue_exit_session: None,
            subscribed_threads: {
                let mut set = std::collections::HashSet::new();
                if !boot.live_session_init.is_empty() {
                    set.insert(boot.live_session_init.clone());
                }
                set
            },
            live_session_id: boot.live_session_init.clone(),
            workspace_security: protocol::WorkspaceSecuritySnapshot::default(),
            tui_config: (*boot.tui_config).clone(),
            session_detail: None,
            cwd,
            current_session_id: boot.live_session_init.clone(),
            // Only set when the user explicitly switches/resumes another thread;
            // initial thread readiness is projected directly from `session_phase`
            // (ADR-0133) rather than pretending a cold boot is switching sessions.
            switching_session: None,
            persistence_health: None,
            server_phase: boot.server_phase_init,
            server_phase_since: std::time::Instant::now(),
            thread_phase_since: std::time::Instant::now(),
            pending_initial_subscribe: boot.pending_initial_subscribe,
            initial_subscribe_in_flight: false,
            initial_init_options: protocol::ThreadInitOptions::default(),
            project_scope: None,
            sessions_overview: Vec::new(),
            host_sessions: Vec::new(),
            queue_blocked_sessions: std::collections::HashSet::new(),
            running_sessions: std::collections::HashSet::new(),
            unseen_outcomes: std::collections::HashMap::new(),
        },
        threads: crate::shell::app::workspaces::Threads::new(initial_thread),
        viewport: Viewport {
            ui: crate::shell::ui::ComponentTree::new(),
            drag: SelectionDrag::default(),
            hovered_step: None,
            view_height: 0,
            modal_body_height: 0,
            spinner_epoch: std::time::Instant::now(),
            carousel_epoch: std::time::Instant::now(),
            transcript_layout: crate::shell::render::layout::Strategy::TurnBand,
            expand_auto_scroll: boot.tui_config.expand_auto_scroll,
            reasoning_default_expanded: view::tui_config::reasoning_default_expanded(
                &boot.tui_config,
            ),
            logs: view::model::log::SurfaceLogs::new(),
            host_console_log: Vec::new(),
        },
        composer: ComposerState {
            last_submit_ms: None,
            input: String::new(),
            backend_completion_signal: None,
            suggestion_index: None,
            completion_dismissed: false,
            command_catalog: boot.command_catalog,
            backend_completions: Vec::new(),
            completion_response_input: None,
            completion_response_cursor: 0,
            completion_requested: None,
            completion_request_id: 0,
            cursor_position: 0,
            caret_motion: crate::shell::composer_grid::CaretMotion::default(),
            input_scroll: 0,
            input_scroll_follow_cursor: true,
            input_drag_scroll: None,
            pending_prompts: boot.initial_pending_prompts,
            draft_autosave_at: std::time::Instant::now(),
            draft_thread: None,
            input_history: if boot.input_history_config.record_commands {
                boot.input_history
            } else {
                // `[input_history] record_commands = false` (default): scrub any
                // legacy `/slash` invocations from the loaded history so they stop
                // showing in the picker, and — since this list is what gets
                // `save_history`d on exit — the on-disk file heals itself too.
                boot.input_history
                    .into_iter()
                    .filter(|e| !e.text.starts_with('/'))
                    .collect()
            },
            history_index: None,
            stashed_draft: view::composer_draft::ComposerDraft::default(),
            history_attachments: std::collections::HashMap::new(),
            history_attachments_order: std::collections::VecDeque::new(),
            session_history_backfill: Vec::new(),
            session_history_backfill_cursor: 0,
            input_history_dedup: boot.input_history_config.dedup,
            input_history_record_commands: boot.input_history_config.record_commands,
            input_history_persist: true,
            pending_images: Vec::new(),
            pending_text_pastes: Vec::new(),
            pending_dispatch: std::collections::VecDeque::new(),
            composer_send_mode: crate::shell::app::ComposerSendMode::default(),
            suppress_steer_restore: std::collections::HashSet::new(),
        },
        human: HumanState {
            pending_permissions: std::collections::VecDeque::new(),
            pending_questions: std::collections::VecDeque::new(),
            pending_inputs: std::collections::VecDeque::new(),
            subagent_permission_parent: HashMap::new(),
            subagent_question_parent: HashMap::new(),
            pending_permission: None,
            active_sheet: None,
            pending_permission_depth: 0,
            pending_question_depth: 0,
            pending_input: None,
            question: None,
            question_scroll: 0,
            question_modal_follow: true,
            permission_confirm_always: false,
            permission_show_details: false,
            permission_scroll: 0,
            permission_max_scroll: 0,
        },
        surfaces: SurfacesState {
            pending_fold_copy: None,
            command_log_scroll: 0,
            theme_files,
            surface_store: crate::shell::surfaces::SurfaceStore::new(),
            drafts: crate::shell::draft_store::DraftLedger::load_for_scope(
                host::paths::get().drafts_dir(),
                &host::paths::get().db_file().to_string_lossy(),
            ),
            dev_toast_pinned: std::env::var("NUO_DEV_TOAST")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false),
            surfaces: match boot.startup_overlay {
                StartupOverlay::SessionsPicker => {
                    crate::shell::surfaces::SurfaceRouter::with_dialog(
                        crate::shell::surfaces::DialogKind::Sessions,
                    )
                }
                StartupOverlay::Settings { .. } => {
                    crate::shell::surfaces::SurfaceRouter::with_scene(
                        crate::shell::surfaces::SceneKind::Settings,
                    )
                }
                _ => crate::shell::surfaces::SurfaceRouter::new(),
            },
            modal_index: 0,
            connection_detail: None,
            config_scroll: 0,
            config_focus: crate::shell::overlays::ConfigFocus::Categories,
            config_category: match boot.startup_overlay {
                StartupOverlay::Settings {
                    category: Some(cat),
                } => cat.min(view::views::ConfigCategory::ALL.len().saturating_sub(1)),
                _ => 0,
            },
            config_detail_index: 0,
            config_hover_index: None,
            config_detail_scroll: 0,
            tab_hover: None,
            tab_hover_since: None,
            tab_reveal_active: false,
            tab_shimmer_active: false,
            websearch_config: None,
            config_dropdown: None,
            config_selected_rect: None,
            pre_attach: pre_attach_initial(),
            dashboard_filter: String::new(),
            dashboard_filtering: false,
            dashboard_view_usage: false,
            host_scroll: 0,
            host_modal_follow: true,
            host_focus: crate::shell::overlays::DashboardFocus::Detail,
            host_detail_scroll: 0,
            host_preview: None,
            host_preview_scroll: 0,
            host_prompting: false,
            host_prompt_new: false,
            host_kill_confirm: None,
            host_kill_confirm_id: None,
            switch_to_target: None,
            pending_help_action: None,
            palette_recents: Default::default(),
            switch_to_prompt: None,
            startup_overlay: boot.startup_overlay,
            color_scheme: Theme::normalize_color_scheme(&boot.tui_config.color_scheme).to_string(),
            custom_color_scheme: boot.tui_config.custom_color_scheme.clone(),
            click_outside_dismiss: boot.tui_config.click_outside_dismiss,
            key_overrides: crate::shell::config::global_key_overrides(&boot.tui_config),
            surface_overrides: crate::shell::config::surface_key_overrides(&boot.tui_config),
            copy_toast_until: None,
            copy_toast_message: String::new(),
            copy_toast_failed: false,
            notice_toast_until: None,
            notice_toast_message: String::new(),
            notice_toast_topic: String::new(),
            notice_toast_severity: view::model::document::NoticeSeverity::Info,
            ctrl_c_armed_until: None,
            esc_armed_until: None,
            pending_provider_delete: None,
            provider_delete_focus: ProviderDeleteChoice::default(),
            key_status: HashMap::new(),
            provider_picker: ProviderPickerSnapshot::default(),
            theme: crate::shell::config::resolve_theme_with_profile(
                &boot.tui_config.color_scheme,
                &boot.tui_config.custom_color_scheme,
                None,
                &boot.profile,
            ),
            profile: boot.profile,
            logo: load_user_logo(),
        },
        provider_editor: ProviderEditorState {
            editor_target: None,
            editor_field: 0,
            editor_key: String::new(),
            editor_model: String::new(),
            editor_model_settings_only: false,
            editor_target_is_builtin: false,
            editor_effort: "high".to_string(),
            editor_effort_levels: Vec::new(),
            editor_thinking_available: false,
            editor_thinking: true,
            editor_vision_override: None,
            editor_tool_override: None,
            custom_field: 0,
            custom_fields: Vec::new(),
            custom_protocol_wire: String::new(),
            custom_budget: protocol::ConnectionBudget::default(),
            custom_models: Vec::new(),
            custom_url_hint: String::new(),
            custom_user_agent: None,
            custom_auth: protocol::ConnectionAuth::Static,
            custom_provider_id: None,
            awaiting_oauth_add: false,
            oauth_pending_message: String::new(),
            oauth_pending_url: String::new(),
            oauth_pending_user_code: String::new(),
            oauth_pending_error: None,
            oauth_selected_item: 0,
            oauth_scroll: 0,
            custom_scroll: 0,
            custom_edit_id: None,
            custom_name: String::new(),
            custom_base_url: String::new(),
            custom_token: String::new(),
            custom_model: String::new(),
            preset_choice: 0,
            preset_scroll: 0,
        },
        telemetry: TelemetryState {
            token_ledger: boot.token_ledger,
            context_tokens: None,
            usage_stats: None,
            usage_loading: false,
            usage_scroll: 0,
            provider_quotas: None,
            connection_usages: std::collections::HashMap::new(),
            context_tokens_by_session: HashMap::new(),
        },
        link: LinkState {
            tx: boot.tx.clone(),
            should_quit: boot.should_quit,
            open_sessions_signal: false,
            open_tree_signal: false,
            last_key_press: std::time::Instant::now(),
            link_down: false,
            relink_requested: false,
            phase: crate::shell::app::state::ClientLinkPhase::Connected {
                server_id: String::new(),
            },
            phase_since: std::time::Instant::now(),
            known_server_id: None,
            supervised: false,
            supervisor_cancel: None,
            supervisor_reconnect_notify: None,
        },
    }
}

pub(crate) async fn run_tui_with_input(
    terminal: &mut Terminal<io::Stdout>,
    tx: mpsc::UnboundedSender<client::ClientOutbound>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    monitor_rx: mpsc::UnboundedReceiver<protocol::MonitorEvent>,
    link_lost: crate::shell::client::LinkLostMarker,
    config: TuiLaunchConfig,
    input: &mut event_loop::input_reader::InputSource,
) -> Result<TuiOutcome, Box<dyn Error>> {
    let TuiLaunchConfig {
        initial_provider,
        initial_model,
        input_history,
        initial_messages,
        initial_commands,
        initial_round_count,
        command_catalog,
        initial_round_interrupts,
        tui_config,
        input_history_config,
        session,
        token_ledger,
        startup_overlay,
        initial_tabs,
        initial_tab_history,
        startup_notices,
        initial_phase,
        initial_server_phase,
        pending_initial_subscribe,
        initial_prompt,
    } = config;
    let session_phase_init = initial_phase.clone();
    let initial_pending_prompts = pending_prompts(initial_prompt);
    let profile = terminal::detect_terminal_profile();
    let restored = restore_transcript(
        initial_messages,
        initial_commands,
        initial_round_interrupts,
        initial_round_count,
        &tui_config,
    );
    let (tui_config, runtime, live_session_init, should_quit, mutation_rx) = start_ui_runtime(
        tui_config,
        &session,
        rx,
        monitor_rx,
        link_lost,
        &initial_provider,
        &initial_model,
        initial_round_count,
    )
    .await;

    let boot = AppBoot {
        live_session_init: &live_session_init,
        initial_provider,
        initial_model,
        session_phase_init,
        server_phase_init: initial_server_phase,
        pending_initial_subscribe,
        restored,
        tui_config,
        command_catalog,
        initial_pending_prompts,
        input_history,
        input_history_config,
        startup_overlay,
        profile,
        token_ledger,
        tx,
        should_quit,
    };
    let mut app = build_app(boot);

    // ADR-0250: reconcile parked drafts against the server's catalog on connect.
    if !live_session_init.is_empty() {
        let draft_ids: Vec<String> = app
            .surfaces
            .drafts
            .known_thread_ids()
            .into_iter()
            .filter(|id| id != &live_session_init)
            .collect();
        if !draft_ids.is_empty() {
            app.send_intent(protocol::AgentRequest::CheckThreadsExist {
                thread_ids: draft_ids,
            });
        }
    }

    // Mount tabs: restore preserved tabs if re-attaching, or mount initial thread/overlay tab
    if let Some(tabs) = initial_tabs {
        for tab in tabs {
            app.surfaces.surfaces.open_tab(tab);
        }
        if let Some(history) = initial_tab_history {
            app.surfaces.surfaces.set_tab_mru_history(history);
        }
        if !live_session_init.is_empty() {
            if let Some(pos) = app
                .surfaces
                .surfaces
                .tabs()
                .iter()
                .position(|t| t.thread_id() == Some(live_session_init.as_str()))
            {
                app.surfaces.surfaces.select_tab(pos);
                app.hand_composer_to_current_thread();
            } else {
                app.surfaces
                    .surfaces
                    .open_tab(crate::shell::surfaces::ClientTab::thread(
                        &live_session_init,
                        None::<String>,
                    ));
            }
        }
    } else {
        if !live_session_init.is_empty() {
            app.surfaces
                .surfaces
                .open_tab(crate::shell::surfaces::ClientTab::thread(
                    &live_session_init,
                    None::<String>,
                ));
        }
        if matches!(startup_overlay, StartupOverlay::Dashboard) {
            app.surfaces
                .surfaces
                .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        } else if matches!(startup_overlay, StartupOverlay::Settings { .. }) {
            app.surfaces
                .surfaces
                .open_tab(crate::shell::surfaces::ClientTab::settings());
        }
    }

    // The boot hand-off (ADR-0247): whatever route mounted the tabs, the
    // composer now belongs to the thread the foreground names, and until now it
    // belonged to nothing — a fresh `App` has no `draft_thread`. This installs
    // the parked draft written by the process that just exited, which is the
    // half of the recovery a preserved tab cannot carry itself.
    app.hand_composer_to_current_thread();

    if startup_overlay == StartupOverlay::SessionsPicker {
        app.surfaces
            .surface_store
            .open(crate::shell::surfaces::DialogKind::Sessions);
    }
    // Replay the attach loop's pre-mount disclosures (ADR-0123
    // `[INV-ATTACH-02]`). They go through the notice funnel, so the reason a
    // tab was retired is in the notice log — not merely flashed and lost.
    for notice in startup_notices {
        app.record_client_notice(notice);
    }
    if matches!(startup_overlay, StartupOverlay::Settings { .. }) {
        app.send_intent(AgentRequest::QueryWebSearchConfig);
    }
    // Hydrate the prompt history from the server (the SSOT for the shared
    // SQLite store — the TUI never opens the database itself).
    app.send_intent(AgentRequest::QueryInputHistory);
    // Pre-query provider quotas in background off the first-frame path.
    app.send_intent(AgentRequest::QueryProviderQuotas {
        provider: None,
        force_refresh: false,
    });

    // Negotiate progressive keyboard enhancement *off* the first-frame path
    // through the single-reader `ProbeHandle`.
    let probe = input.probe_handle();
    std::thread::Builder::new()
        .name("nuo-kbd-enhance".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            crate::shell::terminal::negotiate_keyboard_enhancement(&profile, &probe);
        })?;

    // Run app
    let res =
        event_loop::run_app_loop(terminal, &mut app, runtime, mutation_rx, session, input).await;

    if let Err(err) = res {
        return Err(err.into());
    }

    let switch = app.surfaces.switch_to_target.take();
    let switch_to_prompt = app.surfaces.switch_to_prompt.take();
    // The exit seam for unsent input (ADR-0247): the next `App` is a new
    // instance, so every draft must already be in the ledger. This runs on
    // *every* outcome — switch, fork, `/new`, detach, or a dead link — because
    // all of them end this `App`.
    app.seal_drafts();
    let active_session_id = app.session.live_session_id.clone();
    let exit_tx = Some(app.link.tx.clone());
    let tabs = Some(app.surfaces.surfaces.tabs().to_vec());
    let tab_history = Some(app.surfaces.surfaces.tab_mru_history().to_vec());
    Ok(TuiOutcome {
        history: app.composer.input_history,
        active_session_id,
        exit_tx,
        switch_to: switch,
        switch_to_prompt,
        tabs,
        tab_history,
        relink_requested: app.link.relink_requested,
    })
}

/// Launch configuration for a single-lifetime supervised TUI process
/// (ADR-0260 `[INV-SRVPHASE-02]`, `[INV-SRVPHASE-04]`).
pub(crate) struct SupervisedTuiLaunch {
    pub remote: Option<String>,
    pub token: Option<String>,
    pub project_override: Option<std::path::PathBuf>,
    pub explicit_port: Option<u16>,
    pub initial_subscribe: Option<(protocol::ThreadTarget, Option<std::path::PathBuf>)>,
    pub initial_init_options: protocol::ThreadInitOptions,
    pub project_scope: Option<std::path::PathBuf>,
    pub startup_overlay: StartupOverlay,
    pub initial_prompt: Option<String>,
    pub tui_config: view::tui_config::TuiConfig,
}

/// Run the interactive TUI with a single `App` lifetime and an in-process
/// `ConnectionSupervisor` owning discovery, server spawn, `Hello -> ServerHello`,
/// single-connection monitoring/responses, and in-place reconnection (ADR-0260).
pub(crate) async fn run_supervised_tui(
    terminal: &mut Terminal<io::Stdout>,
    input: &mut event_loop::input_reader::InputSource,
    launch: SupervisedTuiLaunch,
) -> Result<(TuiOutcome, Option<client::ClientTarget>), Box<dyn Error>> {
    let SupervisedTuiLaunch {
        remote,
        token,
        project_override,
        explicit_port,
        initial_subscribe,
        initial_init_options,
        project_scope,
        startup_overlay,
        initial_prompt,
        tui_config,
    } = launch;

    let profile = terminal::detect_terminal_profile();
    let input_history_config = tui_config.input_history.clone();
    let tui_config = Arc::new(tui_config);

    let (mutation_tx, mutation_rx) = tokio::sync::mpsc::channel::<event_loop::AppMutation>(1024);
    let mutations = event_loop::mutations::MutationSink::new(mutation_tx);
    let dirty = Arc::new(AtomicBool::new(true));
    let dirty_notify = Arc::new(tokio::sync::Notify::new());
    let should_quit = Arc::new(AtomicBool::new(false));
    let runtime = event_loop::UiRuntime {
        dirty,
        dirty_notify,
        is_responding: Arc::new(AtomicBool::new(false)),
        awaiting_oauth_add: Arc::new(AtomicBool::new(false)),
        trust_gate_dismissed: Arc::new(AtomicBool::new(false)),
        viewed_session_id: Arc::new(Mutex::new(None::<String>)),
        mutations,
    };

    // Closed placeholder channel until `ConnectionSupervisor` installs the live
    // `ServerLink.req_tx` via `AppMutation::LinkEstablished` (ADR-0267
    // `[INV-SRVPHASE-09]`): dropping the receiver immediately ensures no
    // pre-connect frame can ever vanish silently into an unread buffer.
    let (placeholder_tx, _) = mpsc::unbounded_channel();
    let live_session_init = String::new();
    let initial_thread_phase = if initial_subscribe.is_some() {
        protocol::ThreadPhase::Preparing {
            stage: protocol::PrepareStage::Thread,
        }
    } else {
        protocol::ThreadPhase::Ready
    };

    let mut app = build_app(AppBoot {
        live_session_init: &live_session_init,
        initial_provider: String::new(),
        initial_model: String::new(),
        session_phase_init: initial_thread_phase,
        server_phase_init: protocol::ServerPhase::Ready,
        pending_initial_subscribe: initial_subscribe,
        restored: Vec::new(),
        tui_config: tui_config.clone(),
        command_catalog: tool::CommandCatalog::default(),
        initial_pending_prompts: pending_prompts(initial_prompt),
        input_history: Vec::new(),
        input_history_config,
        startup_overlay,
        profile,
        token_ledger: None,
        tx: placeholder_tx,
        should_quit: should_quit.clone(),
    });

    let supervisor_cancel = tokio_util::sync::CancellationToken::new();
    let reconnect_notify = Arc::new(tokio::sync::Notify::new());
    let resolved_target = Arc::new(std::sync::Mutex::new(None::<client::ClientTarget>));

    app.session.initial_init_options = initial_init_options;
    app.session.project_scope = project_scope;
    app.link.supervised = true;
    app.link.phase = crate::shell::app::state::ClientLinkPhase::Connecting {
        label: if let Some(ref addr) = remote {
            format!("Connecting to {addr}…")
        } else {
            "Starting Nuo server…".to_string()
        },
    };
    app.link.phase_since = std::time::Instant::now();
    app.link.supervisor_cancel = Some(supervisor_cancel.clone());
    app.link.supervisor_reconnect_notify = Some(reconnect_notify.clone());

    if matches!(startup_overlay, StartupOverlay::Dashboard) {
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
    } else if matches!(startup_overlay, StartupOverlay::Settings { .. }) {
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::settings());
    }

    let supervisor_handle = supervisor::spawn_connection_supervisor(supervisor::SupervisorConfig {
        remote,
        token,
        project_override,
        explicit_port,
        runtime: runtime.clone(),
        tui_config,
        should_quit: should_quit.clone(),
        cancel: supervisor_cancel.clone(),
        reconnect_notify,
        resolved_target: resolved_target.clone(),
    });

    let session = HookStartSource::Remote {
        session_id: String::new(),
    };
    let loop_res =
        event_loop::run_app_loop(terminal, &mut app, runtime, mutation_rx, session, input).await;

    supervisor_cancel.cancel();
    supervisor_handle.abort();

    if let Err(err) = loop_res {
        return Err(err.into());
    }

    app.seal_drafts();
    let active_session_id = app.session.live_session_id.clone();
    let exit_tx = Some(app.link.tx.clone());
    let tabs = Some(app.surfaces.surfaces.tabs().to_vec());
    let tab_history = Some(app.surfaces.surfaces.tab_mru_history().to_vec());
    let final_target = resolved_target
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Ok((
        TuiOutcome {
            history: app.composer.input_history,
            active_session_id,
            exit_tx,
            switch_to: app.surfaces.switch_to_target.take(),
            switch_to_prompt: app.surfaces.switch_to_prompt.take(),
            tabs,
            tab_history,
            relink_requested: app.link.relink_requested,
        },
        final_target,
    ))
}

/// What a TUI run produced: the input history to persist and the live active
/// thread id / outbound link for exit-time flush.
pub struct TuiOutcome {
    pub history: Vec<protocol::HistoryEntry>,
    /// Actual live thread id at exit (populated even when subscribed lazily
    /// after `ServerPhase::Initializing` or from a 0-thread Dashboard).
    pub active_session_id: String,
    /// Active outbound link at exit for flushing `RecordInputHistory`.
    pub exit_tx: Option<mpsc::UnboundedSender<client::ClientOutbound>>,
    pub switch_to: Option<String>,
    /// Opening prompt to dispatch after re-attaching to [`Self::switch_to`]
    /// (a retired `/btw <prompt>` fork steer). `None` for a plain switch.
    pub switch_to_prompt: Option<String>,
    pub tabs: Option<Vec<crate::shell::surfaces::ClientTab>>,
    pub tab_history: Option<Vec<crate::shell::surfaces::TabKind>>,
    /// The attachment ended because the link died, not because the operator
    /// asked to leave (ADR-0136).
    pub relink_requested: bool,
}

pub async fn start_tui(
    tx: mpsc::UnboundedSender<client::ClientOutbound>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    monitor_rx: mpsc::UnboundedReceiver<protocol::MonitorEvent>,
    link_lost: crate::shell::client::LinkLostMarker,
    config: TuiLaunchConfig,
) -> Result<TuiOutcome, Box<dyn Error>> {
    run_tui(tx, rx, monitor_rx, link_lost, config).await
}

/// Apply the visible transcript effect of a stream-start signal: settle any
/// in-flight sending prompt to delivered.
///
/// A stream start used to also evict the trailing provider-retry entry here.
/// That entry no longer exists — the live countdown is chrome, retired by the
/// phase write that streaming produces — so this function has exactly one job
/// and cannot leave a stale retry row behind.
pub(crate) fn begin_stream(messages: &mut [TranscriptMessage]) {
    if let Some(m) = messages
        .iter_mut()
        .rev()
        .find(|m| m.role == Role::User && m.delivery == DeliveryStatus::Sending)
    {
        m.delivery = DeliveryStatus::Delivered;
        m.rev += 1;
    }
}

/// Append a disclosed reasoning delta to the current turn's Thinking entry,
/// creating the entry only when the first disclosed delta arrives (that
/// structural path lives at the call site). Returning `Some(id)` permits the
/// cheap per-message patch path; `None` means the caller must create the
/// entry.
///
/// Identity-addressed: resolves the target by scanning backwards
/// for the Thinking entry matching `(round, turn)`, **not** by "is the last
/// message". Command entries (`/delegate`, shell passthrough) and local
/// notices can be appended between reasoning deltas — under `last_mut()`
/// addressing the next delta would fork the trace into a second Thinking
/// entry (the "two Thinking blocks" bug).
///
/// The append itself is [`TranscriptMessage::push_reasoning_delta`], so the
/// primary stream and the subagent folding path share one accumulation
/// mechanism: `content` and the rendered `blocks` advance together or not at
/// all.
pub(crate) fn append_reasoning_delta(
    messages: &mut [TranscriptMessage],
    round: Option<u64>,
    turn: Option<u64>,
    delta: &str,
) -> Option<u64> {
    let target = messages.iter_mut().rfind(|message| {
        message.is_reasoning() && message.round == round && message.turn == turn
    })?;
    target.push_reasoning_delta(delta);
    Some(target.id)
}

/// Append a streamed assistant-text delta to the current turn, creating the
/// message only when the first visible text arrives. Returning `None` means the
/// caller must perform the structural insertion (and request a full transcript
/// snapshot); returning an id permits the cheap per-message patch path.
///
/// Identity-addressed: the target assistant-text entry is resolved
/// by scanning backwards for the entry matching `(round, turn)`, **not** by
/// "is the last message". Command entries and local notices appended between
/// text deltas must not fork the stream into a second entry.
pub(crate) fn append_stream_text_delta(
    messages: &mut [TranscriptMessage],
    round: Option<u64>,
    turn: Option<u64>,
    delta: &str,
) -> Option<u64> {
    let message = messages.iter_mut().rfind(|message| {
        message.role == Role::Assistant
            && matches!(&message.kind, MessageKind::Text)
            && message.round == round
            && message.turn == turn
    })?;
    message.push_stream(delta);
    Some(message.id)
}

#[cfg(test)]
mod streaming_appends_tests {
    //! Identity-addressed streaming appends.
    use super::*;
    use view::model::document::MessageKind;

    fn reasoning_entry(round: u64, turn: u64, content: &str) -> TranscriptMessage {
        let mut m = TranscriptMessage::reasoning(content);
        m.round = Some(round);
        m.turn = Some(turn);
        m
    }

    #[test]
    fn reasoning_delta_appends_across_an_intervening_command_entry() {
        // Regression: dispatching `/delegate` mid-stream pushes a
        // CommandResult entry after the still-streaming Thinking entry. The
        // next reasoning delta must extend the *original* entry, not fork a
        // second Thinking block.
        let mut messages = vec![reasoning_entry(8, 1, "the error chain is")];
        messages.push(TranscriptMessage::pending_command("delegate", "on").with_sent_at_ms(1_000));

        let id = append_reasoning_delta(&mut messages, Some(8), Some(1), " now clear")
            .expect("must resolve the original thinking entry");
        assert_eq!(id, messages[0].id);
        // Still exactly one Thinking entry…
        assert_eq!(
            messages.iter().filter(|m| m.is_reasoning()).count(),
            1,
            "the delta must not fork a second Thinking entry"
        );
        // …and the delta landed inside it, in order.
        let MessageKind::Reasoning { content, .. } = &messages[0].kind else {
            panic!("entry 0 must remain a Thinking entry");
        };
        assert_eq!(content, "the error chain is now clear");
        // The command entry stays between the original position and the end,
        // untouched.
        assert!(messages[1].is_command_result());
    }

    #[test]
    fn reasoning_delta_finds_latest_entry_of_same_turn() {
        // Multiple thinking entries can share a position across retries; the
        // backward scan must hit the newest one.
        let mut messages = vec![
            reasoning_entry(2, 1, "first attempt"),
            reasoning_entry(2, 1, "second attempt"),
        ];
        let id = append_reasoning_delta(&mut messages, Some(2), Some(1), "…").unwrap();
        assert_eq!(id, messages[1].id);
        let MessageKind::Reasoning { content, .. } = &messages[1].kind else {
            panic!()
        };
        assert_eq!(content, "second attempt…");
        let MessageKind::Reasoning { content, .. } = &messages[0].kind else {
            panic!()
        };
        assert_eq!(content, "first attempt");
    }

    #[test]
    fn reasoning_delta_rejects_foreign_positions() {
        // A delta for another turn must not graft onto an older turn's entry.
        let mut messages = vec![reasoning_entry(8, 1, "old")];
        assert_eq!(
            append_reasoning_delta(&mut messages, Some(8), Some(2), "new"),
            None
        );
        assert_eq!(
            append_reasoning_delta(&mut messages, Some(9), Some(1), "new"),
            None
        );
    }

    #[test]
    fn text_delta_appends_across_an_intervening_command_entry() {
        use tool::Role;
        let mut text = TranscriptMessage::new(Role::Assistant, "hello ");
        text.round = Some(3);
        text.turn = Some(1);
        let mut messages = vec![text];
        messages.push(TranscriptMessage::pending_command("delegate", "on").with_sent_at_ms(1_000));

        let id = append_stream_text_delta(&mut messages, Some(3), Some(1), "world")
            .expect("must resolve the original text entry");
        assert_eq!(id, messages[0].id);
        assert!(messages[0].raw.contains("world"));
        assert_eq!(
            messages.iter().filter(|m| m.raw.contains("world")).count(),
            1,
            "the delta must not fork a second text entry"
        );
    }
}

/// Load the user-supplied ASCII logo from `$XDG_CONFIG_HOME/nuo/logo.txt`,
/// clamped to the empty-state bounding box. Best-effort: a missing or unreadable
/// file returns `None`, leaving the built-in wordmark in place.
fn load_user_logo() -> Option<Vec<String>> {
    let path = host::paths::get().logo_file();
    let content = std::fs::read_to_string(&path).ok()?;
    // Re-use the renderer's parser so the clamp stays defined in one place.
    // The parser already strips CRLF/trailing blanks and truncates to the box.
    render::parse_logo(&content)
}

#[cfg(test)]
pub(crate) mod tests;

/// Painted-grid baselines for the tool-step and transcript rendering geometry.
///
/// This module is the crate's visual regression net: it locks the *painted*
/// rows (cell symbols, background-color runs) of the disclosure cards and the
/// turn band at fixed terminal sizes. It is a sibling of [`tests`] rather than a
/// child because it owns insta baselines under `shell/snapshots/`, whose paths
/// are derived from this module's name.
#[cfg(test)]
mod snapshot_tests;
