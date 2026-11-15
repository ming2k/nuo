//! Request lifecycle, performance, and transport telemetry types.

use super::*;

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportObservation {
    #[default]
    Unreported,
    PooledConnection,
    ColdConnection,
}

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportTimings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_sent_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_ready_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u64>,
    #[serde(default)]
    pub retransmits: u32,
    #[serde(default)]
    pub observation: TransportObservation,
    #[serde(skip)]
    pub dispatch_at: Option<std::time::Instant>,
}

/// Shared handle for recording and reading attempt telemetry.
#[derive(Debug, Clone, Default)]
pub struct TransportTelemetry(Arc<Mutex<Option<TransportTimings>>>);

impl TransportTelemetry {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    pub fn record(&self, timings: TransportTimings) {
        if let Ok(mut lock) = self.0.lock() {
            *lock = Some(timings);
        }
    }

    pub fn publish(&self, timings: TransportTimings) {
        self.record(timings);
    }

    pub fn snapshot(&self) -> Option<TransportTimings> {
        self.0.lock().ok().and_then(|lock| *lock)
    }

    pub fn read(&self) -> Option<TransportTimings> {
        self.snapshot()
    }
}

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
