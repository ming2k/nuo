//! Transport middleware pipeline for request transformation and stream framing (ADR-0267).
//!
//! Generic protocol clients (OpenAI, Claude, Gemini) execute this phased pipeline rather than
//! branching on provider names or dialects. Every wire customization (request envelope,
//! codec, cryptographic signature, stream unwrap) is partitioned into strictly ordered phases:
//!
//!   Phase 1: `EnvelopePhase` (Structural JSON body transformation)
//!   Phase 2: `BodyCodecPhase` (Payload byte encoding/compression)
//!   Phase 3: `RequestSignerPhase` (Final immutable request inspection and cryptographic signing)

use crate::request::RequestBuilder;
use nuo_contracts::{
    OpenAiChatDialect, PreflightValidator, ProviderError, ProviderErrorKind, ResolvedAuth,
};
use std::fmt;

/// Phase 1: Structural JSON body transformation (e.g. AgentChat envelope).
pub trait EnvelopePhase: Send + Sync {
    /// Reshape the canonical chat-completions body into the dialect's
    /// envelope, together with any headers the surface's model-binding table
    /// declares for the same identity (`X-Model-Key` etc.). One resolution,
    /// two consistent artifacts: the body slots and the header carriers are
    /// stamped from the same model identity in the same call.
    fn reshape_body(
        &self,
        body: &serde_json::Value,
    ) -> Result<ReshapedEnvelope, ProviderError>;
}

/// The envelope phase's two artifacts: the reshaped body and its header
/// carriers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReshapedEnvelope {
    pub body: serde_json::Value,
    /// Header carriers declared by the surface's model-binding table
    /// (e.g. Qoder's `X-Model-Key`/`X-Model-Source`).
    pub identity_headers: Vec<(String, String)>,
}

/// Phase 2: Payload byte encoding or compression.
pub trait BodyCodecPhase: Send + Sync {
    fn encode_body(&self, body_bytes: &[u8]) -> Result<Vec<u8>, ProviderError>;
}

/// Phase 3: Final immutable request inspection and cryptographic signing.
pub trait RequestSignerPhase: Send + Sync {
    /// Clone this signer as a boxed trait object, so a plan can carry its own
    /// header-stamping copy ([`OutboundPlan::stamp_headers`]).
    fn clone_box_signer(&self) -> Box<dyn RequestSignerPhase>;

    /// The request URL for this surface, derived from the executor's base URL
    /// and the resolved auth.
    ///
    /// Signed surfaces usually POST to a declared inference path rather than
    /// the plain base URL, so the signer — which already owns the surface's
    /// signed-path contract — also owns the URL rewrite (ADR-0271 §2). The
    /// auth argument lets a provider-owned identity carry a server-elected
    /// transport endpoint (e.g. Qoder's region map, integration doc §3.1a)
    /// that overrides the executor's pinned base. The default keeps the
    /// executor's URL.
    fn request_url(&self, base_url: &str, auth: &ResolvedAuth) -> String {
        let _ = auth;
        base_url.to_string()
    }

    fn sign_request(
        &self,
        req: RequestBuilder,
        body_bytes: &[u8],
        auth: &ResolvedAuth,
    ) -> Result<RequestBuilder, ProviderError>;
}

/// A fully planned outbound request (ADR-0271).
///
/// The pipeline produces this; the executor only executes it. Holding the
/// header stamper as a closure keeps the plan opaque — the executor never
/// learns what a dialect's wire looks like.
pub struct OutboundPlan {
    /// The absolute request URL.
    pub url: String,
    /// The final wire body bytes.
    pub body: Vec<u8>,
    /// Stamps every protocol header (auth, identity, signing) onto the
    /// builder. Consumes the builder so header order stays plan-owned.
    pub stamp_headers:
        Box<dyn FnOnce(RequestBuilder) -> Result<RequestBuilder, ProviderError> + Send>,
}

impl TransportPipeline {
    /// Plan the complete outbound request for `body` under `auth`.
    ///
    /// This is the *only* wire-shaping entry point the executor calls: it
    /// replaces the former `prepare_body`/`sign_request`/`rewrite_url` trio
    /// and the executor's wire-shape branch (ADR-0271 §1). `base_url` is the
    /// executor's plain endpoint URL — the seed every phase plans over; a
    /// shaped signer rewrites it to its surface's inference URL, a
    /// pass-through pipeline keeps it verbatim. The planned body is the
    /// canonical chat-completions JSON after envelope reshape and byte
    /// encoding.
    pub fn plan_request(
        &self,
        base_url: &str,
        body: &serde_json::Value,
        auth: &ResolvedAuth,
    ) -> Result<OutboundPlan, ProviderError> {
        // Phase 1 + 2: envelope reshape, then byte encoding.
        let reshaped = match &self.envelope_phase {
            Some(phase) => phase.reshape_body(body)?,
            None => ReshapedEnvelope {
                body: body.clone(),
                identity_headers: Vec::new(),
            },
        };
        let raw = serde_json::to_vec(&reshaped.body).map_err(|e| {
            ProviderError::new(
                "pipeline",
                ProviderErrorKind::Protocol,
                format!("failed to serialize body: {e}"),
            )
        })?;
        let body_bytes = match &self.codec_phase {
            Some(phase) => phase.encode_body(&raw)?,
            None => raw,
        };

        // Phase 3: the signer owns the URL and the header set.
        match &self.signer_phase {
            Some(signer) => {
                let url = signer.request_url(base_url, auth);
                let signer = signer.clone_box_signer();
                let auth = auth.clone();
                let body_for_headers = body_bytes.clone();
                let identity_headers = reshaped.identity_headers;
                let signer_header = move |mut req: RequestBuilder| {
                    req = signer.sign_request(req, &body_for_headers, &auth)?;
                    for (name, value) in &identity_headers {
                        req = req.header(name.as_str(), value.as_str());
                    }
                    Ok(req)
                };
                Ok(OutboundPlan {
                    url,
                    body: body_bytes,
                    stamp_headers: Box::new(signer_header),
                })
            }
            None => {
                // Pass-through wire: the plain chat-completions header
                // contract — dialect-attributed headers, then the bearer
                // (omitted for keyless credentials; some servers reject
                // `Bearer `). The executor's request builder previously
                // inlined this exact set via `request::headers`; the plan
                // now owns it (ADR-0271 §1).
                let token = auth.token.expose_secret().to_string();
                let token = (!token.trim().is_empty()).then_some(token);
                let dialect_headers: Vec<(String, String)> = self
                    .pass_through_headers
                    .as_ref()
                    .map(|f| f(&token.clone().unwrap_or_default()))
                    .unwrap_or_default();
                let stamp_headers = move |mut req: RequestBuilder| {
                    for (name, value) in &dialect_headers {
                        req = req.header(name.as_str(), value.as_str());
                    }
                    if let Some(token) = &token {
                        req = req.header(
                            http::header::AUTHORIZATION,
                            format!("Bearer {token}"),
                        );
                    }
                    Ok(req)
                };
                Ok(OutboundPlan {
                    url: base_url.to_string(),
                    body: body_bytes,
                    stamp_headers: Box::new(stamp_headers),
                })
            }
        }
    }
}

/// Upstream execution and latency metrics emitted by terminal events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamMetrics {
    pub first_token_duration_ms: Option<u64>,
    pub total_duration_ms: Option<u64>,
    pub server_duration_ms: Option<u64>,
}

/// Upstream fault returned inside an HTTP SSE envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFault {
    pub status_code: u16,
    pub message: String,
}

/// Algebraic outcome of parsing an inbound raw SSE event (ADR-0267).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransformedFrame {
    /// Standard LLM chunk payload (canonical JSON string).
    Delta(String),
    /// Control / heartbeat / false `[DONE]` frame to safely skip.
    Skip,
    /// Authoritative stream close with upstream duration and performance telemetry.
    Terminal(StreamMetrics),
    /// Upstream error wrapped inside an envelope (e.g. quota, auth expired).
    UpstreamFault(ProviderFault),
}

/// Intercepts and decodes raw streaming frames into canonical protocol events.
pub trait StreamTransformer: Send + Sync {
    /// Transforms an inbound raw SSE event into a canonical frame outcome.
    fn transform_event(
        &self,
        event_type: Option<&str>,
        data: &str,
    ) -> Result<TransformedFrame, ProviderError>;
}

/// Default pass-through stream transformer.
#[derive(Debug, Clone, Default)]
pub struct PassThroughStreamTransformer;

impl StreamTransformer for PassThroughStreamTransformer {
    fn transform_event(
        &self,
        _event_type: Option<&str>,
        data: &str,
    ) -> Result<TransformedFrame, ProviderError> {
        if data.trim() == "[DONE]" {
            Ok(TransformedFrame::Terminal(StreamMetrics::default()))
        } else {
            Ok(TransformedFrame::Delta(data.to_string()))
        }
    }
}

/// Composable transport pipeline for request and stream transformations.
pub struct TransportPipeline {
    envelope_phase: Option<Box<dyn EnvelopePhase>>,
    codec_phase: Option<Box<dyn BodyCodecPhase>>,
    signer_phase: Option<Box<dyn RequestSignerPhase>>,
    stream_transformer: Box<dyn StreamTransformer>,
    validators: Vec<Box<dyn PreflightValidator>>,
    /// Extra headers the pass-through wire stamps (dialect-attributed headers
    /// such as OpenRouter app attribution), supplied by the executor at
    /// construction; a shaped pipeline ignores it.
    pass_through_headers: Option<Box<dyn Fn(&str) -> Vec<(String, String)> + Send + Sync>>,
}

impl Default for TransportPipeline {
    fn default() -> Self {
        Self {
            envelope_phase: None,
            codec_phase: None,
            signer_phase: None,
            stream_transformer: Box::new(PassThroughStreamTransformer),
            validators: Vec::new(),
            pass_through_headers: None,
        }
    }
}

impl fmt::Debug for TransportPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportPipeline")
            .field("has_envelope", &self.envelope_phase.is_some())
            .field("has_codec", &self.codec_phase.is_some())
            .field("has_signer", &self.signer_phase.is_some())
            .field("validators", &self.validators.len())
            .finish()
    }
}

impl TransportPipeline {
    /// Create a fluent builder for phased pipeline construction.
    pub fn builder() -> TransportPipelineBuilder {
        TransportPipelineBuilder::default()
    }

    /// Perform preflight contract validation before an in-flight request.
    pub fn preflight_assert(&self, auth: &ResolvedAuth) -> Result<(), ProviderError> {
        for validator in &self.validators {
            validator
                .validate_auth(auth)
                .map_err(|msg| ProviderError::authentication("pipeline", msg))?;
        }
        Ok(())
    }

    /// Headers the pass-through wire stamps, resolved per-request from the
    /// bearer token (dialect-attributed headers; the token itself is stamped
    /// by the plan). Shaped pipelines ignore this.
    pub fn with_pass_through_headers(
        mut self,
        headers: impl Fn(&str) -> Vec<(String, String)> + Send + Sync + 'static,
    ) -> Self {
        self.pass_through_headers = Some(Box::new(headers));
        self
    }

    /// Unwrap an incoming stream frame into canonical frame outcome.
    pub fn transform_event(
        &self,
        event_type: Option<&str>,
        data: &str,
    ) -> Result<TransformedFrame, ProviderError> {
        self.stream_transformer.transform_event(event_type, data)
    }

    /// Unwrap an incoming payload for backward-compatible call sites.
    pub fn unwrap_payload(
        &self,
        data: &str,
        provider_label: &'static str,
    ) -> Result<String, ProviderError> {
        match self.transform_event(None, data)? {
            TransformedFrame::Delta(chunk) => Ok(chunk),
            TransformedFrame::Skip | TransformedFrame::Terminal(_) => Ok(String::new()),
            TransformedFrame::UpstreamFault(fault) => Err(ProviderError::new(
                provider_label,
                ProviderErrorKind::Upstream,
                format!(
                    "Upstream error ({}): {}",
                    fault.status_code, fault.message
                ),
            )),
        }
    }

    /// Create the appropriate default pipeline for an OpenAI chat completions dialect.
    pub fn for_openai_chat_dialect(_dialect: OpenAiChatDialect) -> Self {
        Self::default()
    }
}

/// Fluent builder for [`TransportPipeline`].
#[derive(Default)]
pub struct TransportPipelineBuilder {
    envelope_phase: Option<Box<dyn EnvelopePhase>>,
    codec_phase: Option<Box<dyn BodyCodecPhase>>,
    signer_phase: Option<Box<dyn RequestSignerPhase>>,
    stream_transformer: Option<Box<dyn StreamTransformer>>,
    validators: Vec<Box<dyn PreflightValidator>>,
    pass_through_headers: Option<Box<dyn Fn(&str) -> Vec<(String, String)> + Send + Sync>>,
}

impl TransportPipelineBuilder {
    pub fn with_envelope(mut self, phase: impl EnvelopePhase + 'static) -> Self {
        self.envelope_phase = Some(Box::new(phase));
        self
    }

    pub fn with_codec(mut self, phase: impl BodyCodecPhase + 'static) -> Self {
        self.codec_phase = Some(Box::new(phase));
        self
    }

    pub fn with_signer(mut self, phase: impl RequestSignerPhase + 'static) -> Self {
        self.signer_phase = Some(Box::new(phase));
        self
    }

    pub fn with_validator(mut self, validator: impl PreflightValidator + 'static) -> Self {
        self.validators.push(Box::new(validator));
        self
    }

    pub fn with_stream_transformer(
        mut self,
        transformer: impl StreamTransformer + 'static,
    ) -> Self {
        self.stream_transformer = Some(Box::new(transformer));
        self
    }

    /// Headers the pass-through wire stamps, resolved per-request from the
    /// bearer token (dialect-attributed headers; the token itself is stamped
    /// by the plan). Shaped pipelines ignore this.
    pub fn with_pass_through_headers(
        mut self,
        headers: impl Fn(&str) -> Vec<(String, String)> + Send + Sync + 'static,
    ) -> Self {
        self.pass_through_headers = Some(Box::new(headers));
        self
    }

    pub fn build(self) -> TransportPipeline {
        TransportPipeline {
            envelope_phase: self.envelope_phase,
            codec_phase: self.codec_phase,
            signer_phase: self.signer_phase,
            stream_transformer: self
                .stream_transformer
                .unwrap_or_else(|| Box::new(PassThroughStreamTransformer)),
            validators: self.validators,
            pass_through_headers: self.pass_through_headers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_stream_transformer_handles_delta_and_done() {
        let transformer = PassThroughStreamTransformer;

        let delta = transformer.transform_event(None, "hello world").unwrap();
        assert_eq!(delta, TransformedFrame::Delta("hello world".to_string()));

        let done = transformer.transform_event(None, "[DONE]").unwrap();
        assert_eq!(done, TransformedFrame::Terminal(StreamMetrics::default()));
    }
}
