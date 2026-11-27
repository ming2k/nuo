//! Response translator: translates wire AgentResponse stream into AppMutations.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;

use crate::shell::{STREAM_FRAME_INTERVAL, event_loop, is_coalescible_stream_update};
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
    pub tui_config: Arc<nuo_view::tui_config::TuiConfig>,
    pub initial_provider: String,
    pub initial_model: String,
    pub initial_round_count: u64,
    pub live_session: String,
}

/// Per-thread translator state so multiple subscribed threads can stream
/// concurrently over one multiplexed connection without cross-contamination
/// (ADR-0251 `[INV-MUX-03]`).
struct ThreadTranslatorState {
    needs_round_rebase: bool,
    provider: String,
    model: String,
    harness: nuo_protocol::HarnessSnapshot,
    retry_progress: Option<nuo_view::retry::RetryProgress>,
    reasoning_start: Option<std::time::Instant>,
}

impl ThreadTranslatorState {
    fn new(provider: String, model: String, round_counter: u64) -> Self {
        Self {
            needs_round_rebase: false,
            provider,
            model,
            harness: nuo_protocol::HarnessSnapshot {
                loop_status: LoopStatus::Idle,
                round_counter,
                unattended: false,
                confined: true,
                workspace_security: nuo_protocol::WorkspaceSecuritySnapshot::default(),
                retry_pending: false,
                role: None,
                workspace: None,
            },
            retry_progress: None,
            reasoning_start: None,
        }
    }
}

/// Long-lived translator state, moved out of the `tokio::spawn` body.
struct TranslatorState {
    /// Fallback attribution for global/un-scoped responses.
    current_provider: String,
    current_model: String,
    picker: ProviderPickerSnapshot,
    /// Per-thread translation state keyed by `thread_id` (ADR-0251).
    threads: std::collections::HashMap<String, ThreadTranslatorState>,
    /// The live primary session id: the translator updates it on
    /// `/new` / `/session open` / `/resume` / `/fork` and scopes
    /// session-addressed replies (the tree snapshot) against it.
    live_session: String,
    /// ADR-0253 `[INV-INSTANCE-03]`: per-thread active generation (`0` = retired
    /// by an in-place re-key, `>= 1` = active). Replaces `SessionGate`.
    thread_generations: std::collections::HashMap<String, u64>,
    mutations: event_loop::mutations::MutationSink,
    tui_config: Arc<nuo_view::tui_config::TuiConfig>,
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
        let mut initial_threads = std::collections::HashMap::new();
        if !live_session_for_translator.is_empty() {
            initial_threads.insert(
                live_session_for_translator.clone(),
                ThreadTranslatorState::new(
                    initial_provider_for_translator.clone(),
                    initial_model_for_translator.clone(),
                    initial_round_count,
                ),
            );
        }
        let mut state = TranslatorState {
            current_provider: initial_provider_for_translator.clone(),
            current_model: initial_model_for_translator.clone(),
            picker: ProviderPickerSnapshot::default(),
            threads: initial_threads,
            live_session: live_session_for_translator.clone(),
            thread_generations: std::collections::HashMap::new(),
            mutations,
            tui_config: tui_config_clone,
            trust_gate_dismissed: trust_gate_dismissed_clone,
            awaiting_oauth_add: awaiting_oauth_add_clone,
            viewed_session_id: viewed_session_id_clone,
        };

        // High-frequency deltas are coalesced at the translator, not dropped:
        // the first delta wakes the loop immediately, later deltas within the
        // frame budget schedule exactly one trailing wake. The loop itself also
        // enforces the same budget, so a burst of non-stream events cannot turn
        // into a repaint storm.
        let mut last_stream_wakeup: Option<Instant> = None;
        let stream_wakeup_scheduled = Arc::new(AtomicBool::new(false));

        while let Some(resp) = rx.recv().await {
            dirty_clone.store(true, Ordering::Release);

            let now = Instant::now();
            let coalescible =
                is_coalescible_stream_update(&resp) && ir_clone.load(Ordering::SeqCst);
            let due = !coalescible
                || last_stream_wakeup
                    .is_none_or(|last| now.duration_since(last) >= STREAM_FRAME_INTERVAL);
            if due {
                last_stream_wakeup = Some(now);
                dirty_notify_clone.notify_one();
            } else if !stream_wakeup_scheduled.swap(true, Ordering::AcqRel) {
                let wait = last_stream_wakeup
                    .map(|last| STREAM_FRAME_INTERVAL.saturating_sub(now.duration_since(last)))
                    .unwrap_or(STREAM_FRAME_INTERVAL);
                let notify = dirty_notify_clone.clone();
                let scheduled = Arc::clone(&stream_wakeup_scheduled);
                tokio::spawn(async move {
                    tokio::time::sleep(wait).await;
                    scheduled.store(false, Ordering::Release);
                    notify.notify_one();
                });
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
    fn thread_mut(&mut self, thread_id: &str) -> &mut ThreadTranslatorState {
        let default_provider = self.current_provider.clone();
        let default_model = self.current_model.clone();
        self.threads
            .entry(thread_id.to_string())
            .or_insert_with(|| ThreadTranslatorState::new(default_provider, default_model, 0))
    }

    /// Attribution from the per-thread translator mirror (falling back to global).
    fn attribution_for(&self, thread_id: &str) -> (String, String) {
        if let Some(ts) = self.threads.get(thread_id)
            && (!ts.provider.is_empty() || !ts.model.is_empty())
        {
            return (ts.provider.clone(), ts.model.clone());
        }
        (self.current_provider.clone(), self.current_model.clone())
    }

    /// Effort derivation from the translator-owned picker mirror for `thread_id`.
    fn picker_effort_for(&self, thread_id: &str) -> Option<String> {
        let (provider, model) = self.attribution_for(thread_id);
        self.picker
            .rows
            .iter()
            .find(|row| row.id == provider)
            .and_then(|row| row.model_info.iter().find(|m| m.model == model))
            .and_then(|m| m.effort.clone())
    }

    async fn transcript(&self, thread_id: &str, edit: E) {
        self.mutations
            .send(M::Transcript {
                thread_id: Some(thread_id.to_string()),
                edit,
            })
            .await;
    }

    async fn chrome(&self, thread_id: &str, edit: event_loop::mutations::ChromeEdit) {
        self.mutations
            .send(M::ChromeEdit {
                thread_id: Some(thread_id.to_string()),
                edit,
            })
            .await;
    }

    /// Route one wire response to its event-family handler.
    async fn handle(&mut self, resp: AgentResponse) {
        match resp {
            AgentResponse::Round { thread_id, scope } => self.handle_round(thread_id, scope).await,
            AgentResponse::ConversationCleared { .. }
            | AgentResponse::ConversationReplaced { .. }
            | AgentResponse::ThreadSnapshot(_)
            | AgentResponse::ThreadSubscribed { .. } => self.handle_thread_hydration(resp).await,
            AgentResponse::InputHistory(_)
            | AgentResponse::HistorySearch(_)
            | AgentResponse::ThreadsOverview(_)
            | AgentResponse::ThreadsExistence { .. }
            | AgentResponse::ThreadDeleted { .. }
            | AgentResponse::OpenThreadsPanel
            | AgentResponse::ThreadTreeSnapshot { .. }
            | AgentResponse::OpenTreePanel
            | AgentResponse::ThreadDetail(_)
            | AgentResponse::ConnectionDetail(_)
            | AgentResponse::TokenUsageReport { .. }
            | AgentResponse::ContextPlan { .. }
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
