//! Terminal UI frontend, in three layers:
//!
//! - the in-house grid engine (`nuotc`) — a retained cell grid
//!   with dirty tracking and a back/front diff; ADR-0038 — plus the crossterm
//!   backend.
//! - the view modules under this one — the drawing tree + semantic document
//!   model, painting `nuo_wire` domain types into the engine grid:
//!   `model` (document, layout map for hit-testing, selection state),
//!   `view` (the transcript-area renderer the shell drives each frame), the
//!   drawing sub-trees (`components` / `overlays` / `tools` /
//!   `disclosure`), layout strategies (`layout`), and drawing leaves /
//!   shared tokens (`theme`, `design`, `chrome`, `composer`,
//!   `primitives`, …). The view modules are crate-private: the public
//!   surface is the shell entry points, not the widget tree.
//! - the app shell (this module's remaining submodules): application state
//!   ([`app`]), input mapping ([`input`]), and the event/render loop
//!   (`event_loop`). [`start_tui`] is the entry point wired by the
//!   `nuo` binary, which stays a thin shell over this crate.
//!
//! The seam between shell and view is the borrowed `render::TranscriptProps`
//! the event loop fills in each frame; the view modules never reach back into
//! the shell.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod app;
pub mod browser;
pub mod cli;
pub mod clipboard;
pub mod clipboard_ops;
pub mod completion;
pub mod composer_attachments;
pub mod composer_extension;
pub mod config;
mod event_loop;
pub mod headless;
pub mod input;
pub mod interaction;
pub mod keymap;
pub mod phase;
mod pre_attach;
pub mod question_model;
pub mod runner;
pub mod ui;
pub(crate) use pre_attach::{PreAttachDecision, PreAttachSignal, PreAttachState};
mod step_interaction;
pub mod syntax;
mod terminal;
mod transcript;
pub mod trust_gate;

// View layer (merged from the former `nuo-tui` crate)

// Semantic data model.
pub(crate) mod model;

// The Conversation scene's self-owned keyboard scheme (ADR-0172/ADR-0205).
pub(crate) mod session;
// Per-modal keybinding schemes (ADR-0172).
pub(crate) mod modal_keys;
pub(crate) mod sheet;

// Drawing tree.
pub(crate) mod components;
pub(crate) mod disclosure;
pub(crate) mod layout;
pub(crate) mod overlays;
pub(crate) mod tools;
pub mod views;

// Drawing leaves + shared tokens.
pub(crate) mod chrome;
pub(crate) mod composer;
pub(crate) mod design;
pub(crate) mod elevation;
pub(crate) mod empty_state;
pub(crate) mod footer_stack;
pub(crate) mod markdown_table;
pub(crate) mod message_body;
pub(crate) mod notice;
pub(crate) mod primitives;
pub(crate) mod view_header;

pub(crate) mod text_layout;
pub(crate) mod theme;
pub(crate) mod time;

// Transcript-area renderer (the entry point the shell drives each frame).
pub(crate) mod render;
// Re-export the transcript renderer's surface at the `tui` root: the drawing
// leaves used to reach these via the view crate's root namespace (its lib.rs
// glob), so this module now stands in as that parent.
pub(crate) use render::*;

// Misc helpers shared with the shell.
pub(crate) mod fuzzy;
pub(crate) use crate::phase::Phase;
pub(crate) mod providers;
pub(crate) mod surfaces;

#[cfg(test)]
mod snapshot_tests;

pub(crate) use app::{App, CaretOwner, ProviderDeleteChoice, SelectionEdge};
pub(crate) use completion::CompletionKind;
pub(crate) use providers::{CustomField, PROVIDER_PRESETS, provider_label_for};

use nuo_wire::{
    AgentRequest, AgentResponse, LoopStatus, Message, ParentStatus, ProviderPickerSnapshot, Role,
    RoundEvent,
};
use nuotc::{Backend, Terminal};
use std::{
    collections::HashMap,
    error::Error,
    io,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, mpsc};

use crate::model::document::{
    DeliveryStatus, MessageKind, NoticeSeverity, TranscriptMessage, UserMessageOrigin,
    notice_severity_from_core,
};
use crate::model::selection::{SelectionDrag, SelectionState};
use crate::render::Theme;
use crate::transcript::{
    finalize_streaming_reasoning, merge_command_rows, merge_round_interrupt_rows,
    rebase_transcript_rounds, transcript_commands_from_ledger, transcript_interrupts_from_records,
    transcript_messages_from_core, transcript_retry_resolutions_from_records,
};

/// Where the session this TUI drives lives. All sessions in the unified
/// server model are remote (server-hosted).
#[derive(Debug, Clone)]
pub enum SessionSource {
    Remote {
        /// The hosted session's id, learned from the WS handshake.
        session_id: String,
    },
}

impl SessionSource {
    /// The primary session id.
    pub(crate) async fn session_id(&self) -> String {
        match self {
            SessionSource::Remote { session_id } => session_id.clone(),
        }
    }
}

/// Which full-screen overlay (if any) the TUI opens straight into at startup
/// instead of a conversation view. In that mode the overlay is not a transient
/// modal — there is no conversation the user asked for behind it — so closing
/// it quits the program rather than dropping into an empty chat (mirrors how
/// `nuo attach`'s picker behaves). Distinct from `None`, where the TUI
/// opens directly onto a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupOverlay {
    /// Ordinary startup: land on the conversation.
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
                    .and_then(crate::views::ConfigCategory::from_name)
                    .map(|c| c as usize);
                return Some(StartupOverlay::Settings { category: cat });
            }
        } else if let Some(nav) = nav_val {
            let cat = crate::views::ConfigCategory::from_name(&nav).map(|c| c as usize);
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
/// ADR-0175 §6.
fn init_dev_toast() -> (
    Option<std::time::Instant>,
    String,
    bool,
    Option<std::time::Instant>,
    String,
    NoticeSeverity,
    bool,
) {
    let raw = std::env::var("NUO_DEV_TOAST")
        .or_else(|_| std::env::var("NUO_TOAST"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let Some(val) = raw else {
        return (
            None,
            String::new(),
            false,
            None,
            String::new(),
            NoticeSeverity::Info,
            false,
        );
    };

    let pinned = std::env::var("NUO_DEV_TOAST_PINNED")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);

    let duration = std::time::Duration::from_secs(3600);
    let until = Some(std::time::Instant::now() + duration);

    if val == "1" || val.eq_ignore_ascii_case("demo") {
        let msg = "Workspace roots updated\nSkipped roots: `../opencode`".to_string();
        return (
            None,
            String::new(),
            false,
            until,
            msg,
            NoticeSeverity::Warning,
            pinned,
        );
    }

    if let Some((kind, rest)) = val.split_once(':') {
        let trimmed_rest = rest.trim().to_string();
        match kind.trim().to_ascii_lowercase().as_str() {
            "ok" | "copy" | "success" => {
                return (
                    until,
                    trimmed_rest,
                    false,
                    None,
                    String::new(),
                    NoticeSeverity::Info,
                    pinned,
                );
            }
            "fail" | "failed" | "error" | "err" => {
                return (
                    until,
                    trimmed_rest,
                    true,
                    None,
                    String::new(),
                    NoticeSeverity::Error,
                    pinned,
                );
            }
            "warn" | "warning" | "armed" => {
                return (
                    None,
                    String::new(),
                    false,
                    until,
                    trimmed_rest,
                    NoticeSeverity::Warning,
                    pinned,
                );
            }
            "info" => {
                return (
                    None,
                    String::new(),
                    false,
                    until,
                    trimmed_rest,
                    NoticeSeverity::Info,
                    pinned,
                );
            }
            _ => {}
        }
    }

    // Default without prefix: show as Info notice toast
    (
        None,
        String::new(),
        false,
        until,
        val,
        NoticeSeverity::Info,
        pinned,
    )
}

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
fn is_coalescible_stream_update(response: &AgentResponse) -> bool {
    matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::StreamDelta(_)
                | RoundEvent::StreamReasoningDelta(_)
                | RoundEvent::ToolStream { .. }
                | RoundEvent::ToolInputProgress { .. }
                | RoundEvent::SubagentStep {
                    event: nuo_wire::SubagentEvent::StreamDelta(_)
                        | nuo_wire::SubagentEvent::StreamReasoningDelta(_),
                    ..
                },
            ..
        }
    )
}

pub struct TuiLaunchConfig {
    pub initial_provider: String,
    pub initial_model: String,
    pub input_history: Vec<nuo_wire::HistoryEntry>,
    pub initial_messages: Vec<Message>,
    pub initial_commands: Vec<nuo_wire::CommandRecord>,
    pub initial_round_count: u64,
    pub command_catalog: nuo_wire::CommandCatalog,
    pub initial_round_interrupts: Vec<nuo_wire::RoundInterrupt>,
    pub initial_retry_resolutions: Vec<nuo_wire::RetryResolution>,
    pub tui_config: config::TuiConfig,
    pub input_history_config: config::InputHistoryConfig,
    pub session: SessionSource,
    pub token_ledger: Option<Arc<nuo_wire::TokenSourceLedger>>,
    pub startup_overlay: StartupOverlay,
}

pub async fn run_tui(
    tx: mpsc::UnboundedSender<AgentRequest>,
    mut rx: mpsc::UnboundedReceiver<AgentResponse>,
    config: TuiLaunchConfig,
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
        initial_retry_resolutions,
        tui_config,
        input_history_config,
        session,
        token_ledger,
        startup_overlay,
    } = config;
    // Setup terminal with adaptive capability profile (ADR-0180)
    let profile = nuotc::TerminalProfile::detect();
    terminal::enter_terminal(&profile)?;
    let stdout = io::stdout();
    // The nuotc engine owns its grid + diff + crossterm I/O directly. No
    // ratatui, no WideHealBackend wrapper — the engine's retained grid writes
    // wide-glyph trailing cells with the glyph's own background at write time,
    // so ghost cells cannot occur regardless of terminal or multiplexer
    // (ADR-0038).
    let backend = Backend::with_profile(stdout, profile);
    let mut terminal = Terminal::new(backend);
    // Install the signal guard after the terminal enters raw mode + alt screen
    // so any later SIGTERM/SIGINT/SIGHUP restores it instead of stranding it.
    terminal::spawn_signal_guard();
    // Panic hook: a panic anywhere on the main thread unwinds the process
    // without running run_tui's cleanup (raw mode + alt screen + mouse
    // capture stay enabled, leaving the host terminal scrambled). The signal
    // guard covers SIGINT/SIGTERM/SIGHUP/SIGQUIT but not panics; this closes
    // that gap. Installed once per process — the /host re-attach loop calls
    // run_tui repeatedly and must not chain hooks. Background tasks (the
    // response listener, ws pumps) panic without unwinding the terminal:
    // only the thread that owns the terminal restores it.
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
    let tui_config = Arc::new(tui_config);
    let mut restored = transcript_messages_from_core(initial_messages, &tui_config);
    restored = merge_command_rows(restored, transcript_commands_from_ledger(initial_commands));
    restored = merge_round_interrupt_rows(
        restored,
        transcript_interrupts_from_records(initial_round_interrupts),
    );
    restored = merge_round_interrupt_rows(
        restored,
        transcript_retry_resolutions_from_records(initial_retry_resolutions),
    );
    rebase_transcript_rounds(&mut restored, initial_round_count);
    // ADR-0197 M1: the response translator and the monitor client own **no**
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
    let dirty_clone = dirty.clone();
    let dirty_notify = Arc::new(tokio::sync::Notify::new());
    let dirty_notify_clone = dirty_notify.clone();
    let should_quit = Arc::new(AtomicBool::new(false));

    // Loop → translator coordination facts (see `event_loop::runtime`).
    let is_responding = Arc::new(AtomicBool::new(false));
    let ir_clone = is_responding.clone();
    let trust_gate_dismissed = Arc::new(AtomicBool::new(false));
    let trust_gate_dismissed_clone = trust_gate_dismissed.clone();
    let awaiting_oauth_add = Arc::new(AtomicBool::new(false));
    let awaiting_oauth_add_clone = awaiting_oauth_add.clone();
    let viewed_session_id = Arc::new(Mutex::new(None::<String>));
    let viewed_session_id_clone = viewed_session_id.clone();
    // TUI display config the translators need for transcript projection
    // (reasoning disclosure at message creation; per-step-kind defaults).
    // Process config, not application state.
    let tui_config_clone = tui_config.clone();
    // The live primary session id at attach time: seeds both the `App` field
    // and the translator's local mirror.
    let live_session_init = session.session_id().await;
    let live_session_for_translator = live_session_init.clone();
    let initial_provider_for_translator = initial_provider.clone();
    let initial_model_for_translator = initial_model.clone();

    // Spawn the server monitor client (ADR-0096): a *translator* over the
    // monitor stream. It owns the session-row snapshot (its own domain) and
    // publishes whole snapshots to `App` via mutations — no cells, no
    // rev counters.
    {
        let mutations = mutations.clone();
        let dirty = dirty_clone.clone();
        let dirty_notify = dirty_notify_clone.clone();
        tokio::spawn(async move {
            let project_root =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let Some(info) = nuo_client::discover(&project_root) else {
                return;
            };
            let action = nuo_wire::MonitorAction {
                watch: true,
                include_idle: true,
            };
            let Ok(mut rx) = nuo_client::monitor_stream(&info, action).await else {
                return;
            };
            let mut rows: Vec<nuo_wire::MonitoredSession> = Vec::new();
            while let Some(event) = rx.recv().await {
                match event {
                    nuo_wire::MonitorEvent::Snapshot(snap) => {
                        rows = snap.sessions;
                        mutations
                            .send(event_loop::AppMutation::PersistenceHealth(
                                snap.persistence_health,
                            ))
                            .await;
                    }
                    nuo_wire::MonitorEvent::SessionAdded(row)
                    | nuo_wire::MonitorEvent::SessionUpdated(row) => {
                        nuo_client::upsert_session_row(&mut rows, row);
                    }
                    nuo_wire::MonitorEvent::SessionRemoved { session_id } => {
                        rows.retain(|r| r.id != session_id);
                    }
                    // Server-level task diffs (ADR-0190): the dashboard's
                    // task section is folded in a later phase.
                    nuo_wire::MonitorEvent::TaskUpdated(_) => {}
                    nuo_wire::MonitorEvent::TaskRemoved { .. } => {}
                    // Durability-health transitions (ADR-0196 D4): a degraded
                    // state retains the visible banner; Healthy clears it.
                    nuo_wire::MonitorEvent::PersistenceHealth(health) => {
                        mutations
                            .send(event_loop::AppMutation::PersistenceHealth(
                                (!health.is_healthy()).then_some(health),
                            ))
                            .await;
                    }
                    // The server began its graceful shutdown (ADR-0101): the
                    // stream closes right after; the next server interaction
                    // re-discovers or re-spawns.
                    nuo_wire::MonitorEvent::ServerDraining => {}
                }
                mutations
                    .send(event_loop::AppMutation::HostSessions(rows.clone()))
                    .await;
                dirty.store(true, Ordering::SeqCst);
                dirty_notify.notify_one();
            }
        });
    }

    // Spawn the response listener: a **translator** (ADR-0197 M1). It owns
    // only its own routing/bookkeeping locals and mirrors of the values it
    // itself receives; every application-state change is a mutation.
    {
        let mutations = mutations.clone();
        tokio::spawn(async move {
            // Listener-local side routing keys (ADR-0017, widened by ADR-0103):
            // every live aside's `session_id`, learned from `SideViewOpened` and
            // `BtwList`. A *set* (not a single id) so a background aside keeps
            // streaming into the side buffer after the user detaches from its
            // view.
            let mut listener_side_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            // Per-session `(round, turn)` position. The primary and `/btw` side
            // sessions can stream concurrently, so a single global counter
            // cannot reliably stamp transcript components for semantic spacing.
            let mut positions_by_session = HashMap::<String, (u64, u64)>::new();
            // ADR-0026: per-session announced-but-unclaimed provider slots, in
            // announcement order. The translator stamps the step's `input_slot`;
            // the applier resolves the collapse and the progress ticks by that
            // slot. A whole-argument provider never announces, so the queue
            // stays empty and every `ToolCall` inserts normally.
            let mut announced_slots_by_session =
                HashMap::<String, std::collections::VecDeque<usize>>::new();
            // A session switch replaces the transcript before its authoritative
            // idle HarnessState arrives. Rebase the reconstructed tail exactly
            // once when that snapshot supplies the persisted round counter.
            let mut needs_round_rebase = false;
            // Translator-owned mirrors of the values it itself receives (sent to
            // `App` via mutations): attribution, the provider-picker snapshot
            // (effort derivation), and the harness snapshot (idle gating). No
            // shared cells are read for these.
            let mut current_provider = initial_provider_for_translator.clone();
            let mut current_model = initial_model_for_translator.clone();
            let mut picker = ProviderPickerSnapshot::default();
            let mut harness = nuo_wire::HarnessSnapshot {
                loop_status: LoopStatus::Idle,
                round_counter: initial_round_count,
                unattended: false,
                confined: true,
                workspace_security: nuo_wire::WorkspaceSecuritySnapshot::default(),
                retry_pending: false,
                role: None,
                workspace: None,
            };
            // How many provider attempts the round currently in flight has
            // spent. It exists for exactly one purpose: the terminal `Error`
            // arm phrases "Exhausted {n} retry attempts — …" from it, because
            // the wire error carries no retry count.
            //
            // It is a **counter, not state**: it holds no setback, no timer and
            // no failure text, so it is not a second copy of the clause the
            // activity bar renders — that clause is owned by the session's
            // phase and retired with it (ADR-0235), and nothing here sends a
            // clause write at all. The `retry_attempts = 0` writes below exist
            // only so a later error cannot be blamed for an earlier round's
            // attempts.
            let mut retry_attempts: usize = 0;
            let mut reasoning_start: Option<std::time::Instant> = None;
            // The live primary session id: the translator updates it on
            // `/new` / `/session open` / `/resume` / `/fork` and scopes
            // session-addressed replies (the tree snapshot) against it.
            let mut live_session = live_session_for_translator.clone();

            // Attribution + effort derivation from translator-owned mirrors
            // (formerly `event_loop::attribution` / `picker_effort` over
            // shared cells).
            macro_rules! attribution {
                () => {
                    (current_provider.clone(), current_model.clone())
                };
            }
            macro_rules! picker_effort {
                () => {
                    picker
                        .rows
                        .iter()
                        .find(|row| row.id == current_provider)
                        .and_then(|row| row.model_info.iter().find(|m| m.model == current_model))
                        .and_then(|m| {
                            let show = match m.protocol.as_str() {
                                "anthropic" => m.thinking == Some(true),
                                _ => m.effort.is_some(),
                            };
                            show.then(|| m.effort.clone()).flatten()
                        })
                };
            }
            macro_rules! now_ms {
                () => {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0)
                };
            }

            while let Some(resp) = rx.recv().await {
                // Any handled response can change state the loop renders from,
                // so signal a redraw. High-frequency stream deltas deliberately
                // do not wake the loop one-by-one: while responding, its 10fps
                // heartbeat coalesces them into a smooth stream.
                dirty_clone.store(true, Ordering::Release);
                // A side conversation can receive stream deltas while the
                // primary activity indicator is idle — no heartbeat then, so
                // retain the immediate wake.
                let defer_stream_wakeup =
                    is_coalescible_stream_update(&resp) && ir_clone.load(Ordering::SeqCst);
                if !defer_stream_wakeup {
                    dirty_notify_clone.notify_one();
                }
                use event_loop::mutations::AppMutation as M;
                use event_loop::mutations::TranscriptEdit as E;
                match resp {
                    // ADR-0017 + ADR-0103: per-turn events arrive tagged with the
                    // session they belong to. The translator routes each event to
                    // the side buffer when its `session_id` belongs to a live
                    // aside — *whether or not that aside is the focused view* —
                    // and to the primary transcript otherwise. Permission and
                    // user-question requests stay global so their modals surface
                    // regardless of which view is focused.
                    AgentResponse::Round { session_id, event } => {
                        let routes_to_side = listener_side_ids.contains(session_id.as_str());
                        let buffer = if routes_to_side {
                            event_loop::mutations::Buffer::Side
                        } else {
                            event_loop::mutations::Buffer::Primary
                        };
                        macro_rules! transcript {
                            ($edit:expr) => {
                                mutations
                                    .send(M::Transcript {
                                        buffer,
                                        edit: $edit,
                                    })
                                    .await;
                            };
                        }
                        macro_rules! chrome {
                            ($edit:expr) => {
                                mutations
                                    .send(M::ChromeEdit {
                                        session_id: session_id.clone(),
                                        edit: $edit,
                                    })
                                    .await;
                            };
                        }
                        match event {
                            RoundEvent::ContextTokens(snapshot) => {
                                mutations
                                    .send(M::ContextTokens {
                                        session_id: session_id.clone(),
                                        snapshot,
                                    })
                                    .await;
                            }
                            RoundEvent::TurnPerformance(performance) => {
                                chrome!(event_loop::mutations::ChromeEdit::TurnPerformance(
                                    Box::new(performance),
                                ));
                            }
                            RoundEvent::SteerUnavailable { input_id } => {
                                // ADR-0212: Steer is ephemeral to the active round. It MUST NOT silently mutate
                                // into a next-round follow-up item. Remove the optimistic entry from transcript
                                // and restore the text directly to the composer draft.
                                mutations
                                    .send(M::SteerMissed {
                                        session_id,
                                        input_id,
                                    })
                                    .await;
                            }
                            RoundEvent::SteerAdmitted(input) => {
                                let input_id = input.id.clone();
                                let visible = input
                                    .display_text
                                    .clone()
                                    .unwrap_or_else(|| input.text.clone());
                                let mut fallback = TranscriptMessage::new(Role::User, visible);
                                fallback.insert_id = Some(input_id.clone());
                                fallback.sent_at_ms = input.sent_at_ms;
                                fallback.origin = UserMessageOrigin::Steer;
                                transcript!(E::SettleInserted {
                                    insert_id: input_id.clone(),
                                    origin: UserMessageOrigin::Steer,
                                    sent_at_ms: input.sent_at_ms,
                                    fallback: Some(fallback),
                                });
                                mutations
                                    .send(M::DispatchRemoved {
                                        session_id,
                                        input_id,
                                    })
                                    .await;
                            }
                            RoundEvent::SteerCancelled { .. } => {}
                            RoundEvent::SteerCancelFailed { .. } => {}
                            RoundEvent::FollowUpQueued { input_id } => {
                                // The server admitted the follow-up into its
                                // queue: the optimistic entry settles back to
                                // Waiting (the queue bar keeps showing it).
                                mutations
                                    .send(M::DispatchQueued {
                                        session_id: session_id.clone(),
                                        input_id,
                                    })
                                    .await;
                            }
                            RoundEvent::QueueUpdated { items, paused } => {
                                // The authoritative queue snapshot (ADR-0197
                                // M4): full-replace projection.
                                mutations
                                    .send(M::QueueSnapshot {
                                        session_id: session_id.clone(),
                                        items,
                                        paused,
                                    })
                                    .await;
                            }
                            RoundEvent::FollowUpStarted(input) => {
                                let input_id = input.id.clone();
                                let visible = input
                                    .display_text
                                    .clone()
                                    .unwrap_or_else(|| input.text.clone());
                                let mut fallback = TranscriptMessage::new(Role::User, visible);
                                fallback.insert_id = Some(input_id.clone());
                                fallback.sent_at_ms = input.sent_at_ms;
                                fallback.origin = UserMessageOrigin::FollowUp;
                                transcript!(E::SettleInserted {
                                    insert_id: input_id.clone(),
                                    origin: UserMessageOrigin::FollowUp,
                                    sent_at_ms: input.sent_at_ms,
                                    fallback: Some(fallback),
                                });
                                mutations
                                    .send(M::DispatchRemoved {
                                        session_id,
                                        input_id,
                                    })
                                    .await;
                            }
                            RoundEvent::RoundCompleted(_summary) => {
                                retry_attempts = 0;
                                transcript!(E::RetainNotRetry);
                            }
                            RoundEvent::RoundInterrupted(record) => {
                                // C11: the durable twin of the live stop.
                                retry_attempts = 0;
                                chrome!(event_loop::mutations::ChromeEdit::RoundEnded);
                                if !routes_to_side {
                                    mutations.send(M::SetPhase(None)).await;
                                    mutations.send(M::SetResponding(false)).await;
                                }
                                transcript!(E::Interrupted { record });
                            }
                            RoundEvent::Notice(notice) => {
                                // Provider retry has a dedicated, self-refreshing
                                // transcript disclosure driven by RetryScheduled.
                                // Toast-surfaced notices (command acknowledgments)
                                // ride the toast surface; everything else appends.
                                if notice.kind == nuo_wire::NoticeKind::ProviderRetry {
                                    // RetryScheduled owns the retry disclosure.
                                } else if notice.surface == nuo_wire::NoticeSurface::Toast {
                                    mutations
                                        .send(M::NoticeToast {
                                            severity: notice_severity_from_core(notice.severity),
                                            text: notice.render_text(),
                                        })
                                        .await;
                                } else {
                                    let message = TranscriptMessage::notice_from_core(&notice)
                                        .with_sent_at_ms(now_ms!());
                                    transcript!(E::Append { message });
                                }
                            }
                            RoundEvent::Text(t) => {
                                retry_attempts = 0;
                                let (provider, model) = attribution!();
                                let effort = picker_effort!();
                                let mut message = TranscriptMessage::new(Role::Assistant, t)
                                    .with_attribution(provider, model)
                                    .with_effort(effort)
                                    .with_sent_at_ms(now_ms!());
                                if let Some((round, turn)) =
                                    positions_by_session.get(&session_id).copied()
                                {
                                    message.round = Some(round);
                                    message.turn = Some(turn);
                                }
                                transcript!(E::Append { message });
                                // A one-shot text payload is the degenerate case
                                // of the delta stream: it *is* visible model
                                // output, so it moves the session's phase
                                // exactly as a delta does. Otherwise the bar
                                // keeps reading "waiting for model" after the
                                // model has answered — and, since the setback
                                // clause is retired when the phase leaves
                                // `AwaitingModel` (ADR-0235), the countdown for
                                // the attempt that just landed rides on.
                                chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                    Phase::Answering,
                                )));
                                if !routes_to_side {
                                    if harness.loop_status.is_idle() {
                                        mutations.send(M::SetResponding(false)).await;
                                        mutations.send(M::SetPhase(None)).await;
                                    } else {
                                        mutations.send(M::SetPhase(Some(Phase::Answering))).await;
                                    }
                                }
                            }
                            RoundEvent::CommandResult { name, args, result } => {
                                mutations.send(M::ClearSwitchingSession).await;
                                retry_attempts = 0;
                                let invocation = if args.is_empty() {
                                    format!("/{}", name)
                                } else {
                                    format!("/{} {}", name, args)
                                };
                                let mut fallback = TranscriptMessage::command_result(
                                    name.clone(),
                                    args.clone(),
                                    Some(result.clone()),
                                )
                                .with_sent_at_ms(now_ms!());
                                if let Some((round, turn)) =
                                    positions_by_session.get(&session_id).copied()
                                {
                                    fallback.round = Some(round);
                                    fallback.turn = Some(turn);
                                }
                                transcript!(E::SettleCommandResult {
                                    invocation,
                                    result,
                                    fallback: Some(fallback),
                                });
                                if !routes_to_side && harness.loop_status.is_idle() {
                                    mutations.send(M::SetResponding(false)).await;
                                    mutations.send(M::SetPhase(None)).await;
                                }
                            }
                            RoundEvent::RetryResolved(_resolution) => {
                                // ADR-0194: the retry loop recovered; ephemeral
                                // state retires, no permanent notice row.
                                transcript!(E::RetainNotRetry);
                            }
                            RoundEvent::Activity(status) => {
                                // View-scoped chrome: record this session's own
                                // phase regardless of focus. Stale in-flight
                                // activity events must not revive the bar if the
                                // harness is already idle (e.g. after interrupt).
                                if !harness.loop_status.is_idle() {
                                    let folded = Phase::classify(&status);
                                    chrome!(event_loop::mutations::ChromeEdit::ActivityFolded(
                                        folded.clone(),
                                    ));
                                    if !routes_to_side {
                                        mutations.send(M::SetPhase(Some(folded))).await;
                                        mutations.send(M::SetResponding(true)).await;
                                    }
                                }
                            }
                            RoundEvent::TurnStarted { round, turn } => {
                                let turn = turn as u64 + 1;
                                positions_by_session.insert(session_id.clone(), (round, turn));
                                transcript!(E::StampTurnPrompt { round });
                                if !routes_to_side {
                                    mutations.send(M::SetRoundCount(round)).await;
                                    // 1-indexed for display: turn 0 is the first
                                    // model request, shown as `turn 1`.
                                    mutations.send(M::SetCurrentTurn(turn)).await;
                                }
                                chrome!(event_loop::mutations::ChromeEdit::TurnStarted {
                                    round,
                                    turn,
                                });
                                if !routes_to_side {
                                    mutations
                                        .send(M::SetPhase(Some(Phase::AwaitingModel)))
                                        .await;
                                }
                            }
                            RoundEvent::StreamStart => {
                                // A stream lifecycle event is not visible
                                // transcript content; the first visible delta
                                // lazily creates its own typed component. A
                                // successful stream does retire any transient
                                // provider-retry disclosure.
                                transcript!(E::BeginStream);
                                chrome!(event_loop::mutations::ChromeEdit::StreamStarted);
                                if !routes_to_side {
                                    mutations.send(M::SetResponding(true)).await;
                                    mutations.send(M::SetPhase(Some(Phase::Answering))).await;
                                }
                            }
                            RoundEvent::StreamDelta(delta) => {
                                // Visible-text deltas outrank the reasoning phase.
                                if !routes_to_side {
                                    mutations.send(M::SetPhase(Some(Phase::Answering))).await;
                                }
                                chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                    Phase::Answering,
                                )));
                                let position = positions_by_session.get(&session_id).copied();
                                let round = position.map(|(round, _)| round);
                                let turn = position.map(|(_, turn)| turn);
                                let (provider, model) = attribution!();
                                let effort = picker_effort!();
                                let mut created =
                                    TranscriptMessage::new(Role::Assistant, delta.clone())
                                        .with_attribution(provider, model)
                                        .with_effort(effort);
                                if let Some((r, t)) = position {
                                    created.round = Some(r);
                                    created.turn = Some(t);
                                }
                                transcript!(E::StreamTextDelta {
                                    round,
                                    turn,
                                    delta,
                                    created: Some(created),
                                    clear_retry: false,
                                });
                            }
                            RoundEvent::StreamEnd(final_content) => {
                                if !routes_to_side {
                                    mutations.send(M::SetResponding(true)).await;
                                    mutations.send(M::SetPhase(Some(Phase::Finalizing))).await;
                                }
                                retry_attempts = 0;
                                let position = positions_by_session.get(&session_id).copied();
                                let round = position.map(|(round, _)| round);
                                let turn = position.map(|(_, turn)| turn);
                                // Identity-addressed (ADR-0114): the applier
                                // resolves the streaming entry by position. The
                                // fallback (providers that deliver only a final
                                // payload) is built only for non-empty content.
                                let created = if !final_content.is_empty() {
                                    let (provider, model) = attribution!();
                                    let effort = picker_effort!();
                                    let mut message = TranscriptMessage::new(
                                        Role::Assistant,
                                        final_content.clone(),
                                    )
                                    .with_attribution(provider, model)
                                    .with_effort(effort);
                                    if let Some((r, t)) = position {
                                        message.round = Some(r);
                                        message.turn = Some(t);
                                    }
                                    Some(message)
                                } else {
                                    None
                                };
                                transcript!(E::StreamTextFinalize {
                                    round,
                                    turn,
                                    content: final_content,
                                    created,
                                });
                            }
                            RoundEvent::StreamDiscard => {
                                retry_attempts = 0;
                                let position = positions_by_session.get(&session_id).copied();
                                transcript!(E::StreamDiscard {
                                    round: position.map(|(round, _)| round),
                                    turn: position.map(|(_, turn)| turn),
                                });
                            }
                            RoundEvent::UnsentInput { .. } => {
                                // Retraction is removed: entries only grow and
                                // update in place; the prompt is marked Cancelled.
                                retry_attempts = 0;
                                transcript!(E::CancelLastUserPrompt);
                                if !routes_to_side {
                                    mutations.send(M::SetResponding(false)).await;
                                    mutations.send(M::SetPhase(None)).await;
                                }
                            }
                            RoundEvent::StreamReasoningDelta(delta) => {
                                // Phase fact before anything else: the reasoning
                                // stream is alive. Hidden-chain models also land
                                // here (their summary deltas still prove thinking).
                                if !routes_to_side {
                                    mutations.send(M::SetPhase(Some(Phase::Reasoning))).await;
                                }
                                chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                    Phase::Reasoning,
                                )));
                                // Surface only a reasoning summary, never their
                                // full chain: gate at message creation. Unrecognized
                                // ids default to disclosed — only known
                                // `ReasoningSummary` models are gated.
                                let hidden_chain = !nuo_wire::model_by_id(&current_model)
                                    .map(|m| m.thinking.chain_disclosed())
                                    .unwrap_or(true);
                                if hidden_chain {
                                    continue;
                                }
                                let position = positions_by_session.get(&session_id).copied();
                                let round = position.map(|(round, _)| round);
                                let turn = position.map(|(_, turn)| turn);
                                let (provider, model) = attribution!();
                                let effort = picker_effort!();
                                let mut created = TranscriptMessage::reasoning(delta.clone())
                                    .with_attribution(provider, model)
                                    .with_effort(effort);
                                if let Some((r, t)) = position {
                                    created.round = Some(r);
                                    created.turn = Some(t);
                                }
                                // Disclosure default is applied by the applier
                                // (`App::reasoning_default_expanded`).
                                transcript!(E::ReasoningDelta {
                                    round,
                                    turn,
                                    delta,
                                    created: Some(created),
                                });
                                reasoning_start = Some(std::time::Instant::now());
                            }
                            RoundEvent::StreamReasoningEnd(content) => {
                                let duration_ms = reasoning_start
                                    .take()
                                    .map(|started| started.elapsed().as_millis() as u64);
                                let position = positions_by_session.get(&session_id).copied();
                                transcript!(E::ReasoningFinalize {
                                    round: position.map(|(round, _)| round),
                                    turn: position.map(|(_, turn)| turn),
                                    content,
                                    duration_ms,
                                });
                            }
                            RoundEvent::ToolCallStarted { index, id, name } => {
                                // ADR-0026: the call is named but its arguments
                                // are still streaming. Show a running step and
                                // move the phase to the tool verb now, instead
                                // of leaving the bar on the misleading
                                // `answering` phase for the whole argument
                                // stream. The step's call id stays empty until
                                // dispatch; `ToolCall` collapses onto it.
                                let _ = id;
                                announced_slots_by_session
                                    .entry(session_id.clone())
                                    .or_default()
                                    .push_back(index);
                                if !routes_to_side {
                                    mutations
                                        .send(M::SetPhase(Some(Phase::Tool(
                                            event_loop::tool_verb_for(&name),
                                        ))))
                                        .await;
                                    mutations.send(M::SetResponding(true)).await;
                                }
                                chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                    Phase::Tool(event_loop::tool_verb_for(&name)),
                                )));
                                let (provider, model) = attribution!();
                                let effort = picker_effort!();
                                let position = positions_by_session.get(&session_id).copied();
                                let mut message =
                                    TranscriptMessage::tool_step(String::new(), name.clone(), String::new())
                                        .with_attribution(provider, model)
                                        .with_effort(effort)
                                        .with_input_slot(index);
                                if let Some((round, turn)) = position {
                                    message = message.with_round(round).with_turn(turn);
                                }
                                transcript!(E::ToolAnnounce {
                                    slot: index,
                                    name,
                                    message,
                                });
                            }
                            RoundEvent::ToolInputProgress { index, id, bytes } => {
                                // ADR-0026: count-only progress while the
                                // arguments stream ([INV-STREAM-TOOL-03]).
                                let _ = id;
                                if !routes_to_side {
                                    mutations.send(M::SetResponding(true)).await;
                                }
                                transcript!(E::ToolInputProgress { slot: index, bytes });
                            }
                            RoundEvent::ToolCall {
                                id,
                                name,
                                arguments,
                            } => {
                                if !routes_to_side {
                                    mutations
                                        .send(M::SetPhase(Some(Phase::Tool(
                                            event_loop::tool_verb_for(&name),
                                        ))))
                                        .await;
                                }
                                let (provider, model) = attribution!();
                                let effort = picker_effort!();
                                retry_attempts = 0;
                                let position = positions_by_session.get(&session_id).copied();
                                let sent_at_ms = now_ms!();
                                // ADR-0026: collapse onto the announced step for
                                // this call's provider slot, if one is pending.
                                // Otherwise (a whole-argument provider, or an
                                // announcement that never arrived) insert
                                // normally.
                                let announced_slot = announced_slots_by_session
                                    .get_mut(&session_id)
                                    .and_then(|queue| queue.pop_front());
                                match announced_slot {
                                    Some(slot) => {
                                        transcript!(E::ToolCallCollapse {
                                            slot,
                                            call_id: id,
                                            arguments,
                                        });
                                    }
                                    None => {
                                        let mut message =
                                            TranscriptMessage::tool_step(id, name, arguments)
                                                .with_attribution(provider, model)
                                                .with_effort(effort)
                                                .with_sent_at_ms(sent_at_ms);
                                        if let Some((round, turn)) = position {
                                            message = message.with_round(round).with_turn(turn);
                                        }
                                        transcript!(E::ToolStart { message });
                                    }
                                }
                                if !routes_to_side {
                                    mutations.send(M::SetResponding(true)).await;
                                }
                            }
                            RoundEvent::ToolResult {
                                id,
                                name,
                                output,
                                structured,
                                duration_ms,
                            } => {
                                if !routes_to_side {
                                    mutations.send(M::SetPhase(Some(Phase::Preparing))).await;
                                }
                                chrome!(event_loop::mutations::ChromeEdit::PhaseOnly(Some(
                                    Phase::Preparing,
                                )));
                                let (provider, model) = attribution!();
                                let position = positions_by_session.get(&session_id).copied();
                                // The fallback (no matching in-flight call, e.g. a
                                // turn restored from history) is fully finished by
                                // the translator; the applier applies the
                                // lifecycle-aware default disclosure.
                                let mut fallback =
                                    TranscriptMessage::tool_step(id.clone(), name.clone(), "{}")
                                        .with_attribution(provider, model);
                                if let Some((round, turn)) = position {
                                    fallback.round = Some(round);
                                    fallback.turn = Some(turn);
                                }
                                fallback.finish_tool_step(
                                    &id,
                                    output.clone(),
                                    structured.clone(),
                                    duration_ms,
                                );
                                transcript!(E::ToolResult {
                                    id,
                                    name,
                                    output,
                                    structured,
                                    duration_ms,
                                    fallback: Some(fallback),
                                });
                            }
                            RoundEvent::ToolCancelled { id, .. } => {
                                // An in-flight call was aborted: flip it (and any
                                // nested subagent children) to Cancelled.
                                let position = positions_by_session.get(&session_id).copied();
                                let mut fallback =
                                    TranscriptMessage::tool_step(id.clone(), "tool", "{}");
                                if let Some((round, turn)) = position {
                                    fallback.round = Some(round);
                                    fallback.turn = Some(turn);
                                }
                                fallback.cancel_tool_step(&id);
                                fallback.set_tool_step_expanded(false);
                                transcript!(E::ToolCancel {
                                    id,
                                    fallback: Some(fallback)
                                });
                            }
                            RoundEvent::ToolStream { id, stream } => {
                                transcript!(E::ToolStream { id, stream });
                            }
                            RoundEvent::SubagentStep {
                                parent_call_id,
                                event,
                            } => {
                                // Full-duplex (ADR-0029): a subagent's permission
                                // broker or `ask_user` request bubbles up nested
                                // under this `parent_call_id`; the reply is tagged
                                // for down-routing into the child.
                                match &event {
                                    nuo_wire::SubagentEvent::PermissionRequest(req) => {
                                        mutations
                                            .send(M::QueuePermission {
                                                request: req.clone(),
                                                parent_call_id: Some(parent_call_id.clone()),
                                            })
                                            .await;
                                        if !routes_to_side {
                                            mutations
                                                .send(M::SetPhase(Some(Phase::AwaitingUser)))
                                                .await;
                                            mutations.send(M::SetResponding(true)).await;
                                        }
                                    }
                                    nuo_wire::SubagentEvent::UserQuestionRequest(req) => {
                                        mutations
                                            .send(M::QueueQuestion {
                                                request: req.clone(),
                                                parent_call_id: Some(parent_call_id.clone()),
                                            })
                                            .await;
                                        if !routes_to_side {
                                            mutations
                                                .send(M::SetPhase(Some(Phase::AwaitingUser)))
                                                .await;
                                            mutations.send(M::SetResponding(true)).await;
                                        }
                                    }
                                    _ => {}
                                }
                                transcript!(E::SubagentEvent {
                                    parent_call_id,
                                    event,
                                });
                            }
                            RoundEvent::PermissionRequest(request) => {
                                // Stays global regardless of session so the modal
                                // always surfaces (ADR-0017).
                                mutations
                                    .send(M::QueuePermission {
                                        request,
                                        parent_call_id: None,
                                    })
                                    .await;
                                if !routes_to_side {
                                    mutations.send(M::SetPhase(Some(Phase::AwaitingUser))).await;
                                    mutations.send(M::SetResponding(true)).await;
                                }
                            }
                            RoundEvent::UserQuestionRequest(request) => {
                                mutations
                                    .send(M::QueueQuestion {
                                        request,
                                        parent_call_id: None,
                                    })
                                    .await;
                                if !routes_to_side {
                                    mutations.send(M::SetPhase(Some(Phase::AwaitingUser))).await;
                                    mutations.send(M::SetResponding(true)).await;
                                }
                            }
                            RoundEvent::StdinRequest(request) => {
                                mutations.send(M::QueueInput(request)).await;
                                if !routes_to_side {
                                    mutations.send(M::SetPhase(Some(Phase::AwaitingUser))).await;
                                    mutations.send(M::SetResponding(true)).await;
                                }
                            }
                            RoundEvent::Compacted {
                                archived_messages,
                                window_tokens_before,
                                window_tokens_after,
                                summary,
                                tracked_files,
                            } => {
                                let message = TranscriptMessage::compacted_card(
                                    archived_messages,
                                    window_tokens_before,
                                    window_tokens_after,
                                    summary,
                                    tracked_files,
                                )
                                .with_sent_at_ms(now_ms!());
                                transcript!(E::Append { message });
                            }
                            RoundEvent::HarnessState(snapshot) => {
                                harness.loop_status = snapshot.loop_status;
                                harness.round_counter = snapshot.round_counter;
                                let running = !snapshot.loop_status.is_idle();
                                // View-scoped chrome: the authoritative
                                // running/idle transition for this session.
                                chrome!(event_loop::mutations::ChromeEdit::RoundLifecycle {
                                    round_count: snapshot.round_counter,
                                    running,
                                    can_retry: snapshot.retry_pending && !running,
                                });
                                if !routes_to_side {
                                    // ADR-0175: publish a quarantined snapshot to
                                    // the PreAttach mount when the per-run gate
                                    // has not already answered. The applier
                                    // deduplicates (it mounts only when the
                                    // interstitial is absent).
                                    {
                                        harness.workspace_security =
                                            snapshot.workspace_security.clone();
                                        let gate_needed = !trust_gate_dismissed_clone
                                            .load(Ordering::SeqCst)
                                            && crate::trust_gate::gate_request(
                                                &harness.workspace_security,
                                            )
                                            .is_some();
                                        if gate_needed {
                                            mutations
                                                .send(M::PreAttach(crate::PreAttachSignal {
                                                    snapshot: harness.workspace_security.clone(),
                                                }))
                                                .await;
                                        }
                                    }
                                    if running {
                                        // A new round resets the turn counter and
                                        // stamps the elapsed-timer origin.
                                        mutations.send(M::SetCurrentTurn(0)).await;
                                        mutations
                                            .send(M::SetRoundStartedAt(Some(
                                                std::time::Instant::now(),
                                            )))
                                            .await;
                                    }
                                    mutations.send(M::SetResponding(running)).await;
                                    if !running {
                                        // The dispatch cycle is complete: any
                                        // command component still Pending will
                                        // never receive its reply on this pass
                                        // (ADR-0108) — mark Cancelled.
                                        mutations
                                            .send(M::Transcript {
                                                buffer: event_loop::mutations::Buffer::Primary,
                                                edit: E::CancelPendingCommands,
                                            })
                                            .await;
                                        mutations.send(M::SetPhase(None)).await;
                                        mutations.send(M::SetCurrentTurn(0)).await;
                                        mutations.send(M::SetRoundStartedAt(None)).await;
                                    }
                                    if !running && needs_round_rebase {
                                        // A session switch replaced the transcript
                                        // before its authoritative idle snapshot
                                        // arrived: rebase the reconstructed tail
                                        // exactly once, now that the persisted
                                        // round counter is known.
                                        mutations
                                            .send(M::Transcript {
                                                buffer: event_loop::mutations::Buffer::Primary,
                                                edit: E::RebaseRounds {
                                                    round_counter: snapshot.round_counter,
                                                },
                                            })
                                            .await;
                                        needs_round_rebase = false;
                                    }
                                    mutations.send(M::Harness(snapshot.clone())).await;
                                }
                                // A harness state change is always a round
                                // boundary. If the previous round ended
                                // mid-reasoning, `StreamReasoningEnd` never
                                // arrives; freeze the orphaned trace so the
                                // spinner stops.
                                let duration_ms = reasoning_start
                                    .take()
                                    .map(|started| started.elapsed().as_millis() as u64);
                                if !running {
                                    retry_attempts = 0;
                                    // ADR-0026: no announced step survives its
                                    // round; the queue is drained so a later
                                    // call cannot claim a stale slot.
                                    announced_slots_by_session.remove(&session_id);
                                }
                                transcript!(E::FinalizeOrphanedReasoning { duration_ms });
                                // ADR-0026: safety net — a step announced but
                                // never dispatched (its round died mid
                                // arguments) is cancelled, never left running.
                                transcript!(E::FrozenOrphanToolSteps);
                            }
                            RoundEvent::TodosUpdated(_) => {}
                            RoundEvent::UnattendedChanged(enabled) => {
                                if !routes_to_side {
                                    harness.unattended = enabled;
                                    mutations.send(M::HarnessUnattended(enabled)).await;
                                }
                            }
                            RoundEvent::ConfinementChanged(confined) => {
                                if !routes_to_side {
                                    harness.confined = confined;
                                    mutations.send(M::HarnessConfined(confined)).await;
                                }
                            }
                            RoundEvent::RetryScheduled {
                                attempt,
                                max_attempts,
                                delay_ms,
                                message,
                            } => {
                                let delay = std::time::Duration::from_millis(delay_ms);
                                let retry_at = std::time::Instant::now() + delay;
                                let state = crate::app::ProviderRetryState {
                                    attempt,
                                    max_attempts,
                                    retry_at,
                                    failure: message.clone(),
                                };
                                retry_attempts = attempt;
                                // Publish-only (ADR-0235). The clause is
                                // retired by the next phase write for *this*
                                // session — each store owns its own phase —
                                // never by this translator, which is why no
                                // arm of this match carries a clause clear.
                                // The primary's slot is the App mirror, an
                                // aside's its own chrome entry, so a retrying
                                // aside cannot paint a countdown onto the
                                // primary's bar.
                                if routes_to_side {
                                    chrome!(event_loop::mutations::ChromeEdit::TransportSetback(
                                        Box::new(state),
                                    ));
                                } else {
                                    mutations.send(M::SetProviderRetry(state)).await;
                                    // Transport setback, not a workflow phase: the
                                    // countdown rides the dedicated clause channel.
                                    mutations
                                        .send(M::SetPhase(Some(Phase::AwaitingModel)))
                                        .await;
                                    mutations.send(M::SetResponding(true)).await;
                                }
                                let mut fallback = TranscriptMessage::provider_retry(
                                    attempt,
                                    max_attempts,
                                    retry_at,
                                    message,
                                );
                                if let Some((round, turn)) =
                                    positions_by_session.get(&session_id).copied()
                                {
                                    fallback.round = Some(round);
                                    fallback.turn = Some(turn);
                                }
                                let fallback = fallback.with_sent_at_ms(now_ms!());
                                transcript!(E::UpsertRetry {
                                    attempt,
                                    max_attempts,
                                    retry_at,
                                    failure: fallback.raw.clone(),
                                    fallback,
                                });
                            }
                            RoundEvent::Error(e) => {
                                let attempts = std::mem::take(&mut retry_attempts);
                                transcript!(E::RetainNotRetry);
                                // A terminal round error may still carry the raw
                                // retryable-envelope encoding: strip it so the
                                // user sees the message, never the wire framing.
                                let message = if attempts > 1 {
                                    if e.starts_with("Failed after") || e.starts_with("Exhausted") {
                                        e
                                    } else {
                                        format!("Exhausted {attempts} retry attempts — {e}")
                                    }
                                } else {
                                    e
                                };
                                let notice =
                                    TranscriptMessage::notice(NoticeSeverity::Error, message)
                                        .with_sent_at_ms(now_ms!());
                                transcript!(E::Append { message: notice });
                                if !routes_to_side {
                                    mutations.send(M::SetResponding(false)).await;
                                    mutations.send(M::SetPhase(None)).await;
                                }
                                chrome!(event_loop::mutations::ChromeEdit::RoundEnded);
                            }
                            RoundEvent::BackgroundJobStarted(info) => {
                                let label = match &info.spec {
                                    nuo_wire::JobSpec::Process { command, label, .. } => {
                                        label.as_deref().unwrap_or(command)
                                    }
                                    nuo_wire::JobSpec::Timer { label, prompt, .. } => {
                                        label.as_deref().unwrap_or(prompt.as_str())
                                    }
                                };
                                mutations
                                    .send(M::BackgroundTaskStarted {
                                        id: info.id.0.clone(),
                                        label: label.to_string(),
                                        started_at_ms: info.created_at_ms,
                                    })
                                    .await;
                                let message = TranscriptMessage::notice(
                                    NoticeSeverity::Info,
                                    format!("Background job started: {label} ({})", info.id.0),
                                )
                                .with_sent_at_ms(now_ms!());
                                transcript!(E::Append { message });
                            }
                            RoundEvent::BackgroundJobProgress { .. } => {}
                            RoundEvent::BackgroundJobReady { job_id } => {
                                // ADR-0190: a service task reported readiness.
                                let message = TranscriptMessage::notice(
                                    NoticeSeverity::Info,
                                    format!("Background service ready: {}", job_id.0),
                                )
                                .with_sent_at_ms(now_ms!());
                                transcript!(E::Append { message });
                            }
                            RoundEvent::BackgroundJobCompleted(outcome) => {
                                let (label, is_success, exit_code, duration_secs) = match &outcome
                                    .state
                                {
                                    nuo_wire::JobState::Succeeded { duration_ms, .. } => (
                                        format!(
                                            "Background job `{}` completed ({}s)",
                                            outcome.job_id.0,
                                            duration_ms / 1000
                                        ),
                                        true,
                                        Some(0),
                                        duration_ms / 1000,
                                    ),
                                    nuo_wire::JobState::Failed {
                                        duration_ms,
                                        exit_code,
                                        ..
                                    } => (
                                        format!(
                                            "Background job `{}` failed (exit {exit_code}, {}s)",
                                            outcome.job_id.0,
                                            duration_ms / 1000
                                        ),
                                        false,
                                        Some(*exit_code),
                                        duration_ms / 1000,
                                    ),
                                    nuo_wire::JobState::Killed { duration_ms } => (
                                        format!(
                                            "Background job `{}` terminated ({}s)",
                                            outcome.job_id.0,
                                            duration_ms / 1000
                                        ),
                                        false,
                                        None,
                                        duration_ms / 1000,
                                    ),
                                    nuo_wire::JobState::TimedOut { duration_ms } => (
                                        format!(
                                            "Background job `{}` timed out ({}s)",
                                            outcome.job_id.0,
                                            duration_ms / 1000
                                        ),
                                        false,
                                        None,
                                        duration_ms / 1000,
                                    ),
                                    _ => (
                                        format!("Background job `{}` completed", outcome.job_id.0),
                                        true,
                                        None,
                                        0,
                                    ),
                                };
                                mutations
                                    .send(M::BackgroundTaskCompleted {
                                        id: outcome.job_id.0.clone(),
                                        success: is_success,
                                        exit_code,
                                        duration_secs,
                                    })
                                    .await;
                                let severity = if is_success {
                                    NoticeSeverity::Info
                                } else {
                                    NoticeSeverity::Warning
                                };
                                let message = TranscriptMessage::notice(severity, label)
                                    .with_sent_at_ms(now_ms!());
                                transcript!(E::Append { message });
                            }
                        } // end inner `match event`
                    }
                    AgentResponse::ParentStatus(status) => {
                        // ADR-0017: primary-session status for the `/btw` side banner.
                        mutations.send(M::ParentStatus(status)).await;
                    }
                    AgentResponse::SideViewOpened {
                        side_id,
                        messages,
                        commands,
                        round_interrupts,
                        ..
                    } => {
                        // ADR-0017 + ADR-0103 §6: enter the aside view; back-fill
                        // the side buffer from the event's transcript payload.
                        listener_side_ids.insert(side_id.clone());
                        let mut rebuilt =
                            transcript_messages_from_core(messages, &tui_config_clone);
                        rebuilt =
                            merge_command_rows(rebuilt, transcript_commands_from_ledger(commands));
                        rebuilt = merge_round_interrupt_rows(
                            rebuilt,
                            transcript_interrupts_from_records(round_interrupts),
                        );
                        mutations
                            .send(M::Transcript {
                                buffer: event_loop::mutations::Buffer::Side,
                                edit: event_loop::mutations::TranscriptEdit::ReplaceAll {
                                    messages: rebuilt,
                                },
                            })
                            .await;
                        mutations
                            .send(M::SideView(event_loop::SideViewSignal::Opened { side_id }))
                            .await;
                    }
                    AgentResponse::SideViewClosed => {
                        // ADR-0103: leave the aside view. The routing keys are
                        // NOT dropped — background asides keep streaming; only
                        // the view flips.
                        mutations
                            .send(M::SideView(event_loop::SideViewSignal::Closed))
                            .await;
                    }
                    AgentResponse::BtwList(rows) => {
                        // ADR-0103 §5: the asides list is also the routing-truth
                        // source: ids absent from it stop routing to the side buffer.
                        listener_side_ids.retain(|id| rows.iter().any(|row| &row.id == id));
                        for row in rows.iter() {
                            listener_side_ids.insert(row.id.clone());
                        }
                        mutations.send(M::BtwList(rows)).await;
                    }
                    AgentResponse::InputHistory(rows) => {
                        mutations.send(M::InputHistory(rows)).await;
                    }
                    // ADR-0208: dashboard Archivist search hits. The TUI's
                    // Archivist pane lands in a later stage; for now the
                    // response is acknowledged (logged) so the wire stays
                    // exhaustive without inventing premature UI state.
                    AgentResponse::HistorySearch(hits) => {
                        tracing::debug!(count = hits.len(), "history search hits");
                    }
                    AgentResponse::RouteSettings {
                        provider_id,
                        model,
                        overrides,
                    } => {
                        mutations
                            .send(M::RouteSettings {
                                provider_id,
                                model,
                                overrides,
                            })
                            .await;
                    }
                    AgentResponse::PermissionsCleared => {
                        mutations.send(M::ClearPermissions).await;
                        mutations.send(M::SetPhase(None)).await;
                    }
                    AgentResponse::ProviderKeys(status) => {
                        mutations
                            .send(M::KeyStatus(status.into_iter().collect()))
                            .await;
                    }
                    AgentResponse::ProviderPicker(snapshot) => {
                        picker = snapshot.clone();
                        mutations.send(M::ProviderPicker(snapshot)).await;
                    }
                    AgentResponse::ConversationCleared { session_id } => {
                        mutations
                            .send(M::Transcript {
                                buffer: event_loop::mutations::Buffer::Primary,
                                edit: event_loop::mutations::TranscriptEdit::Clear,
                            })
                            .await;
                        mutations.send(M::SetRoundCount(0)).await;
                        needs_round_rebase = false;
                        mutations.send(M::ClearContextTokens).await;
                        // `/new` minted a fresh session and switched to it: track
                        // the post-switch id so session-scoped client state follows.
                        mutations.send(M::LiveSession(session_id.clone())).await;
                        live_session = session_id.clone();
                        mutations.send(M::TokenReport(None)).await;
                        mutations
                            .send(M::SessionTree(nuo_wire::SessionTree::default()))
                            .await;
                    }
                    AgentResponse::ConversationReplaced {
                        session_id,
                        messages,
                        commands,
                        round_interrupts,
                        retry_resolutions,
                    } => {
                        mutations.send(M::ClearSwitchingSession).await;
                        let mut rebuilt =
                            transcript_messages_from_core(messages, &tui_config_clone);
                        rebuilt =
                            merge_command_rows(rebuilt, transcript_commands_from_ledger(commands));
                        rebuilt = merge_round_interrupt_rows(
                            rebuilt,
                            transcript_interrupts_from_records(round_interrupts),
                        );
                        rebuilt = merge_round_interrupt_rows(
                            rebuilt,
                            transcript_retry_resolutions_from_records(retry_resolutions),
                        );
                        mutations
                            .send(M::Transcript {
                                buffer: event_loop::mutations::Buffer::Primary,
                                edit: event_loop::mutations::TranscriptEdit::ReplaceAll {
                                    messages: rebuilt,
                                },
                            })
                            .await;
                        needs_round_rebase = true;
                        // The model-window revision changed; do not reuse an API
                        // anchor from the previous session/projection.
                        mutations.send(M::ClearContextTokens).await;
                        // Track the new id so session-scoped client state follows,
                        // and drop the previous session's cached report.
                        mutations.send(M::LiveSession(session_id.clone())).await;
                        live_session = session_id;
                        mutations.send(M::TokenReport(None)).await;
                        mutations
                            .send(M::SessionTree(nuo_wire::SessionTree::default()))
                            .await;
                    }
                    AgentResponse::SessionsOverview(sessions) => {
                        mutations.send(M::SessionsOverview(sessions)).await;
                    }
                    AgentResponse::OpenSessionsPanel => {
                        mutations.send(M::OpenSessionsPanel).await;
                    }
                    AgentResponse::SessionTreeSnapshot { session_id, tree } => {
                        // A tree query is session-scoped: reject a reply that
                        // raced a primary-session switch.
                        if live_session == session_id {
                            mutations.send(M::SessionTree(tree)).await;
                        }
                    }
                    AgentResponse::OpenTreePanel => {
                        mutations.send(M::OpenTreePanel).await;
                    }
                    AgentResponse::OpenHostPanel => {
                        mutations.send(M::OpenHostPanel).await;
                    }
                    AgentResponse::SessionDetail(detail) => {
                        mutations.send(M::SessionDetail(detail)).await;
                    }
                    AgentResponse::ConnectionDetail(detail) => {
                        mutations.send(M::ConnectionDetail(detail)).await;
                    }
                    AgentResponse::TokenUsageReport { session_id, report } => {
                        // Install the report only when it still belongs to the
                        // session the frontend is viewing — a reply that raced a
                        // session switch would populate the modal with the
                        // previous session's rows.
                        let viewed = viewed_session_id_clone.lock().await.clone();
                        if viewed.as_deref() == Some(session_id.as_str()) {
                            mutations.send(M::TokenReport(Some(report))).await;
                        }
                    }
                    AgentResponse::UsageStatsReport { report } => {
                        // Session-independent by design (ADR-0122).
                        mutations.send(M::UsageStats(report)).await;
                    }
                    AgentResponse::ProviderQuotas(snapshot) => {
                        mutations.send(M::ProviderQuotas(snapshot)).await;
                    }
                    AgentResponse::ComposerCompletions {
                        request_id,
                        text,
                        cursor,
                        items,
                    } => {
                        mutations
                            .send(M::CompletionSignal(
                                event_loop::mutations::CompletionSignal {
                                    request_id,
                                    input: text,
                                    cursor,
                                    items,
                                },
                            ))
                            .await;
                    }
                    AgentResponse::SessionContext(snapshot) => {
                        mutations.send(M::SessionContext(snapshot)).await;
                    }
                    AgentResponse::Exit => {
                        mutations.send(M::Quit).await;
                    }
                    AgentResponse::ProviderSwitched { provider, model } => {
                        // Refreshes the hint bar; NOT appended to the transcript
                        // (the acknowledgment is a command ack, ADR-0088).
                        current_provider = provider.clone();
                        current_model = model.clone();
                        mutations
                            .send(M::ProviderSwitched { provider, model })
                            .await;
                    }
                    AgentResponse::ConnectStatus(status) => {
                        match status {
                            nuo_wire::ConnectStatus::Pending {
                                url,
                                user_code,
                                message,
                                ..
                            } => {
                                if !url.is_empty() {
                                    let url_for_open = url.clone();
                                    tokio::task::spawn_blocking(move || {
                                        if let Err(err) =
                                            crate::browser::open_browser(&url_for_open)
                                        {
                                            tracing::warn!("Failed to open browser: {err}");
                                        }
                                    });
                                }
                                mutations
                                    .send(M::Oauth(crate::app::OauthAddSignal::Pending {
                                        url: url.clone(),
                                        user_code: user_code.clone(),
                                        message: message.clone(),
                                    }))
                                    .await;
                                // The add-flow surfaces the URL/code in the
                                // OauthPending modal, so suppress the transcript
                                // notice there. Only the reconnect flow gets it.
                                let in_add_flow = awaiting_oauth_add_clone.load(Ordering::SeqCst);
                                if !in_add_flow {
                                    let body = if user_code.is_empty() {
                                        format!(
                                            "{message}\n  Open: {url}\n  Waiting for authorization…"
                                        )
                                    } else {
                                        format!(
                                            "{message}\n  Open: {url}\n  Code: {user_code}\n  Waiting for authorization…"
                                        )
                                    };
                                    let notice =
                                        TranscriptMessage::notice(NoticeSeverity::Info, body)
                                            .with_sent_at_ms(now_ms!());
                                    mutations
                                        .send(M::Transcript {
                                            buffer: event_loop::mutations::Buffer::Primary,
                                            edit: event_loop::mutations::TranscriptEdit::Append {
                                                message: notice,
                                            },
                                        })
                                        .await;
                                }
                            }
                            nuo_wire::ConnectStatus::Done { provider } => {
                                mutations
                                    .send(M::Oauth(crate::app::OauthAddSignal::Done))
                                    .await;
                                let notice = TranscriptMessage::notice(
                                    NoticeSeverity::Info,
                                    format!("{provider} authorized."),
                                )
                                .with_sent_at_ms(now_ms!());
                                mutations
                                    .send(M::Transcript {
                                        buffer: event_loop::mutations::Buffer::Primary,
                                        edit: event_loop::mutations::TranscriptEdit::Append {
                                            message: notice,
                                        },
                                    })
                                    .await;
                            }
                            nuo_wire::ConnectStatus::CatalogSyncWarning {
                                provider,
                                message,
                                kind,
                            } => {
                                // A refusal is actionable and permanent (the
                                // account may not use this provider); a
                                // transient failure is not. Say which (ADR-0273).
                                let text = match kind {
                                    nuo_wire::CatalogSyncFailure::Refused => format!(
                                        "{provider}: the upstream refused the model-list request ({message}). This connection is not usable with the current credentials or plan; the previously cached list is still shown."
                                    ),
                                    nuo_wire::CatalogSyncFailure::Transient => format!(
                                        "{provider}: could not refresh the model list ({message}). Showing the previous list."
                                    ),
                                };
                                let notice = TranscriptMessage::notice(
                                NoticeSeverity::Warning,
                                text,
                            )
                            .with_sent_at_ms(now_ms!());
                                mutations
                                    .send(M::Transcript {
                                        buffer: event_loop::mutations::Buffer::Primary,
                                        edit: event_loop::mutations::TranscriptEdit::Append {
                                            message: notice,
                                        },
                                    })
                                    .await;
                            }
                            nuo_wire::ConnectStatus::Failed { provider, message } => {
                                mutations
                                    .send(M::Oauth(crate::app::OauthAddSignal::Failed {
                                        message: message.clone(),
                                    }))
                                    .await;
                                let notice = TranscriptMessage::notice(
                                    NoticeSeverity::Error,
                                    format!("{provider} connect failed: {message}"),
                                )
                                .with_sent_at_ms(now_ms!());
                                mutations
                                    .send(M::Transcript {
                                        buffer: event_loop::mutations::Buffer::Primary,
                                        edit: event_loop::mutations::TranscriptEdit::Append {
                                            message: notice,
                                        },
                                    })
                                    .await;
                            }
                        }
                    }
                    AgentResponse::Error(msg) => {
                        mutations.send(M::ClearSwitchingSession).await;
                        let notice = TranscriptMessage::notice(NoticeSeverity::Error, msg)
                            .with_sent_at_ms(now_ms!());
                        mutations
                            .send(M::Transcript {
                                buffer: event_loop::mutations::Buffer::Primary,
                                edit: event_loop::mutations::TranscriptEdit::Append {
                                    message: notice,
                                },
                            })
                            .await;
                    }
                    AgentResponse::TuiLayoutUpdated(_value) => {
                        // The apply path already set `app.transcript_layout`
                        // optimistically; a save failure surfaces as `Error`.
                    }
                    AgentResponse::TuiColorSchemeUpdated { .. } => {
                        // Applied optimistically; save failures arrive as `Error`.
                    }
                    AgentResponse::WebSearchConfigSnapshot(snapshot) => {
                        mutations.send(M::WebSearchConfig(Some(snapshot))).await;
                    }
                    AgentResponse::WebSearchConfigUpdated(snapshot) => {
                        // Authoritative post-update ack: re-render from persisted
                        // state, discarding any optimistic local edit.
                        mutations.send(M::WebSearchConfig(Some(snapshot))).await;
                    }
                    AgentResponse::CopyToClipboard { text } => {
                        let _ = crate::clipboard::copy(&text).await;
                    }
                }
            }
        });
    }

    let (
        dev_copy_until,
        dev_copy_msg,
        dev_copy_failed,
        dev_notice_until,
        dev_notice_msg,
        dev_notice_sev,
        dev_pinned,
    ) = init_dev_toast();

    let mut app = App {
        last_submit_ms: None,
        surface_store: crate::surfaces::SurfaceStore::new(),
        surfaces: match startup_overlay {
            StartupOverlay::SessionsPicker => {
                crate::surfaces::SurfaceRouter::with_dialog(crate::surfaces::DialogKind::Sessions)
            }
            StartupOverlay::Settings { .. } => {
                crate::surfaces::SurfaceRouter::with_scene(crate::surfaces::SceneKind::Settings)
            }
            _ => crate::surfaces::SurfaceRouter::new(),
        },
        queue_exit_session: None,
        recent_commands: Vec::new(),
        input: String::new(),
        messages: restored,
        side_messages: Vec::new(),
        layout_height_cache: Default::default(),
        in_side_view: false,
        side_session_id: None,
        parent_status: ParentStatus::Idle,
        btw_list: Vec::new(),
        session_chrome: std::collections::HashMap::new(),
        saved_primary_chrome: None,
        session_tree: nuo_wire::SessionTree::default(),
        scroll: 0,
        follow_bottom: true,
        content_lines: 0,
        view_height: 0,
        max_scroll: 0,
        sticky_step: None,
        token_ledger,
        token_report: None,
        context_tokens: None,
        usage_stats: None,
        provider_quotas: None,
        connection_usages: std::collections::HashMap::new(),
        modal_body_height: 0,
        sticky_summary_line: None,
        pin_summary_line: None,
        scroll_settle_pending: false,
        focus_stack: Vec::new(),
        tx: tx.clone(),
        should_quit,
        live_session_id: live_session_init.clone(),
        pending_permissions: std::collections::VecDeque::new(),
        pending_questions: std::collections::VecDeque::new(),
        pending_inputs: std::collections::VecDeque::new(),
        subagent_permission_parent: HashMap::new(),
        subagent_question_parent: HashMap::new(),
        workspace_security: nuo_wire::WorkspaceSecuritySnapshot::default(),
        context_tokens_by_session: HashMap::new(),
        open_sessions_signal: false,
        open_tree_signal: false,
        open_host_signal: matches!(startup_overlay, StartupOverlay::Dashboard),
        view_transitioned: false,
        transcript_changed_pending: false,
        side_transcript_changed_pending: false,
        reasoning_default_expanded: crate::config::reasoning_default_expanded(&tui_config),
        backend_completion_signal: None,
        tui_config: (*tui_config).clone(),
        suggestion_index: None,
        completion_dismissed: false,
        command_catalog,
        backend_completions: Vec::new(),
        completion_response_input: None,
        completion_response_cursor: 0,
        completion_requested: None,
        completion_request_id: 0,
        cursor_position: 0,
        input_scroll: 0,
        input_scroll_follow_cursor: true,
        input_drag_scroll: None,
        modal_index: 0,
        last_key_press: std::time::Instant::now(),
        session_detail: None,
        connection_detail: None,
        config_scroll: 0,
        config_focus: crate::overlays::ConfigFocus::Categories,
        config_category: match startup_overlay {
            StartupOverlay::Settings {
                category: Some(cat),
            } => cat.min(crate::views::ConfigCategory::ALL.len().saturating_sub(1)),
            _ => 0,
        },
        config_detail_index: 0,
        config_hover_index: None,

        config_detail_scroll: 0,
        websearch_config: None,
        config_dropdown: None,
        config_selected_rect: None,
        current_provider: initial_provider,
        current_model: initial_model,
        cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        current_session_id: "test-session".to_string(),
        current_workspace: String::new(),
        current_role: None,
        session_context: None,
        loop_status: LoopStatus::Idle,
        harness_retry_pending: false,
        phase: None,
        provider_retry: None,
        persistence_health: None,
        link_down: false,
        unattended: false,
        confined: true,
        round_count: 0,
        current_turn: 0,
        round_started_at: None,
        pending_permission: None,
        active_sheet: None,
        pending_permission_depth: 0,
        // ADR-0175 §6: `NUO_FORCE_PRE_ATTACH=1` force-mounts the
        // PreAttach interstitial at startup so operators can verify
        // the surface — wording, highlight, navigation, transition —
        // without preparing a quarantined workspace. Resolved to
        // `None` later in this function based on the resolved startup
        // intent; this initializer is the no-forcing default.
        pre_attach: pre_attach_initial(),
        pending_question_depth: 0,
        pending_input: None,
        question: None,
        question_scroll: 0,
        question_modal_follow: true,
        sessions_overview: Vec::new(),
        switching_session: None,
        host_sessions: Vec::new(),
        host_scroll: 0,
        host_modal_follow: true,
        host_focus: crate::overlays::DashboardFocus::Detail,
        host_detail_scroll: 0,
        host_preview: None,
        host_preview_scroll: 0,
        host_prompting: false,
        host_prompt_new: false,
        host_console_log: Vec::new(),
        host_kill_confirm: None,
        host_kill_confirm_id: None,
        switch_to_target: None,
        startup_overlay,
        permission_confirm_always: false,
        permission_show_details: false,
        permission_scroll: 0,
        permission_max_scroll: 0,
        input_history: if input_history_config.record_commands {
            input_history
        } else {
            // `[input_history] record_commands = false` (default): scrub any
            // legacy `/slash` invocations from the loaded history so they stop
            // showing in the picker, and — since this list is what gets
            // `save_history`d on exit — the on-disk file heals itself too.
            input_history
                .into_iter()
                .filter(|e| !e.text.starts_with('/'))
                .collect()
        },
        history_index: None,
        history_draft: String::new(),
        history_draft_images: Vec::new(),
        history_draft_text_pastes: Vec::new(),
        history_attachments: std::collections::HashMap::new(),
        history_attachments_order: std::collections::VecDeque::new(),
        session_history_backfill: Vec::new(),
        session_history_backfill_cursor: 0,
        input_history_dedup: input_history_config.dedup,
        input_history_record_commands: input_history_config.record_commands,
        // This is the production TUI path (see `main` → `run_tui`): the
        // process owns the user's real database, so SQLite persistence
        // is enabled. Tests build `App` directly and keep this `false` so
        // they never write to (or truncate) the user's state directory.
        input_history_persist: true,
        pending_images: Vec::new(),
        pending_text_pastes: Vec::new(),
        pending_dispatch: std::collections::VecDeque::new(),
        composer_send_mode: crate::app::ComposerSendMode::default(),
        scene_namespace_armed: false,
        queue_blocked_sessions: std::collections::HashSet::new(),
        running_sessions: std::collections::HashSet::new(),
        selection: SelectionState::None,
        drag: SelectionDrag::default(),
        ui: crate::ui::ComponentTree::new(),
        hovered_step: None,
        transcript_focused: false,
        transcript_layout: crate::render::layout::Strategy::TurnBand,
        color_scheme: Theme::normalize_color_scheme(&tui_config.color_scheme).to_string(),
        custom_color_scheme: tui_config.custom_color_scheme.clone(),

        click_outside_dismiss: tui_config.click_outside_dismiss,
        expand_auto_scroll: tui_config.expand_auto_scroll,
        key_overrides: tui_config.global_key_overrides(),
        surface_overrides: tui_config.surface_key_overrides(),
        focused_target: None,
        copy_toast_until: dev_copy_until,
        copy_toast_message: dev_copy_msg,
        copy_toast_failed: dev_copy_failed,
        notice_toast_until: dev_notice_until,
        notice_toast_message: dev_notice_msg,
        notice_toast_severity: dev_notice_sev,
        dev_toast_pinned: dev_pinned,
        ctrl_c_armed_until: None,
        esc_armed_until: None,
        spinner_epoch: std::time::Instant::now(),
        carousel_epoch: std::time::Instant::now(),
        injection_stashed_input: String::new(),
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
        custom_client_identity: nuo_wire::ClientIdentity::Native,
        custom_models: Vec::new(),
        custom_url_hint: String::new(),
        custom_user_agent: None,
        custom_auth: nuo_wire::ConnectionAuth::ApiKey,
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
        pending_provider_delete: None,
        provider_delete_focus: ProviderDeleteChoice::default(),
        key_status: HashMap::new(),
        provider_picker: ProviderPickerSnapshot::default(),
        theme: Theme::resolve_with_profile(
            &tui_config.color_scheme,
            &tui_config.custom_color_scheme,
            None,
            &profile,
        ),
        profile,
        logo: load_user_logo(),
        background_tasks: Vec::new(),
    };

    if startup_overlay == StartupOverlay::SessionsPicker {
        app.surface_store
            .open(crate::surfaces::DialogKind::Sessions);
    }
    if matches!(startup_overlay, StartupOverlay::Settings { .. }) {
        app.send_intent(AgentRequest::QueryWebSearchConfig);
    }
    // Hydrate the prompt history from the server (the SSOT for the shared
    // SQLite store — the TUI never opens the database itself, ADR-0197).
    app.send_intent(AgentRequest::QueryInputHistory);

    // Run app
    let res = event_loop::run_app_loop(
        &mut terminal,
        &mut app,
        event_loop::UiRuntime {
            dirty,
            dirty_notify,
            is_responding,
            awaiting_oauth_add,
            trust_gate_dismissed,
            viewed_session_id,
            mutations,
        },
        mutation_rx,
        session,
    )
    .await;

    // Restore terminal
    terminal::restore_terminal();

    if let Err(err) = res {
        return Err(err.into());
    }

    let switch = app.switch_to_target.take();
    Ok(TuiOutcome {
        history: app.input_history,
        switch_to: switch,
    })
}

/// What a TUI run produced (ADR-0096): the input history to persist, and —
/// when the user picked a session in the `/host` panel — the server session
/// to switch to (the caller re-attaches).
pub struct TuiOutcome {
    pub history: Vec<nuo_wire::HistoryEntry>,
    pub switch_to: Option<String>,
}

pub async fn start_tui(
    tx: mpsc::UnboundedSender<AgentRequest>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    config: TuiLaunchConfig,
) -> Result<TuiOutcome, Box<dyn Error>> {
    run_tui(tx, rx, config).await
}

/// Apply the visible transcript effect of a stream-start signal. Retires any
/// transient provider-retry notice entry when streaming commences, and settles
/// any in-flight sending prompt to delivered.
pub(crate) fn begin_stream(messages: &mut Vec<TranscriptMessage>) {
    messages.retain(|m| !m.is_provider_retry());
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
/// Identity-addressed (ADR-0114): resolves the target by scanning backwards
/// for the Thinking entry matching `(round, turn)`, **not** by "is the last
/// message". Command entries (`/delegate`, shell passthrough) and local
/// notices can be appended between reasoning deltas — under `last_mut()`
/// addressing the next delta would fork the trace into a second Thinking
/// entry (the "two Thinking blocks" bug).
pub(crate) fn append_reasoning_delta(
    messages: &mut [TranscriptMessage],
    round: Option<u64>,
    turn: Option<u64>,
    delta: &str,
) -> Option<u64> {
    let target = messages.iter_mut().rfind(|message| {
        message.is_reasoning() && message.round == round && message.turn == turn
    })?;
    target.push_stream(delta);
    if let MessageKind::Reasoning { content, .. } = &mut target.kind {
        content.push_str(delta);
    }
    Some(target.id)
}

/// Append a streamed assistant-text delta to the current turn, creating the
/// message only when the first visible text arrives. Returning `None` means the
/// caller must perform the structural insertion (and request a full transcript
/// snapshot); returning an id permits the cheap per-message patch path.
///
/// Identity-addressed (ADR-0114): the target assistant-text entry is resolved
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
    //! Identity-addressed streaming appends (ADR-0114).
    use super::*;
    use crate::model::document::MessageKind;

    fn reasoning_entry(round: u64, turn: u64, content: &str) -> TranscriptMessage {
        let mut m = TranscriptMessage::reasoning(content);
        m.round = Some(round);
        m.turn = Some(turn);
        m
    }

    #[test]
    fn reasoning_delta_appends_across_an_intervening_command_entry() {
        // Regression (ADR-0114): dispatching `/delegate` mid-stream pushes a
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
        use nuo_wire::Role;
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
