//! Qoder's SSE stream frame decoder and HTTP envelope unwrapper.

use nuo_model_codec::ProviderError;
use crate::pipeline::{ProviderFault, StreamMetrics, StreamTransformer, TransformedFrame};
use serde_json::Value;

/// An unwrapped Qoder SSE event envelope.
#[derive(Debug, PartialEq, Eq)]
pub enum Envelope {
    /// A regular chat completion delta JSON.
    Chunk(String),
    /// An internal `[DONE]` marker or heartbeat inside the envelope to be skipped.
    Done,
    /// An empty frame or non-chunk payload.
    Skip,
    /// Upstream error payload.
    Error(u16, String),
}

/// Unwrap a Qoder HTTP envelope from raw SSE `data: ...` payload.
pub fn unwrap_envelope(payload: &str) -> Envelope {
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return Envelope::Chunk(payload.to_string());
    };

    let Some(status) = value.get("statusCodeValue").and_then(Value::as_u64) else {
        return Envelope::Chunk(payload.to_string());
    };

    let status = status as u16;
    if status != 200 {
        let message = value
            .get("body")
            .and_then(|b| b.as_str())
            .or_else(|| value.get("message").and_then(|m| m.as_str()))
            .unwrap_or("unknown error")
            .to_string();
        return Envelope::Error(status, message);
    }

    match value.get("body") {
        Some(Value::String(body)) => {
            if body.trim() == "[DONE]" {
                Envelope::Done
            } else {
                Envelope::Chunk(body.clone())
            }
        }
        Some(Value::Object(_)) => Envelope::Chunk(value["body"].to_string()),
        _ => Envelope::Skip,
    }
}

/// QoderStreamTransformer implementing `StreamTransformer`.
#[derive(Debug, Clone, Default)]
pub struct QoderStreamTransformer;

impl StreamTransformer for QoderStreamTransformer {
    fn transform_event(
        &self,
        event_type: Option<&str>,
        data: &str,
    ) -> Result<TransformedFrame, ProviderError> {
        if event_type == Some("finish") {
            let mut metrics = StreamMetrics::default();
            if let Ok(val) = serde_json::from_str::<Value>(data) {
                metrics.first_token_duration_ms =
                    val.get("firstTokenDuration").and_then(|v| v.as_u64());
                metrics.total_duration_ms = val.get("totalDuration").and_then(|v| v.as_u64());
                metrics.server_duration_ms =
                    val.get("serverDuration").and_then(|v| v.as_u64());
            }
            return Ok(TransformedFrame::Terminal(metrics));
        }

        match unwrap_envelope(data) {
            Envelope::Chunk(inner) => Ok(TransformedFrame::Delta(inner)),
            Envelope::Done | Envelope::Skip => Ok(TransformedFrame::Skip),
            Envelope::Error(status, message) => {
                Ok(TransformedFrame::UpstreamFault(ProviderFault {
                    status_code: status,
                    message,
                }))
            }
        }
    }
}
