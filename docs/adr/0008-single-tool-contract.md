---
id: ADR-0008
title: "Single Tool Contract: Metadata-as-Data, Capabilities-via-Context"
status: accepted
date: 2026-10-03
scope: capability/tools, architecture/layering, plugin/registration
superseded_by: null
negative_knowledge: true
---

# 0008. Single Tool Contract: Metadata-as-Data, Capabilities-via-Context

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Capability, and Interface Teams
- Informed: System Architects
- Supersedes: [ADR-0007](0007-tool-ownership-and-plugin-boundaries.md) §1 (the two-trait decision)

---

## Context and Problem Statement

ADR-0007 accepted a **two-trait + bridge** design: `nuo_tool::Tool` as the
"plugin" contract and `nuo_wire::Tool` as a "harness session-adapter" contract,
joined by `nuo-harness::tool_bridge`. That was a *pragmatic* compromise to
finish a mid-migration without a cycle. It is **not** the right end state, and it
carries exactly the burdens a modern, future-facing design must not:

1. **Two contracts for one concept.** Every tool author must know which trait to
   implement, and the answer depends on which crate they live in. Two `Tool`
   traits, two `ToolContext`s (a build-time service map *and* a call-time
   context), two `ToolOutput`s (a flat `{content, is_error, metadata}` and a
   rich enum), and duplicate `ToolInvocation`/`InputHandler` definitions that
   had already begun to drift.

2. **A bridge is a permanent tax.** `tool_bridge.rs` exists only to adapt the
   two traits to each other. Every new capability method must be wired through
   the bridge, and every bridge hop is a place where semantics can silently
   diverge.

3. **Method explosion.** `nuo_wire::Tool` has ~20 methods, of which ~15 are
   *static metadata* (`variant`, `aliases`, `is_available`, `requires_user`,
   `spawns_subagent`, `requires_vision`, `affects_control_flow`, `hazard_level`,
   `scope_target`, `permission_label`, `permission_description`, …). Metadata
   modelled as trait methods cannot be inspected, serialized, diffed, or
   enumerated without constructing an instance — the opposite of modern
   data-driven design.

4. **Session concerns leaked into the tool contract.** Subagent event streaming
   and cooperative cancellation are *harness session* concerns, yet they live as
   trait methods (`call_with_events`, `call_structured_with_events`,
   `supports_cooperative_cancel`, `request_cancel`). Verified fact: the three
   leaf capability crates (`nuo-host`, `nuo-acp`, `nuo-persistence`) emit
   **zero** `SubagentEvent`s — only harness meta-tools do. So the event plumbing
   is not a property of tools in general; it is a property of *one* tool family.

5. **`inventory` self-registration is linker-fragile across crates** (the repo
   carries a guard test for exactly this), so it cannot be the plugin channel.

We require a single, data-driven tool contract that a leaf capability crate can
implement without any cycle, and a plugin mechanism that is deterministic and
feature-gated.

---

## Decision Drivers

- **One concept, one contract.** A tool is a tool; there is one trait.
- **Metadata as data.** Static facts about a tool are a struct, inspectable
  without instantiation.
- **Capabilities via context, not trait surface.** Anything a tool needs at
  runtime (cancellation, event sinks, workspace roots, services) is passed
  through the invocation context.
- **Acyclic layering.** The contract must live where every capability crate can
  reach it without a cycle.
- **Deterministic pluggability.** Explicit factories + Cargo features.
- **No dead vocabulary.** No duplicate types, no bridges, no unused methods.

---

## Decision Outcome

### 1. One contract: `nuo_tool::Tool`

`nuo-tool` is the true dependency leaf (it depends on nothing but its derive
macro). It owns the **single** tool contract and the tool-result vocabulary.
`nuo_wire::Tool` is **deleted**; `nuo-harness::tool_bridge` is **deleted**.

The contract is minimal — **one execution method plus one descriptor**:

```rust
pub trait Tool: Send + Sync {
    /// Static facts about this tool, as data.
    fn descriptor(&self) -> &ToolDescriptor;

    /// Execute one call within the invocation context.
    async fn execute(&self, ctx: &ToolContext, args: serde_json::Value)
        -> Result<ToolOutput, ToolError>;
}
```

### 2. Metadata as data: `ToolDescriptor`

All former metadata methods become fields of one struct:

```rust
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters_schema: serde_json::Value,
    pub variant: &'static str,            // default "default"
    pub aliases: &'static [&'static str],
    pub risk: RiskProfile,                // supersedes hazard_level
    pub scopes: Vec<ToolScope>,
    pub scope_target: ScopeTarget,        // declared, not computed per-call
    pub requires_user: bool,
    pub requires_vision: bool,
    pub spawns_subagent: bool,
    pub affects_control_flow: bool,
}
```

Consequences:
- The descriptor is **inspectable, serializable, and testable** without a live
  tool instance — the toolset can be audited (unique names, safe targets,
  permission coverage) as pure data.
- `permission_label`/`permission_description` collapse to sensible defaults
  derived from `name`/`description`; a tool that needs bespoke prompt wording
  puts it in the descriptor.
- `variant` selection, availability gating, and model-capability filtering all
  read the descriptor.

### 3. Capabilities via context

`ToolContext` (call-time) is the **single** invocation context. It carries:
- `session_id`, `call_id`, `correlation_id`, `metadata`;
- the cooperative **cancellation token** (already present);
- the session **workspace roots** (moved from the build-time service map);
- an optional **event sink** for tools that stream (subagent step events,
  shell stdout/stderr) — a `dyn ToolEventSink`, not a trait method;
- an opaque, type-keyed **service map** for capability-specific state
  (configs, registries) — the one surviving idea from the old build-context.

A tool that spawns a subagent (exactly one today: `subagent`/`task`) uses the
event sink from its context. A tool that streams shell output uses the same
sink. No bespoke trait methods, no `_with_events` variants.

### 4. Pluggability: explicit factories + Cargo features

Unchanged from ADR-0007 §3, which remains binding:
- Each capability crate exposes `pub fn create_<cap>_tools(ctx) -> Vec<Arc<dyn Tool>>`.
- The assembly point (`nuo/src/bootstrap.rs`) selects families via Cargo
  features (`["acp"]`, `["web"]`, …).
- `inventory` is **not** used as a cross-crate plugin channel.
- The legacy imperative `nuo_tool::ToolRegistry` and its `register_*` wrappers
  are removed in favor of `create_*` factories.

### 5. Capability ownership (unchanged from ADR-0007 §2)

A tool belongs to the crate that owns its capability. `nuo-harness` implements
only cognitive meta-tools. Web egress moves out of the harness to its own
capability crate. The seven host tools exist **once**, in `nuo-host`.

### 6. Single assembly point, no silent shadowing (unchanged from ADR-0007 §4)

`nuo/src/bootstrap.rs` is the only place a session toolset is assembled; a
`(name, variant)` collision is a hard error, never an `upsert` shadow.

---

## Invariants & Behavioral Boundaries

- **`[INV-TOOL-08] Single Tool Contract`**: Exactly one tool trait exists in the
  workspace (`nuo_tool::Tool`). `nuo_wire::Tool` and `tool_bridge` must not exist.
- **`[INV-TOOL-09] Metadata as Data`**: A tool's static facts are fields of
  `ToolDescriptor`, not trait methods. No new `fn <metadata>(&self)` methods on
  the tool trait.
- **`[INV-TOOL-10] Capabilities via Context`**: Runtime capabilities
  (cancellation, event streaming, workspace roots, services) reach a tool only
  through `ToolContext`. No `*_with_events` / `request_cancel` trait methods.
- **`[INV-TOOL-11] Contract Lives in the Leaf`**: The tool contract and the
  tool-result vocabulary live in `nuo-tool`, which depends on no other `nuo-*`
  crate. Every capability crate reaches them without a cycle.
- **`[INV-TOOL-12] Factory + Feature Pluggability`**: Tool families are selected
  at the single assembly point via explicit `create_*` factories gated by Cargo
  features. `inventory` is not a cross-crate plugin channel.
- **`[INV-TOOL-04]` (ADR-0007) and `[INV-TOOL-06]`/`[INV-TOOL-07]` remain binding**:
  capability ownership, no duplicate identity, single assembly point.

---

## Positive Consequences

- One contract to learn, one `ToolOutput`, one `ToolContext`. No bridge.
- The toolset is auditable as **data** before any tool is instantiated.
- Adding a metadata field never breaks existing tool implementations (it has a
  default in the descriptor), eliminating the method-explosion churn.
- Leaf capability crates implement the contract directly; no cycle, no adapter.
- The one genuinely session-aware tool family (subagent) uses the same event
  sink as shell streaming — one mechanism, not two.

## Negative Consequences & Trade-offs

- **Large, staged migration**: 42 files implement the rich trait today. The
  collapse is mechanical but wide; it must proceed in verifiable increments
  (descriptor introduction → context sink → bridge removal → trait swap).
- **The contract crate grows**: `nuo-tool` must absorb the tool-result
  vocabulary (`ToolOutput` rich enum, `ScopeTarget`, streaming types) and the
  `Message`/`TokenUsage` types those reference. This is the cost of a cycle-free
  single contract; mitigated by `nuo-tool` remaining dependency-free (pure data
  + `serde` + `tokio` primitives).
- **Descriptors are less dynamic** than methods for tools whose metadata is
  genuinely argument-dependent (e.g. `scope_target` varies per call). Mitigation:
  a tool may *refine* its descriptor per call inside `execute` when reporting to
  the permission broker; the descriptor remains the static baseline.

---

## Rejected Alternatives & Negative Knowledge

### 1. Keep the two-trait + bridge design (ADR-0007 as accepted)
- **Why considered**: It compiles today and needs no wide migration.
- **Why rejected**: It *is* the legacy burden. Two contracts for one concept,
  a permanent bridge, duplicate drifting vocabulary, ~15 metadata methods, and
  session concerns leaked into the tool surface. Accepting it means freezing the
  duplication as architecture.

### 2. Collapse to one trait but keep metadata as trait methods
- **Why considered**: Smaller change than introducing a descriptor.
- **Why rejected**: Keeps the method explosion and makes the toolset
  un-auditable without instantiation. Metadata is data; model it as data.

### 3. Keep `inventory` self-registration as the plugin channel
- **Why considered**: "Add a file and you're done" for every capability crate.
- **Why rejected**: Linker-dropped `inventory::submit!` nodes are a *silent*
  failure (a security-gated tool vanishes in release). The repo already carries
  a guard test for this. A plugin channel must fail loudly and deterministically.

### 4. Put session event methods on the contract for all tools
- **Why considered**: Uniform surface; every tool could stream if it wanted.
- **Why rejected**: Verified that leaf capability crates emit zero
  `SubagentEvent`. Modelling a one-family concern as a universal trait method
  forces every tool and both bridges to carry dead surface. The event sink
  belongs in the context, used only by tools that stream.

### 5. Move the contract up into `nuo-wire` (where the rich trait lives today)
- **Why considered**: Least code movement — the rich trait is already there.
- **Why rejected**: `nuo-wire` sits *above* every leaf crate, so leaf
  capability crates cannot implement its trait (cycle). The contract must live
  in the leaf (`nuo-tool`), which is exactly what `[INV-TOOL-11]` requires.

---

## Migration Sequence (staged, each step independently green)

1. **Remove dead legacy** (done): delete duplicate
   `nuo-model-codec::{InputHandler, ToolInvocation}`, the unused
   `call_with_events` method + override, the dead `register_*` wrappers, and the
   unused `nuo-agent` dependencies in `nuo-harness`/`nuo-persistence`.
2. **Introduce `ToolDescriptor`** in `nuo-tool`; add `descriptor()` alongside
   the existing methods (non-breaking). **(done)**
3. **Move pure tool vocabulary down into the leaf** (`nuo-tool`): `ScopeTarget`,
   `ToolStream`, `InputContract`/`InputExpectation`/`InputPrompt`,
   `ShellTermination`/`ShellStream`/`ShellLine`. **(done** — `nuo-model-codec`
   and `nuo-wire` now re-export them, so this must precede the context-sink
   step, which needs `ToolStream` below the harness.)
4. **Add the event sink + workspace roots to `ToolContext`**; route shell
   streaming and subagent events through it. **(done** — `ToolContext` now
   carries `workspace_roots`, a `ToolStreamSink`, and a type-keyed `ServiceMap`
   for harness-specific state such as the subagent event channel.)
5. **Delete `tool_bridge`**; flip leaf tools to `nuo_tool::Tool`. **(done** —
   `nuo_wire::Tool` is a re-export alias of `nuo_tool::Tool`; all 25 harness
   implementors compile unchanged; `tool_bridge.rs` is **deleted** and its
   call sites now use `Arc<dyn Tool>` directly.)
6. **Delete `nuo_wire::Tool`**; flip harness tools; remove `register_*`
   wrappers in favor of `create_*` factories. **(single trait achieved** —
   `nuo_wire::Tool` is an alias, not a second trait; the `register_*` wrappers
   remain.)
7. **Move the tool-result vocabulary** (the rich `ToolOutput` enum) and its
   `Message`/`TokenUsage` dependencies into `nuo-tool`. **(done** — `tokenizer`,
   `usage` (`TokenUsage`), `message`, and the unified `ToolOutput` all live in
   `nuo-tool`; `nuo-model-codec` re-exports them. The former flat
   `{content, is_error, metadata}` struct and the rich variant enum are merged
   into one `ToolOutput`, with compat constructors `success`/`error` and
   accessors `content()`/`is_error()` replacing the old fields.)
8. **Resolve the duplicate agent runtime** (ADR-0009: `nuo-agent` is orphaned
   and feature-bearing; port-then-delete), then **wire ACP + web egress** behind
   Cargo features at the production assembly point. **(ACP done** — the seven
   collaboration tools are now wired into `nuo/src/registry.rs` behind the
   `collaboration` feature; web egress remains.)

> **Open finding (blocks step 7).** There are **two agent runtimes**:
> `nuo-agent::Agent` (`nuo-agent/src/agent/runtime.rs`) and
> `nuo-harness::Agent` (`nuo-harness/src/agent/mod.rs`). The production daemon
> (`nuo`) uses `nuo-harness::Agent`; `nuo-agent::Agent` has **no callers outside
> `nuo-agent`'s own tests**, and neither `nuo` nor `nuo-harness` references the
> `nuo-agent` crate. The ACP collaboration tools are installed only by
> `nuo-agent::Agent` (`install_collaboration_tools` → `acp::register_acp_tools`),
> so in production they are never wired. Before step 7, this duplication must be
> resolved: either `nuo-agent::Agent` is the real cognitive loop and
> `nuo-harness::Agent` should embed it, or `nuo-agent::Agent` is superseded and
> its collaboration wiring must be ported to the harness. This is a separate
> architectural decision (see ADR-0009 when taken).

---

## Links

- Supersedes: [ADR-0007](0007-tool-ownership-and-plugin-boundaries.md) §1.
- Retains from ADR-0007: capability ownership map (§2), single assembly point
  (§4), factory+feature pluggability (§3), web-egress ownership (§5).
- Related: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) (`[INV-TOOL-01]`), [ADR-0006](0006-wire-contract-consolidation.md) (layering).
