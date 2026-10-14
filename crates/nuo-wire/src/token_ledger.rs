//! Token-source accounting: how many tokens came from authoritative upstream
//! usage reports vs. local estimates.
//!
//! When a provider reports real `usage` ([`crate::ProviderCompletionMeta::usage`] or
//! a [`crate::ProviderStreamEvent::Usage`]), the harness books those tokens as
//! **reported**. When it does not, the harness falls back to the local
//! char-class estimator ([`crate::estimate_tokens`]) and books them as
//! **estimated**. This module keeps a running tally so the UI can answer
//! "how accurate is my context meter?" and surface which providers/models
//! are measured vs. guessed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Durable request accounting. Callers await admission and settlement explicitly;
/// a persistence failure is observable and cannot be swallowed by a sync callback.
pub trait UsageStatSink: Send + Sync {
    fn persist_usage<'a>(
        &'a self,
        recorded_at_ms: u64,
        project: &'a str,
        record: RequestUsageRecord,
    ) -> futures::future::BoxFuture<'a, Result<(), String>>;
}

/// Lifecycle state of one concrete provider request attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestUsageStatus {
    #[default]
    InFlight,
    Completed,
    Interrupted,
    Failed,
    /// Restored after a crash while the request was still marked in-flight.
    Abandoned,
}

impl RequestUsageStatus {
    pub fn is_terminal(self) -> bool {
        self != Self::InFlight
    }
}

/// Provenance of the counts attached to a request attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestUsageSource {
    #[default]
    Unknown,
    Reported,
    Estimated,
}

/// Where an attempt's timing samples were captured.
///
/// Client-observed timing is authoritative for the experience at this
/// process boundary, but it must not be presented as provider-internal model
/// timing: network transit, upstream queueing, and proxy buffering remain in
/// the observation. `Provider` is reserved for adapters that receive explicit
/// server-side generation telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceTimingSource {
    #[default]
    Unknown,
    ClientObserved,
    Provider,
}

/// Tokenizer behind the streamed-output count used for observed stream TPS.
/// Provider-reported completion tokens remain the authoritative billing
/// count; this source describes only the client-visible stream counter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamTokenSource {
    #[default]
    Unknown,
    /// The provider supplied output-token identities/counts for the stream.
    Provider,
    /// The client counted streamed output with its exact `cl100k_base`
    /// implementation. Exact for matching models; an approximation otherwise.
    Cl100k,
}

/// How completely the owned transport observed this attempt.
///
/// The ledger must never lose the difference between *measured* and
/// *unmeasured*: a bare absent field cannot say whether a connection paid no
/// setup cost or whether nothing was watching. Absent `dns_us`/`tcp_us`/
/// `tls_us` reads as "reused a pooled socket" only in the second state below;
/// in the first it asserts nothing at all.
pub use nuo_model_codec::endpoint::TransportObservation;

/// High-resolution performance telemetry for one concrete provider attempt.
///
/// Every duration is a monotonic offset measured in microseconds. Optional
/// fields stay absent for legacy records and for stages the active provider
/// cannot expose; absence is never encoded as a fabricated zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestPerformance {
    /// Name resolution, when the attempt needed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_us: Option<u64>,
    /// TCP connect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_us: Option<u64>,
    /// TLS handshake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_us: Option<u64>,
    /// Dispatch to the request's last byte handed to the kernel.
    ///
    /// The closest a client can get to "the server acknowledged my request":
    /// the peer's ACK is the kernel's business. Excludes connection setup and
    /// the upload, so it is the anchor the latency timeline's TTFT uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_sent_us: Option<u64>,
    /// Request dispatch to the connection being ready to carry the request:
    /// the end of the last connection phase that was actually paid (`TLS` end
    /// when a handshake ran, `TCP` end otherwise), or the instant the pool
    /// handed the socket over.
    ///
    /// Distinct from [`Self::stream_ready_us`], which is the response head. A
    /// timeline that anchors its connection moment on the head renders that
    /// moment *after* the request was sent — the wrong order by construction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_us: Option<u64>,
    /// What the transport observed about this attempt. Absent transport fields
    /// support a claim about the connection regime only when this says the
    /// transport was watching. A record written before this field existed
    /// decodes as `Unreported`, which is the truth about it.
    #[serde(default)]
    pub observation: TransportObservation,
    /// Dispatch to the first origin-emitted protocol frame of any class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_frame_us: Option<u64>,
    /// Smallest smoothed RTT observed via `TCP_INFO` (Linux, L1 tap).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u64>,
    /// Retransmitted segments observed via `TCP_INFO`.
    #[serde(default)]
    pub retransmits: u32,
    /// Request dispatch to the provider returning a live response stream
    /// (normally HTTP response headers received).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_ready_us: Option<u64>,
    /// Request dispatch to the first output-bearing event (text, reasoning,
    /// or tool-call payload) observed by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_us: Option<u64>,
    /// First output-bearing event to the last output-bearing event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_us: Option<u64>,
    /// Last output-bearing event to the provider stream ending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_us: Option<u64>,
    /// Request dispatch to a complete, validated assistant response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2e_us: Option<u64>,
    /// Client-counted output tokens across streamed text, reasoning, and tool
    /// payloads. Diagnostic only: the rate uses the attempt's completion count
    /// (provider reported when available), which a reader can verify.
    #[serde(default)]
    pub streamed_output_tokens: u64,
    /// Tokens carried by the first output-bearing event. These tokens are
    /// excluded from the observed stream-rate numerator because they already
    /// existed when the stream clock began.
    #[serde(default)]
    pub first_output_tokens: u64,
    /// Number of output-bearing stream events observed.
    #[serde(default)]
    pub output_events: u32,
    #[serde(default)]
    pub timing_source: PerformanceTimingSource,
    #[serde(default)]
    pub stream_token_source: StreamTokenSource,
    /// Optional provider-native queue time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_queue_us: Option<u64>,
    /// Optional provider-native prompt-prefill time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_prefill_us: Option<u64>,
    /// Optional provider-native decode duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_decode_us: Option<u64>,
    /// Token count paired with `provider_decode_us` by the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_output_tokens: Option<u64>,
}

/// Transport-level timings an attempt observed, handed up by the egress.
///
/// Deliberately separate from [`RequestPerformance`]: these come from the
/// socket and the HTTP layer, not from the protocol adapter, and a provider
/// that cannot supply them reports `None` rather than zero.
///
/// The offsets here are measured from the transport's own dispatch, which the
/// struct carries as a runtime-only anchor. A consumer that anchors its numbers
/// elsewhere must re-anchor every offset before merging the two — and must drop
/// the offsets entirely when no anchor came with them. The durations (`dns_us`,
/// `tcp_us`, `tls_us`, `rtt_us`) are anchor-free and transfer either way.
pub use nuo_model_codec::endpoint::{TransportTelemetry, TransportTimings};

/// Minimum duration of an observed streaming span required for the streaming
/// rate sample to be statistically and physically defensible (20ms).
pub const MIN_DEFENSIBLE_STREAM_SPAN_US: u64 = 20_000;

/// Physically plausible ceiling for single-stream model output (tokens/sec).
/// Any client-observed stream TPS exceeding this indicates burst/buffered
/// transport packet arrival rather than steady-state token decode.
pub const MAX_PLAUSIBLE_STREAM_TPS: f64 = 2_000.0;

impl RequestPerformance {
    /// The streaming rate: `output_tokens / (last token − first token)`.
    ///
    /// One rate, one anchor. `output_tokens` is whatever the caller trusts most
    /// — the provider's completion count when it reported one, the local
    /// estimate otherwise — so a reader can reproduce the division from the two
    /// numbers on screen. There is deliberately no end-to-end rate: it answers a
    /// different question in the same units, and a label cannot carry that.
    ///
    /// `None` when the span cannot support a rate: fewer than two output
    /// events, a span below [`MIN_DEFENSIBLE_STREAM_SPAN_US`], no tokens, or a
    /// result above [`MAX_PLAUSIBLE_STREAM_TPS`] (burst arrival, not decode).
    pub fn stream_tps(self, output_tokens: i64) -> Option<f64> {
        let stream_us = self.stream_us?;
        if stream_us < MIN_DEFENSIBLE_STREAM_SPAN_US || output_tokens <= 0 || self.output_events < 2
        {
            return None;
        }
        let tps = output_tokens as f64 * 1_000_000.0 / stream_us as f64;
        if !tps.is_finite() || tps <= 0.0 || tps > MAX_PLAUSIBLE_STREAM_TPS {
            return None;
        }
        Some(tps)
    }

    /// Provider-native model decode rate when the upstream supplied both the
    /// decode duration and the matching generated-token count.
    pub fn provider_decode_tps(self) -> Option<f64> {
        let decode_us = self.provider_decode_us?;
        let tokens = self.provider_output_tokens?.checked_sub(1)?;
        if decode_us == 0 || tokens == 0 {
            return None;
        }
        let tps = tokens as f64 * 1_000_000.0 / decode_us as f64;
        if !tps.is_finite() || tps <= 0.0 || tps > MAX_PLAUSIBLE_STREAM_TPS {
            return None;
        }
        Some(tps)
    }

    /// Whether the egress reported transport-level telemetry for this attempt.
    ///
    /// Read from the recorded [`TransportObservation`], never inferred from an
    /// absent field: silence about `dns_us`/`tcp_us`/`tls_us` is evidence of a
    /// pooled socket only *once* something was watching the socket. Read from
    /// silence it fabricated a warm pool for every cold handshake.
    pub fn transport_observed(self) -> bool {
        self.observation != TransportObservation::Unreported
    }

    /// Whether the attempt reused a pooled connection, as far as the transport
    /// reported it.
    ///
    /// Three states, not two: `Some(true)` — observed, and no setup was paid;
    /// `Some(false)` — observed, and connection phases were paid; `None` — no
    /// transport telemetry, so a reuse and a handshake are equally unevidenced.
    /// `None` is never a claim of reuse.
    pub fn pooled_connection(self) -> Option<bool> {
        match self.observation {
            TransportObservation::PooledConnection => Some(true),
            TransportObservation::ColdConnection => Some(false),
            TransportObservation::Unreported => None,
        }
    }

    /// Whether `TCP_INFO` sampled this attempt's socket, and therefore whether
    /// [`Self::retransmits`] is a measurement rather than an untouched zero.
    pub fn tcp_info_sampled(self) -> bool {
        self.rtt_us.is_some()
    }
}

/// Stable identity of a concrete network attempt. A ReAct turn may have
/// multiple attempts when the transport retries; those attempts can each be
/// billed and therefore must never overwrite one another.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RequestUsageKey {
    pub session_id: String,
    #[serde(default = "default_request_actor")]
    pub actor_id: String,
    /// User-perceived exchange (ADR-0047 vocabulary).
    pub round: u64,
    /// Model request within the round.
    pub turn: u32,
    pub attempt: u32,
}

/// Canonical actor ID for the top-level session root agent (ADR-0183).
pub const ROOT_ACTOR_ID: &str = "root";

/// Whether the given actor ID represents the root agent (including legacy `"master"` records).
pub fn is_root_actor(actor_id: &str) -> bool {
    actor_id == ROOT_ACTOR_ID || actor_id == "master"
}

fn default_request_actor() -> String {
    ROOT_ACTOR_ID.to_string()
}

/// One request attempt's lifecycle and token accounting. This is the durable
/// fact from which provider/model and turn-level aggregates are derived.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUsageRecord {
    pub key: RequestUsageKey,
    pub provider: String,
    pub model: String,
    pub status: RequestUsageStatus,
    pub source: RequestUsageSource,
    /// Estimate of the exact pre-wire request input. Kept even after reported
    /// usage arrives so the UI can explain estimate-vs-provider drift.
    pub projected_prompt_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub cache_miss_tokens: i64,
    /// Provider-reported reasoning tokens (a diagnostic subset of
    /// `completion_tokens`, never additional billable volume). `0` when the
    /// upstream reports no such counter.
    #[serde(default)]
    pub reasoning_tokens: i64,
    /// Milliseconds the provider spent *generating* this attempt — measured
    /// from request dispatch to a validated assistant response, so it excludes
    /// tool execution and human-decision pauses. Together with
    /// `completion_tokens` this yields the attempt's honest output rate
    /// (`completion_tokens / generation_ms`). `0` for in-flight attempts,
    /// attempts that failed before any response was validated, and records
    /// persisted before this field existed (they deserialize to the default).
    #[serde(default)]
    pub generation_ms: u64,
    /// Structured latency/stream telemetry. `None` identifies legacy records
    /// without inventing zero-duration samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<RequestPerformance>,
    /// Epoch timestamp in milliseconds when this attempt was dispatched.
    #[serde(default)]
    pub started_at_ms: u64,
    /// Detailed failure reason / error payload if this attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RequestUsageRecord {
    /// A compact pushed snapshot for hint bars and other live surfaces.
    pub fn performance_snapshot(&self) -> Option<TurnPerformanceSnapshot> {
        Some(TurnPerformanceSnapshot {
            round: self.key.round,
            turn: self.key.turn,
            attempt: self.key.attempt,
            completion_tokens: self.completion_tokens.max(0) as u64,
            usage_source: self.source,
            performance: self.performance?,
        })
    }
}

/// Live, compact performance update for the latest settled model turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnPerformanceSnapshot {
    pub round: u64,
    pub turn: u32,
    pub attempt: u32,
    pub completion_tokens: u64,
    #[serde(default)]
    pub usage_source: RequestUsageSource,
    pub performance: RequestPerformance,
}

impl TurnPerformanceSnapshot {
    /// The streaming rate, using the attempt's own completion count (provider
    /// reported when available, local estimate otherwise).
    pub fn stream_tps(self) -> Option<f64> {
        self.performance.stream_tps(self.completion_tokens as i64)
    }

    /// Time to first output token in milliseconds.
    pub fn ttft_ms(self) -> Option<f64> {
        self.performance.ttft_us.map(|us| us as f64 / 1_000.0)
    }

    /// Whether the network attempt reused a pooled connection: `Some(true)`
    /// when the transport reported one, `Some(false)` when it reported a cold
    /// start, `None` when nothing was reported. See
    /// [`RequestPerformance::pooled_connection`].
    pub fn pooled_connection(self) -> Option<bool> {
        self.performance.pooled_connection()
    }
}

impl RequestUsageRecord {
    /// Physically implausible *implied output rate* (tokens/sec), used to
    /// detect records poisoned by the quadratic `observe_output` bug (a
    /// stream that re-counted every early token once per later delta). Real
    /// models peak in the low hundreds of tok/s (the fastest rate ever
    /// observed across this install's reported records is 138 tok/s); 10 000
    /// is ~70× that and still orders of magnitude below the bug's output
    /// (up to 172 134 tok/s). Only a measured `generation_ms` can express a
    /// rate, so untimed records keep the absolute companion ceiling below.
    pub const IMPLAUSIBLE_TOKENS_PER_SECOND: f64 = 10_000.0;

    /// Companion absolute ceiling for records with no measured generation
    /// span (legacy rows, or a failure before the clock sealed): no single
    /// assistant response reaches eight figures in tokens.
    pub const IMPLAUSIBLE_COMPLETION_TOKENS: i64 = 10_000_000;

    /// Clamp a poisoned estimated completion count in place, returning
    /// whether the record was repaired. Only estimated records are touched
    /// (a provider-reported count is authoritative by definition, however
    /// surprising), and only when the count is physically impossible —
    /// either its implied tokens/sec rate or, without a measured span, its
    /// absolute size. The repaired shape is `total = prompt,
    /// completion = 0`: the honest statement for an interrupted attempt
    /// whose stream was never validated is "no trustworthy completion
    /// count" — which renders as a `–` rate — not a fabricated
    /// millions-strong figure.
    pub fn sanitize_poisoned_estimate(&mut self) -> bool {
        if self.source != RequestUsageSource::Estimated || self.completion_tokens <= 0 {
            return false;
        }
        let implausible = if self.generation_ms > 0 {
            // Implied rate vs the physical ceiling.
            (self.completion_tokens as f64) * 1000.0 / (self.generation_ms as f64)
                > Self::IMPLAUSIBLE_TOKENS_PER_SECOND
        } else {
            self.completion_tokens > Self::IMPLAUSIBLE_COMPLETION_TOKENS
                || self.total_tokens > Self::IMPLAUSIBLE_COMPLETION_TOKENS
        };
        if !implausible {
            return false;
        }
        self.completion_tokens = 0;
        self.total_tokens = self.prompt_tokens;
        true
    }

    fn totals(&self) -> TokenSourceTotals {
        match self.source {
            RequestUsageSource::Reported => TokenSourceTotals {
                reported_tokens: self.total_tokens,
                prompt_tokens: self.prompt_tokens,
                completion_tokens: self.completion_tokens,
                cache_write_tokens: self.cache_write_tokens,
                cache_read_tokens: self.cache_read_tokens,
                cache_miss_tokens: self.cache_miss_tokens,
                reasoning_tokens: self.reasoning_tokens,
                ..Default::default()
            },
            RequestUsageSource::Estimated => TokenSourceTotals {
                estimated_tokens: self.total_tokens,
                ..Default::default()
            },
            RequestUsageSource::Unknown => TokenSourceTotals::default(),
        }
    }

    fn as_turn(&self) -> TokenTurn {
        TokenTurn {
            round: self.key.round,
            turn: self.key.turn,
            reported: self.source == RequestUsageSource::Reported,
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            cache_write_tokens: self.cache_write_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_miss_tokens: self.cache_miss_tokens,
            reasoning_tokens: self.reasoning_tokens,
        }
    }
}

/// One provider+model pair's accumulated token totals, split by source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceTotals {
    /// Tokens reported authoritatively by the provider's `usage` object.
    pub reported_tokens: i64,
    /// Tokens filled in by the local char-class estimator (provider reported
    /// no usage for those turns).
    pub estimated_tokens: i64,
    /// Reported input tokens (Anthropic: includes cache write+read). `0` for
    /// estimated turns, which carry no input/output split.
    pub prompt_tokens: i64,
    /// Reported output tokens. `0` for estimated turns.
    pub completion_tokens: i64,
    /// Tokens written to a prompt cache (Anthropic `cache_creation_input_tokens`
    /// — billed at a premium). A subset of `reported_tokens`, broken out so the
    /// report can show cache write volume and verify the breakpoints are
    /// creating cache entries.
    pub cache_write_tokens: i64,
    /// Tokens served from a prompt cache (Anthropic `cache_read_input_tokens` —
    /// billed at a ~0.1× discount). A subset of `reported_tokens`, broken out
    /// so the report can show cache hit volume (the payoff of caching).
    pub cache_read_tokens: i64,
    /// Provider-reported prompt-cache misses. A diagnostic subset of prompt
    /// input, not additional billable tokens.
    #[serde(default)]
    pub cache_miss_tokens: i64,
    /// Provider-reported reasoning tokens (OpenAI Responses / chat-completions
    /// details). A diagnostic subset of `completion_tokens`; `0` when the
    /// provider reports no such counter.
    #[serde(default)]
    pub reasoning_tokens: i64,
}

impl TokenSourceTotals {
    /// Total tokens regardless of source.
    pub fn total(&self) -> i64 {
        self.reported_tokens + self.estimated_tokens
    }

    /// Accumulate another entry's counts into this one.
    fn add(&mut self, other: TokenSourceTotals) {
        self.reported_tokens += other.reported_tokens;
        self.estimated_tokens += other.estimated_tokens;
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_miss_tokens += other.cache_miss_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
    }
}

/// One ReAct turn's token counts, kept per `(provider, model)` as a bill line
/// item. `round` identifies the enclosing user exchange; `turn` identifies the
/// model request within it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTurn {
    /// 1-based user-round index. `0` means unknown/legacy.
    #[serde(default)]
    pub round: u64,
    /// 1-based model-request index within the round. `0` means unknown/legacy.
    #[serde(default)]
    pub turn: u32,
    /// `true` = authoritative provider usage; `false` = local char-class estimate.
    pub reported: bool,
    /// Reported input tokens (includes cache write+read for Anthropic).
    pub prompt_tokens: i64,
    /// Reported output tokens.
    pub completion_tokens: i64,
    /// Total tokens booked this turn.
    pub total_tokens: i64,
    /// Anthropic `cache_creation_input_tokens` for this turn.
    pub cache_write_tokens: i64,
    /// Anthropic `cache_read_input_tokens` for this turn.
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub cache_miss_tokens: i64,
    /// Provider-reported reasoning tokens (diagnostic subset of
    /// `completion_tokens`). `0` when unreported.
    #[serde(default)]
    pub reasoning_tokens: i64,
}

/// Internal per-key accumulator: running totals plus the ordered line items.
#[derive(Debug, Default)]
struct Entry {
    totals: TokenSourceTotals,
    turns: Vec<TokenTurn>,
}

/// The key under which a provider+model's totals are accumulated: a
/// `(provider_id, model)` tuple so a session that switches providers or models
/// keeps each one's accuracy picture separate. Using a tuple (rather than a
/// `\u{1f}`-joined string) sidesteps any ambiguity when a provider/model value
/// happens to contain the separator.
fn key(provider: &str, model: &str) -> (String, String) {
    (provider.to_string(), model.to_string())
}

/// A thread-safe running ledger of token counts split by source (reported vs.
/// estimated), keyed by `(provider_id, model)`. Shared between the agent (the
/// writer — books each turn) and the TUI (the reader — renders the report).
#[derive(Default)]
pub struct TokenSourceLedger {
    /// `(provider, model)` → accumulator (totals + per-turn line items). A
    /// [`BTreeMap`] so the report iterates in a stable order.
    entries: Mutex<BTreeMap<(String, String), Entry>>,
    /// Lifecycle-aware request records. Legacy `record*` callers continue to
    /// use `entries`; production request accounting uses this keyed map so a
    /// terminal event updates exactly one attempt and duplicate events are
    /// idempotent.
    requests: Mutex<BTreeMap<RequestUsageKey, RequestUsageRecord>>,
    /// Session selected by the harness. `snapshot()` filters lifecycle records
    /// to this id, preventing usage from another opened session leaking into
    /// the current report.
    active_session: Mutex<Option<String>>,
    /// Optional durable mirror (ADR-0122): every terminally settled request
    /// is forwarded to this sink. `None` in tests / when no store is bound.
    usage_sink: Mutex<Option<Arc<dyn UsageStatSink>>>,
    dirty_usage: Mutex<std::collections::HashSet<RequestUsageKey>>,
    /// Project bucket name stamped onto sink records (empty = unknown).
    usage_project: Mutex<String>,
}

impl std::fmt::Debug for TokenSourceLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSourceLedger")
            .field("active_session", &self.active_session())
            .field("usage_project", &self.usage_snapshot())
            .finish_non_exhaustive()
    }
}

/// Parameters for recording the start of a network request attempt in [`TokenSourceLedger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeginRequestParams<'a> {
    pub session_id: &'a str,
    pub actor_id: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub round: u64,
    pub turn: u32,
    pub projected_prompt_tokens: i64,
}

impl TokenSourceLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cheap shared handle (the canonical way the agent and TUI share one
    /// ledger).
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn set_active_session(&self, session_id: impl Into<String>) {
        *self
            .active_session
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(session_id.into());
    }

    pub fn active_session(&self) -> Option<String> {
        self.active_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Install the durable usage-statistics sink (ADR-0122). Every
    /// terminally settled request attempt is forwarded to it from
    /// [`Self::settle_request`]. Replaces any prior sink.
    pub fn install_usage_sink(&self, sink: Arc<dyn UsageStatSink>) {
        *self.usage_sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    /// Stamp the project bucket name forwarded with sink records. Called by
    /// the driver on session open/switch so records group by project.
    pub fn set_usage_project(&self, project: impl Into<String>) {
        *self.usage_project.lock().unwrap_or_else(|e| e.into_inner()) = project.into();
    }

    /// Current project bucket name (test/diagnostics).
    pub fn usage_snapshot(&self) -> String {
        self.usage_project
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Insert an in-flight request and allocate the next attempt number for
    /// its `(session, actor, round, turn)` tuple.
    pub fn begin_request(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        round: u64,
        turn: u32,
        projected_prompt_tokens: i64,
    ) -> RequestUsageKey {
        self.begin_request_for_actor(BeginRequestParams {
            session_id,
            actor_id: "master",
            provider,
            model,
            round,
            turn,
            projected_prompt_tokens,
        })
    }

    pub fn begin_request_for_actor(&self, params: BeginRequestParams<'_>) -> RequestUsageKey {
        let BeginRequestParams {
            session_id,
            actor_id,
            provider,
            model,
            round,
            turn,
            projected_prompt_tokens,
        } = params;
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let attempt = requests
            .keys()
            .filter(|key| {
                key.session_id == session_id
                    && key.actor_id == actor_id
                    && key.round == round
                    && key.turn == turn
            })
            .map(|key| key.attempt)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let key = RequestUsageKey {
            session_id: session_id.to_string(),
            actor_id: actor_id.to_string(),
            round,
            turn,
            attempt,
        };
        requests.insert(
            key.clone(),
            RequestUsageRecord {
                key: key.clone(),
                provider: provider.to_string(),
                model: model.to_string(),
                status: RequestUsageStatus::InFlight,
                source: RequestUsageSource::Unknown,
                projected_prompt_tokens: projected_prompt_tokens.max(0),
                started_at_ms: now_epoch_ms(),
                ..Default::default()
            },
        );
        self.dirty_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone());
        key
    }

    /// Terminally settle one attempt. Replaying the same event is harmless;
    /// authoritative reported usage can upgrade an estimate, but an estimate
    /// can never downgrade an already reported record. `generation_ms` is the
    /// attempt's provider-generation span (request dispatch → validated
    /// response; `0` when none was measured) and backs the per-attempt output
    /// rate shown by the Context Usage modal.
    pub fn settle_request(
        &self,
        key: &RequestUsageKey,
        status: RequestUsageStatus,
        usage: Option<crate::TokenUsage>,
        estimated_completion_tokens: i64,
        generation_ms: u64,
    ) {
        self.settle_request_with_performance_and_error(
            key,
            status,
            usage,
            estimated_completion_tokens,
            generation_ms,
            None,
            None,
        );
    }

    /// Terminally settle one attempt with optional failure error payload.
    pub fn settle_request_with_error(
        &self,
        key: &RequestUsageKey,
        status: RequestUsageStatus,
        usage: Option<crate::TokenUsage>,
        estimated_completion_tokens: i64,
        generation_ms: u64,
        error: Option<String>,
    ) {
        self.settle_request_with_performance_and_error(
            key,
            status,
            usage,
            estimated_completion_tokens,
            generation_ms,
            None,
            error,
        );
    }

    /// Terminally settle one attempt with structured performance telemetry.
    /// The legacy `generation_ms` remains populated for wire/session
    /// compatibility; new performance surfaces use `performance` exclusively.
    #[allow(clippy::too_many_arguments)]
    pub fn settle_request_with_performance_and_error(
        &self,
        key: &RequestUsageKey,
        status: RequestUsageStatus,
        usage: Option<crate::TokenUsage>,
        estimated_completion_tokens: i64,
        generation_ms: u64,
        performance: Option<RequestPerformance>,
        error: Option<String>,
    ) {
        if !status.is_terminal() {
            return;
        }
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = requests.get_mut(key) else {
            return;
        };
        if record.status.is_terminal() && record.source == RequestUsageSource::Reported {
            return;
        }
        record.status = status;
        record.generation_ms = generation_ms;
        if performance.is_some() {
            record.performance = performance;
        }
        if error.is_some() {
            record.error = error;
        }
        if let Some(usage) = usage {
            record.source = RequestUsageSource::Reported;
            record.prompt_tokens = usage.prompt_tokens.max(0);
            record.completion_tokens = usage.completion_tokens.max(0);
            record.total_tokens = usage.total_tokens.max(0);
            record.cache_write_tokens = usage.cache_creation_input_tokens.max(0);
            record.cache_read_tokens = usage.cache_read_input_tokens.max(0);
            record.cache_miss_tokens = usage.cache_miss_input_tokens.max(0);
            record.reasoning_tokens = usage.reasoning_tokens.max(0);
        } else {
            record.source = RequestUsageSource::Estimated;
            record.prompt_tokens = record.projected_prompt_tokens.max(0);
            record.completion_tokens = estimated_completion_tokens.max(0);
            record.total_tokens = record
                .prompt_tokens
                .saturating_add(record.completion_tokens);
            // Belt-and-braces: a caller bug cannot be allowed to persist a
            // physically impossible streamed count (this exact class of bug
            // once booked 14.7M completion tokens for one interrupted
            // attempt, which rendered as a 130 050 tok/s rate). Silently
            // clamped — this crate carries no tracing dependency, and the
            // repair is visible in the report itself.
            record.sanitize_poisoned_estimate();
        }
        self.dirty_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone());
    }

    pub fn pending_records_for_session(&self, session_id: &str) -> Vec<RequestUsageRecord> {
        let requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let dirty = self.dirty_usage.lock().unwrap_or_else(|e| e.into_inner());
        // `dirty_usage` is a HashSet: iteration order is arbitrary. Persisting
        // in that order made the durable `request_usage_records` mirror
        // nondeterministic (and diverge from the sorted `records_for_session`
        // view), so sort into the stable display order before returning.
        let mut records: Vec<RequestUsageRecord> = dirty
            .iter()
            .filter(|key| key.session_id == session_id)
            .filter_map(|key| requests.get(key).cloned())
            .collect();
        records.sort_by(|a, b| request_display_order(a).cmp(&request_display_order(b)));
        records
    }

    pub fn acknowledge_records(&self, records: &[RequestUsageRecord]) {
        let requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let mut dirty = self.dirty_usage.lock().unwrap_or_else(|e| e.into_inner());
        for record in records {
            if requests.get(&record.key) == Some(record) {
                dirty.remove(&record.key);
            }
        }
    }

    pub async fn persist_request(&self, key: &RequestUsageKey) -> Result<(), String> {
        let sink = self
            .usage_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let record = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        if let (Some(sink), Some(record)) = (sink, record) {
            let project = self.usage_snapshot();
            sink.persist_usage(record.started_at_ms, &project, record.clone())
                .await?;
            self.acknowledge_records(&[record]);
        }
        Ok(())
    }

    pub async fn persist_pending(&self, session_id: &str) -> Result<(), String> {
        for record in self.pending_records_for_session(session_id) {
            self.persist_request(&record.key).await?;
        }
        Ok(())
    }

    /// Owned lifecycle records for one session, in stable request order.
    pub fn records_for_session(&self, session_id: &str) -> Vec<RequestUsageRecord> {
        let mut records = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|record| record.key.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|a, b| request_display_order(a).cmp(&request_display_order(b)));
        records
    }

    /// Replace one session's records from durable state. Any persisted
    /// in-flight request is crash residue and becomes `Abandoned` with an
    /// estimated prompt lower-bound before being exposed. Records persisted
    /// by the quadratic `observe_output` bug (see
    /// [`RequestUsageRecord::sanitize_poisoned_estimate`]) are repaired on
    /// load so a resumed session's report and rates stop showing the poison.
    pub fn restore_session(&self, session_id: &str, records: Vec<RequestUsageRecord>) {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        requests.retain(|key, _| key.session_id != session_id);
        for mut record in records {
            record.key.session_id = session_id.to_string();
            if record.status == RequestUsageStatus::InFlight {
                record.status = RequestUsageStatus::Abandoned;
                record.source = RequestUsageSource::Estimated;
                record.prompt_tokens = record.projected_prompt_tokens.max(0);
                record.total_tokens = record.prompt_tokens;
            }
            // Repair records persisted by the quadratic double-count bug
            // (silently — this crate carries no tracing dependency; the
            // repair is visible in the report itself, and the round/turn
            // identity stays intact).
            record.sanitize_poisoned_estimate();
            requests.insert(record.key.clone(), record);
        }
    }

    /// Book one turn as a line item — the single entry point all the public
    /// recorders funnel through. It appends the turn and folds it into the
    /// running totals. Non-positive totals are ignored; negative io/cache
    /// counts are clamped to zero.
    pub fn record_turn(&self, provider: &str, model: &str, turn: TokenTurn) {
        if turn.total_tokens <= 0 {
            return;
        }
        let turn = TokenTurn {
            prompt_tokens: turn.prompt_tokens.max(0),
            completion_tokens: turn.completion_tokens.max(0),
            cache_write_tokens: turn.cache_write_tokens.max(0),
            cache_read_tokens: turn.cache_read_tokens.max(0),
            cache_miss_tokens: turn.cache_miss_tokens.max(0),
            reasoning_tokens: turn.reasoning_tokens.max(0),
            ..turn
        };
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(key(provider, model)).or_default();
        if turn.reported {
            entry.totals.reported_tokens += turn.total_tokens;
            entry.totals.prompt_tokens += turn.prompt_tokens;
            entry.totals.completion_tokens += turn.completion_tokens;
            entry.totals.cache_write_tokens += turn.cache_write_tokens;
            entry.totals.cache_read_tokens += turn.cache_read_tokens;
            entry.totals.cache_miss_tokens += turn.cache_miss_tokens;
            entry.totals.reasoning_tokens += turn.reasoning_tokens;
        } else {
            entry.totals.estimated_tokens += turn.total_tokens;
        }
        entry.turns.push(turn);
    }

    /// Book one turn's token usage. When `reported` is `true`, the provider
    /// reported authoritative usage and `tokens` are real counts; when `false`,
    /// `tokens` are a local estimate.
    pub fn record(&self, provider: &str, model: &str, tokens: i64, reported: bool) {
        self.record_turn(
            provider,
            model,
            TokenTurn {
                reported,
                total_tokens: tokens,
                ..Default::default()
            },
        );
    }

    /// Book one turn's reported usage, including its prompt-cache split. The
    /// cache write/read counts are tracked as a breakout (they're already
    /// folded into `tokens` by the provider's usage parser); `cache_*` are
    /// clamped to non-negative. Callers with no caching pass `0, 0`.
    pub fn record_reported(
        &self,
        provider: &str,
        model: &str,
        tokens: i64,
        cache_write: i64,
        cache_read: i64,
    ) {
        self.record_turn(
            provider,
            model,
            TokenTurn {
                reported: true,
                total_tokens: tokens,
                cache_write_tokens: cache_write,
                cache_read_tokens: cache_read,
                ..Default::default()
            },
        );
    }

    /// The most recent *reported* turn for a `(provider, model)`, if any.
    ///
    /// Used by the TUI context meter as the authoritative anchor: the
    /// provider-reported `prompt_tokens` already measures the serialized
    /// request size (system prompt + every prior turn + tool schemas + per-
    /// message template overhead), which is more accurate than any local
    /// estimate of the transcript. `completion_tokens` is included because the
    /// assistant's last reply is now part of history.
    pub fn last_reported_turn(&self, provider: &str, model: &str) -> Option<TokenTurn> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(&key(provider, model))?;
        entry.turns.iter().rev().copied().find(|turn| turn.reported)
    }

    /// A snapshot of the ledger suitable for rendering (owned, no lock held).
    pub fn snapshot(&self) -> TokenSourceReport {
        let active_session = self.active_session();
        self.snapshot_filtered(active_session.as_deref())
    }

    pub fn snapshot_for_session(&self, session_id: &str) -> TokenSourceReport {
        self.snapshot_filtered(Some(session_id))
    }

    fn snapshot_filtered(&self, session_id: Option<&str>) -> TokenSourceReport {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<TokenSourceRow> = entries
            .iter()
            .map(|((provider, model), entry)| TokenSourceRow {
                provider: provider.to_string(),
                model: model.to_string(),
                totals: entry.totals,
                turns: entry.turns.clone(),
                requests: Vec::new(),
            })
            .collect();
        drop(entries);

        let requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        for record in requests.values().filter(|record| {
            session_id.is_none_or(|session_id| record.key.session_id == session_id)
        }) {
            let row = if let Some(row) = rows
                .iter_mut()
                .find(|row| row.provider == record.provider && row.model == record.model)
            {
                row
            } else {
                let index = rows.len();
                rows.push(TokenSourceRow {
                    provider: record.provider.clone(),
                    model: record.model.clone(),
                    totals: TokenSourceTotals::default(),
                    turns: Vec::new(),
                    requests: Vec::new(),
                });
                &mut rows[index]
            };
            row.requests.push(record.clone());
            if record.status.is_terminal() {
                row.totals.add(record.totals());
                row.turns.push(record.as_turn());
            }
        }
        rows.sort_by(|a, b| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)));
        for row in &mut rows {
            row.requests
                .sort_by(|a, b| request_display_order(a).cmp(&request_display_order(b)));
        }
        let grand_total =
            rows.iter()
                .map(|r| r.totals)
                .fold(TokenSourceTotals::default(), |mut acc, t| {
                    acc.add(t);
                    acc
                });
        TokenSourceReport { rows, grand_total }
    }
}

fn request_display_order(record: &RequestUsageRecord) -> (u64, u8, u32, u32, &str) {
    (
        record.key.round,
        u8::from(!is_root_actor(&record.key.actor_id)),
        record.key.turn,
        record.key.attempt,
        record.key.actor_id.as_str(),
    )
}

/// Wall-clock epoch milliseconds. Kept here (rather than at each call site)
/// so the usage-stat day bucket derives from one definition of "now".
fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One row of the report: a single provider+model and its source split.
///
/// Serialisable so an attached frontend can receive the server-side report
/// over the wire ([`crate::AgentResponse::TokenUsageReport`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceRow {
    pub provider: String,
    pub model: String,
    pub totals: TokenSourceTotals,
    /// The ordered per-turn line items behind `totals`.
    pub turns: Vec<TokenTurn>,
    /// Lifecycle-aware attempts behind this provider/model row.
    pub requests: Vec<RequestUsageRecord>,
}

/// A full snapshot of the ledger: per-row breakdown + a grand total.
///
/// Serialisable so an attached frontend can receive the server-side report
/// over the wire ([`crate::AgentResponse::TokenUsageReport`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceReport {
    pub rows: Vec<TokenSourceRow>,
    pub grand_total: TokenSourceTotals,
}

impl TokenSourceReport {
    /// Most recent completed primary-model attempt carrying structured
    /// performance telemetry. Subagent actors are deliberately excluded from
    /// the session hint: it describes the principal conversation's last
    /// model turn.
    pub fn latest_turn_performance(&self) -> Option<TurnPerformanceSnapshot> {
        self.rows
            .iter()
            .flat_map(|row| row.requests.iter())
            .filter(|record| {
                is_root_actor(&record.key.actor_id)
                    && record.status == RequestUsageStatus::Completed
                    && record.performance.is_some()
            })
            .max_by_key(|record| (record.key.round, record.key.turn, record.key.attempt))
            .and_then(RequestUsageRecord::performance_snapshot)
    }
}

/// Slice counterpart of [`TokenSourceReport::latest_turn_performance`] for
/// attach/resume paths that already hold the durable request records.
pub fn latest_turn_performance(records: &[RequestUsageRecord]) -> Option<TurnPerformanceSnapshot> {
    records
        .iter()
        .filter(|record| {
            is_root_actor(&record.key.actor_id)
                && record.status == RequestUsageStatus::Completed
                && record.performance.is_some()
        })
        .max_by_key(|record| (record.key.round, record.key.turn, record.key.attempt))
        .and_then(RequestUsageRecord::performance_snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Collecting sink for tests: records everything it receives.
    #[derive(Default)]
    struct CollectingSink {
        received: StdMutex<Vec<(u64, String, RequestUsageRecord)>>,
    }

    impl UsageStatSink for CollectingSink {
        fn persist_usage<'a>(
            &'a self,
            recorded_at_ms: u64,
            project: &'a str,
            record: RequestUsageRecord,
        ) -> futures::future::BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.received
                    .lock()
                    .unwrap()
                    .push((recorded_at_ms, project.to_string(), record));
                Ok(())
            })
        }
    }

    #[test]
    fn settled_requests_are_mirrored_to_the_usage_sink() {
        let ledger = TokenSourceLedger::new();
        let sink = Arc::new(CollectingSink::default());
        ledger.install_usage_sink(sink.clone());
        ledger.set_usage_project("bucket-42");

        let key = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
        ledger.settle_request(
            &key,
            RequestUsageStatus::Completed,
            Some(crate::TokenUsage {
                prompt_tokens: 900,
                completion_tokens: 100,
                total_tokens: 1_000,
                ..Default::default()
            }),
            0,
            2_000,
        );
        // A duplicate terminal event must not mirror twice (the reported
        // idempotency fence fires before the sink forward).
        ledger.settle_request(&key, RequestUsageStatus::Failed, None, 5, 0);

        futures::executor::block_on(ledger.persist_request(&key)).unwrap();
        let received = sink.received.lock().unwrap();
        assert_eq!(received.len(), 1, "one terminal settle → one sink record");
        assert_eq!(received[0].1, "bucket-42");
        assert_eq!(received[0].2.total_tokens, 1_000);
        assert_eq!(received[0].2.status, RequestUsageStatus::Completed);
    }

    #[test]
    fn ledger_without_sink_still_settles() {
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "openai", "gpt", 1, 1, 100);
        ledger.settle_request(&key, RequestUsageStatus::Failed, None, 10, 0);
        assert_eq!(ledger.records_for_session("s1").len(), 1);
    }

    #[test]
    fn lifecycle_attempts_are_keyed_idempotent_and_session_scoped() {
        let ledger = TokenSourceLedger::new();
        let first = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
        let retry = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
        let other = ledger.begin_request("s2", "anthropic", "claude", 1, 1, 500);
        let subagent = ledger.begin_request_for_actor(BeginRequestParams {
            session_id: "s1",
            actor_id: "subagent:call-1",
            provider: "openai",
            model: "gpt",
            round: 3,
            turn: 1,
            projected_prompt_tokens: 300,
        });
        assert_eq!(first.attempt, 1);
        assert_eq!(retry.attempt, 2);
        assert_eq!(other.attempt, 1);
        assert_eq!(subagent.attempt, 1, "a distinct actor has its own attempts");

        ledger.settle_request(&first, RequestUsageStatus::Failed, None, 25, 0);
        ledger.settle_request(
            &retry,
            RequestUsageStatus::Completed,
            Some(crate::TokenUsage {
                prompt_tokens: 990,
                completion_tokens: 110,
                total_tokens: 1_100,
                ..Default::default()
            }),
            0,
            2_000,
        );
        // A duplicate weaker terminal event cannot downgrade reported usage.
        ledger.settle_request(&retry, RequestUsageStatus::Failed, None, 999, 9_999);
        ledger.settle_request(&subagent, RequestUsageStatus::Completed, None, 30, 1_000);

        let report = ledger.snapshot_for_session("s1");
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].requests.len(), 3);
        assert_eq!(report.rows[0].totals.reported_tokens, 1_100);
        assert_eq!(report.rows[0].totals.estimated_tokens, 1_355);
        assert_eq!(
            report.rows[0].requests[1].status,
            RequestUsageStatus::Completed
        );
        assert_eq!(
            report.rows[0].requests[1].source,
            RequestUsageSource::Reported
        );
        // The per-attempt generation span is booked at settle, and the
        // idempotency fence keeps a replayed settle from overwriting it.
        assert_eq!(report.rows[0].requests[1].generation_ms, 2_000);
        assert_eq!(report.rows[0].requests[0].generation_ms, 0);
    }

    #[test]
    fn attempt_records_timestamp_and_error() {
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "p1", "m1", 1, 1, 500);
        assert!(ledger.records_for_session("s1")[0].started_at_ms > 0);

        ledger.settle_request_with_error(
            &key,
            RequestUsageStatus::Failed,
            None,
            0,
            120,
            Some("429 Too Many Requests: Rate limit exceeded".to_string()),
        );

        let records = ledger.records_for_session("s1");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, RequestUsageStatus::Failed);
        assert_eq!(records[0].generation_ms, 120);
        assert_eq!(
            records[0].error.as_deref(),
            Some("429 Too Many Requests: Rate limit exceeded")
        );
    }

    #[test]
    fn restore_marks_crash_residue_abandoned() {
        let ledger = TokenSourceLedger::new();
        let key = RequestUsageKey {
            session_id: "old".to_string(),
            actor_id: "master".to_string(),
            round: 4,
            turn: 2,
            attempt: 1,
        };
        ledger.restore_session(
            "restored",
            vec![RequestUsageRecord {
                key,
                provider: "relay".to_string(),
                model: "model".to_string(),
                status: RequestUsageStatus::InFlight,
                projected_prompt_tokens: 700,
                ..Default::default()
            }],
        );
        let records = ledger.records_for_session("restored");
        assert_eq!(records[0].status, RequestUsageStatus::Abandoned);
        assert_eq!(records[0].source, RequestUsageSource::Estimated);
        assert_eq!(records[0].prompt_tokens, 700);
        assert_eq!(records[0].total_tokens, 700);
    }

    /// The quadratic `observe_output` bug (summing `StreamingCounter::push`'s
    /// *running total* once per delta) persisted absurd completion counts on
    /// interrupted/failed attempts — e.g. a real turn booked 14 786 219
    /// completion tokens over 113 s and rendered as 130 050 tok/s. Both the
    /// load path and the settle path must repair such records, judging by the
    /// implied rate (real models peak ≈138 tok/s; the ceiling is 10 000).
    #[test]
    fn implausible_estimated_completion_is_repaired() {
        // Settle path: a caller passing a poisoned estimate is clamped.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "p1", "m1", 1, 1, 800);
        ledger.settle_request(
            &key,
            RequestUsageStatus::Interrupted,
            None,
            14_786_219,
            113_696,
        );
        let records = ledger.records_for_session("s1");
        assert_eq!(records[0].status, RequestUsageStatus::Interrupted);
        assert_eq!(records[0].completion_tokens, 0);
        assert_eq!(records[0].total_tokens, records[0].prompt_tokens);

        // A *small* poisoned count whose implied rate is still impossible
        // (2 393 tokens in 2.165 s → 1 105 tok/s... is under the 10 000
        // ceiling and survives; 9 500 in 2 s → 4 750 tok/s also survives).
        // The rate ceiling only fires far beyond physical reality, so these
        // remain — the ceiling catches the quadratic blow-up (which always
        // rockets past 10 000 tok/s within a few hundred deltas), not
        // merely-fast streams.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s2", "p1", "m1", 1, 1, 800);
        ledger.settle_request(&key, RequestUsageStatus::Interrupted, None, 9_500, 2_000);
        let records = ledger.records_for_session("s2");
        assert_eq!(records[0].completion_tokens, 9_500);

        // A count implying >10 000 tok/s is repaired even at modest size.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s3", "p1", "m1", 1, 1, 800);
        ledger.settle_request(&key, RequestUsageStatus::Failed, None, 25_000, 2_000);
        let records = ledger.records_for_session("s3");
        assert_eq!(records[0].completion_tokens, 0);
        assert_eq!(records[0].total_tokens, 800);

        // Untimed poison: an eight-figure count with no measured span is
        // repaired via the absolute companion ceiling.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s4", "p1", "m1", 1, 1, 800);
        ledger.settle_request(&key, RequestUsageStatus::Failed, None, 98_732_687, 0);
        let records = ledger.records_for_session("s4");
        assert_eq!(records[0].completion_tokens, 0);

        // Load path: a poisoned record persisted by an older build is
        // repaired on restore.
        let ledger = TokenSourceLedger::new();
        let poisoned = RequestUsageRecord {
            key: RequestUsageKey {
                session_id: "old".to_string(),
                actor_id: "master".to_string(),
                round: 1,
                turn: 44,
                attempt: 1,
            },
            provider: "relay".to_string(),
            model: "model".to_string(),
            status: RequestUsageStatus::Interrupted,
            source: RequestUsageSource::Estimated,
            projected_prompt_tokens: 64_572,
            prompt_tokens: 64_572,
            completion_tokens: 14_786_219,
            total_tokens: 14_850_791,
            generation_ms: 113_696,
            ..Default::default()
        };
        ledger.restore_session("repaired", vec![poisoned]);
        let records = ledger.records_for_session("repaired");
        assert_eq!(records[0].completion_tokens, 0);
        assert_eq!(records[0].total_tokens, 64_572);
        // The generation span survives — the *rate* column falls back to `–`
        // (zero completion), not a fabricated figure.

        // A plausible estimated completion is untouched.
        let plausible = RequestUsageRecord {
            key: RequestUsageKey {
                session_id: "old".to_string(),
                actor_id: "master".to_string(),
                round: 2,
                turn: 1,
                attempt: 1,
            },
            provider: "relay".to_string(),
            model: "model".to_string(),
            status: RequestUsageStatus::Completed,
            source: RequestUsageSource::Estimated,
            projected_prompt_tokens: 1_000,
            prompt_tokens: 1_000,
            completion_tokens: 2_400,
            total_tokens: 3_400,
            generation_ms: 40_000,
            ..Default::default()
        };
        ledger.restore_session("plausible", vec![plausible.clone()]);
        let restored = ledger.records_for_session("plausible");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].completion_tokens, 2_400);
        assert_eq!(restored[0].total_tokens, 3_400);

        // A provider-reported count is authoritative and never clamped.
        let reported = RequestUsageRecord {
            key: RequestUsageKey {
                session_id: "old".to_string(),
                actor_id: "master".to_string(),
                round: 3,
                turn: 1,
                attempt: 1,
            },
            provider: "relay".to_string(),
            model: "model".to_string(),
            status: RequestUsageStatus::Completed,
            source: RequestUsageSource::Reported,
            prompt_tokens: 50_000,
            completion_tokens: 12_000_000,
            total_tokens: 12_050_000,
            generation_ms: 600_000,
            ..Default::default()
        };
        ledger.restore_session("reported", vec![reported.clone()]);
        let restored = ledger.records_for_session("reported");
        assert_eq!(restored.len(), 1);
        // Authoritative provider counts are never clamped.
        assert_eq!(restored[0].completion_tokens, 12_000_000);
        assert_eq!(restored[0].total_tokens, 12_050_000);
    }

    #[test]
    fn records_reported_and_estimated_separately() {
        let ledger = TokenSourceLedger::new();
        ledger.record("openai", "gpt-4o", 100, true);
        ledger.record("openai", "gpt-4o", 50, false);
        let report = ledger.snapshot();
        assert_eq!(report.rows.len(), 1);
        let row = &report.rows[0];
        assert_eq!(row.provider, "openai");
        assert_eq!(row.model, "gpt-4o");
        assert_eq!(row.totals.reported_tokens, 100);
        assert_eq!(row.totals.estimated_tokens, 50);
        assert_eq!(row.totals.total(), 150);
    }

    #[test]
    fn separates_providers_and_models() {
        let ledger = TokenSourceLedger::new();
        ledger.record("openai", "gpt-4o", 100, true);
        ledger.record("google", "gemini-2.5", 80, true);
        ledger.record("kimi", "k2", 30, false);
        let report = ledger.snapshot();
        assert_eq!(report.rows.len(), 3);
        assert_eq!(report.grand_total.reported_tokens, 180);
        assert_eq!(report.grand_total.estimated_tokens, 30);
    }

    #[test]
    fn ignores_non_positive_tokens() {
        let ledger = TokenSourceLedger::new();
        ledger.record("openai", "gpt-4o", 0, true);
        ledger.record("openai", "gpt-4o", -5, false);
        assert!(ledger.snapshot().rows.is_empty());
    }

    #[test]
    fn snapshot_is_stable_order() {
        let ledger = TokenSourceLedger::new();
        ledger.record("zeta", "z1", 10, true);
        ledger.record("alpha", "a1", 10, true);
        let report = ledger.snapshot();
        // BTreeMap keeps alphabetical order by the composite key.
        assert_eq!(report.rows[0].provider, "alpha");
        assert_eq!(report.rows[1].provider, "zeta");
    }

    #[test]
    fn round_trips_provider_model_containing_the_old_separator() {
        // Regression: the old `\u{1f}`-joined string key would mis-split a
        // provider/model that itself contained the separator byte. A tuple key
        // makes the boundary structural and unambiguous.
        let ledger = TokenSourceLedger::new();
        ledger.record("custom\u{1f}relay", "model\u{1f}v2", 40, true);
        let row = &ledger.snapshot().rows[0];
        assert_eq!(row.provider, "custom\u{1f}relay");
        assert_eq!(row.model, "model\u{1f}v2");
        assert_eq!(row.totals.reported_tokens, 40);
    }

    #[test]
    fn record_reported_books_cache_breakout() {
        // The cache-aware overload folds write/read into reported_tokens AND
        // accumulates them as a separate breakout, so the report can show
        // hit-rate without losing the real billed total.
        let ledger = TokenSourceLedger::new();
        // Turn 1: a cache write (the first turn populates the cache).
        ledger.record_reported("anthropic", "claude-sonnet-4-5", 13200, 5000, 0);
        // Turn 2: a cache read (subsequent turn hits the cache).
        ledger.record_reported("anthropic", "claude-sonnet-4-5", 8200, 0, 8000);
        let row = &ledger.snapshot().rows[0];
        assert_eq!(
            row.totals.reported_tokens, 21400,
            "all reported tokens summed"
        );
        assert_eq!(row.totals.cache_write_tokens, 5000);
        assert_eq!(row.totals.cache_read_tokens, 8000);
        assert_eq!(row.totals.estimated_tokens, 0);
    }

    #[test]
    fn record_reported_clamps_negative_cache_counts() {
        // A malformed usage object shouldn't corrupt the ledger: negative cache
        // counts are clamped to zero rather than subtracting from the total.
        let ledger = TokenSourceLedger::new();
        ledger.record_reported("anthropic", "claude", 1000, -50, -10);
        let row = &ledger.snapshot().rows[0];
        assert_eq!(row.totals.reported_tokens, 1000);
        assert_eq!(row.totals.cache_write_tokens, 0);
        assert_eq!(row.totals.cache_read_tokens, 0);
    }

    #[test]
    fn record_reported_ignores_non_positive_total() {
        // Parity with the plain `record` guard: a zero/negative total is a
        // no-op even when cache counts are present.
        let ledger = TokenSourceLedger::new();
        ledger.record_reported("anthropic", "claude", 0, 100, 200);
        ledger.record_reported("anthropic", "claude", -5, 100, 200);
        assert!(ledger.snapshot().rows.is_empty());
    }

    #[test]
    fn grand_total_aggregates_cache_counters() {
        // `snapshot` folds cache counters into the grand total via `add`, so a
        // multi-provider report surfaces the session-wide cache hit volume.
        let ledger = TokenSourceLedger::new();
        ledger.record_reported("anthropic", "claude-opus", 5000, 1000, 3000);
        ledger.record_reported("openai", "gpt-4o", 2000, 0, 0);
        let report = ledger.snapshot();
        assert_eq!(report.grand_total.reported_tokens, 7000);
        assert_eq!(report.grand_total.cache_write_tokens, 1000);
        assert_eq!(report.grand_total.cache_read_tokens, 3000);
    }

    #[test]
    fn record_keeps_per_round_line_items() {
        // Each booking appends an ordered line item and splits input/output for
        // reported turns, powering the detail drill-in.
        let ledger = TokenSourceLedger::new();
        ledger.record_turn(
            "anthropic",
            "claude",
            TokenTurn {
                turn: 0,
                round: 0,
                reported: true,
                prompt_tokens: 1000,
                completion_tokens: 200,
                total_tokens: 1200,
                cache_write_tokens: 800,
                cache_read_tokens: 0,
                cache_miss_tokens: 0,
                reasoning_tokens: 0,
            },
        );
        ledger.record("anthropic", "claude", 50, false);
        let row = &ledger.snapshot().rows[0];
        assert_eq!(row.turns.len(), 2);
        assert!(row.turns[0].reported);
        assert_eq!(row.turns[0].prompt_tokens, 1000);
        assert_eq!(row.turns[0].completion_tokens, 200);
        assert!(!row.turns[1].reported);
        assert_eq!(row.turns[1].total_tokens, 50);
        assert_eq!(row.totals.prompt_tokens, 1000);
        assert_eq!(row.totals.completion_tokens, 200);
        assert_eq!(row.totals.reported_tokens, 1200);
        assert_eq!(row.totals.estimated_tokens, 50);
    }

    #[test]
    fn last_reported_turn_returns_most_recent_reported_for_key() {
        // The context meter anchors on the newest reported turn for the
        // active (provider, model). Estimated turns are skipped, other keys
        // are ignored, and the most-recent reported turn wins.
        let ledger = TokenSourceLedger::new();
        // Older reported turn for the active key.
        ledger.record_turn(
            "openai",
            "gpt-4o",
            TokenTurn {
                reported: true,
                prompt_tokens: 500,
                completion_tokens: 50,
                total_tokens: 550,
                ..Default::default()
            },
        );
        // A stray estimated turn for the active key must not be returned.
        ledger.record("openai", "gpt-4o", 40, false);
        // Noise in a different key.
        ledger.record_reported("anthropic", "claude", 9999, 0, 0);

        // Newest reported turn for the active key.
        ledger.record_turn(
            "openai",
            "gpt-4o",
            TokenTurn {
                reported: true,
                prompt_tokens: 4000,
                completion_tokens: 300,
                total_tokens: 4300,
                ..Default::default()
            },
        );

        let last = ledger
            .last_reported_turn("openai", "gpt-4o")
            .expect("a reported turn exists for the key");
        assert_eq!(last.prompt_tokens, 4000);
        assert_eq!(last.completion_tokens, 300);
        assert_eq!(last.total_tokens, 4300);

        // Missing key / never-reported key -> None.
        assert!(ledger.last_reported_turn("openai", "gpt-5").is_none());
        assert!(ledger.last_reported_turn("mistral", "large").is_none());
    }

    fn sample_performance() -> RequestPerformance {
        RequestPerformance {
            stream_ready_us: Some(41_000),
            ttft_us: Some(120_000),
            stream_us: Some(1_000_000),
            tail_us: Some(8_000),
            e2e_us: Some(1_128_000),
            streamed_output_tokens: 101,
            first_output_tokens: 1,
            output_events: 101,
            timing_source: PerformanceTimingSource::ClientObserved,
            stream_token_source: StreamTokenSource::Cl100k,
            ..Default::default()
        }
    }

    #[test]
    fn performance_settlement_stores_telemetry_and_rate_helpers() {
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "openai", "gpt-4o", 1, 1, 100);
        ledger.settle_request_with_performance_and_error(
            &key,
            RequestUsageStatus::Completed,
            Some(crate::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 101,
                total_tokens: 201,
                ..Default::default()
            }),
            0,
            1_128,
            Some(sample_performance()),
            None,
        );

        let record = &ledger.records_for_session("s1")[0];
        let performance = record.performance.expect("telemetry stored");
        assert_eq!(performance.ttft_us, Some(120_000));
        assert_eq!(performance.output_events, 101);

        // One rate: the attempt's completion count over the first→last token
        // span. 101 tokens over 1_000_000 µs → exactly 101 tok/s.
        let stream = performance
            .stream_tps(record.completion_tokens)
            .expect("stream rate");
        assert!((stream - 101.0).abs() < f64::EPSILON);
        // No provider-native telemetry yet → decode rate stays absent, never 0.
        assert_eq!(performance.provider_decode_tps(), None);

        // The hint-bar snapshot projection carries the same sample.
        let snapshot = record.performance_snapshot().expect("snapshot");
        assert_eq!(snapshot.round, 1);
        assert_eq!(snapshot.completion_tokens, 101);
        assert_eq!(snapshot.stream_tps(), Some(stream));
    }

    #[test]
    fn the_streaming_rate_is_one_division_with_honest_refusals() {
        // 100 tokens over 1 s → 100 tok/s. The reader can reproduce it.
        let normal = RequestPerformance {
            stream_us: Some(1_000_000),
            output_events: 101,
            ..Default::default()
        };
        assert_eq!(normal.stream_tps(100), Some(100.0));

        let snapshot = TurnPerformanceSnapshot {
            round: 1,
            turn: 1,
            attempt: 1,
            completion_tokens: 100,
            usage_source: RequestUsageSource::Reported,
            performance: normal,
        };
        assert_eq!(snapshot.stream_tps(), Some(100.0));

        // A single event has no span: `–`, never a fabricated rate.
        let single_chunk = RequestPerformance {
            stream_us: Some(0),
            output_events: 1,
            ..Default::default()
        };
        assert_eq!(single_chunk.stream_tps(100), None);

        // A sub-20 ms span is a burst, not a decode pace.
        let burst = RequestPerformance {
            stream_us: Some(5_000),
            output_events: 2,
            ..Default::default()
        };
        assert_eq!(burst.stream_tps(505), None);

        // No tokens → no rate.
        assert_eq!(normal.stream_tps(0), None);
        // A span the attempt never recorded → no rate.
        let untimed = RequestPerformance::default();
        assert_eq!(untimed.stream_tps(100), None);
    }

    #[test]
    fn a_silent_transport_never_claims_a_pooled_connection() {
        // Nothing reported: no reuse and no handshake may be asserted. This is
        // the shape every record in the wild had, which is why the timeline
        // used to read "reused warm pool connection" for every cold handshake.
        // A timing measured by the agent says nothing about the connection: an
        // absent `dns_us` here means nobody watched the socket, not that there
        // was nothing to watch. This is the record shape every attempt in the
        // wild had, and the reason the timeline used to call them all "reused
        // warm pool connection".
        let silent = RequestPerformance {
            stream_ready_us: Some(120_000),
            ttft_us: Some(300_000),
            stream_us: Some(1_000_000),
            output_events: 10,
            ..Default::default()
        };
        assert!(!silent.transport_observed());
        assert_eq!(silent.pooled_connection(), None);
        assert_eq!(
            TurnPerformanceSnapshot {
                performance: silent,
                ..Default::default()
            }
            .pooled_connection(),
            None
        );

        // Observed, and no setup paid: a real pooled socket.
        let pooled = RequestPerformance {
            connected_us: Some(390_000),
            request_sent_us: Some(400_000),
            stream_ready_us: Some(500_000),
            observation: TransportObservation::PooledConnection,
            ..Default::default()
        };
        assert!(pooled.transport_observed());
        assert_eq!(pooled.pooled_connection(), Some(true));

        // Observed, and setup paid: a real cold start.
        let cold = RequestPerformance {
            dns_us: Some(20_000),
            tcp_us: Some(40_000),
            tls_us: Some(90_000),
            connected_us: Some(150_000),
            request_sent_us: Some(400_000),
            observation: TransportObservation::ColdConnection,
            ..Default::default()
        };
        assert_eq!(cold.pooled_connection(), Some(false));

        // Observed, and the socket was sampled: `retransmits` is a measurement.
        let sampled = RequestPerformance {
            rtt_us: Some(42_000),
            observation: TransportObservation::PooledConnection,
            ..Default::default()
        };
        assert!(sampled.tcp_info_sampled());

        // Observed, but no `TCP_INFO`: a zero retransmit count on this record
        // would be an untouched field, not a clean socket. This is the pair the
        // renderer keys on.
        let unsampled = RequestPerformance {
            observation: TransportObservation::ColdConnection,
            retransmits: 0,
            ..Default::default()
        };
        assert!(!unsampled.tcp_info_sampled());
    }

    #[test]
    fn transport_observation_survives_a_json_round_trip_and_legacy_records_stay_unreported() {
        let perf = RequestPerformance {
            observation: TransportObservation::ColdConnection,
            connected_us: Some(150_000),
            ..Default::default()
        };
        let json = serde_json::to_string(&perf).expect("serialize");
        let restored: RequestPerformance = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, perf);

        // A record written before the field existed must not read as "observed":
        // that would restore exactly the fabrication the field removes.
        let legacy: RequestPerformance =
            serde_json::from_str(r#"{"ttft_us":300000,"output_events":4}"#).expect("legacy");
        assert_eq!(legacy.observation, TransportObservation::Unreported);
        assert_eq!(legacy.pooled_connection(), None);
        assert_eq!(legacy.connected_us, None);
    }

    #[test]
    fn legacy_settlement_keeps_performance_none_and_json_round_trips() {
        // Legacy path (no telemetry argument): the field must stay None, both
        // in memory and across a persist/reload JSON round trip that predates
        // the field (serde default).
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "openai", "gpt-4o", 1, 1, 100);
        ledger.settle_request(&key, RequestUsageStatus::Completed, None, 42, 900);
        let legacy = &ledger.records_for_session("s1")[0];
        assert!(legacy.performance.is_none());

        let json = serde_json::to_string(legacy).expect("serialize");
        let reloaded: RequestUsageRecord = serde_json::from_str(&json).expect("deserialize");
        assert!(reloaded.performance.is_none());
        assert_eq!(reloaded.generation_ms, 900);

        // A fresh JSON payload carrying telemetry round trips losslessly.
        let timed = RequestUsageRecord {
            key: legacy.key.clone(),
            status: RequestUsageStatus::Completed,
            source: RequestUsageSource::Reported,
            completion_tokens: 10,
            generation_ms: 500,
            performance: Some(sample_performance()),
            ..Default::default()
        };
        let json = serde_json::to_string(&timed).expect("serialize");
        let reloaded: RequestUsageRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(reloaded.performance, Some(sample_performance()));
    }

    #[test]
    fn reported_upgrade_preserves_fresh_but_not_stale_performance() {
        // Terminal replay rules: an estimated settlement followed by a
        // reported one upgrades counts — and the newer call's telemetry wins
        // because it describes the authoritative measurement pass. A
        // *reported* record, however, can never be rewritten by a later
        // estimate replay, so its telemetry survives untouched too.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "openai", "gpt-4o", 1, 1, 100);

        ledger.settle_request_with_performance_and_error(
            &key,
            RequestUsageStatus::Completed,
            None,
            40,
            800,
            Some(sample_performance()),
            None,
        );
        // Reported upgrade: usage + fresher telemetry replace the estimate.
        let upgraded = RequestPerformance {
            ttft_us: Some(90_000),
            ..sample_performance()
        };
        ledger.settle_request_with_performance_and_error(
            &key,
            RequestUsageStatus::Completed,
            Some(crate::TokenUsage {
                completion_tokens: 60,
                total_tokens: 160,
                ..Default::default()
            }),
            0,
            700,
            Some(upgraded),
            None,
        );
        let record = &ledger.records_for_session("s1")[0];
        assert_eq!(record.source, RequestUsageSource::Reported);
        assert_eq!(record.performance.map(|p| p.ttft_us), Some(Some(90_000)));

        // A later estimate replay can neither downgrade the usage nor the
        // settled telemetry.
        ledger.settle_request_with_performance_and_error(
            &key,
            RequestUsageStatus::Completed,
            None,
            99,
            5_000,
            None,
            None,
        );
        let record = &ledger.records_for_session("s1")[0];
        assert_eq!(record.source, RequestUsageSource::Reported);
        assert_eq!(record.completion_tokens, 60);
        assert_eq!(record.performance.map(|p| p.ttft_us), Some(Some(90_000)));
    }

    #[test]
    fn latest_turn_performance_picks_newest_completed_master_attempt() {
        let ledger = TokenSourceLedger::new();
        let older = ledger.begin_request("s1", "openai", "gpt-4o", 1, 1, 0);
        ledger.settle_request_with_performance_and_error(
            &older,
            RequestUsageStatus::Completed,
            None,
            10,
            100,
            Some(sample_performance()),
            None,
        );
        // A non-completed newer attempt must not become the hint sample.
        let failed = ledger.begin_request("s1", "openai", "gpt-4o", 2, 1, 0);
        ledger.settle_request_with_performance_and_error(
            &failed,
            RequestUsageStatus::Failed,
            None,
            0,
            50,
            Some(sample_performance()),
            Some("boom".to_string()),
        );
        // A subagent actor is excluded even when completed.
        let subagent = ledger.begin_request_for_actor(BeginRequestParams {
            session_id: "s1",
            actor_id: "subagent:call_1",
            provider: "openai",
            model: "gpt-4o",
            round: 2,
            turn: 2,
            projected_prompt_tokens: 0,
        });
        ledger.settle_request_with_performance_and_error(
            &subagent,
            RequestUsageStatus::Completed,
            None,
            500,
            400,
            Some(sample_performance()),
            None,
        );
        // Newest completed master attempt.
        let newest = ledger.begin_request("s1", "openai", "gpt-4o", 2, 3, 0);
        let newest_performance = RequestPerformance {
            ttft_us: Some(777_000),
            ..sample_performance()
        };
        ledger.settle_request_with_performance_and_error(
            &newest,
            RequestUsageStatus::Completed,
            None,
            20,
            300,
            Some(newest_performance),
            None,
        );

        let report = ledger.snapshot();
        let latest = report.latest_turn_performance().expect("a master sample");
        assert_eq!((latest.round, latest.turn), (2, 3));
        assert_eq!(latest.performance.ttft_us, Some(777_000));

        // Slice counterpart agrees for persisted-record paths.
        let records = ledger.records_for_session("s1");
        let slice_latest = latest_turn_performance(&records).expect("slice");
        assert_eq!(slice_latest, latest);

        // Nothing qualified → None rather than a fabricated default sample.
        let empty = TokenSourceLedger::new();
        assert!(empty.snapshot().latest_turn_performance().is_none());
    }
}
