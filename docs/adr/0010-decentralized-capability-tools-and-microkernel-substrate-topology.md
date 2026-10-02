---
id: ADR-0010
title: "Decentralized Capability Tools and Microkernel Substrate Topology: nuo-host and nuo-model-codec"
status: accepted
date: 2026-10-02
scope: architecture/subsystems, substrate/tools, microkernel/topology
superseded_by: null
negative_knowledge: true
---

# 0010. Decentralized Capability Tools and Microkernel Substrate Topology: nuo-host and nuo-model-codec

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Runtime & Tooling Teams
- Informed: System Architects

---

## Context and Problem Statement

As Nuo evolved from a monolithic cognitive engine inherited from Muta, the distribution of tools and substrate crates exhibited severe architectural drift:

1. **The "God Harness" Problem**: All built-in tools (filesystem operations, command execution, web search, memory recall, and inter-agent communication) were historically lumped into `nuo-harness/src/tools/`. The execution harness became a bloated, omniscient layer with direct dependencies on every disparate capability.
2. **Missing Protocol Tooling**: Although the Agent Coordination Protocol (`acp`) established standard primitives for channels, envelopes, and mailboxes, it shipped without canonical tool definitions. Consumers were forced to write duplicate, private tool wrappers.
3. **The "Contracts" Junk Drawer Anti-Pattern**: The legacy `nuo-contracts` crate accumulated dead code (`mesh.rs`, obsolete `Tool` traits), misplaced domain substrates (LLM wire types, MCP proxies), and client presentation details, creating rebuild amplification and blurring module boundaries.
4. **Semantics Drift in Substrate Naming**:
   - `nuo-model-wire`: The term "wire" implies raw network socket drivers, whereas the crate's true core responsibility is **Target Code Generation / Dialect Translation / Bidirectional Codec** (projecting internal Session IR and parameters into vendor-specific JSON payloads, and demuxing SSE stream chunks back into canonical events).
   - `nuo-system`: "System" is an overloaded umbrella term. In reality, the crate defines the **Host Machine Execution Environment & Sandboxing** (process supervision, Bubblewrap/JobObject isolation, secure workspace file I/O, path topology).

---

## Decision Drivers

- **Decentralized Capability Ownership**: Each domain capability crate must natively own and provide its own tools implementing the zero-runtime `nuo-tool::Tool` standard.
- **Microkernel Host/Harness Separation**: The harness must act solely as an orchestrator, policy governor, context injector, and permission checkpoint; it must only implement cognitive meta-tools (`ask_user`, `subagent`, `todo`).
- **Semantic Precision & Concrete Responsibility**: Eliminate vague umbrella words (`system`, `contracts`, `protocol`) in favor of concrete roles (`host`, `model-codec`, `client`).
- **Clean Decoupling of Polyglot Clients**: Web frontends interact with the daemon over WebSocket JSON conventions without requiring artificial Rust-to-TypeScript code generation dependencies in the protocol substrate.

---

## Decision Outcome

1. **Decentralize Tool Implementation into Canonical Capability Substrates**:
   - **`acp::tools`**: The `acp` crate natively exports 7 canonical inter-agent collaboration tools (`delegate_to_peer`, `list_peers`, `publish_to_channel`, `read_channel`, `list_channels`, `open_channel`, `subscribe_to_channel`).
   - **`nuo-host::tools`**: The host environment crate natively exports 7 canonical OS/workspace tools (`read_text`, `write_file`, `edit_text`, `list_dir`, `find_files`, `search_text`, `execute_command`).
   - **`nuo-persistence::tools`**: The persistence crate natively exports `recall_memory` backed by SQLite FTS5 and the Ebbinghaus forgetting curve.

2. **Re-ground `nuo-system` as `nuo-host`**:
   - Rename `nuo-system` to `nuo-host`.
   - The name explicitly identifies the entity running the agent: the physical or virtual host environment providing process execution, workspace containment, sandboxing, and host tools.

3. **Re-ground `nuo-model-wire` as `nuo-model-codec`**:
   - Rename `nuo-model-wire` to `nuo-model-codec`.
   - Accurately captures its core responsibility as a bidirectional compiler/codec: encoding Session IR into vendor API dialects (OpenAI, Anthropic, Google Gemini), and decoding vendor SSE streams into normalized events.

4. **Dismantle `nuo-contracts` into Domain Substrates and `nuo-client`**:
   - Client-daemon interaction types and streaming event definitions are consolidated into `nuo-client`.
   - Dead legacy code (`mesh.rs`, old `Tool` trait) is eliminated without replacement.
   - Web frontend communicates via decoupled WebSocket JSON contracts.

---

## Negative Knowledge: Rejected Alternatives

### 1. Keeping All Built-in Tools in `nuo-harness`
- **Why discarded**: Concentrating tools inside the harness creates circular dependency hazards, inflates compilation time, and prevents subagents, CLI utilities, and third-party embeddings from reusing ACP or OS tools without importing the entire execution harness.

### 2. Renaming `nuo-contracts` to `nuo-protocol`
- **Why discarded**: "Protocol" is an equally ambiguous umbrella term. ACP is already a protocol, MCP is a protocol, and model streaming is a protocol. Creating a generic `nuo-protocol` crate would recreate the exact same junk drawer within months.

### 3. Renaming `nuo-system` to `nuo-os`
- **Why discarded**: "OS" describes an operating system itself (e.g. Linux, Redox, or speculative "AI OS" marketing buzzwords). The crate is not an operating system kernel; it is the **Host Environment** that executes, confines, and serves the agent. `nuo-host` unambiguously captures "who runs it".

### 4. Forcing `nuo-client` Wire Types to Derive TypeScript via `ts-rs`
- **Why discarded**: SvelteKit/browser web applications cannot compile Rust. Hard-coupling Rust crates to TypeScript generation introduces fragile macro dependencies and burdens frontend developers with installing Rust toolchains. Standard front-end/back-end decoupling via JSON/WebSocket specifications provides cleaner engineering hygiene.

---

## System Invariants Upheld

- `[INV-TOOL-01]`: All tools exposed to agents must implement the zero-agent-runtime `nuo-tool::Tool` trait.
- `[INV-CAPABILITY-01]`: Capabilities own their tools. Harness owns policy, assembly, and meta-tools.
- `[INV-HOST-01]`: All local command execution and file mutations must pass through `nuo-host` sandbox and containment boundaries.
