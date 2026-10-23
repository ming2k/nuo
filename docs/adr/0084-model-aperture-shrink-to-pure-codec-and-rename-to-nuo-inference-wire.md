---
id: ADR-0084
title: "Model Aperture: Shrink `nuo-model-codec` to a Pure Wire Codec and Rename to `nuo-inference-wire`"
status: accepted
date: 2026-10-21
scope: codec/nuo-inference-wire, provider/nuo-provider, wire/nuo-protocol, server/nuo-server, agent/nuo-agent, harness/nuo-harness, transport/nuo-provider-transport, providers/all
superseded_by: null
negative_knowledge: true
---

# 0084. Model Aperture: Shrink `nuo-model-codec` to a Pure Wire Codec and Rename to `nuo-inference-wire`

- Status: Accepted
- Date: 2026-10-21
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Server Runtime
- Amends: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) §2 (the `nuo-model-codec` bullet and its flat-topology diagram row)
- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md) (dialect surface ownership wording for the renamed crate)

---

## Context and Problem Statement

`crates/nuo-model-codec` (13,266 lines, 44 modules) has drifted from its stated
purpose. Its README declares "Zero Domain Leakage" and `[INV-MOD-01]` claims
wire codecs must be free of application-level agent concepts, yet the crate
today hosts:

1. **The `Provider` trait** (`capability.rs`, 569 lines) — the *cognitive
   contract* of "how the harness talks to a model" (`chat`, `stream_chat`,
   `provider_id`, `prompt_hints`, debug capture toggles). This is the highest
   layer of the model lane, living in the lowest layer. All downstream crates
   (`nuo-protocol`, `nuo-agent`, `nuo-harness`, `nuo-server`, every
   `providers/nuo-provider-*`) consume the cognitive contract through the wire
   crate — a pure dependency inversion.
2. **A second inference engine** (`decision/`, 660 lines) — the TypeSafe Jev
   System One decision endpoint, semantically unrelated to chat codecs; it
   shares only transport with the generative lane.
3. **Auth, identity, connection, catalog, effort, token classification** —
   ~9,000 lines of client/domain substrate that is neither codec nor contract.

Meanwhile the name itself misleads: `model-codec` parses as `[model] codec` —
"the thing that encodes models" (weights/tokenizers). The crate's own type
system disagrees with the name: `WireRequest`, `WireChunk`, `WireClient`,
`WireProtocol`, `wire_surface.rs` — every internal term says *wire*, only the
crate name says *codec*.

**Negative knowledge is on file**: this crate was *previously named*
`nuo-model-wire` and renamed to `nuo-model-codec` in ADR-0001's era because the
wire name "misrepresented its role as a codec". That renaming treated the
symptom (name) not the disease (scope): the crate was never a pure codec to
begin with. This ADR corrects the causal order — shrink first, then rename.

## Decision Drivers

- Driver 1: Honoring `[INV-MOD-01]` by mechanism, not exhortation — after the
  split, the codec crate's dependency list (serde, bytes, thiserror; no tokio,
  no async-trait) structurally forbids hosting a cognitive contract.
- Driver 2: Repair the dependency inversion — contract crates (`nuo-provider`)
  must never source their types from an implementation transport crate.
- Driver 3: Name-accountability — the crate encodes *the channel to a model
  API*, not the model; `wire` expresses that without the `[model] codec`
  misparse.

## Architectural Decisions

### Subsystem Boundaries & Structural Changes

Four-way split, executed in staging:

| Stage | Action |destination |
|---|---|---|
| S1 | Move `capability.rs` (Provider trait, ModelRequest, ToolSpec, streams, turn context) | `crates/nuo-provider/src/capability.rs` (contract crate, already directly or transitively depended on by all consumers) |
| S2 | Move `decision/` to `crates/nuo-decision` | new dedicated crate (**executed 2026-10-21**; see verification record below) |
| S3 | Delete compat shroud + stub modules; keep channel metadata | **executed 2026-10-21** (stubs deleted, zero dead re-exports) |
| S4 | Rename `nuo-model-codec` → `nuo-inference-wire` | **executed 2026-10-21** (directory, Cargo.toml, all dependents, all docs) |

**Stage S1 (this change) — executed 2026-10-21:** mechanically:

- `crates/nuo-model-codec/src/capability.rs` → `crates/nuo-provider/src/capability.rs`.
  The moved module now legitimately depends on `nuo_model_codec` (for
  `Message`, `TokenUsage`, `TransportTelemetry`, `WireProtocol`,
  `ModelCapabilities`, `RouteFingerprint`, wire vocabulary) — contract crate
  referencing its wire substrate is the *correct* direction. The reverse edge
  (`codec → provider`) is prohibited by `[INV-AP-02]`.
- `nuo-provider` re-exports the contract types at the crate root, and its
  previous forwarding shells (`provider/src/capability.rs` did
  `pub use nuo_model_codec::Provider`) become the *owner* declaration.
- `nuo-protocol` (`nuo-wire` alias) flips its re-exports from
  `nuo_model_codec::capability::X` to `nuo_provider::capability::X`.
  Downstream (`nuo-agent`, `nuo-harness`, `nuo-persistence`, `nuo-server`,
  provider impl crates) continue to compile through the `nuo_wire::` façade;
  only the façade's one hop changes.
- `crates/nuo-model-codec` keeps a **temporary compatibility re-export**
  `pub use nuo_provider::capability::*;` in a dedicated module
  (`capability_compat`) for the residual references not yet flipped
  (`providers/nuo-provider-openai/src/responses/mod.rs` etc.). It is
  `#[doc(hidden)]` and scheduled for deletion in S3. This is safe because
  codec does not re-export *from* provider; it consolidates
  TextDelta-combinator utility help around the contract that moved out.
- `ToolSpec` type identity: `types.rs` defines `pub type ToolSpec = WireTool;`
  while `capability.rs` defined a *separate* `ToolSpec` struct; both exported
  at the crate root. After the move, `nuo-provider` owns the struct form;
  `nuo_model_codec::ToolSpec` continues to resolve to the `types.rs` alias
  (`WireTool`). Call sites verified: providers read `ToolSpec` fields
  (`name`, `description`, `parameters`) — present on `WireTool`; structural
  compatibility is asserted by the test in S1.
- `decision/` stays put in S1 (zero internal deps besides `error` +
  `WireClient`), to keep the change surgical.

  *S1 verification record:* `cargo check` green across the closed set
  {`nuo-model-codec`, `nuo-provider`, `nuo-protocol`, `nuo-provider-transport`,
  `nuo-harness`, `nuo-agent`, `nuo-persistence`, `nuo-catalog`}; `cargo test`
  for `nuo-model-codec`/`nuo-provider`/`nuo-protocol`/**/`nuo-harness`/
  `nuo-agent`/`nuo-persistence`/`nuo-provider-transport`/`nuo-catalog` all
  pass (0 failed); `cargo clippy` clean on all S1-touched files; single
  `pub trait Provider` authority confirmed in `nuo-provider::capability`
  (codec duplicate removed via the S1 shroud, `INV-AP-02` direction held).
  The dead zero-implementer `CatalogDiscovery::list_models` variant and the
  `nuo-provider` forwarding shell were removed with negative-knowledge notes
  in-module.

  *S2 verification record:* `crates/nuo-decision` established (engine, 700
  lines: `DecisionClient` with its own transport implementation over the
  shared `WireClient` pool, `DecisionEndpoint`, protocol drivers, typed
  Q/A vocabulary). The generative `WireClient::execute_decision` method was
  removed from the wire crate — System One framing is not a wire-codec
  concern. `nuo-decision` depends only on `nuo-model-codec` (transport
  substrate direction: `[INV-AP-02]` compliant). The `nuo-protocol` facade
  flipped to `pub use nuo_decision::*`; `nuo-model-codec::decision` remains
  as a deletion-marker stub for S3. `decision_jev_test.rs` (8 tests) moved
  with the engine. Full workspace gate green: check 0 bad, clippy 0,
  fmt clean, 126 test targets 0 failed, ADR lineage clean.

  *S3 verification record:* Shroud module `capability.rs` and stub module
  `decision/mod.rs` completely removed from the wire crate; all residual
  forwarding references in `nuo-protocol`, `nuo-provider`, and provider
  impls flipped to their authoritative sources (`nuo_provider::capability`
  and `nuo_tool`). The crate now contains zero backward-compat forwarding
  shells.

  *S4 verification record:* Directory `crates/nuo-model-codec` renamed to
  `crates/nuo-inference-wire`. Package name, 26 workspace dependent manifests,
  and 117 Rust files updated to `nuo_inference_wire`. Living documentation
  (`docs/dev/subsystems.md`, `architecture.md`, `release-and-versioning.md`,
  migration guides) updated with former-name annotations. Historical ADRs
  preserve negative-knowledge lineage intact.

### Fallback behavior of `Provider::stream_chat_events`

The default `stream_chat_events` implementation (text-delta folding +
terminal `Completed` event) moves with the trait; `WireProvider`
(`nuo-agent/src/provider/wire.rs`) overrides it elsewhere, so no behavioral
change is intended or observed.

### Normative Invariants Established

- **`[INV-AP-01] Codec Aperture`**: `nuo-inference-wire` (formerly
  `nuo-model-codec`) shall contain only channel encodings: wire types, vendor
  dialects, SSE decoding, and channel metadata. It must not host cognitive
  contracts (`Provider` trait), decision engines, or any type whose *methods*,
  not just data, express agent semantics. Enforcement (as executed in S3,
  with one honest amendment): the *codec core* (`types`, `protocol/`,
  `stream`, `wire_protocol`, `error`) is dependency-pure — its modules
  reference no runtime, no async traits, no agent vocabulary; the crate's
  single `pub trait Provider` gate is a CI grep. The `WireClient` transport
  face (one module) legitimately keeps `tokio`/`futures` because the
  *channel itself* is I/O — the original "no tokio anywhere in the crate"
  phrasing was over-broad and would have contradicted a transport-owning
  channel crate; the aperture invariant is about *semantics*, not about
  the presence of a runtime. If a later pass extracts `WireClient` into a
  transport crate, this clause tightens to full dependency pruning.
- **`[INV-AP-02] Contract Direction`**: `nuo-model-wire` must not depend on
  `nuo-provider`, `nuo-decision`, `nuo-agent`, `nuo-harness`, or
  `nuo-server`. Contracts flow down to the wire substrate; the wire substrate
  never sources domain vocabulary from above. Enforcement: cargo dependency
  graph check in CI (`cargo tree -e features -i nuo-provider -p nuo-model-wire`)
  and the facade-compat module deletion gate in S3.

## Consequences

- **Positive**: `[INV-MOD-01]` becomes mechanically true instead of
  README-asserted; the `[model] codec` misparse disappears at the moment of
  rename, and the historical `nuo-model-wire` name is not resurrected
  (no "formerly" cycle with ADR-0001's negative knowledge); `decision/` gains a governance boundary for its System One
  lifecycle; contract crates own their vocabulary.
- **Negative & Trade-offs**: one hop of indirection remains during the S1→S3
  window (compat re-exports in codec); crates.io publishes the new name at
  the next minor bump while the old name ships one caret-compatible
  transitional release carrying only re-exports; ~89 documentation references
  to `nuo-model-codec` are updated in waves (this ADR's links + release
  doc + subsystems doc in S4).

## Rejected Alternatives & Negative Knowledge

### Alternative A: Keep `nuo-model-codec` narrowed (Rejected)
- **Why considered**: zero rename cost, `codec` is standard in the serde/ecosystem.
- **Why rejected**: the noun-phrase miscalibration is a *grammar* problem
  ("codec-of-models" vs "codec-of-the-model-channel"), not solvable by
  narrowing; the misparse is materially reinforced by `tokenizer.rs` living
  in the crate (true model-tokenization).

### Alternative B: Rename to `nuo-dialects` (Rejected)
- **Why considered**: matches in-repo vocabulary (`ProviderDialect`,
  `OpenAiChatDialect`, ADR-0014 `DialectSurface`).
- **Why rejected**: conflates *knowledge of the vendor dialects* with the
  *channel itself*; the crate's own names (`WireRequest`, `WireClient`,
  `WireProtocol`) all speak `wire`. Renaming above the type vocabulary would
  reintroduce exactly the name/grammar mismatch this ADR repairs.

### Alternative C: Rename first, shrink later (Rejected)
- **Why considered**: the 2026 rename of `nuo-model-wire` →
  `nuo-model-codec` in ADR-0001's era followed this order.
- **Why rejected**: on record as negative knowledge — renaming first
  addressed symptom not cause (scope); it embedded a codec label on a crate
  that was never a pure codec. This ADR inverts the causal order.

### Alternative D: Rename to `nuo-model-transport` (Rejected)
- **Why considered**: "transport" names the vendor-facing integration role.
- **Why rejected**: transport denotes *carrying bytes* (I/O); post-shrink the
  crate is zero-I/O by `[INV-AP-01]` (no tokio in its dependency table), so
  the name would contradict its own dependency graph. It also collides with
  the *actual* transport crate `nuo-provider-transport` (HTTP egress, SSE
  pipeline), re-creating at crate level the exact ambiguity this ADR removes
  inside one.

### Alternative E: Rename to `nuo-inference-adapter` (Rejected)
- **Why considered**: "inference" is the correct domain word (the crate
  consumes vendor *inference* APIs; its SSE demuxer normalizes *inference
  output*).
- **Why rejected**: "adapter" names a wrapper role (aligning object
  interfaces), while the crate is the channel's grammar itself (types,
  dialects, decoding) — stateless bidirectional translation, not wrapping.
  The domain word survives in the accepted name; the role word does not.

### Alternative F: Rename to `nuo-inference-dialect` (Rejected)
- **Why considered**: the vendor-serialization half (~1,540 of ~2,600
  post-shrink lines) is literally per-vendor dialect code
  (`ProviderDialect`, `OpenAiChatDialect`, …), qualified by the correct
  domain word.
- **Why rejected**: a dialect presupposes a standard language it deviates
  from, and the crate *owns* that standard — the vendor-neutral canonical
  types (`WireRequest`, `WireChunk`, `WireMessage`) are the dependency core
  every consumer imports via the `nuo_wire` facade. Naming the whole after
  its variant half misattributes the neutral contract. It also collides
  with ADR-0014's `DialectSurface`, which denotes a *narrower* vendor
  auth/envelope surface than the crate as a whole.

### Accepted Synthesis: `nuo-inference-wire`
The domain term ("inference", per the crate's own vocabulary —
`wire_protocol.rs` opens with "Exact inference wire protocol used by a
route") combines with the mechanism root ("wire", matching the `Wire*` type
family). Naming the *channel* (wire) qualified by its *purpose* (inference)
avoids both misparses: it is not a codec *of models*, and it is not an
adapter/transport *around* models — it is the inference channel's grammar.

## Links

- Related ADRs: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md),
  [ADR-0006](0006-wire-contract-consolidation.md) (nuo-wire consolidation and
  the zero-async wire invariant this split mirrors),
  [ADR-0008](0008-single-tool-contract.md),
  [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md),
  [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md)