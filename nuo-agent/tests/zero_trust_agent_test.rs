#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::MockProvider;
use acp::signature::HmacSigner;
use acp::{AgentAddress, AgentEnvelope, MessageIntent, SignatureEnforcement};
use std::sync::Arc;

#[tokio::test]
async fn test_agent_strict_zero_trust_envelope_enforcement() {
    let dev_signer = HmacSigner::new("agent-dev-key", b"correct-shared-secret");
    let evil_signer = HmacSigner::new("agent-evil-key", b"wrong-attacker-secret");
    let verifier = Arc::new(dev_signer.clone());

    let provider = MockProvider::new();
    provider.push_text("Task successfully executed").await;

    let target_addr = AgentAddress::parse("agent://local/secure-target").unwrap();
    let source_addr = AgentAddress::parse("agent://local/dev").unwrap();

    let agent = Agent::builder(target_addr.clone())
        .provider(provider)
        .with_zero_trust(verifier, SignatureEnforcement::Strict)
        .build()
        .await
        .unwrap();

    // 1. Send unsigned envelope -> MUST be rejected under Strict policy
    let unsigned_envelope = AgentEnvelope::new(
        source_addr.clone(),
        target_addr.clone(),
        MessageIntent::delegate("Run unauthorized task"),
    );
    let unsigned_res = agent.handle_envelope(&unsigned_envelope).await;
    assert!(
        unsigned_res.is_err(),
        "Strict zero-trust policy must reject unsigned envelope"
    );

    // 2. Send envelope signed with untrusted/wrong key -> MUST be rejected
    let mut evil_envelope = AgentEnvelope::new(
        source_addr.clone(),
        target_addr.clone(),
        MessageIntent::delegate("Run spoofed task"),
    );
    evil_envelope.sign_with(&evil_signer).await.unwrap();
    let evil_res = agent.handle_envelope(&evil_envelope).await;
    assert!(
        evil_res.is_err(),
        "Strict zero-trust policy must reject envelope with invalid key signature"
    );

    // 3. Send envelope signed with trusted key -> MUST be accepted and processed
    let mut valid_envelope = AgentEnvelope::new(
        source_addr.clone(),
        target_addr.clone(),
        MessageIntent::delegate("Run trusted task"),
    );
    valid_envelope.sign_with(&dev_signer).await.unwrap();
    let valid_res = agent.handle_envelope(&valid_envelope).await;
    assert!(
        valid_res.is_ok(),
        "Envelope with valid cryptographic signature must be accepted"
    );
}
