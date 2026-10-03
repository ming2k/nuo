//! Turn-level orchestration policy on top of the `Agent` struct.
//!
//! `Agent` (in [`crate::agent`]) runs a single ReAct turn against a provider.
//! This module wraps every turn with the cross-cutting policy a frontend
//! cannot reasonably reimplement: context compaction (pre-turn and mid-turn
//! pruning), retry with exponential backoff, permission relay, and the
//! `/schedule` cron + countdown scheduler.
//!
//! Frontends drive the harness through [`execute_round`],
//! [`start_interactive_round`], and
//! `start_schedule_scheduler`. They own only the UI-specific input path (slash commands for the CLI, menus/dialogs for a
//! future GUI); the actual round machinery is shared here.
//!
//! All items are `pub` because they are assembled by the binary, which knows
//! the concrete provider/tool instances and the frontend's request channel.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::task::{Context, Poll};
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use futures::stream::{BoxStream, StreamExt};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{Agent, RequestTokenEstimate, RoundBegin, RoundLifecycle};
use nuo_wire::{
    AgentEvent, AgentResponse, HarnessError, HarnessSnapshot, ImagePart, InjectionKind, LoopStatus,
    Message, ModelRequest, NoticeKind, NoticeSeverity, NoticeSource, NoticeSurface, Provider,
    ProviderStreamEvent, Role, RoundEvent,
};
use nuo_persistence::{
    CommitTurn,
    config::Config,
    session::{ContextProjectionCheckpoint, ContextProjectionResult, SessionStore},
};

/// Wrap a session-scoped [`RoundEvent`] in the [`AgentResponse::Round`]
/// envelope (ADR-0017). Every round-scoped emitter routes through this so the
/// session id is attached uniformly, letting the TUI key transcript buffers
/// by `session_id` and dispatch primary vs `/btw` side events correctly.
pub fn round_response(session_id: &str, event: RoundEvent) -> AgentResponse {
    AgentResponse::Round {
        session_id: session_id.to_string(),
        event,
    }
}

/// Wall-clock now in Unix-epoch milliseconds. The timestamp source for
/// round-interrupt records (C11): it must be a payload field because event-log
/// compaction drops envelope timestamps.
pub(crate) fn unix_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The user-facing explanation for withholding image attachments (ADR-0230).
///
/// One wording, because there is one kind of evidence: the probe's outcome. The
/// harness deliberately does **not** read the vendor's error text to decide
/// whether images were the cause — every vendor formats that differently — so it
/// states the empirical finding instead of attributing a claim to the provider
/// that it may never have made.
fn image_withheld_notice(model: &str) -> nuo_wire::AgentNotice {
    nuo_wire::AgentNotice::new(
        NoticeKind::ImageInputWithheld,
        NoticeSeverity::Warning,
        format!("{model} cannot take images — continuing without them"),
        NoticeSource::Harness,
    )
    .with_body(format!(
        "A request to {model} was refused; retrying it without the attachments succeeded, so the \
         images were the cause. They were left out of the retried request — and of later requests \
         on this route. The images stay in this session's history and reappear when you switch \
         back to a model that accepts them. To force images onto this route anyway, set Vision to \
         \"force on\" for it in the model editor."
    ))
    .with_surface(NoticeSurface::Toast)
}

pub struct ProxyProvider {
    pub holder: Arc<RwLock<Arc<dyn Provider>>>,
    /// Whether `/debug trace` is armed. Read on every call so the
    /// toggle takes effect for the very next round-trip.
    debug_enabled: Arc<AtomicBool>,
    /// Dump directory while capture is on; `None` when off.
    debug_dir: Arc<std::sync::Mutex<Option<PathBuf>>>,
    /// Monotonic counter for unique filenames within the same millisecond.
    debug_seq: AtomicU64,
}

impl ProxyProvider {
    pub fn new(holder: Arc<RwLock<Arc<dyn Provider>>>) -> Self {
        Self {
            holder,
            debug_enabled: Arc::new(AtomicBool::new(false)),
            debug_dir: Arc::new(std::sync::Mutex::new(None)),
            debug_seq: AtomicU64::new(0),
        }
    }

    /// Resolve a capture record for the upcoming call, or `None` when capture
    /// is off. Clones the request messages once (only when armed) so the call
    /// can still move the originals into the inner provider.
    fn begin_capture(
        &self,
        provider: &str,
        model: &str,
        kind: &'static str,
        request: &ModelRequest,
    ) -> Option<PendingCapture> {
        if !self.debug_enabled.load(Ordering::SeqCst) {
            return None;
        }
        let dir = self
            .debug_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()?;
        Some(PendingCapture {
            provider: provider.to_string(),
            model: model.to_string(),
            kind,
            dir,
            request: request.clone(),
            seq: self.debug_seq.fetch_add(1, Ordering::SeqCst),
        })
    }
}

#[async_trait]
impl Provider for ProxyProvider {
    /// Delegate to the currently active inner provider so attribution tracks
    /// the live provider even after a mid-session `/models` switch.
    fn provider_id(&self) -> String {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .provider_id()
    }

    fn model(&self) -> String {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .model()
    }

    fn wire_protocol(&self) -> Option<nuo_wire::WireProtocol> {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .wire_protocol()
    }

    fn effort(&self) -> Option<nuo_wire::effort::Effort> {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .effort()
    }

    fn model_capabilities(&self) -> nuo_wire::ModelCapabilities {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .model_capabilities()
    }

    fn prompt_hints(&self) -> nuo_wire::ProviderPromptHints {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .prompt_hints()
    }

    fn route_fingerprint(&self) -> nuo_wire::RouteFingerprint {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .route_fingerprint()
    }

    fn continuation_mode(&self) -> nuo_wire::ContinuationMode {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .continuation_mode()
    }

    fn set_debug_capture(&self, enabled: bool, dir: PathBuf) {
        self.debug_enabled.store(enabled, Ordering::SeqCst);
        *self
            .debug_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = if enabled { Some(dir) } else { None };
    }

    fn debug_capture_enabled(&self) -> bool {
        self.debug_enabled.load(Ordering::SeqCst)
    }

    async fn chat(
        &self,
        request: ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let started = Instant::now();
        let capture = self.begin_capture(&provider_id, &model, "chat", &request);
        let result = p.chat(request).await;
        if let Some(capture) = capture {
            let item = match &result {
                Ok(completion) => serde_json::json!({
                    "status": "ok",
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "completion": completion,
                }),
                Err(error) => serde_json::json!({
                    "status": "error",
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "error": error,
                }),
            };
            write_capture(&capture, &[item]);
        }
        result
    }
    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let capture = self.begin_capture(&provider_id, &model, "stream_chat", &request);
        let stream = p.stream_chat(request).await;
        match (capture, stream) {
            (Some(capture), Err(error)) => {
                write_capture(
                    &capture,
                    &[serde_json::json!({ "status": "error", "error": error })],
                );
                Err(error)
            }
            (Some(capture), Ok(stream)) => Ok(CapturedStream {
                inner: stream,
                items: Vec::new(),
                capture,
            }
            .boxed()),
            (None, Ok(stream)) => Ok(stream),
            (None, Err(error)) => Err(error),
        }
    }
    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let capture = self.begin_capture(&provider_id, &model, "stream_chat_events", &request);
        let stream = p.stream_chat_events(request).await;
        match (capture, stream) {
            (Some(capture), Err(error)) => {
                write_capture(
                    &capture,
                    &[serde_json::json!({ "status": "error", "error": error })],
                );
                Err(error)
            }
            (Some(capture), Ok(stream)) => Ok(CapturedStream {
                inner: stream,
                items: Vec::new(),
                capture,
            }
            .boxed()),
            (None, Ok(stream)) => Ok(stream),
            (None, Err(error)) => Err(error),
        }
    }

    /// Delegate usage support to the live inner provider so attribution tracks
    /// the active provider even after a mid-session `/models` swap.
    fn usage_supported(&self) -> bool {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .usage_supported()
    }
}

// /debug trace

/// A queued capture record awaiting its response. Held across the inner call
/// (for `chat`) or inside a [`CapturedStream`] (for the streaming paths) and
/// flushed once the round-trip is complete.
struct PendingCapture {
    provider: String,
    model: String,
    kind: &'static str,
    dir: PathBuf,
    request: ModelRequest,
    seq: u64,
}

/// Stream wrapper that tees every item into a buffer and flushes a single
/// capture file on drop — so one streaming round-trip yields one complete JSON
/// file, whether the stream ran to completion, errored, or was cancelled
/// mid-stream (a cancelled stream simply writes whatever was collected).
///
/// The wrapper is `Unpin`: its only pinned field (`inner: BoxStream`) is a
/// `Pin<Box<…>>`, which is itself `Unpin`, so `Pin::new(&mut self.inner)` is
/// sound. This keeps `poll_next` free of unsafe.
struct CapturedStream<S> {
    inner: S,
    items: Vec<serde_json::Value>,
    capture: PendingCapture,
}

impl<S> Stream for CapturedStream<S>
where
    S: Stream + Unpin,
    S::Item: Serialize,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(item)) => {
                this.items
                    .push(serde_json::to_value(&item).unwrap_or(serde_json::Value::Null));
                Poll::Ready(Some(item))
            }
            other => other,
        }
    }
}

impl<S> Drop for CapturedStream<S> {
    fn drop(&mut self) {
        write_capture(&self.capture, &self.items);
    }
}

/// Serialize one capture record and write it atomically to the dump directory.
/// Failures are logged and swallowed: debug capture must never break a real
/// turn. Files are owner-only (`0o600`) via `atomic_write_bytes` — request
/// messages can carry pasted secrets, the same privacy profile as `/export`.
fn write_capture(capture: &PendingCapture, items: &[serde_json::Value]) {
    let timestamp = chrono::Utc::now();
    let record = serde_json::json!({
        "timestamp": timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "provider": capture.provider,
        "model": capture.model,
        "kind": capture.kind,
        "request": {
            "messages": &capture.request.messages,
            "tools": &capture.request.tool_specs,
        },
        "response": { "items": items },
    });
    let bytes = match serde_json::to_vec_pretty(&record) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "network capture serialize failed");
            return;
        }
    };
    let provider_slug = slug(&capture.provider);
    let model_slug = slug(&capture.model);
    let stamp = timestamp.format("%Y%m%d-%H%M%S%.3f");
    let file = capture.dir.join(format!(
        "{stamp}_{seq:04}_{provider_slug}_{model_slug}.json",
        seq = capture.seq,
    ));
    if let Err(error) = nuo_persistence::fsutil::atomic_write_bytes(&file, &bytes) {
        tracing::warn!(%error, file = %file.display(), "network capture write failed");
    }
    prune_capture_dir(&capture.dir);
}

/// How many capture files one directory keeps. A capture is the full request
/// context of one round-trip — on a long session each file is as big as the
/// context itself, so an armed `/debug trace` writing unbounded captures
/// grows the data dir faster than everything else combined. Debug data is
/// disposable by definition; the newest [`MAX_CAPTURE_FILES`] are plenty to
/// diagnose a provider issue.
const MAX_CAPTURE_FILES: usize = 50;

/// Delete the oldest captures beyond [`MAX_CAPTURE_FILES`]. Names sort
/// chronologically (timestamp first), so a name sort is an age sort.
fn prune_capture_dir(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.ends_with(".json") && e.file_type().is_ok_and(|t| !t.is_dir())).then_some(name)
        })
        .collect();
    if names.len() <= MAX_CAPTURE_FILES {
        return;
    }
    names.sort();
    let excess = names.len() - MAX_CAPTURE_FILES;
    for name in names.into_iter().take(excess) {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// Lowercase alnum/hyphen filename component, empty -> `"anon"`.
fn slug(value: &str) -> String {
    let mut out: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .map(|character| character.to_ascii_lowercase())
        .collect();
    if out.is_empty() {
        out.push_str("anon");
    }
    out
}

#[derive(Clone)]
pub struct ContextProjectionSettings {
    /// Token thresholds resolved against the active model's context window.
    /// Pressure (estimated in tokens) is compared against these to decide when
    /// to prune and when to run a full summarizing compaction.
    pub budget: nuo_wire::ContextBudget,
    pub preserve_rounds: usize,
    /// Use the active model to produce an anchored structured summary.
    pub summarize: bool,
    /// Enable cheap tool-result pruning (pre-turn and mid-turn).
    pub prune: bool,
    /// Token budget of the most recent tool results protected from pruning
    /// (ADR-0120: token-native; the config key was already tokens, the old
    /// `× CHARS_PER_TOKEN` conversion existed only to feed a char-space
    /// pruner).
    pub prune_protect_tokens: usize,
}

impl ContextProjectionSettings {
    /// Mid-turn pruning only fires when it can reclaim at least this many
    /// tokens, to avoid pruning churn for negligible gains (ADR-0283 Quantum Floor).
    pub const PRUNE_MIN_RECLAIM_TOKENS: usize = nuo_wire::PRUNE_QUANTUM_FLOOR_TOKENS;

    /// Resolve settings for the active model's context window. `window_tokens`
    /// is the live model's context window (tokens); `0` means unknown and the
    /// policy's fallback window is substituted.
    /// Resolve settings from a compaction policy.
    pub fn from_policy(policy: &nuo_wire::CompactionPolicy, window_tokens: usize) -> Self {
        Self {
            budget: policy.resolve(window_tokens),
            preserve_rounds: policy.preserve_rounds,
            summarize: policy.summarize,
            prune: policy.prune,
            prune_protect_tokens: policy.prune_protect_tokens,
        }
    }

    pub fn from_config(config: &Config, window_tokens: usize) -> Self {
        Self {
            budget: config.compaction.resolve(window_tokens),
            preserve_rounds: config.compaction.preserve_rounds,
            summarize: config.compaction.summarize,
            prune: config.compaction.prune,
            prune_protect_tokens: config.compaction.prune_protect_tokens,
        }
    }

    /// Adjust the post-compaction history target so the complete projected
    /// request — checkpoint, system prompt, injected context, and tool schemas
    /// — lands near `target_utilization`, not merely the durable transcript.
    pub fn for_request(&self, request: RequestTokenEstimate) -> Self {
        let mut resolved = self.clone();
        let floor = resolved.budget.target_tokens.clamp(1, 2_000);
        resolved.budget.target_tokens = resolved
            .budget
            .target_tokens
            .saturating_sub(request.overhead_tokens)
            .max(floor);
        resolved
    }
}

#[cfg(test)]
mod projection_settings_tests {
    use super::*;

    #[test]
    fn compaction_target_accounts_for_projected_request_overhead() {
        let settings = ContextProjectionSettings {
            budget: nuo_wire::CompactionPolicy::default().resolve(200_000),
            preserve_rounds: 6,
            summarize: true,
            prune: true,
            prune_protect_tokens: 24_000,
        };
        let resolved = settings.for_request(RequestTokenEstimate {
            history_tokens: 100_000,
            overhead_tokens: 12_000,
            total_tokens: 112_000,
            temporary_context_tokens: 0,
        });

        assert_eq!(settings.budget.target_tokens, 50_000);
        assert_eq!(resolved.budget.target_tokens, 38_000);
        assert_eq!(resolved.budget.compaction_threshold_tokens, 170_000);
    }
}

/// Emit the current harness snapshot (mode, round counter, loop
/// status, delegated flag, retry affordance) to the UI for a running round.
///
/// Running snapshots are emitted after lifecycle admission but
/// immediately before `execute_round` performs the counter bump. Project
/// that admitted round here so frontends receive the authoritative display
/// value without locally guessing from transcript length.
///
/// For a *running* snapshot the `/retry` affordance is definitionally off — the
/// round is executing — so `retry_pending` is forced `false`.
pub fn send_harness_state_running(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
) {
    let round_counter = agent.round_count().saturating_add(1);
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::HarnessState(HarnessSnapshot {
            loop_status: LoopStatus::Running,
            round_counter,
            unattended: agent.unattended(),
            confined: agent.is_confined(),
            workspace_security: agent.workspace_security(),
            retry_pending: false,
            role: agent.active_role(),
            workspace: agent
                .workspace_root()
                .map(|p| p.to_string_lossy().to_string()),
        }),
    ));
}

/// Publish the authoritative [`HarnessSnapshot`] for a session.
///
/// Queries the durable store for the `/retry` affordance (`retry_pending`)
/// when idle, ensuring the projection stays strictly consistent with the
/// underlying session state.
pub async fn send_harness_state_for_session(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
    session: &SessionStore,
    loop_status: LoopStatus,
) {
    // Running snapshots are emitted after lifecycle admission but
    // immediately before `execute_round` performs the counter bump. Project
    // that admitted round here so frontends receive the authoritative display
    // value without locally guessing from transcript length.
    let round_counter = agent
        .round_count()
        .saturating_add(u64::from(!loop_status.is_idle()));
    let retry_pending = loop_status.is_idle() && session.retry_pending().await.is_some();
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::HarnessState(HarnessSnapshot {
            loop_status,
            round_counter,
            unattended: agent.unattended(),
            confined: agent.is_confined(),
            workspace_security: agent.workspace_security(),
            retry_pending,
            role: agent.active_role().or_else(|| session.role()),
            workspace: session
                .workspace()
                .map(|w| w.root.to_string_lossy().to_string())
                .or_else(|| {
                    agent
                        .workspace_root()
                        .map(|p| p.to_string_lossy().to_string())
                }),
        }),
    ));
    if let Some(performance) =
        nuo_wire::latest_turn_performance(&session.request_usage_records().await)
    {
        let _ = tx.send(round_response(
            session_id,
            RoundEvent::TurnPerformance(performance),
        ));
    }
}

#[derive(Clone)]
pub struct RoundContext {
    pub agent: Arc<Agent>,
    pub tx: mpsc::UnboundedSender<AgentResponse>,
    pub token: CancellationToken,
    pub session: Arc<SessionStore>,
    /// Session id this round belongs to (ADR-0017). Tags every emitted
    /// [`RoundEvent`] so the TUI routes primary vs `/btw` side events correctly.
    pub session_id: String,
    pub projection: ContextProjectionSettings,
    pub retry_max_attempts: usize,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
    /// Emit the frontend's natural-completion signal. Repeat drivers
    /// call `execute_round` internally and must not release a user's paused
    /// next-round outbox between their own continuation iterations.
    pub emit_round_completed: bool,
}

/// Which kind of round `execute_round` is about to run — a fresh round, or
/// the `/retry` resume of a stopped one. See ADR-0128.
#[derive(Clone, Debug)]
pub enum RoundDriver {
    /// A normal round: a new prompt is admitted, the round counter bumps, and
    /// turn numbering starts at 0.
    Fresh,
    /// `/retry`: the round that stopped before completing continues *as
    /// itself*. The counter must not bump, turns number onward from the
    /// committed count, and the history is re-seeded from the checkpoint.
    Resume {
        /// The durable resume point captured when the round stopped.
        point: nuo_wire::RetryPoint,
    },
}

pub struct RoundInput {
    pub prompt: String,
    pub hidden: bool,
    pub display_prompt: Option<String>,
    /// Exact TUI send time for user-authored messages, in Unix-epoch milliseconds.
    pub sent_at_ms: Option<u64>,
    /// Inline images pasted into the prompt, attached to the user message.
    pub images: Vec<ImagePart>,
    /// Which round this input drives: a fresh prompt or a `/retry` resume of
    /// the stopped round. Replaces the old boolean: a resume carries its
    /// checkpoint, which is what makes the round "complete itself" instead of
    /// starting over.
    pub driver: RoundDriver,
}

impl RoundInput {
    /// Shorthand for the common fresh-prompt construction sites.
    pub fn fresh(prompt: String) -> Self {
        Self {
            prompt,
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Fresh,
        }
    }

    /// Shorthand for the `/retry` resume construction site.
    pub fn resume(point: nuo_wire::RetryPoint) -> Self {
        Self {
            prompt: String::new(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Resume { point },
        }
    }

    /// Whether this input bypasses the UserPromptSubmit hook gate. A resume
    /// re-sends the already-admitted request; the gate already ran when the
    /// stopped round was admitted.
    pub fn is_retry(&self) -> bool {
        matches!(self.driver, RoundDriver::Resume { .. })
    }
}

#[derive(Clone)]
pub struct InteractiveRoundContext {
    pub agent: Arc<Agent>,
    pub tx: mpsc::UnboundedSender<AgentResponse>,
    pub lifecycle: Arc<RoundLifecycle>,
    pub session: Arc<SessionStore>,
    /// Session id this round belongs to (ADR-0017). Tags every emitted
    /// [`RoundEvent`] so the TUI routes primary vs `/btw` side events correctly.
    pub session_id: String,
    pub projection: ContextProjectionSettings,
    pub retry_max_attempts: usize,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
}

pub async fn start_interactive_round(context: InteractiveRoundContext, input: RoundInput) {
    // Snapshot the counter *before* `begin`: this is the number the round
    // this task is about to run will be admitted under (fresh rounds bump in
    // `execute_round`; a `/retry` resume keeps the stopped round's frozen
    // number, which is also what `round_count` returns until the resume
    // re-admits it). Capturing it here keeps the superseded predecessor's
    // tail — which runs concurrently with this round's own bump — from
    // misattributing its interrupt record to whichever round owns the live
    // counter at tail time.
    let round_at_admission = match &input.driver {
        RoundDriver::Resume { point } => point.round,
        RoundDriver::Fresh => context.agent.round_count().saturating_add(1),
    };
    let RoundBegin {
        token,
        generation,
        previous,
    } = context.lifecycle.begin().await;
    if let Some(previous) = previous {
        // A newer round is replacing the still-live predecessor: park the
        // superseded reason *before* cancelling so the predecessor's tail can
        // label its own unwind (C11). Without this the stale round's
        // generation-guarded cleanup is silent and leaves no trace.
        //
        // The park is stamped with the *superseding input's* send time when
        // the user authored one: the stop the marker describes is anchored to
        // that send (the predecessor was still running when it left the
        // composer), and the resume seam-merge places the marker before the
        // first user message sent later than `at_ms` — a tail-time or
        // park-time clock read can land a few milliseconds *after* the send,
        // dropping the marker below the newer round's answer where it reads
        // as an interrupt of a round that completed normally. A hidden or
        // clock-less input falls back to the park moment.
        context.lifecycle.record_interrupt_at(
            nuo_wire::RoundInterruptReason::Superseded,
            input.sent_at_ms,
        );
        context.agent.reject_pending_permissions();
        context.agent.reject_pending_user_questions();
        context.agent.reject_pending_inputs();
        let _ = context.tx.send(AgentResponse::PermissionsCleared);
        previous.cancel();
    }
    let (stale_steer, stale_follow_up) = context
        .agent
        .begin_session_queues(context.session_id.clone(), generation);
    for stale in stale_steer.into_iter().chain(stale_follow_up) {
        let _ = context.tx.send(round_response(
            &context.session_id,
            RoundEvent::SteerUnavailable { input_id: stale.id },
        ));
    }
    let _ = context.tx.send(round_response(
        &context.session_id,
        RoundEvent::Activity("starting request".to_string()),
    ));
    // The spawned tail records the round-interrupt into its own store handle
    // (C11); `RoundContext` consumes `context.session`, so keep an extra Arc.
    let session_for_tail = Arc::clone(&context.session);

    tokio::spawn(async move {
        // Supervised round task: the tail below (close_user_input_round →
        // terminal event → lifecycle.finish → Idle) must run even when the
        // round body panics. Before this wrapper, a panic skipped all of it:
        // `RoundLifecycle::is_running()` stayed true forever (monitor rows
        // and `/btw` banners stuck on "Running") and parked user-input
        // requests were never resolved. The panic is converted into an
        // ordinary `HarnessError::Other` so the existing error mapping emits
        // a visible `RoundEvent::Error` instead of silence.
        send_harness_state_running(&context.tx, &context.session_id, &context.agent);
        let result = {
            let round_fut = execute_round(
                RoundContext {
                    agent: context.agent.clone(),
                    tx: context.tx.clone(),
                    token: token.clone(),
                    session: context.session,
                    session_id: context.session_id.clone(),
                    projection: context.projection,
                    retry_max_attempts: context.retry_max_attempts,
                    retry_base_ms: context.retry_base_ms,
                    retry_max_ms: context.retry_max_ms,
                    emit_round_completed: true,
                },
                input,
            );
            match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(round_fut)).await {
                Ok(result) => result,
                Err(payload) => {
                    let detail = payload
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string payload".to_string());
                    tracing::error!(panic = %detail, "agent round panicked");
                    Err(HarnessError::Other(format!(
                        "internal error: agent round panicked: {detail}"
                    )))
                }
            }
        };
        let (pending_steer, pending_follow_up) = context.agent.close_session_queues(generation);
        for pending in pending_steer.into_iter().chain(pending_follow_up) {
            let _ = context.tx.send(round_response(
                &context.session_id,
                RoundEvent::SteerUnavailable {
                    input_id: pending.id,
                },
            ));
        }
        let is_current = context.lifecycle.is_current(generation);
        // Consume the reason parked by whichever stop site cancelled this
        // round (C11). Taken before the match so every interrupted arm —
        // including the generation-suppressed one below — sees it. But a
        // parked reason alone does not mean the round *stopped*: a stop site
        // parks unconditionally (even while idle), and a late Esc Esc can
        // land after the round already passed its last cancellation
        // checkpoint. Only an actually-stopped round keeps its record —
        // a natural completion (`Ok(Completed)`) and a hook-denied prompt
        // (`Ok(NotStarted)`) are successes, not interrupts, and must not be
        // projected back as `▲ interrupted · <reason>` on resume.
        let stopped = match &result {
            Ok(RoundCompletion::Completed) | Ok(RoundCompletion::NotStarted) => false,
            Err(_) => true,
        };
        let interrupt_record = if stopped {
            // Attribution: this round's own admitted number, not the live
            // agent counter — by the time a superseded round's tail runs
            // here, the superseding round has already bumped that counter,
            // and stamping `round N+1` over round N's stop read as an
            // interrupt of the wrong (normally completed) round. The counter
            // is only read for error outcomes: a round that never got far
            // enough to know its own number (and never will — it is dead)
            // has no honest number to claim.
            context
                .lifecycle
                .take_interrupt()
                .map(|parked| nuo_wire::RoundInterrupt {
                    reason: parked.reason,
                    at_ms: parked.at_ms,
                    round: result.as_ref().err().map(|_| round_at_admission),
                    detail: None,
                })
                .or_else(|| {
                    if let Err(error) = &result {
                        let at_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        Some(nuo_wire::RoundInterrupt {
                            reason: nuo_wire::RoundInterruptReason::Error,
                            at_ms,
                            round: Some(round_at_admission),
                            detail: Some(error.to_string()),
                        })
                    } else {
                        None
                    }
                })
        } else {
            // Success: drop whatever was parked so it cannot leak into a
            // later round either (defense in depth behind `begin`'s clear).
            context.lifecycle.take_interrupt();
            None
        };
        match result {
            Ok(_) => {}
            Err(HarnessError::Interrupted) => {}
            Err(error) if is_current => {
                let _ = context.tx.send(round_response(
                    &context.session_id,
                    RoundEvent::Error(error.to_string()),
                ));
            }
            Err(_) => {}
        }
        if let Some(record) = interrupt_record {
            // One record + one live event per *stopped* round. The visible
            // `[Interrupted]` arm above, the generation-suppressed supersede
            // arm (the silent `Err(_) => {}` above — previously no trace at
            // all), and the phase-1 unsend (which returns
            // `Ok(RoundCompletion::Unsent)` after emitting `UnsentInput`).
            // A round that completed naturally is deliberately excluded: its
            // history committed and `RoundCompleted` already told the story
            // — an interrupt record there would project a false
            // "▲ interrupted" marker into the resumed transcript. The record
            // is durable projection state; the live event lets every attached
            // frontend render the stop with its reason immediately.
            if let Err(error) = session_for_tail
                .record_round_interrupt(record.clone())
                .await
            {
                tracing::warn!(?error, "could not persist round interrupt record");
            }
            if record.reason != nuo_wire::RoundInterruptReason::Error {
                let _ = context.tx.send(round_response(
                    &context.session_id,
                    RoundEvent::RoundInterrupted(record),
                ));
            }
        }
        if context.lifecycle.finish(generation).await {
            // Idle snapshot with the session in hand: this is the exact
            // moment a just-failed round's `/retry` point (armed inside
            // `execute_round`'s error path, above) becomes visible to the
            // frontends, and equally the moment a just-completed round's
            // stale point stops being offered.
            send_harness_state_for_session(
                &context.tx,
                &context.session_id,
                &context.agent,
                &session_for_tail,
                LoopStatus::Idle,
            )
            .await;
            // ADR-0209: Proactive session ledger snapshot upon round finish/interruption.
            if let Some(ledger) = context.agent.token_ledger() {
                let report = ledger.snapshot_for_session(&context.session_id);
                let _ = context.tx.send(AgentResponse::TokenUsageReport {
                    session_id: context.session_id.clone(),
                    report,
                });
            }
        }
    });
}

/// How [`execute_round`] ended when it did **not** fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundCompletion {
    /// The round reached its terminal path naturally — the model stopped
    /// calling tools, the full history committed, and `RoundCompleted` was
    /// emitted. A stop site may still have parked a reason in the window
    /// after the last cancellation checkpoint (an Esc Esc that landed too
    /// late to change the outcome); that park describes a stop that never
    /// happened and must **not** produce an interrupt record.
    Completed,
    /// A `UserPromptSubmit` hook denied the prompt: no round was opened, no
    /// model request was made. Nothing was interrupted, so no interrupt
    /// record applies.
    NotStarted,
}

pub async fn execute_round(
    context: RoundContext,
    mut input: RoundInput,
) -> Result<RoundCompletion, HarnessError> {
    let RoundContext {
        agent,
        tx,
        token,
        session,
        session_id,
        projection,
        retry_max_attempts,
        retry_base_ms,
        retry_max_ms,
        emit_round_completed,
    } = context;
    // Bind accounting to the session that admitted this round. The master
    // agent survives `/session open` and `/resume`, so its construction-time
    // thread id is not sufficient for attribution.
    agent.set_thread_id(session_id.clone());
    if let Some(ledger) = agent.token_ledger() {
        ledger.set_active_session(session_id.clone());
    }
    let _ = tx.send(round_response(
        &session_id,
        RoundEvent::Activity("saving request".to_string()),
    ));

    // UserPromptSubmit hooks (ADR-0025): a hook may deny the prompt or prepend
    // context. Hidden control prompts and retries bypass the gate.
    if !input.hidden && !input.is_retry() {
        match agent.fire_user_prompt_submit(&input.prompt).await {
            crate::hooks::UserPromptVerdict::Deny(reason) => {
                let _ = tx.send(round_response(
                    &session_id,
                    RoundEvent::Text(format!("Prompt blocked by hook: {reason}")),
                ));
                return Ok(RoundCompletion::NotStarted);
            }
            crate::hooks::UserPromptVerdict::Prepend(context) => {
                input.prompt = format!("{context}\n\n{}", input.prompt);
            }
            crate::hooks::UserPromptVerdict::Allow => {}
        }
    }

    // Phase 1: Pre-flight Aspect Evaluation (ADR-0183)
    if !input.hidden && !input.is_retry() {
        if token.is_cancelled() {
            return Err(HarnessError::Interrupted);
        }
        let aspects = agent.aspects();
        let pre_flight = tokio::select! {
            _ = token.cancelled() => return Err(HarnessError::Interrupted),
            pf = aspects.evaluate_pre_flight(&input.prompt, false) => pf,
        };
        tracing::debug!(
            tier = ?pre_flight.tier,
            thinking = pre_flight.enable_thinking,
            complexity = pre_flight.estimated_complexity,
            "Spatiotemporal Aspect: Pre-flight evaluated"
        );
    }

    // The prompt is now admitted. Bump exactly once before request assembly so
    // hooks, token accounting, todos, and emitted positions share one round
    // number. A prompt rejected by UserPromptSubmit never opens a round.
    // A `/retry` resume is the exception: it continues the round that already
    // bumped (its number is frozen in the resume point), so the counter stays
    // put — the transcript keeps one contiguous `round N` band.
    let resumed_point = match input.driver {
        RoundDriver::Resume { point } => Some(point),
        RoundDriver::Fresh => None,
    };
    if resumed_point.is_none() {
        agent.bump_round();
        // A freshly admitted round supersedes whatever was parked: the user
        // moved on (new prompt, `/compact` follow-up, scheduled job), so any
        // older `/retry` point is stale by construction and must stop being
        // offered (ADR-0128).
        if let Err(error) = session.clear_retry_pending().await {
            tracing::warn!(%error, "could not clear stale retry point on fresh round");
        }
    }
    let admitted_round = resumed_point
        .as_ref()
        .map(|point| point.round)
        .unwrap_or_else(|| agent.round_count());

    let admitted_session_id = session.id().await;
    let prompt_for_memory = if !input.hidden {
        Some(input.prompt.clone())
    } else {
        None
    };
    let prompt_for_titler = if !input.hidden && resumed_point.is_none() {
        prompt_for_memory.clone()
    } else {
        None
    };
    // Build `round_history` — the round's working scratch — from the session's
    // authoritative `model_window` plus the new user message (ADR-0048). A
    // `/retry` resume instead re-seeds from the stopped round's checkpoint
    // watermark (see the branch below) and never pushes a user message.
    let mut round_history = if let Some(point) = resumed_point.as_ref() {
        // `/retry` (ADR-0128): re-seed the round's history from the durable
        // checkpoint the stopped round left behind. The window may have moved
        // since (a compaction, a `/btw` aside sharing the store — none apply
        // to a parked round, but the clamp keeps the invariant anyway), so
        // the watermark is a *cap*: never re-send content the stopped round
        // never committed (a partially streamed response was already
        // discarded with `StreamDiscard` before the point was armed).
        let window = session.model_window().await;
        let watermark = point.history_watermark.min(window.len());
        window[..watermark].to_vec()
    } else {
        let mut th = session.model_window().await;
        // ADR-0214: no automatic turn-intake environment scan. Code structure
        // and workspace change information enter through scoped, on-demand tool
        // retrieval (`code_query`) and optional request-local reminders, never
        // as a silently-committed history injection.
        th.push(if input.hidden {
            crate::conversation_context::hidden_user(InjectionKind::HiddenRoundInput, input.prompt)
        } else {
            let message = Message::new(Role::User, input.prompt);
            let message = match input.display_prompt {
                Some(display) => message.with_display_content(display),
                None => message,
            };
            let message = match input.sent_at_ms {
                Some(sent_at_ms) => message.with_sent_at_ms(sent_at_ms),
                None => message,
            };
            if input.images.is_empty() {
                message
            } else {
                message.with_images(input.images)
            }
        });
        th
    };
    // Persist admission immediately. Mid-round crash recovery must not restore
    // the transcript from round N while leaving the session counter at N-1.
    session
        .commit_turn(CommitTurn {
            messages: &round_history,
            round_counter: Some(admitted_round),
            usage_records: &[],
            retry_point: None,
            round_interrupt: None,
            operation_id: None,
            expected_revision: None,
        })
        .await?;

    // Session Title (ADR-0022 on-demand refinement): asynchronously generate a concise
    // session title concurrently upon admission of the first user prompt. Runs in
    // the background without blocking TTFT or waiting for round convergence.
    if let Some(prompt) = prompt_for_titler {
        let (_, has_title) = session.title().await;
        if !has_title {
            agent.spawn_session_titler(Arc::clone(&session), prompt);
        }
    }

    // Install the mid-round save point (ADR-0048) so every ReAct-turn boundary
    // durably appends its new messages to the session log. This is the fix for
    // the resume-after-crash gap: without it, a round that ran side-effecting
    // tools and then crashed rewinds the transcript to the previous round,
    // leaving it out of sync with the filesystem. The closure clones the
    // session `Arc` and the message slice (the `BoxFuture` is `'static`), then
    // delegates to `SessionStore::append_turn`, which writes only the delta.
    {
        let session_for_round = Arc::clone(&session);
        let agent_for_round = Arc::clone(&agent);
        let accounting_ledger = agent.token_ledger();
        let accounting_session_id = session_id.clone();
        let tx_for_turn = tx.clone();
        agent.set_turn_persist(Arc::new(move |messages: &[Message]| {
            let session = Arc::clone(&session_for_round);
            let agent = Arc::clone(&agent_for_round);
            let snapshot = messages.to_vec();
            let ledger = accounting_ledger.clone();
            let session_id = accounting_session_id.clone();
            let tx = tx_for_turn.clone();
            Box::pin(async move {
                // One lock acquisition, one event batch, at most one snapshot
                // write per turn — the three mutations a turn produces (new
                // message tail, round counter, settled usage attempts) are a
                // single persistence transaction, not three full-snapshot
                // setters.
                let usage_records = ledger
                    .as_ref()
                    .map(|ledger| ledger.pending_records_for_session(&session_id));
                let usage_slice = usage_records.as_deref().unwrap_or(&[]);
                let outcome = session
                    .commit_turn(CommitTurn {
                        messages: &snapshot,
                        round_counter: Some(agent.round_count()),
                        usage_records: usage_slice,
                        retry_point: None,
                        round_interrupt: None,
                        operation_id: None,
                        expected_revision: None,
                    })
                    .await
                    .map(|_| ());
                if outcome.is_ok()
                    && let Some(ref ledger) = ledger
                {
                    ledger.acknowledge_records(usage_slice);
                }
                // ADR-0209: Proactive push of token ledger updates on mid-round turn boundary.
                // When an LLM turn produces tool calls or outputs and commits, stream the live
                // report immediately so open telemetry dialogs update turn-by-turn without polling.
                if let Some(ref ledger) = ledger {
                    let report = ledger.snapshot_for_session(&session_id);
                    let _ = tx.send(AgentResponse::TokenUsageReport {
                        session_id: session_id.clone(),
                        report,
                    });
                }
                outcome
            })
        }));
    }
    // Install the request-projection archive sink (ADR-0218): each freshly
    // assembled request is enqueued to the session's forensic archive, stored
    // outside the transcript and never replayed into a later request. The sink
    // is fire-and-forget so it never blocks request dispatch.
    {
        let session_for_projection = Arc::clone(&session);
        let projection_session_id = session_id.clone();
        agent.set_request_projection_persist(Arc::new(move |record| {
            session_for_projection
                .try_record_request_projection(projection_session_id.clone(), record);
        }));
    }
    let _ = tx.send(round_response(
        &session_id,
        RoundEvent::Activity("preparing context".to_string()),
    ));
    // Cheap tool-result pruning to relieve pressure before considering a full
    // compaction. Gated by the model-relative `prune_utilization` threshold
    // (ADR-0019) so it engages only once pressure crosses that fraction of the
    // window — not every turn — mirroring the mid-turn gate. Pruning also
    // self-limits to runs that reclaim meaningful space.
    //
    // The estimate is BPE tokenization over the whole prepared request —
    // real CPU-bound work — so it runs on the blocking pool, never on the
    // async executor (starving it stalls TUI rendering and stream forwarding).
    // First estimate of a session pays full price once; later passes reuse
    // the content-addressed weights cache (O(new bytes), not O(session)).
    if token.is_cancelled() {
        return Err(HarnessError::Interrupted);
    }
    let mut request_estimate = estimate_off_executor(&agent, &round_history).await;
    // ADR-0283, ADR-0284: Budget-driven pruning engages strictly when window pressure
    // breaches the model-relative prune threshold watermark. In the healthy cruise zone,
    // history mutation is completely deactivated to preserve 100% KV-cache prefix stability
    // and eliminate artificial context size drops across round boundaries.
    if projection.prune
        && request_estimate.total_tokens > projection.budget.prune_threshold_tokens
    {
        prune_and_commit(
            &mut round_history,
            &session,
            &projection,
            agent.token_weights_handle(),
        )
        .await?;
        request_estimate = estimate_off_executor(&agent, &round_history).await;
    }
    if request_estimate.total_tokens > projection.budget.compaction_threshold_tokens {
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Activity("compacting context".to_string()),
        ));
        let extra = agent.fire_pre_compact().await;
        let compaction_settings = projection.for_request(request_estimate);
        if let Some(checkpoint) = compact_round_history(
            &mut round_history,
            &session,
            &compaction_settings,
            Some(agent.provider.clone()),
            extra,
        )
        .await?
        {
            send_compaction(&tx, &session_id, &checkpoint);
        }
        agent.fire_post_compact().await;
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Activity("preparing context".to_string()),
        ));
    }

    let tool_activity = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let streamed_text = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut attempt: usize = 0;
    let retry_limit = retry_max_attempts.clamp(1, 60);
    let mut compacted_after_overflow = false;
    // An in-flight **image-cause probe** (ADR-0230): `true` means the attempt now
    // running had its attachments withheld speculatively, because a request
    // refusal occurred while they were attached. If that attempt succeeds, images
    // were the cause and the route is latched; if it fails, the hypothesis is
    // disproved, nothing is latched, and the refusal of the request the user
    // actually composed is what surfaces. No vendor text is consulted at any
    // point — the probe's outcome is the evidence.
    let mut image_probe = false;
    // Faults the retry loop recovered from, in order. Fed the durable
    // `RetryResolution` (and the live `RetryResolved` event) when the round
    // ultimately completes — the success-side mirror of a round interrupt.
    let mut retry_faults: Vec<String> = Vec::new();
    // Keep the ReAct turn alive across network attempts. Completed prior turns
    // are already durably checkpointed above; retaining this state means a
    // retry resumes the pending provider request with the same history, guard
    // registry, hooks, and accounting instead of replaying side effects.
    //
    // A `/retry` resume (ADR-0128) re-seeds the state from the durable point
    // instead of starting at turn 0: the stopped round's committed turns stay
    // committed, so the resumed execution numbers its next turn M+1 and the
    // transcript's `round N · turn M` sequence is never broken. `attempt`
    // still restarts at 0 for the resume itself — the provider retry budget
    // is per user-visible attempt to complete the round, and the user asking
    // to retry is exactly a new budget.
    let mut streaming_round = match resumed_point.as_ref() {
        Some(point) => agent.resume_streaming_round(point),
        None => agent.begin_streaming_round(),
    };
    let result = loop {
        attempt += 1;
        let activity_for_run = tool_activity.clone();
        let streamed_for_run = streamed_text.clone();
        let accounting_ledger = agent.token_ledger();
        let accounting_session = Arc::clone(&session);
        let accounting_session_id = session_id.clone();
        let result = agent
            .resume_streaming_with_events(
                &mut round_history,
                &token,
                &mut streaming_round,
                |event| {
                    if matches!(event, AgentEvent::ToolCall { .. }) {
                        activity_for_run.store(true, Ordering::SeqCst);
                    }
                    if matches!(event, AgentEvent::ModelRequestStarted { .. })
                        && let Some(ledger) = accounting_ledger.clone()
                    {
                        // Persist the in-flight state without blocking streaming.
                        // The task reads the ledger when it runs, so if completion
                        // races ahead it writes the newer terminal record rather
                        // than overwriting it with a stale in-flight snapshot.
                        let session = Arc::clone(&accounting_session);
                        let session_id = accounting_session_id.clone();
                        tokio::spawn(async move {
                            let records = ledger.pending_records_for_session(&session_id);
                            if let Err(error) = session.set_request_usage_records(records).await {
                                tracing::warn!(
                                    %error,
                                    "could not persist in-flight request usage"
                                );
                            }
                        });
                    }
                    relay_agent_event(&tx, &session_id, event, &streamed_for_run);
                },
            )
            .await;

        let Err(mut error) = result else {
            // The probe is confirmed by its *outcome* (ADR-0230): the identical
            // turn succeeded once the attachments were withheld, which is
            // evidence enough to latch the route — the vendor never had to
            // explain itself in a format we can parse.
            if std::mem::take(&mut image_probe) {
                agent.suppress_images_for_current_route();
                let _ = tx.send(round_response(
                    &session_id,
                    RoundEvent::Notice(image_withheld_notice(&agent.provider.model())),
                ));
                tracing::warn!(
                    model = %agent.provider.model(),
                    "confirmed by retry: this route refuses image input; withheld from now on"
                );
            }
            break result;
        };
        if let Err(error) = persist_request_usage(&agent, &session, &session_id).await {
            tracing::warn!(%error, "could not persist request usage after failed attempt");
        }
        let is_context_overflow = matches!(&error, HarnessError::Provider(err) if err.kind() == nuo_wire::ProviderErrorKind::ContextOverflow);
        if is_context_overflow && !compacted_after_overflow && !tool_activity.load(Ordering::SeqCst)
        {
            let overflow_settings = ContextProjectionSettings {
                preserve_rounds: projection.preserve_rounds.max(1),
                ..projection.clone()
            }
            .for_request(agent.estimate_next_request_tokens(&round_history));
            if compact_round_history(
                &mut round_history,
                &session,
                &overflow_settings,
                Some(agent.provider.clone()),
                Vec::new(),
            )
            .await?
            .is_some()
            {
                compacted_after_overflow = true;
                if streamed_text.swap(false, Ordering::SeqCst) {
                    let _ = tx.send(round_response(&session_id, RoundEvent::StreamDiscard));
                }
                if let Some(checkpoint) = session.last_projection().await {
                    send_compaction(&tx, &session_id, &checkpoint);
                }
                attempt = attempt.saturating_sub(1);
                continue;
            }
        }

        // An image refusal is *information*, not a wall (ADR-0230).
        //
        // Vision is the capability vendors do not reliably publish, so an
        // undeclared route is attempted with images. When the provider refuses
        // for that reason, the failure must not end the round: the durable
        // transcript keeps its images (ADR-0186 forbids rewriting history), so
        // the identical request would fail forever — and `/retry` would re-send
        // it unchanged. So the route learns, the *same* turn is re-projected
        // without attachments, and the conversation continues.
        //
        // **How the refusal is recognized is deliberately not a parsing
        // problem.** Every vendor formats its error envelope differently, and
        // their prose drifts, so recognition rests on the only two facts we
        // control: the request was refused (`is_request_refusal`), and it
        // carried attachments. From there the round loop runs the experiment —
        // retry the identical turn without the attachments. **Success is the
        // only evidence**, and no vendor error text is parsed anywhere: the
        // classification that remains (`is_request_refusal`) is derived from the
        // HTTP status the transport already mapped (INV-VISION-6).
        if !agent.images_suppressed_for_current_route()
            && let HarnessError::Provider(provider_error) = &error
            && provider_error.is_request_refusal()
            && round_history.iter().any(|message| {
                message
                    .images
                    .as_ref()
                    .is_some_and(|images| !images.is_empty())
            })
        {
            // The armed checkpoint is what the retry re-sends, so re-project
            // *it* — the turn's history, tools, hooks, and accounting stay
            // exactly as assembled, and only the attachments disappear. The strip
            // is unconditional (`strip_images`): the route is not latched yet,
            // because withholding them to see what happens IS the experiment. It
            // must actually happen, or the "retry" would be byte-identical and
            // prove nothing.
            let stripped = streaming_round
                .project_pending_request(|request| {
                    crate::agent::strip_images(&mut request.messages)
                })
                .unwrap_or(0);
            if stripped == 0 {
                // Nothing to withhold — the checkpoint carried no attachments
                // (the refusal arrived before assembly, say). Re-sending would be
                // byte-identical, so this is not an experiment; fall through to
                // the ordinary error path.
            } else if !image_probe {
                // Hypothesis armed. Note what the round is now running: an
                // experiment whose *outcome* is the evidence. Nothing here reads
                // the provider's error text, so no vendor format can mislead the
                // recovery — and no wording can withhold a capability by itself.
                image_probe = true;
                if streamed_text.swap(false, Ordering::SeqCst) {
                    let _ = tx.send(round_response(&session_id, RoundEvent::StreamDiscard));
                }
                tracing::warn!(
                    model = %agent.provider.model(),
                    withheld = stripped,
                    "request refused while carrying attachments; probing without them"
                );
                // A recovery, not a fault: a learned limitation must not consume
                // the transient-fault budget, exactly like the overflow
                // compaction above.
                attempt = attempt.saturating_sub(1);
                continue;
            } else if std::mem::take(&mut image_probe) {
                // The probe failed, so images were *not* the cause. Surface the
                // refusal of the request the user actually composed rather than
                // a second failure of the harness's own modified probe.
                tracing::warn!(
                    model = %agent.provider.model(),
                    "retrying without images did not help; the refusal is not about them"
                );
                error = HarnessError::Provider(provider_error.clone());
            }
        }

        let (message, retry_after_ms) = match &error {
            HarnessError::Provider(err) => match err.retry_disposition() {
                nuo_wire::RetryDisposition::Retry { retry_after_ms } => {
                    (err.message().to_string(), retry_after_ms)
                }
                _ => break Err(error),
            },
            _ => break Err(error),
        };
        if attempt >= retry_limit {
            break Err(HarnessError::Other(message));
        }
        retry_faults.push(message.clone());
        if streamed_text.swap(false, Ordering::SeqCst) {
            let _ = tx.send(round_response(&session_id, RoundEvent::StreamDiscard));
        }
        let base_ms = retry_delay_ms(attempt, retry_after_ms, retry_base_ms, retry_max_ms);
        // Apply equal jitter (half fixed, half random) to de-synchronise
        // clients that fail in unison — e.g. many sessions behind one load
        // balancer all hitting a flaky upstream at once. The jittered value
        // stays within `[base/2, base]`, so the configured cap and any
        // server `Retry-After` are still honoured. `apply_jitter_ms` is a
        // pure function; the RNG is injected here so it stays out of tests.
        let delay_ms = apply_jitter_ms(base_ms, |_| fastrand::u64(0..base_ms));
        tracing::warn!(
            attempt = attempt + 1,
            max_attempts = retry_limit,
            delay_ms,
            base_ms,
            resumed_after_tools = tool_activity.load(Ordering::SeqCst),
            "retrying after transient provider error"
        );
        let checkpoint_note = if tool_activity.load(Ordering::SeqCst) {
            " Completed tool results are preserved; this retries only the pending model request."
        } else {
            ""
        };
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Notice(
                nuo_wire::AgentNotice::new(
                    NoticeKind::ProviderRetry,
                    NoticeSeverity::Warning,
                    format!("Retrying provider request ({}/{retry_limit})", attempt + 1),
                    NoticeSource::Harness,
                )
                .with_body(format!(
                    "Waiting {}s before retrying: {}{}",
                    delay_ms.div_ceil(1_000),
                    public_retry_reason(&message),
                    checkpoint_note,
                ))
                .with_surface(NoticeSurface::Toast),
            ),
        ));
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::RetryScheduled {
                attempt: attempt + 1,
                max_attempts: retry_limit,
                delay_ms,
                message,
            },
        ));
        tokio::select! {
            _ = token.cancelled() => return Err(HarnessError::Interrupted),
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
        }
    };
    if session.id().await != admitted_session_id {
        return Err(HarnessError::Interrupted);
    }
    // Only emit saving activity on natural completion. An interrupted or failed
    // round must never re-arm the activity bar that was already idled.
    if result.is_ok() {
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Activity("saving response".to_string()),
        ));
    }

    let (retry_point, outcome) = match result {
        Ok(outcome) => (Some(None), Ok(outcome)),
        Err(error) => {
            // The round failed terminally with committed history — this is
            // exactly the "/retry me" state (ADR-0128). Park the resume point
            // so the user's `/retry` continues *this* round: same number,
            // turns onward from what was committed, history at the watermark
            // the failed round durably left behind.
            let point = nuo_wire::RetryPoint {
                round: admitted_round,
                turns_committed: streaming_round.committed_turns(),
                history_watermark: round_history.len(),
                paused_ms: agent.round_paused_ms(),
                at_ms: unix_epoch_ms(),
            };
            (Some(Some(point)), Err(error))
        }
    };

    let ledger = agent.token_ledger();
    let usage_records = ledger
        .as_ref()
        .map(|ledger| ledger.pending_records_for_session(&session_id));
    let usage_slice = usage_records.as_deref().unwrap_or(&[]);

    // Commit all terminal round state (messages, usage records, retry point)
    // in a single atomic persistence transaction instead of multiple full-snapshot writes.
    // The committed revision is the durable boundary this completion follows
    // (ADR-0236 D5); it travels on the completion notification so a client can
    // order the completion against later state.
    let committed_revision = session
        .commit_turn(CommitTurn {
            messages: &round_history,
            round_counter: None,
            usage_records: usage_slice,
            retry_point,
            round_interrupt: None,
            operation_id: None,
            expected_revision: None,
        })
        .await?;

    if let Some(ref ledger) = ledger {
        ledger.acknowledge_records(usage_slice);
    }

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            // The final committed history still reconciles the meter on the
            // error path; there is no completion event, so ordering is free.
            send_context_projection(&tx, &session_id, &agent, &round_history).await;
            if matches!(error, HarnessError::Interrupted)
                && (streamed_text.load(Ordering::SeqCst) || tool_activity.load(Ordering::SeqCst))
            {
                let _ = tx.send(round_response(
                    &session_id,
                    RoundEvent::Text("... [Interrupted]".to_string()),
                ));
            }
            return Err(error);
        }
    };

    let visible = outcome.message.content.trim().to_string();
    if !visible.is_empty() && !streamed_text.load(Ordering::SeqCst) {
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Text(visible.clone()),
        ));
    }

    // Record dialogue into role-scoped cognitive memory (pure user <-> role turns)
    if let (Some(prompt), Some(role)) = (prompt_for_memory, agent.active_role())
        && !prompt.trim().is_empty()
        && !visible.is_empty()
        && let Ok(store) = nuo_persistence::get_role_memory_store()
    {
        let _ = store.record_dialogue(&role, Some(&session_id), &prompt, &visible);
    }

    // ADR-0236 D5 / invariant #5: publish the authoritative completion as soon
    // as the commit is acknowledged, *before* any optional projection (the
    // context estimate below, the state mirrors further down). None of those
    // may gate a completed round.
    if emit_round_completed {
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::RoundCompleted(nuo_wire::RoundSummary {
                round: agent.round_count(),
                output_tokens: outcome.token_usage.completion_tokens.max(0) as u64,
                duration_ms: outcome.duration_ms,
                paused_ms: outcome.paused_ms,
                generation_ms: outcome.generation_ms,
                session_revision: committed_revision,
            }),
        ));
        // ADR-0209 Tenet II: Proactive push of token ledger updates on turn
        // boundary. The token source report is an authoritative in-memory
        // projection from the ledger, streamed immediately so open telemetry
        // dialogs update live without polling.
        if let Some(ledger) = agent.token_ledger() {
            let report = ledger.snapshot_for_session(&session_id);
            let _ = tx.send(AgentResponse::TokenUsageReport {
                session_id: session_id.clone(),
                report,
            });
        }
    }

    // Optional: reconcile the pre-wire estimate from the final committed
    // history after completion has been published. This reconciles the meter
    // after interruption, tool cancellation, or response commit instead of
    // leaving it anchored to a request shape that is no longer AI-visible.
    send_context_projection(&tx, &session_id, &agent, &round_history).await;

    // Mirror the unified task list so resume restores the sticky panel. The
    // value is compared against the session's current list to skip the write
    // (and avoid an event-log entry) on turns where nothing changed — the
    // common case.
    //
    // Auto-clear: once every item reaches a terminal status (completed or
    // cancelled), the task is finished and the list is dropped so a done list
    // does not linger in the panel (and the prompt) indefinitely. An empty
    // list is a no-op here.
    let agent_todos = agent.todos();
    if !agent_todos.items.is_empty() && agent_todos.is_all_done() {
        agent.clear_todos();
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::TodosUpdated(nuo_wire::TodoList::default()),
        ));
        if let Err(err) = session.set_todos(nuo_wire::TodoList::default()).await {
            tracing::warn!(error = %err, "could not clear todos");
        }
    } else {
        let stored_todos = session.todos().await;
        if agent_todos != stored_todos
            && let Err(err) = session.set_todos(agent_todos).await
        {
            tracing::warn!(error = %err, "could not persist todos");
        }
    }

    // Mirror session-scoped runtime state to the durable session (ADR-0048
    // Phase 2): the disabled-tool mask and the round counter. Each is compared
    // against the durable value and skipped on a match to avoid a no-op
    // event-log entry (mirroring the todos
    // diff above).
    let agent_disabled = agent.disabled_tools_snapshot();
    if agent_disabled != session.disabled_tools().await
        && let Err(err) = session.set_disabled_tools(agent_disabled).await
    {
        tracing::warn!(error = %err, "could not persist disabled tools");
    }
    let agent_round = agent.round_count();
    if agent_round != session.round_counter().await
        && let Err(err) = session.set_round_counter(agent_round).await
    {
        tracing::warn!(error = %err, "could not persist round counter");
    }

    // ADR-0194: The round recovered from transient provider faults.
    // Retired: emitting `RetryResolved` and writing to `session.record_retry_resolution`.
    // Transient transport retries are purely in-flight execution details, not post-round
    // dialogue notices. The settled tail was already committed cleanly above.
    if !retry_faults.is_empty() {
        tracing::info!(
            session = %session_id,
            round = agent_round,
            attempts = retry_faults.len(),
            "round recovered after transient provider retries"
        );
    }

    // Phase 5: Round EOL Aspect Hook (ADR-0183)
    agent.aspects().fire_round_eol(&agent, Arc::clone(&session));

    // The cross-session usage aggregate is deliberately NOT computed here. It
    // folds every day blob in the 400-day window (~85-110 ms release, ~410-430
    // ms debug against a year of history) and would sit on the critical path to
    // the tail's idle snapshot, which is what clears the activity bar off
    // `finalizing response`. The `/usage` overlay fetches it on demand instead
    // (`AgentRequest::QueryUsageStats`); live telemetry is carried by the
    // in-memory `TokenUsageReport` published with the completion above.
    Ok(RoundCompletion::Completed)
}

async fn send_context_projection(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Arc<Agent>,
    messages: &[Message],
) {
    let started = std::time::Instant::now();
    let estimate = estimate_off_executor(agent, messages).await;
    if started.elapsed() >= std::time::Duration::from_millis(250) {
        tracing::warn!(session = %session_id,
            duration_ms = started.elapsed().as_millis() as u64,
            "slow local final context projection");
    }
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::ContextTokens(nuo_wire::ContextTokenSnapshot::from_estimate(
            estimate,
            nuo_wire::ContextTokenSource::Projection,
        )),
    ));
}

async fn persist_request_usage(
    agent: &Agent,
    session: &SessionStore,
    session_id: &str,
) -> Result<(), String> {
    let Some(ledger) = agent.token_ledger() else {
        return Ok(());
    };
    session
        .set_request_usage_records(ledger.pending_records_for_session(session_id))
        .await
}

/// Run one full request estimate on the blocking pool. `Agent` is `Send +
/// Sync`; the model-request assembly and BPE tokenization are pure CPU work
/// over immutable inputs, so `spawn_blocking` is safe and keeps the async
/// executor free for stream forwarding and UI. Falls back to inline execution
/// only if the runtime is shutting down (the round is tearing down anyway).
async fn estimate_off_executor(agent: &Arc<Agent>, messages: &[Message]) -> RequestTokenEstimate {
    let agent = Arc::clone(agent);
    let snapshot = messages.to_vec();
    tokio::task::spawn_blocking(move || agent.estimate_next_request_tokens(&snapshot))
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "estimate task aborted; treating pressure as zero");
            RequestTokenEstimate::new(0, 0)
        })
}

/// Session-weight estimate (nested subagent children included — the
/// pressure/prune number, **not** the wire estimate) on the blocking pool,
/// through the shared content-addressed weights cache. Companion to
/// [`estimate_off_executor`]: BPE tokenization never runs on the async
/// executor, and repeated passes pay O(new bytes), not O(session). A
/// panicked/aborted task reads as `0` (no pressure), matching
/// `estimate_off_executor`'s fallback.
async fn estimate_session_weight_off_executor(
    weights: Arc<nuo_wire::MessageTokenWeights>,
    messages: &[Message],
) -> usize {
    let snapshot = messages.to_vec();
    tokio::task::spawn_blocking(move || {
        nuo_wire::estimate_tokens_weighted(&snapshot, &weights)
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "session-weight estimate task aborted; treating as zero");
        0
    })
}

pub fn retry_delay_ms(
    attempt: usize,
    retry_after_ms: Option<u64>,
    base_ms: u64,
    max_ms: u64,
) -> u64 {
    // Stepped backoff schedule: 1s, 2s, 5s, 10s, 10s, then continuously 10s (scaled by base_ms).
    const STEP_MULTIPLIERS: [u64; 4] = [1, 2, 5, 10];
    let step_idx = attempt.saturating_sub(1);
    let multiplier = STEP_MULTIPLIERS[step_idx.min(STEP_MULTIPLIERS.len() - 1)];
    let stepped_backoff = base_ms.saturating_mul(multiplier);
    match retry_after_ms {
        Some(ms) => ms.max(base_ms).min(max_ms.max(1)),
        None => stepped_backoff.min(max_ms.max(1)),
    }
}

/// Apply "equal jitter" to a backoff delay, the variant recommended for
/// client-side retries: half the delay is fixed, the other half is randomised.
/// Unlike "full jitter" (`[0, base]`) it never collapses to a near-zero delay,
/// so a retry never fires immediately; unlike "no jitter" it still de-synchronises
/// clients that fail in unison (e.g. behind the same load balancer). The result
/// is always in `[base/2, base]`, so the configured upper bound is respected and
/// a server-supplied `Retry-After` (which bypasses `retry_delay_ms`) is honoured
/// while still being jittered to avoid thundering-herd on its expiry.
///
/// `roll` is an injected `[0, base] -> u64` closure so this stays a pure,
/// deterministic, unit-testable function; the only call site (`execute_round`)
/// supplies `fastrand`. A `base` of 0 is degenerate and passed through unchanged.
pub fn apply_jitter_ms(base: u64, roll: impl Fn(u64) -> u64) -> u64 {
    if base == 0 {
        return 0;
    }
    let half = base / 2;
    half + roll(base - half).min(base - half)
}

pub fn public_retry_reason(message: &str) -> String {
    let first = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("transient provider error");
    const MAX_CHARS: usize = 96;
    if first.chars().count() <= MAX_CHARS {
        first.to_string()
    } else {
        let mut compact: String = first.chars().take(MAX_CHARS.saturating_sub(1)).collect();
        compact.push('…');
        compact
    }
}

pub fn relay_agent_event(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    event: AgentEvent,
    streamed_text: &std::sync::atomic::AtomicBool,
) {
    let response = match event {
        AgentEvent::Notice(notice) => round_response(session_id, RoundEvent::Notice(notice)),
        AgentEvent::ModelRequestStarted {
            round,
            turn,
            context_tokens,
        } => {
            // The projection is session-scoped and was computed at the exact
            // pre-wire boundary, after hooks and request preparation.
            let _ = tx.send(round_response(
                session_id,
                RoundEvent::ContextTokens(nuo_wire::ContextTokenSnapshot::new(
                    context_tokens,
                    nuo_wire::ContextTokenSource::Projection,
                )),
            ));
            // Structured turn signal first, so the Activity modal can show
            // `round N · turn M · waiting for model` with the turn as a
            // first-class field rather than text-mining it out of the status
            // string. The bare status follows as the `Activity` below.
            let _ = tx.send(round_response(
                session_id,
                RoundEvent::TurnStarted { round, turn },
            ));
            round_response(
                session_id,
                RoundEvent::Activity("waiting for model".to_string()),
            )
        }
        AgentEvent::TurnPerformance(performance) => {
            round_response(session_id, RoundEvent::TurnPerformance(performance))
        }
        AgentEvent::ContextTokens(snapshot) => {
            round_response(session_id, RoundEvent::ContextTokens(snapshot))
        }
        AgentEvent::SteerAdmitted(input) => {
            round_response(session_id, RoundEvent::SteerAdmitted(input))
        }
        AgentEvent::AssistantDelta { delta, start } => {
            if start {
                let _ = tx.send(round_response(session_id, RoundEvent::StreamStart));
            }
            streamed_text.store(true, Ordering::SeqCst);
            round_response(session_id, RoundEvent::StreamDelta(delta))
        }
        AgentEvent::AssistantEnd(content) => round_response(
            session_id,
            RoundEvent::StreamEnd(content.trim().to_string()),
        ),
        AgentEvent::AssistantDiscard => round_response(session_id, RoundEvent::StreamDiscard),
        AgentEvent::ReasoningDelta { delta, start } => {
            if start {
                let _ = tx.send(round_response(session_id, RoundEvent::StreamStart));
            }
            streamed_text.store(true, Ordering::SeqCst);
            round_response(session_id, RoundEvent::StreamReasoningDelta(delta))
        }
        AgentEvent::ReasoningEnd(content) => {
            round_response(session_id, RoundEvent::StreamReasoningEnd(content))
        }
        AgentEvent::ToolCall {
            id,
            name,
            arguments,
        } => round_response(
            session_id,
            RoundEvent::ToolCall {
                id,
                name,
                arguments,
            },
        ),
        AgentEvent::ToolResult {
            id,
            name,
            output,
            structured,
            duration_ms,
        } => round_response(
            session_id,
            RoundEvent::ToolResult {
                id,
                name,
                output,
                structured,
                duration_ms,
            },
        ),
        AgentEvent::ToolCancelled { id, name } => {
            round_response(session_id, RoundEvent::ToolCancelled { id, name })
        }
        AgentEvent::ToolStream { id, stream } => {
            round_response(session_id, RoundEvent::ToolStream { id, stream })
        }
        AgentEvent::TodosUpdated(todos) => {
            round_response(session_id, RoundEvent::TodosUpdated(todos))
        }
        AgentEvent::UnattendedChanged(enabled) => {
            round_response(session_id, RoundEvent::UnattendedChanged(enabled))
        }
        AgentEvent::ConfinementChanged(enabled) => {
            round_response(session_id, RoundEvent::ConfinementChanged(enabled))
        }
        AgentEvent::PermissionRequest(request) => {
            round_response(session_id, RoundEvent::PermissionRequest(request))
        }
        AgentEvent::UserQuestionRequest(request) => {
            round_response(session_id, RoundEvent::UserQuestionRequest(request))
        }
        AgentEvent::StdinRequest(request) => {
            round_response(session_id, RoundEvent::StdinRequest(request))
        }
        AgentEvent::Subagent {
            parent_call_id,
            event,
        } => round_response(
            session_id,
            RoundEvent::SubagentStep {
                parent_call_id,
                event,
            },
        ),
        AgentEvent::BackgroundJobStarted(info) => {
            round_response(session_id, RoundEvent::BackgroundJobStarted(info))
        }
        AgentEvent::BackgroundJobProgress { job_id, line } => round_response(
            session_id,
            RoundEvent::BackgroundJobProgress { job_id, line },
        ),
        AgentEvent::BackgroundJobReady { job_id } => {
            round_response(session_id, RoundEvent::BackgroundJobReady { job_id })
        }
        AgentEvent::BackgroundJobCompleted(outcome) => {
            round_response(session_id, RoundEvent::BackgroundJobCompleted(outcome))
        }
        AgentEvent::CatalogInvalidated => {
            let config = nuo_persistence::config::Config::load();
            let usage = nuo_persistence::connection_usage::ConnectionUsage::load();
            let _ = tx.send(AgentResponse::ProviderPicker(
                crate::catalog::build_picker_state(&config, &usage),
            ));
            return;
        }
    };
    let _ = tx.send(response);
}

pub async fn compact_round_history(
    history: &mut Vec<Message>,
    session: &SessionStore,
    settings: &ContextProjectionSettings,
    provider: Option<Arc<dyn Provider>>,
    extra_context: Vec<String>,
) -> Result<Option<ContextProjectionCheckpoint>, String> {
    // ADR-0296 Universal Causal Compaction:
    // Preserves the latest complete user round (Round N) as volatile tail,
    // and folds all completed prior rounds [0..N-1].
    let preserve_rounds = settings.preserve_rounds.max(1);
    compact_round_history_with_mode(
        history,
        session,
        settings,
        provider,
        crate::compaction::CompactionCutMode::PreserveTailRounds(preserve_rounds),
        extra_context,
    )
    .await
}

pub async fn compact_round_history_with_mode(
    history: &mut Vec<Message>,
    session: &SessionStore,
    settings: &ContextProjectionSettings,
    provider: Option<Arc<dyn Provider>>,
    mode: crate::compaction::CompactionCutMode,
    extra_context: Vec<String>,
) -> Result<Option<ContextProjectionCheckpoint>, String> {
    let provider = if settings.summarize { provider } else { None };

    // Canonical path: Session IR Native Causal Compaction (ADR-0255 / ADR-0296)
    let mut ir = session.session_ir().await;
    if let Some(outcome) = crate::compaction::CausalCompactor::compact_session_ir(
        &mut ir,
        provider,
        mode,
        extra_context,
    )
    .await?
    {
        session.commit_session_ir(&ir).await?;
        let active_msgs = ir.resolve_active_messages();
        let tokens_after = nuo_wire::pressure::estimate_tokens(&active_msgs);
        let checkpoint = ContextProjectionCheckpoint {
            operation: nuo_persistence::session::ContextProjectionKind::Compact,
            archived_messages: outcome.nodes_folded,
            active_messages: active_msgs.len(),
            window_tokens_before: outcome.tokens_before,
            window_tokens_after: tokens_after,
            summary: Some(outcome.summary),
            tracked_files: outcome.tracked_files,
        };
        *history = active_msgs;
        Ok(Some(checkpoint))
    } else {
        Ok(None)
    }
}

/// Prune old tool results in place and durably commit the change. Pruning is an
/// implicit model-context projection step: it keeps the conversation and the
/// `tool_call_id` chain intact (only stale tool *bodies* are cleared), so unlike
/// a summarizing compaction it does **not** surface a transcript notice — it
/// only records a durable checkpoint and a `debug` trace for observability.
///
/// The before/after session-weight estimates (children included — the pressure
/// number, not the wire estimate) run through the shared weights cache on the
/// blocking pool, per the same executor discipline as `estimate_session_weight_off_executor`.
pub async fn prune_and_commit(
    history: &mut [Message],
    session: &SessionStore,
    settings: &ContextProjectionSettings,
    weights: Arc<nuo_wire::MessageTokenWeights>,
) -> Result<(), String> {
    let window_tokens_before =
        estimate_session_weight_off_executor(Arc::clone(&weights), history).await;
    // ADR-0283 Damper 1: Quantum Reclaim Gate ($Q_{min} >= 4,000 tokens)
    // Strictly forbids micro-pruning for negligible gains.
    let min_reclaim = settings.budget.quantum_floor_tokens;
    let Some(outcome) =
        nuo_wire::prune_tool_results(history, settings.prune_protect_tokens, min_reclaim)
    else {
        return Ok(());
    };
    let window_tokens_after =
        estimate_session_weight_off_executor(Arc::clone(&weights), history).await;
    let checkpoint = ContextProjectionCheckpoint {
        operation: nuo_persistence::session::ContextProjectionKind::Prune,
        archived_messages: outcome.originals.len(),
        active_messages: history.len(),
        window_tokens_before,
        window_tokens_after,
        summary: None,
        tracked_files: Vec::new(),
    };
    tracing::debug!(
        pruned_tool_results = checkpoint.archived_messages,
        window_tokens_before,
        window_tokens_after,
        reclaimed_tokens = outcome.reclaimed_tokens,
        "pruned tool results under budget pressure"
    );
    session
        .commit_context_projection(ContextProjectionResult {
            model_window: history.to_owned(),
            archived_originals: outcome.originals,
            checkpoint,
        })
        .await
}

pub fn send_compaction(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    checkpoint: &ContextProjectionCheckpoint,
) {
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::Compacted {
            archived_messages: checkpoint.archived_messages,
            window_tokens_before: checkpoint.window_tokens_before,
            window_tokens_after: checkpoint.window_tokens_after,
            summary: checkpoint.summary.clone(),
            tracked_files: checkpoint.tracked_files.clone(),
        },
    ));
}

#[cfg(test)]
mod title_tests {
    use super::*;
    use crate::AgentIdentity;
    use async_trait::async_trait;
    use nuo_wire::{Message, ModelRequest, ProviderStreamEvent, Role};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TitleProvider {
        consults: AtomicUsize,
    }

    #[async_trait]
    impl nuo_wire::Provider for TitleProvider {
        async fn chat(
            &self,
            _request: ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            self.consults.fetch_add(1, Ordering::SeqCst);
            Ok(nuo_wire::ProviderCompletion::message(Message::new(
                Role::Assistant,
                "Fixing the build",
            )))
        }
        async fn stream_chat(
            &self,
            _request: ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(futures::stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<ProviderStreamEvent, nuo_wire::ProviderError>,
            >,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[allow(dead_code)]
    struct AutoCleanArcSession(Arc<SessionStore>, tempfile::TempDir);
    impl std::ops::Deref for AutoCleanArcSession {
        type Target = Arc<SessionStore>;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    async fn fresh_title_session() -> AutoCleanArcSession {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::for_path(dir.path().join("session.json")));
        AutoCleanArcSession(store, dir)
    }

    async fn await_title(session: &SessionStore) -> (Option<String>, bool) {
        for _ in 0..200 {
            let (title, manual) = session.title().await;
            if title.is_some() {
                return (title, manual);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        session.title().await
    }

    #[tokio::test]
    async fn session_title_spawned_concurrently_on_first_prompt() {
        let session = fresh_title_session().await;
        let provider = Arc::new(TitleProvider {
            consults: AtomicUsize::new(0),
        });
        let agent = Arc::new(Agent::new(
            provider.clone(),
            Vec::new(),
            AgentIdentity::default(),
        ));

        let (title, has_title) = session.title().await;
        assert!(title.is_none());
        assert!(!has_title);

        agent.spawn_session_titler(session.clone(), "Fix production memory leak".into());
        let (title, has_title) = await_title(&session).await;
        assert_eq!(title.as_deref(), Some("Fixing the build"));
        assert!(has_title);
    }

    #[tokio::test]
    async fn manual_title_lock_is_not_overwritten_by_spawn_session_titler() {
        let session = fresh_title_session().await;
        session
            .set_title(Some("My own title".into()), true)
            .await
            .unwrap();
        let provider = Arc::new(TitleProvider {
            consults: AtomicUsize::new(0),
        });
        let agent = Arc::new(Agent::new(
            provider.clone(),
            Vec::new(),
            AgentIdentity::default(),
        ));
        agent.spawn_session_titler(session.clone(), "Hello".into());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (title, manual) = session.title().await;
        assert_eq!(title.as_deref(), Some("My own title"));
        assert!(manual);
        assert_eq!(provider.consults.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod capture_prune_tests {
    use super::*;

    /// An armed `/debug trace` writes one full-context capture per round-trip
    /// and nothing ever removed them — on a long session the capture dir grew
    /// faster than every other data path combined. Retention keeps the newest
    /// [`MAX_CAPTURE_FILES`] and deletes older ones in age order.
    #[test]
    fn capture_prune_keeps_newest_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..(MAX_CAPTURE_FILES + 5) {
            let name = format!("20260101-0000{i:02}.000_0001_anon_m.json");
            std::fs::write(root.join(name), b"x").unwrap();
        }
        // A foreign file must not count toward or be affected by retention.
        std::fs::write(root.join("notes.txt"), b"x").unwrap();

        prune_capture_dir(root);

        let mut remaining: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        assert_eq!(remaining.len(), MAX_CAPTURE_FILES + 1);
        assert!(remaining.contains(&"notes.txt".to_string()));
        // The five oldest captures are gone; the newest survives.
        assert!(!remaining.contains(&"20260101-000000.000_0001_anon_m.json".to_string()));
        assert!(remaining.contains(&format!(
            "20260101-0000{}.000_0001_anon_m.json",
            MAX_CAPTURE_FILES + 4
        )));
    }

    #[test]
    fn capture_prune_is_noop_at_or_below_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("20260101-000000.000_0001_anon_m.json"),
            b"x",
        )
        .unwrap();
        prune_capture_dir(dir.path());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
