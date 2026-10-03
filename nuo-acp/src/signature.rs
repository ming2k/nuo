//! Cryptographic envelope signing and zero-trust identity verification.
//!
//! Provides deterministic canonicalization, anti-replay nonce/timestamp validation,
//! and non-repudiation signing for [`crate::AgentEnvelope`].

use crate::envelope::AgentEnvelope;
use crate::error::{ProtocolError, Result};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// Security enforcement policy for zero-trust envelope verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignatureEnforcement {
    /// Permissive: if a signature is present, verify it. Unsigned envelopes are accepted.
    #[default]
    Permissive,
    /// Strict zero-trust: all envelopes MUST possess a valid cryptographic signature.
    /// Unsigned or invalid envelopes are strictly rejected.
    Strict,
}

/// Cryptographic signature payload attached to an [`AgentEnvelope`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeSignature {
    /// Public identifier or fingerprint of the signing key.
    pub key_id: String,
    /// Hex-encoded cryptographic signature.
    pub signature: String,
    /// Unix epoch timestamp in seconds when the signature was created.
    pub timestamp: i64,
    /// Unique anti-replay nonce.
    pub nonce: String,
}

impl EnvelopeSignature {
    pub fn new(
        key_id: impl Into<String>,
        signature: impl Into<String>,
        timestamp: i64,
        nonce: impl Into<String>,
    ) -> Self {
        Self {
            key_id: key_id.into(),
            signature: signature.into(),
            timestamp,
            nonce: nonce.into(),
        }
    }
}

/// Computes a deterministic canonical byte sequence for an envelope under given timestamp and nonce.
pub fn canonical_bytes(envelope: &AgentEnvelope, timestamp: i64, nonce: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(envelope.id.as_bytes());
    out.push(b'|');
    out.extend_from_slice(envelope.source.as_str().as_bytes());
    out.push(b'|');
    out.extend_from_slice(envelope.target.as_str().as_bytes());
    out.push(b'|');
    if let Some(corr) = envelope.correlation_id {
        out.extend_from_slice(corr.as_bytes());
    }
    out.push(b'|');
    let intent_summary = envelope.intent.summary();
    out.extend_from_slice(intent_summary.as_bytes());
    out.push(b'|');
    out.extend_from_slice(timestamp.to_string().as_bytes());
    out.push(b'|');
    out.extend_from_slice(nonce.as_bytes());
    out
}

/// Asynchronous signer trait for producing verifiable envelope signatures.
#[async_trait]
pub trait EnvelopeSigner: Send + Sync {
    /// Identifier of the signing key.
    fn key_id(&self) -> &str;

    /// Produces a signature over canonical bytes.
    async fn sign(&self, canonical: &[u8]) -> Result<String>;
}

/// Asynchronous verifier trait for validating incoming envelope signatures.
#[async_trait]
pub trait EnvelopeVerifier: Send + Sync {
    /// Verifies the signature over canonical bytes for the specified key id.
    async fn verify(&self, key_id: &str, canonical: &[u8], signature: &str) -> Result<bool>;
}

/// Standard HMAC-SHA256 signer and verifier for zero-trust envelope exchange.
#[derive(Clone)]
pub struct HmacSigner {
    key_id: String,
    secret: Vec<u8>,
}

impl HmacSigner {
    pub fn new(key_id: impl Into<String>, secret: impl AsRef<[u8]>) -> Self {
        Self {
            key_id: key_id.into(),
            secret: secret.as_ref().to_vec(),
        }
    }
}

#[async_trait]
impl EnvelopeSigner for HmacSigner {
    fn key_id(&self) -> &str {
        &self.key_id
    }

    async fn sign(&self, canonical: &[u8]) -> Result<String> {
        let mut mac = HmacSha256::new_from_slice(&self.secret).map_err(|e| {
            ProtocolError::RoutingError(format!("HMAC key initialization failed: {e}"))
        })?;
        mac.update(canonical);
        let result = mac.finalize();
        Ok(hex::encode(result.into_bytes()))
    }
}

#[async_trait]
impl EnvelopeVerifier for HmacSigner {
    async fn verify(&self, key_id: &str, canonical: &[u8], signature: &str) -> Result<bool> {
        if key_id != self.key_id {
            return Ok(false);
        }
        let expected_bytes = match hex::decode(signature) {
            Ok(b) => b,
            Err(_) => return Ok(false),
        };
        let mut mac = match HmacSha256::new_from_slice(&self.secret) {
            Ok(m) => m,
            Err(_) => return Ok(false),
        };
        mac.update(canonical);
        Ok(mac.verify_slice(&expected_bytes).is_ok())
    }
}

impl AgentEnvelope {
    /// Signs this envelope in-place with the provided [`EnvelopeSigner`].
    pub async fn sign_with(&mut self, signer: &dyn EnvelopeSigner) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0))
            .as_secs() as i64;
        let nonce = uuid::Uuid::new_v4().to_string();
        let canonical = canonical_bytes(self, now, &nonce);
        let sig_str = signer.sign(&canonical).await?;

        self.signature = Some(EnvelopeSignature {
            key_id: signer.key_id().to_string(),
            signature: sig_str,
            timestamp: now,
            nonce,
        });

        Ok(())
    }

    /// Verifies the signature of this envelope with optional clock skew bound.
    pub async fn verify_with(
        &self,
        verifier: &dyn EnvelopeVerifier,
        max_skew: Option<Duration>,
    ) -> Result<bool> {
        let Some(sig) = &self.signature else {
            return Ok(false);
        };

        if let Some(skew) = max_skew {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::from_secs(0))
                .as_secs() as i64;
            let diff = now.abs_diff(sig.timestamp);
            if diff > skew.as_secs() {
                return Ok(false);
            }
        }

        let canonical = canonical_bytes(self, sig.timestamp, &sig.nonce);
        verifier
            .verify(&sig.key_id, &canonical, &sig.signature)
            .await
    }

    /// Applies a [`SignatureEnforcement`] policy to this envelope (ADR-0002
    /// `[INV-ACP-02]`).
    ///
    /// This is the **single source of truth** for the zero-trust admission
    /// decision; a runtime must call it rather than re-implementing the policy.
    ///
    /// - `Permissive`: an *unsigned* envelope is admitted; a *signed* envelope
    ///   must still verify (a bad signature is always rejected — a signer that
    ///   gets it wrong is not silently trusted).
    /// - `Strict`: every envelope must carry a valid signature. A missing
    ///   verifier, an unsigned envelope, an invalid signature, or a stale
    ///   timestamp is rejected.
    ///
    /// Returns `Ok(())` when admitted, or an [`AdmissionRejection`] naming why.
    pub async fn enforce(
        &self,
        verifier: Option<&dyn EnvelopeVerifier>,
        enforcement: SignatureEnforcement,
        max_skew: Option<Duration>,
    ) -> std::result::Result<(), AdmissionRejection> {
        match verifier {
            Some(verifier) => match self.verify_with(verifier, max_skew).await {
                Ok(true) => Ok(()),
                Ok(false) => {
                    // A present-but-invalid signature is always a rejection; an
                    // absent signature is a rejection only under Strict.
                    if enforcement == SignatureEnforcement::Strict || self.signature.is_some() {
                        Err(AdmissionRejection::InvalidSignature)
                    } else {
                        Ok(())
                    }
                }
                Err(err) => Err(AdmissionRejection::VerifyFailed(err.to_string())),
            },
            None => {
                if enforcement == SignatureEnforcement::Strict {
                    Err(AdmissionRejection::NoVerifierConfigured)
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// Why an envelope was rejected at the zero-trust admission boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionRejection {
    /// Strict policy is active but no verifier was configured.
    NoVerifierConfigured,
    /// The envelope's signature is missing (Strict) or invalid.
    InvalidSignature,
    /// Signature verification itself failed (malformed input, etc.).
    VerifyFailed(String),
}

impl std::fmt::Display for AdmissionRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoVerifierConfigured => {
                write!(f, "strict zero-trust policy active but no verifier configured")
            }
            Self::InvalidSignature => write!(f, "rejected by zero-trust policy: invalid signature"),
            Self::VerifyFailed(err) => write!(f, "signature verification failed: {err}"),
        }
    }
}

impl std::error::Error for AdmissionRejection {}
