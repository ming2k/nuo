---
id: ADR-0002
title: "Canonical Agent Coordination Protocol (ACP) Standard"
status: accepted
date: 2026-10-02
scope: workspace/substrate, comm/acp, security/envelopes
superseded_by: null
negative_knowledge: true
---

# 0002. Canonical Agent Coordination Protocol (ACP) Standard

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Security & Distributed Systems Teams
- Informed: System Architects

---

## Context and Problem Statement

Early multi-agent prototypes in Nuo and Muta relied on ad-hoc, proprietary communication schemes:
1. **Legacy Mesh Protocol (`mesh.rs`)**: An unversioned internal mesh topology using raw memory addresses, unstructured payload maps, and synchronous in-memory mailboxes. It could not scale across network boundaries, lack cryptographic verification, and suffered from memory leaks.
2. **Missing Inter-Agent Protocol Standard**: Without a formal wire specification, subagents and remote assistants could not reliably discover peers, establish multi-party discussion channels, or delegate tasks with traceable causality.
3. **No Zero-Trust Security Guarantees**: Inter-agent messages had no integrity checks, leaving agents vulnerable to payload spoofing, replay attacks, and unauthorized permission elevation.

We require an open, canonical inter-agent coordination standard that guarantees message integrity, structured peer discovery, asynchronous channel collaboration, and zero-runtime tooling.

---

## Decision Drivers

- **Standardized Addressing**: Unambiguous URI schemes for agents (`agent://<id>`) and collaborative topics (`acp://channel/<name>`).
- **Cryptographic Envelope Verification**: Tamper-evident message envelopes verified with HMAC-SHA256 signatures, timestamps, and nonces.
- **Asynchronous Collaborative Channels**: Multi-agent pub/sub messaging channels enabling shared broadcast contexts and coordinated task solving.
- **Native Tool Exposure**: Direct provision of canonical ACP collaboration tools for cognitive agents conforming to `nuo-tool::Tool`.
- **Decoupled Autonomous Specification**: Independent protocol evolution suitable for adoption across polyglot runtimes.

---

## Decision Outcome

We establish the **Agent Coordination Protocol (ACP)** as the canonical inter-agent standard implemented in the autonomous `acp` crate:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Agent Coordination Protocol                     │
│                                                                        │
│   ┌───────────────────────┐             ┌──────────────────────────┐   │
│   │     acp::Envelope     │             │       acp::Fabric        │   │
│   │  • Header (HMAC, ID)  │ ──────────► │  • Addressing Engine     │   │
│   │  • Payload (JSON DTO) │             │  • Mailbox Deliveries    │   │
│   │  • Trace & Nonce      │             │  • Channel Subscriptions │   │
│   └───────────────────────┘             └──────────────────────────┘   │
│                                                                        │
│   ┌────────────────────────────────────────────────────────────────┐   │
│   │  Native Collaboration Tools (acp::tools):                      │   │
│   │  • delegate_to_peer       • list_peers     • open_channel      │   │
│   │  • publish_to_channel     • read_channel   • list_channels     │   │
│   │  • subscribe_to_channel                                        │   │
│   └────────────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────────┘
```

### 1. Canonical URI Addressing
Every entity in the multi-agent mesh possesses a distinct, parseable URI:
- **Individual Agents**: `agent://{agent_id}`
- **Collaborative Channels**: `acp://channel/{channel_name}`
- **Ephemeral Subtasks**: `agent://{parent_id}/subtask/{task_id}`

### 2. Zero-Trust Envelope Architecture
All inter-agent messages are encapsulated in `acp::Envelope`:
```rust
pub struct Envelope {
    pub id: Uuid,
    pub sender: AgentAddress,
    pub recipient: TargetAddress,
    pub timestamp: DateTime<Utc>,
    pub nonce: u64,
    pub payload_signature: String, // HMAC-SHA256 over canonicalized payload
    pub payload: Value,
}
```
Envelopes with stale timestamps, invalid nonces, or mismatched HMAC signatures are dropped at the ingress layer prior to agent cognition.

### 3. Native Collaboration Tools
`acp::tools` natively exports 7 canonical inter-agent tools implementing `nuo-tool::Tool`:
- `delegate_to_peer`: Direct task handoff to a designated agent address with causal trace tokens.
- `list_peers`: Discover active peer agents registered in the fabric.
- `open_channel`: Create a scoped multi-party broadcast channel.
- `publish_to_channel`: Post a message to a channel for collaborative observation.
- `read_channel`: Retrieve historical channel messages.
- `list_channels`: Query open channels and subscription memberships.
- `subscribe_to_channel`: Join a channel to receive real-time notifications.

---

## Invariants & Behavioral Boundaries

- **`[INV-ACP-01] Canonical Addressing`**: All inter-agent message targeting must use standard `agent://` or `acp://` URIs. Raw identifiers and IP strings are prohibited.
- **`[INV-ACP-02] Envelope Signature Integrity`**: Messages crossing agent or process boundaries must be enveloped and signed. Unsigned payloads are rejected at the transport boundary.
- **`[INV-ACP-03] Autonomous Protocol Boundary`**: The `acp` crate must never depend on `nuo-harness` or `nuo`. It represents a standalone protocol specification.

---

## Negative Knowledge & Rejected Alternatives

### 1. Retaining Legacy Muta Mesh (`mesh.rs`)
- **Why considered**: `mesh.rs` was already wired into legacy daemon code.
- **Why rejected**: Lacked cryptographic verification, lacked multi-party broadcast channels, and tightly coupled agent identity to local memory pointers, preventing remote or containerized agent collaboration.

### 2. Raw JSON Over Plain WebSockets
- **Why considered**: Lowest implementation overhead for local agent communication.
- **Why rejected**: Completely lacks tamper-evident security, sender authentication, and causality tracing. Open agent systems require cryptographically verifiable envelopes to prevent agent impersonation.
