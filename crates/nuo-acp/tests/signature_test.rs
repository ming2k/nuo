#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_acp::signature::HmacSigner;
use nuo_acp::{AgentAddress, AgentEnvelope, MessageIntent};
use std::time::Duration;

#[tokio::test]
async fn test_envelope_cryptographic_signing_and_verification() {
    let signer = HmacSigner::new("agent-dev-key", b"super-secret-key-12345");
    let untrusted_signer = HmacSigner::new("agent-evil-key", b"wrong-secret");

    let source = AgentAddress::parse("agent://local/dev").unwrap();
    let target = AgentAddress::parse("agent://local/kanban").unwrap();
    let mut envelope = AgentEnvelope::new(
        source,
        target,
        MessageIntent::delegate("Create issue for authentication failure"),
    );

    assert!(envelope.signature.is_none());

    // Sign the envelope
    envelope.sign_with(&signer).await.unwrap();
    assert!(envelope.signature.is_some());
    let sig = envelope.signature.as_ref().unwrap();
    assert_eq!(sig.key_id, "agent-dev-key");

    // Valid verification with max allowed clock skew of 60 seconds
    let verified = envelope
        .verify_with(&signer, Some(Duration::from_secs(60)))
        .await
        .unwrap();
    assert!(verified, "valid signature must verify successfully");

    // Verification fails with wrong key/secret
    let wrong_sig_verified = envelope
        .verify_with(&untrusted_signer, Some(Duration::from_secs(60)))
        .await
        .unwrap();
    assert!(
        !wrong_sig_verified,
        "untrusted signer must fail verification"
    );

    // Tampering test: tamper with intent payload
    let mut tampered = envelope.clone();
    tampered.intent = MessageIntent::delegate("Tampered malicious task");
    let tampered_verified = tampered
        .verify_with(&signer, Some(Duration::from_secs(60)))
        .await
        .unwrap();
    assert!(
        !tampered_verified,
        "tampered envelope must fail verification"
    );
}

/// Zero-trust admission policy (`AgentEnvelope::enforce`), the single source of
/// truth ported from the former `nuo-agent` runtime (ADR-0009).
#[tokio::test]
async fn strict_policy_rejects_unsigned_and_forged_envelopes() {
    use nuo_acp::{AdmissionRejection, SignatureEnforcement};

    let dev = HmacSigner::new("agent-dev-key", b"correct-shared-secret");
    let evil = HmacSigner::new("agent-evil-key", b"wrong-attacker-secret");
    let source = AgentAddress::parse("agent://local/dev").unwrap();
    let target = AgentAddress::parse("agent://local/secure-target").unwrap();
    let skew = Some(Duration::from_secs(300));

    // 1. Unsigned envelope -> rejected under Strict.
    let unsigned = AgentEnvelope::new(source.clone(), target.clone(), MessageIntent::delegate("x"));
    assert_eq!(
        unsigned.enforce(Some(&dev), SignatureEnforcement::Strict, skew).await,
        Err(AdmissionRejection::InvalidSignature),
        "Strict must reject an unsigned envelope"
    );

    // 2. Forged signature (untrusted key) -> rejected.
    let mut forged =
        AgentEnvelope::new(source.clone(), target.clone(), MessageIntent::delegate("y"));
    forged.sign_with(&evil).await.unwrap();
    assert_eq!(
        forged.enforce(Some(&dev), SignatureEnforcement::Strict, skew).await,
        Err(AdmissionRejection::InvalidSignature),
        "Strict must reject a forged signature"
    );

    // 3. Valid signature -> admitted.
    let mut valid =
        AgentEnvelope::new(source.clone(), target.clone(), MessageIntent::delegate("z"));
    valid.sign_with(&dev).await.unwrap();
    assert_eq!(
        valid.enforce(Some(&dev), SignatureEnforcement::Strict, skew).await,
        Ok(()),
        "a validly signed envelope must be admitted"
    );

    // 4. Strict with no verifier -> rejected.
    assert_eq!(
        valid.enforce(None, SignatureEnforcement::Strict, skew).await,
        Err(AdmissionRejection::NoVerifierConfigured)
    );
}

/// Permissive policy: unsigned is admitted, but a *bad* signature is still
/// rejected (a signer that gets it wrong is never silently trusted).
#[tokio::test]
async fn permissive_policy_admits_unsigned_but_rejects_forged() {
    use nuo_acp::SignatureEnforcement;

    let dev = HmacSigner::new("agent-dev-key", b"correct-shared-secret");
    let evil = HmacSigner::new("agent-evil-key", b"wrong-attacker-secret");
    let source = AgentAddress::parse("agent://local/dev").unwrap();
    let target = AgentAddress::parse("agent://local/target").unwrap();
    let skew = Some(Duration::from_secs(300));

    let unsigned = AgentEnvelope::new(source.clone(), target.clone(), MessageIntent::delegate("x"));
    assert!(
        unsigned
            .enforce(Some(&dev), SignatureEnforcement::Permissive, skew)
            .await
            .is_ok(),
        "Permissive admits an unsigned envelope"
    );

    let mut forged =
        AgentEnvelope::new(source.clone(), target.clone(), MessageIntent::delegate("y"));
    forged.sign_with(&evil).await.unwrap();
    assert!(
        forged
            .enforce(Some(&dev), SignatureEnforcement::Permissive, skew)
            .await
            .is_err(),
        "Permissive still rejects a present-but-invalid signature"
    );
}
