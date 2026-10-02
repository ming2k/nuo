#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use acp::signature::HmacSigner;
use acp::{AgentAddress, AgentEnvelope, MessageIntent};
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
