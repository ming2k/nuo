//! Response translator: translates wire AgentResponse stream into AppMutations.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

use crate::shell::{event_loop, is_coalescible_stream_update, session_gate};
use nuo_protocol::{AgentResponse, LoopStatus, ProviderPickerSnapshot};

/// Translator-side alias for the mutation type (formerly a `use` inside the
/// event loop).
type M = event_loop::mutations::AppMutation;
/// Translator-side alias for the transcript edit type.
type E = event_loop::mutations::TranscriptEdit;

mod command;
mod interaction;
mod notice;
mod provider;
mod round;
mod session;
mod tool;

pub struct ResponseTranslatorConfig {
    pub rx: mpsc::UnboundedReceiver<AgentResponse>,
    pub runtime: event_loop::UiRuntime,
    pub link_lost: crate::shell::client::LinkLostMarker,
    pub tui_config: Arc<crate::shell::config::TuiConfig>,
    pub initial_provider: String,
    pub initial_model: String,
    pub initial_round_count: u64,
    pub live_session: String,
}

/// Long-lived translator state, moved out of the `tokio::spawn` body.
struct TranslatorState {
    /// A session switch replaces the transcript before its authoritative idle
    /// `HarnessState` arrives. Rebase the reconstructed tail exactly once when
    /// that snapshot supplies the persisted round counter.
    needs_round_rebase: bool,
    /// Translator-owned mirrors of the values it itself receives (sent to
    /// `App` via mutations): attribution, the provider-picker snapshot
    /// (effort derivation), and the harness snapshot (idle gating). No
    /// shared cells are read for these.
    current_provider: String,
    current_model: String,
    picker: ProviderPickerSnapshot,
    harness: nuo_protocol::HarnessSnapshot,
    /// The round's retry episode, as the *typed* state machine
    /// (`nuo_view::retry`). One value holds the attempt pair, the
    /// backoff instant and the fault history, so the three consumers
    /// that need it — the bar clause, the receipt-log line and the
    /// settled turn anchor — cannot disagree about any of them.
    ///
    /// It is the only retry state in the translator: there is no
    /// parallel counter and no second copy of the count. `None` means
    /// the round in flight has not retried. Boundary writes below exist so
    /// a later round's error can never be blamed for an earlier round's
    /// attempts.
    retry_progress: Option<nuo_view::retry::RetryProgress>,
    reasoning_start: Option<std::time::Instant>,
    /// The live primary session id: the translator updates it on
    /// `/new` / `/session open` / `/resume` / `/fork` and scopes
    /// session-addressed replies (the tree snapshot) against it.
    live_session: String,
    /// ADR-0166 `[INV-TOOL-ID-06]`: sessions this client has switched
    /// away from. A superseded round can still emit terminal frames after
    /// the switch (`cancel_current` signals a token; it does not join the
    /// task), and those frames must not write into the replacement
    /// buffer. See [`session_gate`] for why this is a superseded set and
    /// not an equality check against the live id.
    session_gate: session_gate::SessionGate,
    mutations: event_loop::mutations::MutationSink,
    tui_config: Arc<crate::shell::config::TuiConfig>,
    trust_gate_dismissed: Arc<std::sync::atomic::AtomicBool>,
    awaiting_oauth_add: Arc<std::sync::atomic::AtomicBool>,
    viewed_session_id: Arc<tokio::sync::Mutex<Option<String>>>,
}

/// Turn position and send time carried by a turn-scoped round event.
#[derive(Clone, Copy)]
struct TurnCtx {
    turn_context: Option<nuo_protocol::TurnContext>,
    turn_position: Option<(u64, u64)>,
    turn_sent_at: Option<u64>,
}

pub(crate) fn spawn_response_translator(config: ResponseTranslatorConfig) {
    let ResponseTranslatorConfig {
        mut rx,
        runtime,
        link_lost,
        tui_config,
        initial_provider,
        initial_model,
        initial_round_count,
        live_session,
    } = config;

    let mutations = runtime.mutations.clone();
    let link_lost_for_reader = link_lost.clone();
    let dirty_clone = runtime.dirty.clone();
    let dirty_notify_clone = runtime.dirty_notify.clone();
    let ir_clone = runtime.is_responding.clone();
    let trust_gate_dismissed_clone = runtime.trust_gate_dismissed.clone();
    let awaiting_oauth_add_clone = runtime.awaiting_oauth_add.clone();
    let viewed_session_id_clone = runtime.viewed_session_id.clone();
    let tui_config_clone = tui_config.clone();
    let live_session_for_translator = live_session.clone();
    let initial_provider_for_translator = initial_provider.clone();
    let initial_model_for_translator = initial_model.clone();

    tokio::spawn(async move {
        // Turn position and send time ride each turn-scoped event
        // (`RoundScope::Turn`); the translator keeps no per-session
        // position or timestamp map (ADR-0168). A mid-stream attach is
        // therefore as correct as a client that observed `TurnStarted`.
        let mut state = TranslatorState {
            needs_round_rebase: false,
            current_provider: initial_provider_for_translator.clone(),
            current_model: initial_model_for_translator.clone(),
            picker: ProviderPickerSnapshot::default(),
            harness: nuo_protocol::HarnessSnapshot {
                loop_status: LoopStatus::Idle,
                round_counter: initial_round_count,
                unattended: false,
                confined: true,
                workspace_security: nuo_protocol::WorkspaceSecuritySnapshot::default(),
                retry_pending: false,
                role: None,
                workspace: None,
            },
            retry_progress: None,
            reasoning_start: None,
            live_session: live_session_for_translator.clone(),
            session_gate: session_gate::SessionGate::new(),
            mutations,
            tui_config: tui_config_clone,
            trust_gate_dismissed: trust_gate_dismissed_clone,
            awaiting_oauth_add: awaiting_oauth_add_clone,
            viewed_session_id: viewed_session_id_clone,
        };

        while let Some(resp) = rx.recv().await {
            // Any handled response can change state the loop renders from,
            // so signal a redraw. High-frequency stream deltas deliberately
            // do not wake the loop one-by-one: while responding, its 10fps
            // heartbeat coalesces them into a smooth stream.
            dirty_clone.store(true, Ordering::Release);
            // A side thread can receive stream deltas while the
            // primary activity indicator is idle — no heartbeat then, so
            // retain the immediate wake.
            let defer_stream_wakeup =
                is_coalescible_stream_update(&resp) && ir_clone.load(Ordering::SeqCst);
            if !defer_stream_wakeup {
                dirty_notify_clone.notify_one();
            }
            state.handle(resp).await;
        }
        // The response stream ended. Only a *transport* failure is a
        // fault: the reader records that case with its real error, so the
        // disclosure names what actually happened instead of guessing
        // (ADR-0136). A clean close is a server instruction — `Exit`
        // already delivered that, and inventing a fault here would
        // contradict it.
        let transport_failure = link_lost_for_reader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(reason) = transport_failure {
            state.mutations.send(M::LinkLost { reason }).await;
        }
    });
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl TranslatorState {
    /// Attribution from the translator-owned mirrors.
    fn attribution(&self) -> (String, String) {
        (self.current_provider.clone(), self.current_model.clone())
    }

    /// Effort derivation from the translator-owned picker mirror. The picker
    /// row already carries the server's resolved depth — no protocol
    /// sniffing, no re-derivation (ADR-0126).
    fn picker_effort(&self) -> Option<String> {
        self.picker
            .rows
            .iter()
            .find(|row| row.id == self.current_provider)
            .and_then(|row| {
                row.model_info
                    .iter()
                    .find(|m| m.model == self.current_model)
            })
            .and_then(|m| m.effort.clone())
    }

    async fn transcript(&self, edit: E) {
        self.mutations.send(M::Transcript { edit }).await;
    }

    async fn chrome(&self, session_id: &str, edit: event_loop::mutations::ChromeEdit) {
        self.mutations
            .send(M::ChromeEdit {
                session_id: session_id.to_string(),
                edit,
            })
            .await;
    }

    /// Route one wire response to its event-family handler.
    async fn handle(&mut self, resp: AgentResponse) {
        match resp {
            // Per-turn events arrive tagged with the session they
            // belong to; there is a single primary transcript buffer now
            // (the aside substrate is retired, ADR-0058), so the
            // translator routes every event to it. Permission and
            // user-question requests stay global so their modals surface
            // regardless of which view is focused.
            AgentResponse::Round { thread_id, scope } => self.handle_round(thread_id, scope).await,
            AgentResponse::ConversationCleared { .. }
            | AgentResponse::ConversationReplaced { .. }
            | AgentResponse::ThreadSnapshot(_) => self.handle_thread_hydration(resp).await,
            AgentResponse::InputHistory(_)
            | AgentResponse::HistorySearch(_)
            | AgentResponse::ThreadsOverview(_)
            | AgentResponse::OpenThreadsPanel
            | AgentResponse::ThreadTreeSnapshot { .. }
            | AgentResponse::OpenTreePanel
            | AgentResponse::ThreadDetail(_)
            | AgentResponse::ConnectionDetail(_)
            | AgentResponse::TokenUsageReport { .. }
            | AgentResponse::UsageStatsReport { .. }
            | AgentResponse::ComposerCompletions { .. }
            | AgentResponse::ThreadContext(_)
            | AgentResponse::ServerEndpointsStatus(_)
            | AgentResponse::ThreadPhase(_) => self.handle_thread_metadata(resp).await,
            AgentResponse::RouteSettings { .. }
            | AgentResponse::PermissionsCleared
            | AgentResponse::ProviderKeys(_)
            | AgentResponse::ProviderPicker(_)
            | AgentResponse::ProviderSwitched { .. }
            | AgentResponse::ProviderQuotas(_)
            | AgentResponse::ConnectStatus(_)
            | AgentResponse::StateDelta(_) => self.handle_provider_state(resp).await,
            AgentResponse::Error(_)
            | AgentResponse::TuiLayoutUpdated(_)
            | AgentResponse::TuiColorSchemeUpdated { .. }
            | AgentResponse::WebSearchConfigSnapshot(_)
            | AgentResponse::WebSearchConfigUpdated(_)
            | AgentResponse::CopyToClipboard { .. }
            | AgentResponse::Exit => self.handle_notice_and_client_ops(resp).await,
        }
    }
}
