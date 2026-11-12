//! Durable request usage records and token-source totals.

use super::*;

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

/// Canonical actor ID for the top-level session root agent.
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
    /// Precise financial cost in nano-USD (ADR-0065). `None` when pricing is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_nano_usd: Option<u64>,
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

    pub(super) fn totals(&self) -> TokenSourceTotals {
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

    pub(super) fn as_turn(&self) -> TokenTurn {
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
    pub(super) fn add(&mut self, other: TokenSourceTotals) {
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
pub(super) struct Entry {
    pub(super) totals: TokenSourceTotals,
    pub(super) turns: Vec<TokenTurn>,
}

/// The key under which a provider+model's totals are accumulated: a
/// `(provider_id, model)` tuple so a session that switches providers or models
/// keeps each one's accuracy picture separate. Using a tuple (rather than a
/// `\u{1f}`-joined string) sidesteps any ambiguity when a provider/model value
/// happens to contain the separator.
pub(super) fn key(provider: &str, model: &str) -> (String, String) {
    (provider.to_string(), model.to_string())
}
