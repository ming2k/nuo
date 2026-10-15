# Architecture Overview

This article explains the shape of the Nuo system: what the major pieces are, how they relate, and why the boundaries fall where they do. It is an orientation, not a specification. For the reasoning behind each decision, consult the [ADRs](../adr/index.md).

---

## The Concentric Entry Point

Nuo ships exactly one executable, `nuo`. The same binary acts as the terminal interface, the headless runner, and the session server control plane. This "concentric" model (see [ADR-0005](../adr/0005-unified-binary-and-concentric-runtime-architecture.md) and [ADR-0061](../adr/0061-pure-thread-topology-headless-unified-entry-and-unburdened-cli-architecture.md)) means there is no separate client and daemon binary to keep in version lockstep — the binary decides *what was asked* and dispatches to the right role.

---

## The Entity Tiers

The system operates across three orthogonal tiers, described in detail in the [Lifecycle and Action Semantics Reference](../reference/lifecycle-and-action-semantics.md):

1. **Client tier** — terminal viewports and the headless CLI, which attach to and detach from sessions.
2. **Server tier** — the long-lived session server that hosts every thread across every workspace.
3. **Thread tier** — the conversational state, projected into sovereign tabs.

Because the server is the single source of truth for thread state, any number of clients can attach to the same thread without divergence; the client re-projects a reactive view from server-sent updates.

---

## The Substrate and the Harness

Two boundaries carry most of the architectural weight:

- **The terminal canvas substrate (`nuotc`)**: a retained-mode 2D canvas with differential, double-buffered rendering and a Flexbox layout solver. It is deliberately decoupled from all AI domain concepts (see [ADR-0003](../adr/0003-autonomous-terminal-canvas-substrate-nuotc.md)) so the terminal engine can be reasoned about without knowing what an agent is.
- **The execution harness (`nuo-harness`)**: it outfits an agent with a role and prompts, enforces hazard controls and confirmation checkpoints, and detects loops. The harness is the *policy* layer; the model providers are the *transport* layer.

---

## Why Lanes Are Orthogonal

Model invocation is not a single "auth mode". Nuo distinguishes strictly exclusive invitation lanes — local-direct (static key), local-direct (OAuth), and subscription proxy — so that credentials never escape their lane and server-enforced entitlement stays server-enforced. This is ratified in [ADR-0014](../adr/0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md); the [Connect a Model Provider](../how-to/connect-a-provider.md) guide walks through using them.

---

## Where to Go Next

- [Getting Started](../tutorials/getting-started.md): build and run the system.
- [Reference: Lifecycle and Action Semantics](../reference/lifecycle-and-action-semantics.md): the precise action matrix and TUI spatial topology.
- [Architectural Decision Records](../adr/index.md): the reasoning behind each ratified decision.
