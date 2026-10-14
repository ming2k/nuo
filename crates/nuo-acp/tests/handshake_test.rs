//! Protocol-level coverage for the ACP peer handshake / pairing flow
//! (`MessageIntent::Handshake` / `HandshakeAck`), exercised directly on the
//! fabric — no agent runtime involved.
//!
//! Ported from the former `nuo-agent/tests/p2p_lifecycle_and_pairing_test.rs`
//! (ADR-0009): the handshake/pairing protocol is owned by `nuo-acp`, so its
//! coverage must live here, independent of any cognitive runtime, so it
//! survives the retirement of the orphaned `nuo-agent` runtime.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_acp::*;
use std::time::Duration;
use uuid::Uuid;

fn manifest(uri: &str, name: &str, description: &str) -> AgentManifest {
    AgentManifest::new(AgentAddress::parse(uri).unwrap(), name, description)
}

/// A peer answers a `Handshake` with a `HandshakeAck` carrying its identity.
#[tokio::test]
async fn handshake_establishes_an_identity_pairing() {
    let room = Fabric::new("squad");
    let client = manifest("agent://local/client", "Client", "Initiates");
    let server = manifest("agent://local/server", "Server", "Serves");

    let client_addr = client.address.clone();
    let server_addr = server.address.clone();
    let server_instance = Uuid::new_v4();

    let mut server_mailbox = room.join(server, 16).await;
    let client_mailbox = room.join(client, 16).await;
    let client_handle = client_mailbox.handle();

    // Server answers the handshake with a confirmed association.
    let server_addr_for_reply = server_addr.clone();
    let responder = tokio::spawn(async move {
        let request = server_mailbox.recv().await.unwrap();
        assert!(matches!(request.intent, MessageIntent::Handshake(_)));
        let association = "assoc-1234".to_string();
        let ack = request.reply(
            server_addr_for_reply.clone(),
            MessageIntent::handshake_ack(
                server_instance,
                "pubkey-server",
                association.clone(),
                AgentManifest::new(server_addr_for_reply, "Server", "Serves"),
            ),
        );
        server_mailbox.send(ack).await.unwrap();
        association
    });

    let envelope = AgentEnvelope::new(
        client_addr.clone(),
        server_addr.clone(),
        MessageIntent::handshake(client_addr.clone(), Uuid::new_v4(), "pubkey-client", None),
    );
    let reply = client_handle
        .request(envelope, Duration::from_secs(5))
        .await
        .unwrap();

    let association = responder.await.unwrap();
    match reply.intent {
        MessageIntent::HandshakeAck(ack) => {
            assert_eq!(ack.instance_id, server_instance);
            assert_eq!(ack.association_id, association);
            assert_eq!(ack.manifest.name, "Server");
        }
        other => panic!("expected HandshakeAck, got {other:?}"),
    }
}
