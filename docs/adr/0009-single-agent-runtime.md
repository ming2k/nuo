---
id: ADR-0009
title: "Single Agent Runtime and Contract-Layer Ownership of Agent Identity"
status: accepted
date: 2026-10-03
scope: architecture/layering, runtime/agent, workspace/topology
superseded_by: null
negative_knowledge: true
---

# 0009. Single Agent Runtime and Contract-Layer Ownership of Agent Identity

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime & Interface Teams
- Informed: System Architects
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md) §"Cognitive & Tool Lifecycle" (single runtime)

---

## Context and Problem Statement

While executing the ADR-0008 tool migration, an investigation of tool wiring
surfaced a structural defect larger than the tool contract itself:

1. **Two agent runtimes exist.** `nuo-agent::Agent`
   (`nuo-agent/src/agent/runtime.rs`) and `nuo-harness::Agent`
   (`nuo-harness/src/agent/mod.rs`) both implement a ReAct cognitive loop over
   providers, tools, sessions, and token budgets.
2. **One of them is orphaned.** `nuo-agent::Agent` has **zero callers outside
   `nuo-agent`'s own tests**. Neither the daemon (`nuo`) nor `nuo-harness`
   references the `nuo-agent` crate. The production daemon runs
   `nuo-harness::Agent` (6,437 lines of agent code vs. `nuo-agent`'s 2,132).
   ADR-0005 already states the intended topology: *"Cognitive & Tool Lifecycle:
   Orchestrated by `nuo-harness` over `nuo-agent`"* — i.e. `nuo-harness` should
   drive `nuo-agent`, but in practice it reimplements the loop instead.
3. **A layering inversion.** The **only** inbound dependency edge to the
   6,299-line `nuo-agent` crate was `nuo-wire → nuo-agent`, and it existed solely
   to re-export two tiny pure value modules (`agent_kind.rs` 60 lines,
   `identity.rs` 138 lines). The *contract* layer depending on a *runtime* crate
   is a dependency inversion (`[INV-DEP-01]` spirit).
4. **Duplicated vocabulary.** `AgentKind` / `AgentIdentity` were defined in
   `nuo-agent` while `AgentIdentity` is used heavily in `nuo-wire`
   (`agent_role.rs`, `policy_schema.rs`) — the same "single source of truth"
   violation ADR-0006 `[INV-WIRE-04]` bans.

The repository is carrying a complete second cognitive runtime that nothing in
production executes (yet which holds the only implementation of several
multi-agent features), plus a contract→runtime inversion. Both are legacy
burden — but the runtime must be *ported from*, not blindly deleted.

---

## Decision Drivers

- **One runtime.** The product runs exactly one cognitive loop.
- **Correct dependency direction.** Contracts (values) sit below runtimes; a
  runtime may depend on the contract layer, never the reverse.
- **Single source of truth.** Shared vocabulary is defined once.
- **No dead weight.** A crate with no consumers is not architecture; it is a
  liability (it still compiles, still costs CI, still misleads readers).

---

## Decision Outcome

### 1. `AgentKind` / `AgentIdentity` move to the contract layer

Both types are pure value vocabulary (three strings / a two-variant enum, `serde`
only). They are relocated into `nuo-wire`:

- `nuo-wire::agent_kind::AgentKind`
- `nuo-wire::identity::AgentIdentity`

`nuo-agent` now **re-exports** them from `nuo-wire` (`nuo-agent/src/agent_kind.rs`,
`nuo-agent/src/identity.rs` are one-line re-exports). This **removes the
`nuo-wire → nuo-agent` edge**; the direction is now `nuo-agent → nuo-wire`.

`nuo-wire` no longer depends on `nuo-agent`.

### 2. `nuo-agent` is orphaned — but feature-bearing, not dead

**Correction (same day, after deeper verification).** An earlier draft of this
ADR called `nuo-agent` "superseded/dead." That was **wrong**, and the correction
matters because it changes the follow-up:

- `nuo-agent` has **zero production callers** (orphaned), but it is **not dead**:
  its 130+ tests pass, and it uniquely implements capabilities the production
  harness does **not** have:
  - the **ACP collaboration tool wiring** (`install_collaboration_tools` /
    `install_direct_delegation_tools` → `acp::register_acp_tools`),
  - **multi-party channel turns** (`run_channel_turn`, unread-channel backlog),
  - **zero-trust envelope verification** (`signature_verifier`,
    `SignatureEnforcement`, P2P `HandshakeAck` pairing),
  - a **skill sandbox** (`SkillStack`).
- The production daemon (`nuo/src/bootstrap.rs`) wires **none** of these; it
  never registers ACP tools. So the capability exists, is tested, and is
  **unreachable in the shipped product**.

The correct characterization is therefore: **`nuo-agent` is an orphaned second
runtime that holds the only implementation of several multi-agent features.**
It is not to be deleted before those features are either ported to the harness
or deliberately retired.

Decision: `nuo-harness::Agent` is **the** single production runtime
(`[INV-AGENT-05]`). `nuo-agent` is a **feature donor**, not a peer runtime. The
follow-up (tracked here, not executed in this step) is:

- **(a)** Port the ACP collaboration wiring (and, per product intent, the
  channel / signature / pairing features) from `nuo-agent` into the production
  path — the ACP tools via `nuo-acp::create_acp_tools` behind a Cargo feature at
  `nuo/src/bootstrap.rs` (ADR-0008 step 8); then
- **(b)** Once ported, delete the `nuo-agent` *runtime* (`Agent`, loop, session
  machinery) and its tests, retaining only any value types still referenced.

Deleting first (without porting) would silently drop multi-agent capability the
product is meant to have. Porting first makes the deletion a bounded, verifiable
action.

**Progress on (a):**

- The seven ACP collaboration tools are wired into production behind the
  `collaboration` feature (§3 below). **Done.**
- Their behavioral coverage — previously only in `nuo-agent/tests/` — is ported
  into `nuo-acp/tests/collaboration_tools_test.rs`, exercising the tools
  **directly** through `nuo_tool::Tool` (no runtime). **Done.** This means the
  coverage survives the eventual retirement of `nuo-agent`.
- The peer **handshake/pairing** protocol coverage is ported into
  `nuo-acp/tests/handshake_test.rs` (`MessageIntent::Handshake` / `HandshakeAck`).
  **Done.**
- The zero-trust **admission decision** (`AgentEnvelope::enforce` +
  `AdmissionRejection`) is extracted from the runtime into `nuo-acp` as the
  single source of truth and tested (`nuo-acp/tests/signature_test.rs`). **Done.**
  A runtime must now *call* `enforce` rather than re-implement the policy.

### 4. `nuo-agent` retirement is a product decision, not a cleanup (correction)

A second verification pass (same day) **blocked the mechanical deletion** that
§2's follow-up (b) implied. The evidence is conflicting:

- `nuo-agent` is **documented as a published crates.io SDK** (Cluster C,
  `docs/dev/release-and-versioning.md`) and its `Cargo.toml` describes a
  "peer delegation SDK".
- It carries **23 test targets, all passing** — it is maintained, not abandoned.
- Its **channel-turn cognition** (`Agent::run_channel_turn`, unread-channel
  backlog, skill sandbox, embedding memory) is **not ported** anywhere; the
  harness and daemon genuinely lack it.

Therefore §2's "port then delete" is only **partially** satisfied: the
*protocol-level* coverage (tools, handshake, zero-trust enforcement) is now
independent, but the *runtime cognition* is not. Deleting the crate would retire
a documented SDK and drop unported capability.

**Decision:** `nuo-agent` is **not** deleted mechanically. Its retirement is a
**product decision** — "does the project still ship a standalone embedding agent
SDK?" — which must be made deliberately, not inferred from "nothing in the
workspace imports it." Until then, `nuo-agent` remains a workspace member with
its runtime intact. This ADR records the finding so the decision is not made by
accident.

If the product decides to retire the SDK, the sequence is: (1) decide whether
channel-turn cognition is needed in the daemon — if so, port it to the harness
first; (2) then delete `nuo-agent` (runtime + tests), since the protocol-level
coverage already lives in `nuo-acp`.

### 3. ACP collaboration tools wired into the production runtime (done)

The ACP tools (`nuo-acp/src/tools.rs`, seven tools) were installed only by the
orphaned `nuo-agent::Agent`. They are now wired into the **production** assembly
path (`nuo/src/registry.rs::assemble_hosted`):

- `register_session_mailbox` returns the session's `acp::MailboxHandle`.
- The session's ACP `AcpToolContext` is built from the daemon fabric, the
  session address (`agent://local/session/<id>`), and that handle.
- `nuo_acp::create_acp_tools(ctx)` → `nuo_harness::bridge_substrate_tools` →
  `agent.dynamic_tool_sink().replace("acp", tools)` — the same seam MCP uses.

This is gated by the `nuo` crate's **`collaboration`** Cargo feature (default
off). The daemon always links the ACP fabric for its own mesh / Hypervisor /
Archivist infrastructure; the feature gates only the **model-facing tool
surface**, so lean single-agent builds omit the multi-agent tools without
dropping the internal fabric. This satisfies ADR-0005 §5 (feature-gated ACP) and
ADR-0008 step 8.

---

## Invariants & Behavioral Boundaries

- **`[INV-AGENT-05] Single Shipped Runtime`**: The **product** (the `nuo`
  daemon and its frontends) executes exactly one cognitive loop —
  `nuo-harness::Agent`. `nuo-agent::Agent` must not be linked into the shipped
  binary graph. (`nuo-agent` may remain a workspace crate as an embedding SDK;
  the invariant governs the *shipped* graph, not crate existence. See §4:
  whether the SDK itself is retired is a separate product decision.)
- **`[INV-AGENT-06] Contract Below Runtime`**: A runtime crate may depend on the
  contract layer; the contract layer (`nuo-wire`) must never depend on a runtime
  crate (`nuo-agent`, `nuo-harness`). Reaffirms the ADR-0005 `[INV-DEP-01]`
  spirit for the agent axis.
- **`[INV-WIRE-04]` (ADR-0006) remains binding**: `AgentKind`/`AgentIdentity`
  are defined once, in `nuo-wire`.

---

## Positive Consequences

- The dependency graph is acyclic and correctly oriented: `nuo-agent → nuo-wire`,
  not the reverse.
- `AgentIdentity` has one definition, used consistently by `nuo-wire`'s role and
  policy modules.
- The orphaned second runtime is now *visible* as such (no inbound edges), making
  the follow-up port-then-delete a bounded, verifiable action.

## Negative Consequences & Trade-offs

- `nuo-agent` still compiles and still carries its orphaned runtime + tests
  until the follow-up (port then delete) lands. Mitigation: this ADR names the
  follow-up explicitly so it is not silently dropped.
- If the follow-up chooses option (b) (delegate harness → agent), the two loops
  must be reconciled; this is a larger change than (a).

---

## Rejected Alternatives & Negative Knowledge

### 1. Leave the inversion and the dual runtime as-is
- **Why considered**: Zero change cost; everything compiles.
- **Why rejected**: A contract layer depending on a runtime is a dependency
  inversion, and a 6,100-line runtime with no production consumers is legacy
  burden that misleads every reader into thinking `nuo-agent` is the product's
  loop — while it silently holds the only implementation of ACP collaboration,
  channel turns, and signature verification.

### 2. Delete `nuo-agent` immediately in this change
- **Why considered**: It has no inbound edges — the deletion looks safe.
- **Why rejected**: "No inbound *crate* edges" is not "no users": its own 130+
  tests exercise a runtime that uniquely implements ACP collaboration, channel
  turns, and signature verification. Deleting 11,500 lines before those features
  are ported (or deliberately retired) would silently drop multi-agent
  capability. Port-then-delete is safer and reversible.

### 3. Keep `AgentKind`/`AgentIdentity` in `nuo-agent` and have `nuo-wire` keep depending on it
- **Why considered**: Smallest diff (change nothing).
- **Why rejected**: Preserves the contract→runtime inversion and the duplicated
  vocabulary. The two types are pure values with no runtime dependency, so there
  is no justification for them to live in a runtime crate.

### 4. Move `AgentKind`/`AgentIdentity` into `nuo-tool`
- **Why considered**: `nuo-tool` is the true leaf.
- **Why rejected**: They are *agent* vocabulary, not *tool* vocabulary. The tool
  leaf should stay tool-scoped; agent identity belongs in the contract layer
  (`nuo-wire`) where the role/policy modules already use it.

---

## Links

- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md) (single cognitive runtime).
- Related: [ADR-0006](0006-wire-contract-consolidation.md) (`[INV-WIRE-04]`), [ADR-0008](0008-single-tool-contract.md) (step 8: wire ACP into the production runtime).
- Code: `nuo-wire/src/{agent_kind,identity}.rs`, `nuo-agent/src/{agent_kind,identity}.rs`, `nuo-agent/src/agent/runtime.rs`, `nuo-harness/src/agent/`.
