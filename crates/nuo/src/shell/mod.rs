//! Terminal user interface, presentation substrate, and interactive rendering engine for Nuo.
//!
//! Subsystem architecture ratified under ADR-0003 (Nuotc Canvas Substrate),
//! ADR-0072 (Elm Architecture Declarative Component Paradigm), ADR-0073 (Storybook Protocol),
//! and ADR-0103 (Presentation Substrate Purification).
//!
//! Layer structure:
//! - The in-house grid engine (`nuotc`) — a retained cell grid with dirty tracking and a back/front diff.
//! - The view and component tree — painting `nuo_protocol` domain types into the engine grid:
//!   `model`, `render`, `components`, `overlays`, `tools`, `disclosure`, `layout`,
//!   `declarative` (ADR-0072), `theme`, `design`, `chrome`, `composer`, `primitives`.
//! - The interactive application runtime: application state ([`app`]), input mapping ([`input`]),
//!   and the event/render loop (`event_loop`). [`start_tui`] is the entry point wired by the `nuo` binary.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// Link the composition root in the test build so every `providers/*` crate's
// `inventory` baseline registration is present in the UI test binary: the
// registry-lookup tests (model search, vision gate) resolve baselines through
// it (`[INV-PROV-09]` linkage).
#[cfg(test)]
extern crate nuo_server;

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
pub mod disclosure;
pub mod event_loop;
pub mod focus;
pub mod headless;
pub mod input;
pub mod interaction;
pub mod keymap;
pub mod modal_keys;
pub mod mouse_mode;
pub mod overlays;
pub mod pre_attach;
pub mod question_model;
pub mod render;
pub mod runner;
pub mod session;
pub(crate) mod session_gate;
pub mod sheet;
pub mod step_interaction;
pub mod surfaces;
pub mod terminal;
pub mod transcript;
pub mod ui;
pub mod view_header;
pub mod views;

pub use app::{App, CaretOwner, ProviderDeleteChoice, SelectionEdge};
pub use completion::CompletionKind;
pub use focus::{Focus, FocusPlane, TranscriptFocus};
pub use nuo_ui_primitives::design::*;
pub use nuo_ui_primitives::{
    Theme, chord, design, elevation, fuzzy, key, keyvocab, primitives, syntax, theme, time,
};
pub use nuo_view::phase::Phase;
pub use nuo_view::providers::{CustomField, PROVIDER_PRESETS, provider_label_for};
pub use nuo_view::{
    disclosure_config, empty_state, footer_stack, layout, markdown_table, message_body, model,
    notice, phase, providers, text_layout, tools, trust_gate,
};
pub use pre_attach::{PreAttachDecision, PreAttachSignal, PreAttachState};

/// Test support: flatten a rendered grid into one string per row.
#[cfg(test)]
pub(crate) fn grid_rows(grid: &nuotc::CellGrid) -> Vec<String> {
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
    TelemetryState, TranscriptState,
};
use nuo_protocol::{AgentRequest, AgentResponse, LoopStatus, ProviderPickerSnapshot, RoundEvent};

use nuo_tool::{Message, Role};

use nuotc::{Backend, Terminal};
use std::{collections::HashMap, error::Error, io, sync::Arc, sync::atomic::AtomicBool};
use tokio::sync::{Mutex, mpsc};

use nuo_view::model::document::{DeliveryStatus, MessageKind, TranscriptMessage};
#[cfg(test)]
use nuo_view::model::document::{NoticeSeverity, UserMessageOrigin};
use nuo_view::model::selection::{SelectionDrag, SelectionState};
pub use transcript::{finalize_streaming_reasoning, rebase_transcript_rounds};
use transcript::{
    merge_command_rows, merge_round_interrupt_rows, transcript_commands_from_ledger,
    transcript_interrupts_from_records, transcript_messages_from_core,
};

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
                    .and_then(crate::shell::views::ConfigCategory::from_name)
                    .map(|c| c as usize);
                return Some(StartupOverlay::Settings { category: cat });
            }
        } else if let Some(nav) = nav_val {
            let cat = crate::shell::views::ConfigCategory::from_name(&nav).map(|c| c as usize);
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
                        event: nuo_tool::SubagentEvent::StreamDelta(_)
                            | nuo_tool::SubagentEvent::StreamReasoningDelta(_),
                        ..
                    }
            )
    )
}

pub struct TuiLaunchConfig {
    pub initial_provider: String,
    pub initial_model: String,
    pub input_history: Vec<nuo_protocol::HistoryEntry>,
    pub initial_messages: Vec<Message>,
    pub initial_commands: Vec<nuo_protocol::CommandRecord>,
    pub initial_round_count: u64,
    pub command_catalog: nuo_tool::CommandCatalog,
    pub initial_round_interrupts: Vec<nuo_protocol::RoundInterrupt>,
    pub tui_config: config::TuiConfig,
    pub input_history_config: config::InputHistoryConfig,
    pub session: HookStartSource,
    pub token_ledger: Option<Arc<nuo_protocol::TokenSourceLedger>>,
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
    pub initial_phase: nuo_protocol::ThreadPhase,
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
    tx: mpsc::UnboundedSender<AgentRequest>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    link_lost: crate::shell::client::LinkLostMarker,
    config: TuiLaunchConfig,
) -> Result<TuiOutcome, Box<dyn Error>> {
    let profile = terminal::detect_terminal_profile();
    terminal::enter_terminal(&profile)?;
    let stdout = io::stdout();
    let backend = Backend::new(stdout).with_profile(profile);
    let mut terminal = Terminal::new(backend);
    terminal::spawn_signal_guard();
    ensure_panic_hook();
    let outcome = run_tui_with_terminal(&mut terminal, tx, rx, link_lost, config).await;
    terminal::restore_terminal();
    outcome
}

pub async fn run_tui_with_terminal(
    terminal: &mut Terminal<io::Stdout>,
    tx: mpsc::UnboundedSender<AgentRequest>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    link_lost: crate::shell::client::LinkLostMarker,
    config: TuiLaunchConfig,
) -> Result<TuiOutcome, Box<dyn Error>> {
    let mut input = event_loop::input_reader::InputSource::spawn()?;
    run_tui_with_input(terminal, tx, rx, link_lost, config, &mut input).await
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

fn pending_prompts(initial_prompt: Option<String>) -> Vec<nuo_protocol::AgentRequest> {
    initial_prompt
        .map(|text| {
            vec![nuo_protocol::AgentRequest::Prompt {
                text,
                images: Vec::new(),
                sent_at_ms: None,
            }]
        })
        .unwrap_or_default()
}

fn restore_transcript(
    initial_messages: Vec<Message>,
    initial_commands: Vec<nuo_protocol::CommandRecord>,
    initial_round_interrupts: Vec<nuo_protocol::RoundInterrupt>,
    initial_round_count: u64,
    tui_config: &config::TuiConfig,
) -> Vec<nuo_view::model::document::TranscriptMessage> {
    let mut restored = transcript_messages_from_core(initial_messages, tui_config);
    restored = merge_command_rows(restored, transcript_commands_from_ledger(initial_commands));
    restored = merge_round_interrupt_rows(
        restored,
        transcript_interrupts_from_records(initial_round_interrupts),
    );
    rebase_transcript_rounds(&mut restored, initial_round_count);
    restored
}

async fn start_ui_runtime(
    tui_config: config::TuiConfig,
    session: &crate::shell::HookStartSource,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    link_lost: crate::shell::client::LinkLostMarker,
    initial_provider: &str,
    initial_model: &str,
    initial_round_count: u64,
) -> (
    Arc<config::TuiConfig>,
    event_loop::UiRuntime,
    String,
    Arc<AtomicBool>,
    tokio::sync::mpsc::Receiver<event_loop::AppMutation>,
) {
    let tui_config = Arc::new(tui_config);
    // M1: the response translator and the monitor client own **no**
    // application state. They consume wire frames and produce typed
    // `AppMutation`s onto a bounded channel; the event loop — the sole
    // `App` writer — drains and applies them each iteration (see
    // `event_loop::apply`). There are no shared state cells and no
    // per-frame mirroring; the few cross-task facts that flow the other
    // way live on `UiRuntime` (see its module docs).
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

    event_loop::translators::spawn_monitor_translator(&runtime);
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
struct AppBoot<'a> {
    live_session_init: &'a String,
    initial_provider: String,
    initial_model: String,
    session_phase_init: nuo_protocol::ThreadPhase,
    restored: Vec<TranscriptMessage>,
    tui_config: Arc<config::TuiConfig>,
    command_catalog: nuo_tool::CommandCatalog,
    initial_pending_prompts: Vec<nuo_protocol::AgentRequest>,
    input_history: Vec<nuo_protocol::HistoryEntry>,
    input_history_config: config::InputHistoryConfig,
    startup_overlay: StartupOverlay,
    profile: nuotc::TerminalProfile,
    token_ledger: Option<Arc<nuo_protocol::TokenSourceLedger>>,
    tx: mpsc::UnboundedSender<AgentRequest>,
    should_quit: Arc<AtomicBool>,
}

/// Build the initial [`App`] from a captured [`AppBoot`].
///
/// The literal is kept whole (rather than spread across per-sub-state
/// constructors) so the initial values and their order stay auditable in one
/// place; [`run_tui_with_input`] remains startup/teardown orchestration.
fn build_app(boot: AppBoot<'_>) -> App {
    App {
        session: SessionState {
            queue_exit_session: None,
            session_chrome: std::collections::HashMap::new(),
            session_tree: nuo_protocol::ThreadTree::default(),
            live_session_id: boot.live_session_init.clone(),
            workspace_security: nuo_protocol::WorkspaceSecuritySnapshot::default(),
            tui_config: (*boot.tui_config).clone(),
            session_detail: None,
            current_provider: boot.initial_provider,
            current_model: boot.initial_model,
            cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            current_session_id: "test-session".to_string(),
            session_phase: boot.session_phase_init.clone(),
            switching_session: (boot.session_phase_init != nuo_protocol::ThreadPhase::Ready)
                .then(|| boot.live_session_init.clone()),
            current_workspace: String::new(),
            current_role: None,
            session_context: None,
            loop_status: LoopStatus::Idle,
            harness_retry_pending: false,
            phase: None,
            provider_retry: None,
            persistence_health: None,
            unattended: false,
            confined: true,
            round_count: 0,
            current_turn: 0,
            round_started_at: None,
            sessions_overview: Vec::new(),
            host_sessions: Vec::new(),
            queue_blocked_sessions: std::collections::HashSet::new(),
            running_sessions: std::collections::HashSet::new(),
        },
        transcript: TranscriptState {
            command_log: None,
            logs: nuo_view::model::log::SurfaceLogs::new(),
            messages: boot.restored,
            layout_height_cache: Default::default(),
            scroll: 0,
            follow_bottom: true,
            content_lines: 0,
            view_height: 0,
            max_scroll: 0,
            sticky_step: None,
            modal_body_height: 0,
            sticky_summary_line: None,
            pin_summary_line: None,
            scroll_settle_pending: false,
            focus_stack: Vec::new(),
            transcript_changed_pending: false,
            reasoning_default_expanded: crate::shell::config::reasoning_default_expanded(
                &boot.tui_config,
            ),
            host_console_log: Vec::new(),
            selection: SelectionState::None,
            drag: SelectionDrag::default(),
            ui: crate::shell::ui::ComponentTree::new(),
            hovered_step: None,
            focus: Focus::composer(),
            transcript_layout: crate::shell::render::layout::Strategy::TurnBand,
            expand_auto_scroll: boot.tui_config.expand_auto_scroll,
            spinner_epoch: std::time::Instant::now(),
            carousel_epoch: std::time::Instant::now(),
        },
        composer: ComposerState {
            last_submit_ms: None,
            recent_commands: Vec::new(),
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
            draft_capsule: nuo_view::composer_mode::DraftCapsule::default(),
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
            injection_stashed_input: String::new(),
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
            surface_store: crate::shell::surfaces::SurfaceStore::new(),
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
                } => cat.min(
                    crate::shell::views::ConfigCategory::ALL
                        .len()
                        .saturating_sub(1),
                ),
                _ => 0,
            },
            config_detail_index: 0,
            config_hover_index: None,
            config_detail_scroll: 0,
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
            switch_to_new: false,
            switch_to_new_prompt: None,
            switch_to_prompt: None,
            startup_overlay: boot.startup_overlay,
            color_scheme: Theme::normalize_color_scheme(&boot.tui_config.color_scheme).to_string(),
            custom_color_scheme: boot.tui_config.custom_color_scheme.clone(),
            click_outside_dismiss: boot.tui_config.click_outside_dismiss,
            key_overrides: boot.tui_config.global_key_overrides(),
            surface_overrides: boot.tui_config.surface_key_overrides(),
            copy_toast_until: None,
            copy_toast_message: String::new(),
            copy_toast_failed: false,
            notice_toast_until: None,
            notice_toast_message: String::new(),
            notice_toast_topic: String::new(),
            notice_toast_severity: nuo_view::model::document::NoticeSeverity::Info,
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
            custom_budget: nuo_protocol::ConnectionBudget::default(),
            custom_models: Vec::new(),
            custom_url_hint: String::new(),
            custom_user_agent: None,
            custom_auth: nuo_protocol::ConnectionAuth::Static,
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
            token_report: None,
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
            background_tasks: Vec::new(),
        },
    }
}

pub(crate) async fn run_tui_with_input(
    terminal: &mut Terminal<io::Stdout>,
    tx: mpsc::UnboundedSender<AgentRequest>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
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

    // Mount tabs: restore preserved tabs if re-attaching, or mount initial thread tab (ADR-0039, ADR-0058)
    if let Some(tabs) = initial_tabs {
        for tab in tabs {
            app.surfaces.surfaces.open_tab(tab);
        }
        if let Some(history) = initial_tab_history {
            app.surfaces.surfaces.set_tab_mru_history(history);
        }
        if let Some(pos) = app
            .surfaces
            .surfaces
            .tabs()
            .iter()
            .position(|t| t.thread_id() == Some(live_session_init.as_str()))
        {
            app.surfaces.surfaces.select_tab(pos);
            app.restore_active_tab_context();
        } else {
            app.surfaces
                .surfaces
                .open_tab(crate::shell::surfaces::ClientTab::thread(
                    &live_session_init,
                    None::<String>,
                ));
        }
    } else {
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                &live_session_init,
                None::<String>,
            ));
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

    // Negotiate progressive keyboard enhancement *off* the first-frame path.
    //
    // The probe writes `ESC[?u ESC[c` and waits for the terminal to answer,
    // bounded by a 2 s timeout inside crossterm. A terminal that never answers
    // (multiplexers, CI ptys, emulators without kitty-protocol support) used to
    // stall the very first frame for that entire budget — measured at ~2057 ms
    // versus ~53 ms when the terminal replies. Running it here, after a short
    // delay that lets the UI paint, makes a silent terminal cost the operator
    // nothing; the flags then land a few hundred ms into the session.
    //
    // Safe against the input reader: crossterm's internal reader does not drop
    // events that fail this thread's filter — it parks them in `skipped_events`
    // and re-offers them to the next reader (crossterm `event/read.rs`), so no
    // keystroke is swallowed. Only the two protocol replies are consumed.
    std::thread::Builder::new()
        .name("nuo-kbd-enhance".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            crate::shell::terminal::negotiate_keyboard_enhancement(&profile);
        })?;

    // Run app
    let res =
        event_loop::run_app_loop(terminal, &mut app, runtime, mutation_rx, session, input).await;

    if let Err(err) = res {
        return Err(err.into());
    }

    let switch = app.surfaces.switch_to_target.take();
    let switch_to_new = app.surfaces.switch_to_new;
    let switch_to_new_prompt = app.surfaces.switch_to_new_prompt.take();
    let switch_to_prompt = app.surfaces.switch_to_prompt.take();
    let tabs = Some(app.surfaces.surfaces.tabs().to_vec());
    let tab_history = Some(app.surfaces.surfaces.tab_mru_history().to_vec());
    Ok(TuiOutcome {
        history: app.composer.input_history,
        switch_to: switch,
        switch_to_new,
        switch_to_new_prompt,
        switch_to_prompt,
        tabs,
        tab_history,
        relink_requested: app.link.relink_requested,
    })
}

/// What a TUI run produced: the input history to persist, and —
/// when the user picked a session in the `/host` panel — the server session
/// to switch to (the caller re-attaches).
pub struct TuiOutcome {
    pub history: Vec<nuo_protocol::HistoryEntry>,
    pub switch_to: Option<String>,
    pub switch_to_new: bool,
    pub switch_to_new_prompt: Option<String>,
    /// Opening prompt to dispatch after re-attaching to [`Self::switch_to`]
    /// (a retired `/btw <prompt>` fork steer). `None` for a plain switch.
    pub switch_to_prompt: Option<String>,
    pub tabs: Option<Vec<crate::shell::surfaces::ClientTab>>,
    pub tab_history: Option<Vec<crate::shell::surfaces::TabKind>>,
    /// The attachment ended because the link died, not because the operator
    /// asked to leave (ADR-0136). The runner re-dials and re-attaches, so a
    /// transient socket failure does not end the session.
    pub relink_requested: bool,
}

pub async fn start_tui(
    tx: mpsc::UnboundedSender<AgentRequest>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    link_lost: crate::shell::client::LinkLostMarker,
    config: TuiLaunchConfig,
) -> Result<TuiOutcome, Box<dyn Error>> {
    run_tui(tx, rx, link_lost, config).await
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
    use nuo_view::model::document::MessageKind;

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
        use nuo_tool::Role;
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
    let path = nuo_host::paths::get().logo_file();
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
