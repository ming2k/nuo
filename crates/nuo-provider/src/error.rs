//! Runtime conversion from transport failures into the protocol-owned
//! provider error vocabulary.
//!
//! The error types themselves live in `nuo-protocol`; this module only maps
//! netune's rich diagnostic into its pure mirror so the contract crate does
//! not link the transport stack.

pub use nuo_protocol::error::*;

use crate::transport::TransportDiagnostic as NetuneDiagnostic;

fn mirror_diagnostic(diagnostic: NetuneDiagnostic) -> TransportDiagnostic {
    let phase = serde_json::to_value(diagnostic.phase())
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string());
    let retry = match diagnostic.retry() {
        netune_trace::RetryVerdict::Retryable => RetryVerdict::Retryable,
        netune_trace::RetryVerdict::Conditional => RetryVerdict::Conditional,
        netune_trace::RetryVerdict::Terminal => RetryVerdict::Terminal,
    };
    TransportDiagnostic {
        code: diagnostic.code().to_string(),
        phase,
        retry,
        message: diagnostic.message().to_string(),
        trace_id: diagnostic.trace_id().as_str().to_string(),
        dropped_events: diagnostic.dropped_events,
        dropped_diagnostics: diagnostic.dropped_diagnostics,
    }
}
impl From<crate::transport::TransportError> for ProviderError {
    fn from(err: crate::transport::TransportError) -> Self {
        match err {
            crate::transport::TransportError::Http {
                status,
                message,
                retry_after,
            } => {
                let kind = if status.as_u16() == 401 || status.as_u16() == 403 {
                    ProviderErrorKind::Authentication
                } else if status.as_u16() == 408 {
                    ProviderErrorKind::Timeout
                } else if status.as_u16() == 429 {
                    ProviderErrorKind::RateLimited
                } else if status.is_server_error() {
                    ProviderErrorKind::Upstream
                } else {
                    ProviderErrorKind::Protocol
                };
                let pe = ProviderError::new("egress", kind, message).with_status(status.as_u16());
                if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
                    pe.retryable(retry_after)
                } else {
                    pe
                }
            }
            crate::transport::TransportError::Transport {
                label,
                message,
                retryable,
                diagnostic,
            } => {
                let mut pe = ProviderError::new(label, ProviderErrorKind::Transport, message);
                if retryable {
                    pe = pe.retryable(None);
                }
                match diagnostic {
                    Some(diagnostic) => pe.with_diagnostic(mirror_diagnostic(*diagnostic)),
                    None => pe,
                }
            }
            crate::transport::TransportError::Timeout { label, diagnostic } => {
                let pe = ProviderError::new(
                    label,
                    ProviderErrorKind::Timeout,
                    "transport error: request timed out",
                )
                .retryable(None);
                match diagnostic {
                    Some(diagnostic) => pe.with_diagnostic(mirror_diagnostic(*diagnostic)),
                    None => pe,
                }
            }
            crate::transport::TransportError::InvalidRequest { label, message } => {
                ProviderError::new(label, ProviderErrorKind::InvalidRequest, message)
            }
            crate::transport::TransportError::Stream {
                label,
                message,
                retryable,
                diagnostic,
            } => {
                let mut pe = ProviderError::new(label, ProviderErrorKind::Transport, message);
                if retryable {
                    pe = pe.retryable(None);
                }
                match diagnostic {
                    Some(diagnostic) => pe.with_diagnostic(mirror_diagnostic(*diagnostic)),
                    None => pe,
                }
            }
            crate::transport::TransportError::Decode { label, message } => {
                ProviderError::new(label, ProviderErrorKind::Decode, message)
            }
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use crate::transport::TransportError;
    use netune_trace::{DiagnosticCode, DiagnosticPhase, RetryVerdict as NetuneRetry, TraceId};

    fn diagnostic() -> NetuneDiagnostic {
        NetuneDiagnostic {
            code: DiagnosticCode::EGRESS_TCP_FAILED,
            phase: DiagnosticPhase::Tcp,
            retry: NetuneRetry::Retryable,
            message: "connect failed: refused".into(),
            trace_id: TraceId::new("01924f4e-0000-7000-8000-000000000000"),
            dropped_events: 0,
            dropped_diagnostics: 0,
        }
    }

    #[test]
    fn the_transport_diagnosis_crosses_into_the_provider_error() {
        let error: ProviderError = TransportError::Transport {
            label: "Test",
            message: "connect failed: refused".into(),
            retryable: true,
            diagnostic: Some(Box::new(diagnostic())),
        }
        .into();

        assert_eq!(error.kind(), ProviderErrorKind::Transport);
        let carried = error
            .diagnostic()
            .expect("the provider error must retain the diagnosis");
        assert_eq!(carried.code(), "NET-E-TCP-FAILED");
        assert_eq!(carried.phase(), "tcp");
        assert!(carried.is_retryable());
        assert_eq!(carried.trace_id(), "01924f4e-0000-7000-8000-000000000000");
        assert!(
            error.to_string().contains("NET-E-TCP-FAILED"),
            "the rendered error must name the stable code: {error}"
        );
    }

    #[test]
    fn a_diagnosis_round_trips_through_the_serialized_error() {
        let error: ProviderError = TransportError::Timeout {
            label: "Test",
            diagnostic: Some(Box::new(NetuneDiagnostic {
                code: DiagnosticCode::EGRESS_TIMEOUT,
                phase: DiagnosticPhase::Request,
                retry: NetuneRetry::Retryable,
                message: "request timed out during request write".into(),
                trace_id: TraceId::new("01924f4e-0000-7000-8000-000000000001"),
                dropped_events: 2,
                dropped_diagnostics: 1,
            })),
        }
        .into();

        let json = serde_json::to_string(&error).expect("serialize");
        let decoded: ProviderError = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, error);
        assert_eq!(
            decoded.diagnostic().map(TransportDiagnostic::code),
            Some("NET-E-TIMEOUT")
        );
    }
}
