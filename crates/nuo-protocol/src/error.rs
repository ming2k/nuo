//! Typed failures crossing provider, tool, and harness boundaries.
//!
//! The vocabulary is protocol-owned so lightweight layers (persistence, UI,
//! guardrails) can name a provider failure without compiling the transport
//! stack. The netune-backed diagnostic is mirrored as pure data: the numeric
//! code is converted to its stable symbol at the transport boundary, so
//! `nuo-protocol` never links `netune-trace`.

use serde::{Deserialize, Serialize};

/// A source-compatible mirror of the transport's retry verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryVerdict {
    /// A fresh attempt may succeed.
    Retryable,
    /// The same attempt would fail the same way.
    #[default]
    Terminal,
    /// Safe only under a caller's replay policy.
    Conditional,
}

/// Pure failure evidence attached to an outbound transport error.
///
/// `code` is the stable netune symbol (`NET-E-TCP-FAILED`), captured at the
/// boundary where the real diagnostic exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportDiagnostic {
    /// The stable netune code symbol.
    #[serde(deserialize_with = "deserialize_code")]
    pub code: String,
    /// Where in the transport the failure was raised (snake-case phase name).
    pub phase: String,
    /// Whether another attempt may succeed.
    #[serde(default)]
    pub retry: RetryVerdict,
    /// The bounded, producer-supplied account of the failure.
    pub message: String,
    /// The correlation id shared with the full trace, when one was captured.
    #[serde(default)]
    pub trace_id: String,
    /// Events evicted from the trace before the failure.
    #[serde(default)]
    pub dropped_events: u32,
    /// Diagnostics evicted from the side table before the failure.
    #[serde(default)]
    pub dropped_diagnostics: u32,
}

impl TransportDiagnostic {
    /// The stable, greppable code (`NET-E-TCP-FAILED`).
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The transport phase that produced the failure.
    pub fn phase(&self) -> &str {
        &self.phase
    }

    /// The producer's bounded account.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The trace id that joins this error to the full attempt trace.
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Whether the producer judged a fresh attempt safe.
    pub fn is_retryable(&self) -> bool {
        matches!(self.retry, RetryVerdict::Retryable)
    }
}

/// Accept both the current symbol form and the numeric form written by older
/// persisted errors.
fn deserialize_code<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(u64),
        Text(String),
    }
    Ok(match Raw::deserialize(deserializer)? {
        Raw::Number(number) => number.to_string(),
        Raw::Text(text) => text,
    })
}
/// Stable classification of a provider failure.
///
/// This enum is transport-independent so provider adapters can expose HTTP,
/// local-model, and future out-of-process failures through the same contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum ProviderErrorKind {
    Transport,
    Timeout,
    RateLimited,
    QuotaExhausted,
    Authentication,
    InvalidRequest,
    ContextOverflow,
    Upstream,
    Decode,
    Protocol,
    Unavailable,
    Other,
}

/// Whether and when a provider request may be attempted again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RetryDisposition {
    #[default]
    Never,
    Retry {
        retry_after_ms: Option<u64>,
    },
}

/// A machine-readable provider failure with a user-facing message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderError {
    provider: String,
    kind: ProviderErrorKind,
    status: Option<u16>,
    retry: RetryDisposition,
    message: String,
    /// The netune failure evidence, when this error crossed the transport
    /// boundary. Optional so old persisted errors decode unchanged; boxed so
    /// the common error path stays small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diagnostic: Option<Box<crate::error::TransportDiagnostic>>,
}

impl ProviderError {
    pub fn new(
        provider: impl Into<String>,
        kind: ProviderErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            kind,
            status: None,
            retry: RetryDisposition::Never,
            message: message.into(),
            diagnostic: None,
        }
    }

    pub fn retryable(mut self, retry_after_ms: Option<u64>) -> Self {
        self.retry = RetryDisposition::Retry { retry_after_ms };
        self
    }

    pub fn authentication(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::Authentication, message)
    }

    pub fn invalid_request(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::InvalidRequest, message)
    }

    pub fn protocol(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::Protocol, message)
    }

    pub fn context_overflow(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::ContextOverflow, message)
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub fn map_message(mut self, f: impl FnOnce(String) -> String) -> Self {
        self.message = f(self.message);
        self
    }

    pub fn with_retry_after_if_absent(mut self, retry_after_ms: Option<u64>) -> Self {
        if let RetryDisposition::Retry {
            retry_after_ms: ref mut existing,
        } = self.retry
            && existing.is_none()
        {
            *existing = retry_after_ms;
        }
        self
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub const fn kind(&self) -> ProviderErrorKind {
        self.kind
    }

    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    pub const fn retry_disposition(&self) -> RetryDisposition {
        self.retry
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Attach the netune diagnosis that produced this failure.
    pub fn with_diagnostic(mut self, diagnostic: crate::error::TransportDiagnostic) -> Self {
        self.diagnostic = Some(Box::new(diagnostic));
        self
    }

    /// The netune diagnosis carried by this failure, when one was recorded.
    ///
    /// `kind()` stays Nuo's provider-level classification; the diagnostic's
    /// `code()` is netune's stable transport-level contract. A caller that
    /// needs to explain *where* the request stopped reads the latter.
    pub fn diagnostic(&self) -> Option<&crate::error::TransportDiagnostic> {
        self.diagnostic.as_deref()
    }

    pub const fn is_context_overflow(&self) -> bool {
        matches!(self.kind, ProviderErrorKind::ContextOverflow)
    }

    /// Whether this failure is a **deterministic refusal of the request we just
    /// sent** — the only kind of failure from which the image-cause probe can
    /// draw a valid inference.
    ///
    /// The probe reasons: *re-send the same request with the attachments
    /// withheld; if that succeeds, the attachments were the cause.* That
    /// inference is only sound when **re-sending the identical bytes would have
    /// failed identically**. Two properties are therefore required, and the
    /// status codes below are chosen to satisfy both:
    ///
    /// 1. **Deterministic.** A transient failure breaks the inference outright,
    ///    because a re-send tends to succeed *regardless of what changed* — so
    ///    "succeeded after stripping" would be evidence of nothing, while the
    ///    harness would latch it as proof that the route rejects images. That is
    ///    why `408`, `429`, `5xx`, and transport/timeout failures are excluded
    ///    even though they are common. (They are also already retried with
    ///    backoff by the transport layer; the probe would race that recovery.)
    /// 2. **Caused by the payload.** An endpoint, method, or conflict failure is
    ///    deterministic but says nothing about the body, so probing it wastes a
    ///    round trip before the real error surfaces — which is why `404`, `405`,
    ///    `409`, and `410` are excluded despite arriving as
    ///    [`ProviderErrorKind::InvalidRequest`].
    ///
    /// What remains is the "this payload is unacceptable" family: `400` and
    /// `422` (validation), `413` (too large — a base64 image is the likeliest
    /// cause), and `415` (unsupported media type, the canonical image
    /// rejection).
    ///
    /// [`ProviderErrorKind::ContextOverflow`] is excluded on purpose: a
    /// too-long prompt has its own recovery (compaction), and letting it also
    /// arm the probe would withhold images to fix a problem that is not about
    /// them.
    ///
    /// An in-band refusal (HTTP 2xx carrying an `error` object) has no status to
    /// read, so the kinds only a request-shape refusal produces are accepted
    /// there. This is the *only* classification the image recovery consults, and
    /// it reads none of the vendor's prose.
    pub const fn is_request_refusal(&self) -> bool {
        if matches!(self.kind, ProviderErrorKind::ContextOverflow) {
            return false;
        }
        match self.status {
            Some(status) => matches!(status, 400 | 413 | 415 | 422),
            None => matches!(
                self.kind,
                ProviderErrorKind::InvalidRequest
                    | ProviderErrorKind::Protocol
                    | ProviderErrorKind::Other
            ),
        }
    }

    // Deliberately absent: any predicate that decides "was this refusal about
    // images?" from the vendor's *text* — an `is_image_rejection` reading
    // `error.message`, a marker list, an upstream code table. Every vendor
    // formats its error envelope differently and its prose drifts, so such a
    // predicate is a permanent maintenance liability whose failure modes are
    // both bad: a false negative re-bricks a session, a false positive withholds
    // a capability that works. The harness answers the question from an
    // **outcome differential** instead — retry the identical turn with the
    // attachments withheld and observe whether the refusal goes away
    // — which needs to understand no vendor format at all. The only
    // classification kept here is [`Self::is_request_refusal`], which derives
    // from the HTTP status the transport already mapped and reads none of the
    // vendor's prose.
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        // The code is the stable, greppable part of the failure; the message is
        // review context. Surface it so a transcript or log names the netune
        // phase without requiring the reader to parse the prose.
        if let Some(diagnostic) = &self.diagnostic {
            write!(f, " [{}]", diagnostic.code())?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

/// A typed harness error.
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    Provider(ProviderError),
    /// The active round was cancelled by the user.
    Interrupted,
    /// Any other terminal failure; the message is user-facing.
    Other(String),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(f),
            Self::Other(message) => f.write_str(message),
            Self::Interrupted => write!(f, "Interrupted"),
        }
    }
}

impl std::error::Error for HarnessError {}

impl From<String> for HarnessError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

impl From<ProviderError> for HarnessError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

#[cfg(test)]
mod request_refusal_tests {
    use super::*;

    /// A status-mapped failure, the shape the transport produces.
    fn http(status: u16, kind: ProviderErrorKind) -> ProviderError {
        ProviderError::new("mock", kind, "message").with_status(status)
    }

    #[test]
    fn payload_rejections_may_arm_the_image_probe() {
        // The "this payload is unacceptable" family: deterministic AND
        // body-caused, so the probe's inference is sound.
        for status in [400, 413, 415, 422] {
            assert!(
                http(status, ProviderErrorKind::InvalidRequest).is_request_refusal(),
                "{status} must be probeable"
            );
        }
    }

    #[test]
    fn transient_failures_are_never_probed_because_the_inference_would_be_invalid() {
        // THE decisive exclusion. These are not merely uninformative: a re-send
        // tends to succeed regardless of what changed, so "succeeded after
        // stripping the images" would prove nothing — yet the harness would latch
        // it as evidence that the route rejects images, permanently disabling a
        // working capability for the session.
        for (status, kind) in [
            (500, ProviderErrorKind::Upstream),
            (502, ProviderErrorKind::Upstream),
            (503, ProviderErrorKind::Upstream),
            (504, ProviderErrorKind::Upstream),
            (429, ProviderErrorKind::RateLimited),
            (408, ProviderErrorKind::Timeout),
            (503, ProviderErrorKind::Unavailable),
        ] {
            assert!(
                !http(status, kind).is_request_refusal(),
                "{status} is transient and must not arm the probe"
            );
        }
    }

    #[test]
    fn endpoint_and_auth_failures_are_deterministic_but_not_about_the_payload() {
        // These WOULD be safe (they recur, so the probe self-disproves and
        // latches nothing) but useless: probing them makes the user wait an
        // extra round trip — e.g. before seeing "model not found" after a typo.
        // Note they arrive as `InvalidRequest`, so a kind-based test would have
        // swept them in.
        for status in [404, 405, 409, 410] {
            assert!(
                !http(status, ProviderErrorKind::InvalidRequest).is_request_refusal(),
                "{status} is not caused by the request body"
            );
        }
        for status in [401, 403] {
            assert!(
                !http(status, ProviderErrorKind::Authentication).is_request_refusal(),
                "{status} is an authorization failure"
            );
        }
    }

    #[test]
    fn context_overflow_is_left_to_its_own_recovery() {
        // A too-long prompt is recovered by compaction. Arming the image probe
        // here would withhold images to fix a problem that is not about them —
        // and it arrives as 400/413/422, i.e. inside the probeable range.
        for status in [400, 413, 422] {
            assert!(
                !http(status, ProviderErrorKind::ContextOverflow).is_request_refusal(),
                "{status} with an overflow payload belongs to compaction"
            );
        }
    }

    #[test]
    fn in_band_refusals_fall_back_to_their_kind() {
        // A vendor that reports a refusal inside a 2xx body leaves no status to
        // read, so the kinds only a request-shape refusal produces are accepted.
        for kind in [
            ProviderErrorKind::InvalidRequest,
            ProviderErrorKind::Protocol,
            ProviderErrorKind::Other,
        ] {
            assert!(
                ProviderError::new("mock", kind, "in-band").is_request_refusal(),
                "{kind:?} without a status is an in-band refusal"
            );
        }
        // ...while a statusless transient kind still is not.
        for kind in [
            ProviderErrorKind::Transport,
            ProviderErrorKind::Timeout,
            ProviderErrorKind::RateLimited,
            ProviderErrorKind::Authentication,
            ProviderErrorKind::ContextOverflow,
            ProviderErrorKind::Upstream,
            ProviderErrorKind::Decode,
            ProviderErrorKind::Unavailable,
        ] {
            assert!(
                !ProviderError::new("mock", kind, "in-band").is_request_refusal(),
                "{kind:?} must never arm the probe"
            );
        }
    }
}
