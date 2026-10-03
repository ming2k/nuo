---
id: ADR-0007
title: "Tool Ownership, Contract Layering, and Compile-Time Plugin Mechanism"
status: accepted
date: 2026-10-03
scope: capability/tools, architecture/layering, plugin/registration
superseded_by: null
negative_knowledge: true
---

# 0007. Tool Ownership, Contract Layering, and Compile-Time Plugin Mechanism

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Capability, and Interface Teams
- Informed: System Architects

---

## Context and Problem Statement

ADR-0001 (`[INV-TOOL-01]`) mandates that capabilities natively own their tools
using the zero-runtime `nuo-tool::Tool` trait, and that `nuo-harness` implement
only cognitive meta-tools. Executing that mandate collided with two structural
facts and produced an ambiguous, half-migrated state:

1. **Two `Tool` traits exist simultaneously.**
   - `nuo_tool::Tool` (`nuo-tool/src/lib.rs`): zero-runtime, `execute(&ToolContext, Value) -> Result<ToolOutput>`.
   - `nuo_wire::Tool` (`nuo-wire/src/capability.rs`): session-rich — adds
     `variant`, `requires_user`, `spawns_subagent`, cooperative cancellation,
     `scope_target`, hazard/permission surfaces, and `call(&str) -> Result<String, String>`.
   A bridge (`nuo-harness/src/tool_bridge.rs`) exists solely to adapt between them.

2. **A hard dependency constraint forces the split.** `nuo-wire` depends on
   every leaf crate (`nuo-host`, `nuo-agent`, `nuo-mcp`, `nuo-model-codec`,
   `nuo-tool`). Therefore **no leaf crate may depend on `nuo-wire`** (it would
   form a cycle). The consequence is unavoidable:
   - Leaf crates **cannot** implement `nuo_wire::Tool`.
   - Leaf crates **cannot** use `nuo_wire::register_tool!` (the macro lives in `nuo-wire`).

3. **Two registration mechanisms coexist.**
   - Compile-time self-registration: `nuo_wire::register_tool!` + `inventory`,
     collected by `collect_toolset(&ToolContext)`. Used only inside `nuo-harness`.
   - Imperative factories: `nuo_host::create_system_tools`, `nuo_acp::register_acp_tools`,
     `nuo_persistence::create_persistence_tools`, `nuo_agent::install_collaboration_tools`.
     Used by leaf crates, bridged in `nuo/src/bootstrap.rs`.

4. **Resulting hazards observed in the tree.**
   - **Duplicate implementations**: the seven host tools exist twice — as
     `nuo_tool::Tool` in `nuo-host/src/tools.rs` and as `nuo_wire::Tool` in
     `nuo-harness/src/tools/*.rs`. Six collide by name (`read_text`, `write_file`,
     `edit_text`, `list_dir`, `find_files`, `search_text`); the seventh diverges
     in name (harness `run_command` vs host `execute_command`). At assembly the
     bridged host versions are `upsert`-ed **after** `collect_toolset`, silently
     shadowing the harness copies for the six colliding names.
   - **Duplicate agent runtime**: `nuo-agent` defines a *second* `Agent`
     (`nuo-agent/src/agent/runtime.rs`) that owns its own `nuo_tool::ToolRegistry`
     and installs the ACP collaboration tools (`install_collaboration_tools`).
     That runtime has **no callers outside `nuo-agent`'s own tests** — the
     production daemon uses `nuo-harness::Agent`, which never references
     `nuo-agent`. So the ACP tools are wired into a runtime the product does not
     run.
   - **Linker fragility**: a test (`nuo-harness/tests/it/orchestration.rs`)
     exists specifically to detect `inventory::submit!` nodes dropped by the
     linker — evidence the team does not fully trust compile-time self-registration
     across crate boundaries.

The boundary is unclear because the repository is mid-migration between two
coherent-but-incompatible designs. This ADR picks one and fixes the rules.

---

## Decision Drivers

- **`[INV-TOOL-01]` fidelity**: capabilities own their tools on the zero-runtime trait.
- **Dependency acyclicity**: the layering must be expressible in Cargo without cycles.
- **Compile-time pluggability**: adding or removing a tool family must be a
  one-line change gated at compile time, with no central enumeration to maintain.
- **Debuggability**: the assembled toolset must be deterministic and inspectable;
  silent shadowing must be impossible.

---

## Decision Outcome

### 1. Two-tier contract layering (both traits are kept)

> **Superseded by [ADR-0008](0008-single-tool-contract.md) §1.** The two-trait
> design below was a pragmatic mid-migration compromise; ADR-0008 replaces it
> with a **single** tool contract (`nuo_tool::Tool`, metadata-as-data,
> capabilities-via-context). The rest of this ADR (capability ownership §2,
> factory+feature pluggability §3, single assembly point §4, web-egress §5)
> remains binding.

- **`nuo_tool::Tool` is the plugin contract.** Every capability crate
  (`nuo-host`, `nuo-acp`, `nuo-persistence`, `nuo-mcp`, and future web/egress
  crates) implements **this** trait. It is zero-runtime and dependency-free.
- **`nuo_wire::Tool` is the harness session-adapter contract.** It is the shape
  the agent loop dispatches against (variants, cancellation, permission
  surfacing, subagent awareness). It is **not** a public plugin surface; it
  lives at the top of the substrate stack precisely because it must reference
  every leaf type.
- `nuo-harness/src/tool_bridge.rs` is the **single** sanctioned adapter between
  the two. No other crate may re-implement the adaptation.

This keeps `[INV-TOOL-01]` intact while acknowledging that the rich trait
*cannot* live below the leaves.

### 2. Capability ownership map (the boundary rule)

A tool belongs to the crate that owns the **capability**, not the crate that
happens to need it:

| Tool family | Owning crate | Trait | Assembly |
| :--- | :--- | :--- | :--- |
| Filesystem & shell (`read_text`, `write_file`, `edit_text`, `list_dir`, `find_files`, `search_text`, `execute_command`) | `nuo-host` | `nuo_tool::Tool` | factory |
| Memory & persistence (`recall_memory`) | `nuo-persistence` | `nuo_tool::Tool` | factory |
| Agent coordination (`delegate_to_peer`, channels, peers) | `nuo-acp` | `nuo_tool::Tool` | factory |
| MCP bridge | `nuo-mcp` | `nuo_tool::Tool` | dynamic sink |
| Web egress (`read_url`, `search_web`) | dedicated egress capability crate (see §5) | `nuo_tool::Tool` | factory |
| Cognitive meta-tools (`ask_user`, `subagent`, `todo`) | `nuo-harness` | `nuo_wire::Tool` | self-register (inventory) |

`nuo-harness` implements **only** the cognitive meta-tools, per ADR-0001.

### 3. Compile-time plugin mechanism: explicit factories + Cargo features

- Each capability crate exposes a factory:
  `pub fn create_<cap>_tools(ctx) -> Vec<Arc<dyn nuo_tool::Tool>>`.
- The assembly point (`nuo/src/bootstrap.rs`) selects families via **Cargo
  features** (`features = ["acp"]`, `["web"]`, …). Absent feature ⇒ crate not
  compiled in ⇒ factory not called.
- `inventory` self-registration is **retained only for `nuo-harness`'s own
  cognitive meta-tools**, where no cross-crate linker fragility exists.

Rationale: the observed linker-fragility guard test shows `inventory` is unsafe
as a *cross-crate* plugin channel. Explicit factories + features give the same
"one-line change" ergonomics at the crate level with deterministic, inspectable
assembly.

### 4. Single assembly point, no silent shadowing

- `nuo/src/bootstrap.rs` is the **only** place a `ToolSet` is assembled for a
  session.
- A name collision between two families is a **hard error**, not a shadow.
  `ToolSet::upsert` shadowing must not be reachable from assembly; colliding
  families are a configuration bug and must panic in debug/test builds.
- The seven host tools must exist **once**. The duplicate harness
  implementations are deleted; harness keeps only `ask_user`, `subagent`, `todo`
  (and `read_image`/`code_query` if they are judged cognitive rather than host —
  to be resolved when this ADR is executed).

### 5. Web egress is a capability, not a harness concern

`read_url` / `search_web` currently live in `nuo-harness/src/tools/web/`. Per
this ADR they belong in a dedicated egress capability crate (or `nuo-host` if a
separate crate is not warranted), so the harness stops hosting network tools.

---

## Invariants & Behavioral Boundaries

- **`[INV-TOOL-02] Plugin Trait Is Zero-Runtime`**: Every capability-owned tool
  implements `nuo_tool::Tool`. No leaf crate implements `nuo_wire::Tool`
  (impossible without a cycle).
- **`[INV-TOOL-03] Single Adapter`**: `nuo-harness::tool_bridge` is the only
  bridge between `nuo_tool::Tool` and `nuo_wire::Tool`.
- **`[INV-TOOL-04] Capability Ownership`**: A tool is implemented by the crate
  owning its capability (see the map in §2), never by `nuo-harness` except for
  the cognitive meta-tools.
- **`[INV-TOOL-05] Compile-Time Family Gating`**: Tool families are selected at
  the assembly point via Cargo features over explicit `create_*_tools` factories.
- **`[INV-TOOL-06] No Duplicate Tool Identity`**: A `(name, variant)` pair is
  defined exactly once in the workspace. Assembly-time collisions are a hard
  error, never a silent shadow.
- **`[INV-TOOL-07] Single Assembly Point`**: The session `ToolSet` is built only
  in `nuo/src/bootstrap.rs`.

---

## Positive Consequences

- The boundary is expressible and checkable: capability crates are leaves,
  harness is the adapter layer, `nuo` is the assembler.
- No duplicate tool identities; no silent shadowing.
- Pluggability is deterministic and feature-gated; removing a family is a
  feature-flag flip, not a code deletion.

## Negative Consequences & Trade-offs

- Two traits remain, which is more concepts than one. Mitigation: the split is
  documented here and enforced by the single-adapter invariant.
- The assembly point grows an explicit family list. Mitigation: this list is the
  single, inspectable source of truth — preferable to linker-dependent magic.
- Moving web egress out of the harness is a non-trivial refactor.

---

## Rejected Alternatives & Negative Knowledge

### 1. Collapse to a single `nuo_tool::Tool` and push session capabilities down
- **Why considered**: One trait is simpler; no bridge needed.
- **Why rejected**: Cooperative cancellation, permission surfacing, subagent
  spawning, and variant selection are **harness session concerns**, not tool
  concerns. Pushing them into `nuo-tool` would drag the harness's session model
  (and its dependencies) into the lowest substrate crate, inverting layering and
  re-creating the junk-drawer problem ADR-0001/0006 eliminated.

### 2. Keep the rich `nuo_wire::Tool` as the sole plugin trait and let leaves use it
- **Why considered**: One trait, no bridge.
- **Why rejected**: Physically impossible without a dependency cycle —
  `nuo-wire` already depends on every leaf crate. Leaves cannot depend on
  `nuo-wire`.

### 3. Move `register_tool!`/`inventory` down into `nuo-tool` for all crates
- **Why considered**: "Add a file and you're done" for every capability crate.
- **Why rejected**: `inventory` submissions are silently dropped by the linker
  across crate boundaries — the repository already carries a dedicated guard
  test for exactly this failure. A plugin mechanism whose failure mode is
  *silent tool disappearance in release* is unacceptable for a security-gated
  agent.

### 4. Keep both duplicate host-tool implementations and let `upsert` shadow
- **Why considered**: Zero migration cost; both variants already work.
- **Why rejected**: Shadowing makes the assembled toolset depend on assembly
  order. Two implementations of `read_text` with different permission/scope
  semantics means the effective security posture is an accident of ordering —
  a correctness and safety hazard, not a convenience.

### 5. Leave ACP tools reachable only from the test-only `nuo-agent` runtime
- **Why considered**: The fabric is session-scoped; wiring into the production
  assembly point is fiddly.
- **Why rejected**: A capability whose tools are reachable only from a runtime
  the product does not execute is effectively dead. `nuo-agent`'s `Agent`
  installs the ACP tools, but the daemon runs `nuo-harness::Agent`; the
  factory + feature pattern (§3) is the mechanism for wiring session-scoped
  dependencies into the runtime that actually runs.

---

## Links

- Related ADRs: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) (`[INV-TOOL-01]`, microkernel harness), [ADR-0006](0006-wire-contract-consolidation.md) (contract layering).
- Related code: `nuo-tool/src/lib.rs`, `nuo-wire/src/capability.rs`, `nuo-wire/src/tool_registry.rs`, `nuo-harness/src/tool_bridge.rs`, `nuo/src/bootstrap.rs`.
