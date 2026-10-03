//! Cross-host collaboration: two routers, each owning a local agent, bridged by
//! a transport link. This is the "agents on different hosts" topology — the
//! in-process room tests do not exercise the gateway path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use nuo_acp::{
    AgentAddress, AgentEnvelope, AgentRouter, Mailbox, MailboxHandle, MessageIntent,
    TransportBridge, attach_outbound, in_memory_pair,
};
use std::time::Duration;

/// A node in the test topology: one router plus the members registered on it.
struct Node {
    router: AgentRouter,
    members: HashMap<String, Mailbox>,
}

impl Node {
    fn new() -> Self {
        Self {
            router: AgentRouter::new(),
            members: HashMap::new(),
        }
    }

    /// Registers a member, returning a handle for sending on its behalf.
    async fn join(&mut self, alias: &str, uri: &str) -> MailboxHandle {
        let address = AgentAddress::parse(uri).unwrap();
        let mailbox = self.router.register(address, 32).await;
        let handle = mailbox.handle();
        self.members.insert(alias.to_string(), mailbox);
        handle
    }

    fn take(&mut self, alias: &str) -> Mailbox {
        self.members
            .remove(alias)
            .expect("member should be present")
    }
}

#[tokio::test]
async fn agents_on_separate_hosts_delegate_across_a_transport_bridge() {
    // ---------------------------------------------------------------
    // Two hosts, each with its own router and address namespace.
    // ---------------------------------------------------------------
    let mut host_a = Node::new();
    let mut host_b = Node::new();

    // The dev agent lives on host A, the kanban agent on host B.
    let dev_addr = AgentAddress::parse("agent://host-a/dev").unwrap();
    let kanban_addr = AgentAddress::parse("agent://host-b/kanban").unwrap();

    let dev_mailbox = host_a.join("dev", "agent://host-a/dev").await;
    let _kanban_mailbox = host_b.join("kanban", "agent://host-b/kanban").await;
    let mut kanban_inbox = host_b.take("kanban");

    // ---------------------------------------------------------------
    // Wire the bridge: each host forwards the other's address space, and each
    // inbound pump injects the peer's traffic back into the local router.
    // ---------------------------------------------------------------
    let (link_a, link_b) = in_memory_pair(64);

    // host A -> host B
    attach_outbound(&host_a.router, "agent://host-b/", link_a.sender.clone()).await;
    TransportBridge::new("agent://host-a/").spawn_inbound(host_a.router.clone(), link_a);

    // host B -> host A
    attach_outbound(&host_b.router, "agent://host-a/", link_b.sender.clone()).await;
    TransportBridge::new("agent://host-b/").spawn_inbound(host_b.router.clone(), link_b);

    // ---------------------------------------------------------------
    // The remote peer answers the delegation.
    // ---------------------------------------------------------------
    let responder = {
        let expected_source = dev_addr.clone();
        let reply_from = kanban_addr.clone();
        tokio::spawn(async move {
            let request = tokio::time::timeout(Duration::from_secs(5), kanban_inbox.recv())
                .await
                .expect("kanban agent should receive the remote delegation")
                .expect("inbox open");

            assert_eq!(request.source, expected_source);
            assert!(matches!(request.intent, MessageIntent::Delegate(_)));

            let reply = request.reply(
                reply_from,
                MessageIntent::resolve("Created LIN-501 remotely"),
            );
            kanban_inbox.send(reply).await.unwrap();
        })
    };

    // ---------------------------------------------------------------
    // Host A delegates to the remote agent and awaits the correlated reply.
    // ---------------------------------------------------------------
    let envelope = AgentEnvelope::new(
        dev_addr.clone(),
        kanban_addr.clone(),
        MessageIntent::delegate("Log a bug for the remote crash"),
    );
    let request_id = envelope.id;

    let reply_rx = dev_mailbox.register_reply(request_id).await;
    host_a.router.route(envelope).await.unwrap();

    let reply = tokio::time::timeout(Duration::from_secs(5), reply_rx)
        .await
        .expect("reply should arrive across the bridge")
        .expect("reply channel open");

    assert_eq!(reply.correlation_id, Some(request_id));
    match reply.intent {
        MessageIntent::Resolve(payload) => assert_eq!(payload.output, "Created LIN-501 remotely"),
        other => panic!("expected Resolve, got {other:?}"),
    }

    responder.await.unwrap();
}

#[tokio::test]
async fn unmatched_remote_target_is_reported_not_silently_dropped() {
    let host = Node::new();
    let sender = AgentAddress::parse("agent://host-a/dev").unwrap();
    let _dev = host.router.register(sender.clone(), 8).await;

    // No gateway covers host-z, so routing must fail loudly.
    let envelope = AgentEnvelope::new(
        sender,
        AgentAddress::parse("agent://host-z/ghost").unwrap(),
        MessageIntent::delegate("nobody home"),
    );

    let err = host.router.route(envelope).await.unwrap_err();
    assert!(
        matches!(err, nuo_acp::ProtocolError::Unreachable(_)),
        "expected Unreachable, got {err:?}"
    );
}

#[tokio::test]
async fn longest_prefix_gateway_wins() {
    let host = Node::new();
    let sender = AgentAddress::parse("agent://host-a/dev").unwrap();
    let _dev = host.router.register(sender.clone(), 8).await;

    let (broad_link, mut broad_rx) = in_memory_pair(8);
    let (narrow_link, mut narrow_rx) = in_memory_pair(8);

    // A broad gateway for the host, and a narrower one for a specific subtree.
    attach_outbound(&host.router, "agent://host-b/", broad_link.sender.clone()).await;
    attach_outbound(
        &host.router,
        "agent://host-b/team-x/",
        narrow_link.sender.clone(),
    )
    .await;

    let envelope = AgentEnvelope::new(
        sender,
        AgentAddress::parse("agent://host-b/team-x/kanban").unwrap(),
        MessageIntent::delegate("route me precisely"),
    );

    host.router.route(envelope).await.unwrap();

    let landed = tokio::time::timeout(Duration::from_secs(2), narrow_rx.receiver.recv())
        .await
        .expect("narrow gateway should receive the envelope")
        .expect("channel open");
    assert_eq!(
        landed.target,
        AgentAddress::parse("agent://host-b/team-x/kanban").unwrap()
    );

    // The broader gateway must not also receive it.
    let leaked = tokio::time::timeout(Duration::from_millis(80), broad_rx.receiver.recv()).await;
    assert!(leaked.is_err(), "envelope must not be sent to two gateways");
}
