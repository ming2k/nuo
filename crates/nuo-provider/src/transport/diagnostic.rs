//! Failure evidence attached to an outbound transport error.
//!
//! netune v0.0.26 made failure narration part of the trace and made every
//! public attempt return the evidence it recorded. Nuo keeps the hot path
//! unchanged and carries only the cold-path diagnosis across its transport
//! boundary: the stable code, the phase, the retry verdict, the bounded
//! message, the trace id, and the drop counts that bound the claim.

use netune_trace::{
    DiagnosticCode, DiagnosticPhase, DiagnosticRecord, Fidelity, RetryVerdict, TraceCollector,
    TraceId, TraceSnapshot,
};

/// The netune diagnosis attached to one failed transport attempt.
///
/// This is deliberately smaller than a [`TraceSnapshot`]: the full event log
/// belongs to the per-attempt trace sink, not to an error value that callers
/// clone and render. The trace id joins the two records.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransportDiagnostic {
    /// The stable netune code. Its numeric value is the persisted contract.
    pub code: DiagnosticCode,
    /// Where in the transport the failure was raised.
    pub phase: DiagnosticPhase,
    /// Whether another attempt may succeed.
    pub retry: RetryVerdict,
    /// The bounded, producer-supplied account of the failure.
    pub message: String,
    /// The correlation id shared with the full trace, when one was captured.
    pub trace_id: TraceId,
    /// Events evicted from the trace before the failure. Non-zero means the
    /// record is a window, not the whole story.
    pub dropped_events: u32,
    /// Diagnostics evicted from the side table before the failure.
    pub dropped_diagnostics: u32,
}

impl TransportDiagnostic {
    pub(crate) fn from_record(record: DiagnosticRecord, snapshot: &TraceSnapshot) -> Self {
        Self {
            code: record.code,
            phase: record.phase,
            retry: record.retry,
            message: record.message,
            trace_id: snapshot.meta.id.clone(),
            dropped_events: snapshot.dropped_events(),
            dropped_diagnostics: snapshot.dropped_diagnostics(),
        }
    }

    /// Read the terminal diagnostic from a snapshot, if the attempt recorded
    /// one.
    pub(crate) fn from_snapshot(snapshot: &TraceSnapshot) -> Option<Self> {
        snapshot
            .diagnostics
            .last_error()
            .cloned()
            .map(|record| Self::from_record(record, snapshot))
    }

    /// The stable, greppable code (`NET-E-TCP-FAILED`).
    pub fn code(&self) -> DiagnosticCode {
        self.code
    }

    /// The transport phase that produced the failure.
    pub fn phase(&self) -> DiagnosticPhase {
        self.phase
    }

    /// The retry verdict recorded by the producer.
    pub fn retry(&self) -> RetryVerdict {
        self.retry
    }

    /// The producer's bounded account.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The trace id that joins this error to the full attempt trace.
    pub fn trace_id(&self) -> &TraceId {
        &self.trace_id
    }

    /// Whether the producer judged a fresh attempt safe.
    pub fn is_retryable(&self) -> bool {
        matches!(self.retry, RetryVerdict::Retryable)
    }
}

/// The phase reached by the last transport event the collector recorded.
///
/// This delegates to netune's pure `diagnose` rather than maintaining a second
/// phase mapping. It exists because a caller-side deadline cancels the netune
/// future before the substrate can record its own terminal diagnostic; the
/// event log still names the phase the attempt was waiting in.
pub(crate) fn last_phase(collector: &TraceCollector) -> DiagnosticPhase {
    let meta = collector.meta();
    let log = collector.log();
    let diagnostics = collector.diagnostics();
    netune_trace::diagnose(&meta, Fidelity::l1(), &log, &diagnostics).last_phase
}

/// A short human label for a diagnostic phase.
pub(crate) fn phase_label(phase: DiagnosticPhase) -> &'static str {
    match phase {
        DiagnosticPhase::Dispatch => "request setup",
        DiagnosticPhase::Dns => "DNS resolution",
        DiagnosticPhase::Tcp => "TCP connection",
        DiagnosticPhase::Tls => "TLS handshake",
        DiagnosticPhase::Proxy => "proxy tunnel",
        DiagnosticPhase::Pool => "connection pool",
        DiagnosticPhase::Http1 => "HTTP/1.1 response",
        DiagnosticPhase::Http2 => "HTTP/2 response",
        DiagnosticPhase::Quic => "QUIC connection",
        DiagnosticPhase::Http3 => "HTTP/3 response",
        DiagnosticPhase::WebSocket => "WebSocket upgrade",
        DiagnosticPhase::Request => "request write",
        DiagnosticPhase::Response => "response body",
        DiagnosticPhase::Server => "server handler",
        DiagnosticPhase::Listener => "listener",
        DiagnosticPhase::Drain => "drain",
        DiagnosticPhase::Sink => "trace sink",
        DiagnosticPhase::Trace => "trace recording",
        DiagnosticPhase::Unknown => "request",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netune_trace::EventKind;

    #[test]
    fn the_last_event_names_the_phase() {
        let collector = TraceCollector::start(8);
        collector.mark(EventKind::Dispatch, 0, 0);
        collector.mark(EventKind::TlsEnd, 0, 0);
        collector.mark(EventKind::RequestWriteEnd, 0, 0);
        assert_eq!(last_phase(&collector), DiagnosticPhase::Request);

        collector.mark(EventKind::HeadComplete, 0, 0);
        assert_eq!(last_phase(&collector), DiagnosticPhase::Response);
    }

    #[test]
    fn an_empty_log_is_dispatch() {
        assert_eq!(
            last_phase(&TraceCollector::start(4)),
            DiagnosticPhase::Dispatch
        );
    }
}
