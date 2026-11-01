---
id: ADR-0143
title: "Codec Aperture Completed: Delete the Unwired Wire-Codec Lane Rather Than Document It"
status: accepted
date: 2026-11-01
scope: codec/nuo-inference-wire, provider/nuo-provider, providers/nuo-provider-google-antigravity, providers/nuo-provider-openai, harness/nuo-harness, docs/dev/subsystems.md
superseded_by: null
negative_knowledge: true
---

# 0143. Codec Aperture Completed: Delete the Unwired Wire-Codec Lane Rather Than Document It

- Status: Accepted
- Date: 2026-11-01
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Harness Runtime, Documentation Governance
- Amends: [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md) — corrects two stages it records as executed and verified but which never landed, and finishes the aperture it started. Restates its `[INV-AP-01]`.
- Amends: [ADR-0088](0088-pure-zero-io-inference-wire-and-transport-pruning.md) — extends its zero-I/O pruning from *transport* to *codecs*, and overturns its §2 rejected alternative, whose stated failure mode no longer holds.
- Supersedes in part: [ADR-0086](0086-dual-lane-inference-wire-topology-and-nuo-decision-retirement.md) §1 and `[INV-AP-04]` — the discriminative lane and the shared transport substrate it mandates are deleted here. [INV-AP-05] (protocol purity) survives untouched and is strengthened.

---

## Context and Problem Statement

ADR-0084 ("Model Aperture") split a 13,000-line catch-all crate four ways and
renamed the remnant `nuo-inference-wire`, declaring its purpose to be "only
channel encodings: wire types, vendor dialects, SSE decoding, and channel
metadata." It recorded all four stages as executed with verification records.

Auditing the shipped code against that record found the aperture half-finished
and the documentation asserting things that are not true:

| ADR-0084 asserts | Reality |
| --- | --- |
| S2 created `crates/nuo-decision` ("**executed 2026-10-21**", with a verification record) | No such crate exists |
| `WireProvider` (`nuo-agent/src/provider/wire.rs`) overrides `stream_chat_events` | No `WireProvider`, no `nuo-agent` crate |
| Scope covers `agent/nuo-agent` (cited 7 times) | No such crate |
| `[INV-AP-01]`: S3 removed `tokio`/`async-trait`; the codec is dependency-pure | `async-trait` was still a dependency, used by `credentials.rs` |

More seriously, the *codec* the ADR preserved was never called. Measuring every
reference from outside the crate (all `crates/*/src`, `providers/*/src`,
`tools/*/src`):

| Symbol group | External references |
| --- | --- |
| `WireProtocol` (the protocol identity enum) | 60+ — live |
| `find_balanced_object`, `loop_detector`, `token_classification::TokenBreakdown` | live |
| `protocol::google_interactions::{parse_response, parse_chunk}` | live (antigravity) |
| `Endpoint`, `ProviderVendor`, `WireClient`, `WireRequest`, `WireResponse`, `WireChunk` | **0** |
| `StaticApiKey`, `DynamicCredentials`, `CredentialsProvider`, `error::WireError` | **0** |
| `discriminative` lane (631 lines) | **0** |
| five per-vendor `build_request` + the `protocol::build_request` / `parse_response` dispatcher | **0** (reachable only from this crate's own tests) |
| `generative/stream.rs` (`SseDecoder`, `parse_vendor_chunk`, `StreamAccumulator`) | **0** — `nuo-provider/src/transport/sse.rs` is the one actually used |

The cost of leaving it was not the line count. It was that **nothing kept the
dead implementation honest**: the `openai_responses` codec's module doc claimed
it supported `previous_response_id` continuation and server-side storage while
its `WireRequest` had no field for either. The claim survived because no caller
existed to notice. A second implementation that nobody runs will always drift
away from the one that ships — and then teach the next reader the wrong thing.

### The prior decision knew, and kept it anyway

This is not an oversight being cleaned up; it is a reversal that has to be named.
[ADR-0086](0086-dual-lane-inference-wire-topology-and-nuo-decision-retirement.md)
diagnosed the discriminative lane as having **"Zero Production Consumers"**
— its words, item 4 under *Context* — and its response was to *relocate* the
lane into this crate and declare a `[INV-AP-04]` that two lanes "model
generative and discriminative as structurally independent, non-intersecting
lanes sharing only transport substrate."

Relocation made it tidier; it did not give the lane a consumer. One year later it
still had none, and the shared transport substrate the invariant named
(`WireClient`, `Endpoint`, `CredentialsProvider`, `WireError`) turned out to have
no consumer either. ADR-0086 also specified a crate tree with ten files that do
not exist (`client.rs`, `cache.rs`, `effort.rs`, `instructions.rs`,
`reasoning.rs`, `wire_surface.rs`, `model.rs`, `model_providers.rs`, `auth.rs`,
and `discriminative/` is gone too) — the diagram was a plan, not a description,
and was read as the latter ever since.

`[INV-AP-04]` is therefore superseded: there is one lane, because there is one
`nuo-inference-wire` and what remains feeds generative callers. `[INV-AP-05]`
(protocol purity: no discriminative or non-generative engine may be re-exported
from `nuo-protocol`) is untouched and now holds more strongly — the engine it was
guarding against no longer exists.

## Decision Drivers

- **A second implementation with zero callers is a trap, not an API.** It invites
  a future contributor to "reuse the codec" and inherit code no test exercises
  against a real endpoint.
- **Documentation that records unexecuted work as verified destroys the audit
  trail.** The failure mode is not "docs are stale"; it is that a reader trusts a
  verification record and skips checking.
- **Dependency pruning is the aperture's mechanism.** ADR-0084 itself says the
  structural guarantee is "the codec crate's dependency list forbids hosting a
  cognitive contract." That only holds if the deps that exist are the ones
  needed.
- **No backward compatibility** is the standing project rule: a public surface
  with no consumers has no compatibility obligation to honour.

## Architectural Decisions

### 1. Delete every symbol with zero external references

Removed outright, with no shim, alias, or deprecation period:

- `src/discriminative/` (631 lines) and its integration test
- `src/credentials.rs` — `ProviderVendor`, `StaticApiKey`, `DynamicCredentials`, `CredentialsProvider`
- `src/endpoint.rs` — the codec-side `Endpoint` (a near-duplicate of `nuo_provider::endpoint::Endpoint`, which is the live one)
- `src/generative/protocol/{openai_responses,openai_chat_completions,anthropic_messages,google_generate_content}.rs`
- `src/generative/protocol/mod.rs`'s `build_request` / `parse_response` dispatcher
- `src/generative/stream.rs` — `SseDecoder`, `parse_vendor_chunk`, `StreamAccumulator`, `parse_stream_chunk`
- `src/error.rs` — `WireError` and its `Result` alias (below)
- `google_interactions::build_request` (the parsers stay)
- `tests/wire_protocol_test.rs`, `tests/wire_credentials_and_guard_test.rs`, `tests/fixtures/corpus/`

**`WireClient` is not in that list, because it was already gone.** ADR-0088 §1
excised `src/client.rs` when it converged every provider onto `nuo-egress`, and
this ADR takes no credit for it. That distinction matters: writing "this cut
deleted `WireClient`" would have been a claim about work done a year earlier, and
ADR-0088's own record is the authority for it.

**`WireError` went further than "unused".** It had zero constructors *and* zero
external consumers, and both parsers that still name a `Result` in their
signature never return `Err` — they degrade to empty fields. Rather than delete
the error type and leave a vestigial `Result` wrapper propping up a phantom
failure channel, the parsers were made total (`-> WireResponse` /
`-> Option<WireChunk>`), which removed the failure branch from the caller too.
The `map_err(|e| …)` chains at `nuo-provider-google-antigravity` were dead code
guarding an impossible error.

### 2. Keep exactly the live surface, with its consumers named

`WireProtocol`; `find_balanced_object`; `loop_detector`; `token_classification`;
`protocol::google_interactions::{parse_response, parse_chunk}`; and the `types`
payload structs those parsers return. Each has a named consumer in the crate's
module docs, so the next reader can check the claim in one grep.

### 3. Prune the dependencies to match

`http` and `async-trait` are dropped. `thiserror` also has no user left once
`WireError` is gone, so it goes too — the manifest now holds only crates a
surviving line of code names. The crate cannot perform I/O, holds no `async fn`,
and cannot host a transport, which is what `[INV-AP-01]` was always trying to
say.

### 4. Correct the record instead of preserving it

ADR-0084 keeps its text (history is not rewritten) but its unexecuted stages are
recorded here as negative knowledge. `[INV-AP-01]` is restated against what the
crate now structurally is rather than against a stage that never ran.

## Normative Invariants Established

- **`[INV-AP-01]` (restated)**: `nuo-inference-wire` carries no I/O, no async
  trait, and no transport. Mechanically: its dependency set is
  `serde`/`serde_json`/`thiserror`/`uuid`, so a transport cannot be added without
  a visible dependency change, and the build fails the moment a `tokio` or
  `async-trait` import appears without that manifest entry. `[INV-AP-02]`
  (contract direction) is unchanged and unaffected.
- **`[INV-AP-03]` No Unwired Second Implementation**: a wire codec lives in the
  provider that ships it. `nuo-inference-wire` may host a codec only when a
  crate outside it calls that codec's parser or builder from `src` (not merely
  from tests). Enforced by review against the consumer table in `src/lib.rs`,
  which names a consumer for every surviving symbol.
- **`[INV-AP-04]` (superseded)**: the dual-lane separation is withdrawn. One
  lane remains; there is no discriminative half, so a rule forbidding the lanes
  from importing each other's types has nothing left to forbid.
- **`[INV-AP-05]` (unchanged, strengthened)**: `nuo-protocol` re-exports no
  discriminative or non-generative model engine. The engine this guarded against
  is deleted, so the invariant now holds by absence.

## Consequences

- **Positive**: 5,146 → 1,676 lines (−67%) across the crate. Every remaining
  symbol has a named external consumer. The crate can no longer be mistaken for a
  second provider implementation, and its dependency list now *is* the aperture
  invariant instead of contradicting it.
- **Positive**: the drift that motivated the audit is mechanically harder to
  repeat — a doc claim about a capability now sits next to a symbol some shipped
  caller exercises.
- **Negative & Trade-offs**: the crate is no longer a general-purpose codec
  library. An external or future consumer that wants to encode a wire body must
  use the owning provider. That is the intended answer, not a regression: the
  generic encoder never encoded anything the product sent.
- **Negative & Trade-offs**: deleting tests lowers the raw test count. Those
  tests covered deleted code paths, so the surviving coverage is unchanged; the
  one live test in the removed credential file (`StreamLoopDetector`) was
  preserved as `tests/wire_loop_detector_test.rs`.

## Rejected Alternatives & Negative Knowledge

### 1. Keep the dead layer, document it as unmaintained
- **Why considered**: cheapest option; zero risk of breaking an unknown consumer.
- **Why rejected**: 4,000+ lines whose only readers are future contributors
  looking for a codec to reuse. Labelling them "unmaintained" is exactly the
  documentation-only approach that let the `previous_response_id` claim rot in
  the first place.

### 2. Keep the codec and wire it up as the real request builder
- **Why considered**: makes the abstraction true rather than deleting it.
- **Why rejected**: every provider already builds a correct, endpoint-specific
  body — `nuo-provider-openai` alone carries dialect-aware tool shapes, reasoning
  replay, cache projection, and opaque artifact replay that the generic codec has
  no vocabulary for. Unifying onto it would be a rewrite of four providers to
  serve an abstraction with no remaining justification. The evidence pointed the
  other way: the generic codec was never right, which is why no provider adopted
  it.

### 3. This overturns ADR-0088 §2, and here is why that is safe now
[ADR-0088](0088-pure-zero-io-inference-wire-and-transport-pruning.md) explicitly
*rejected* "Discarding Canonical Wire IR (`WireRequest`, `ContentBlock`)", on
this stated failure mode:

> *"Destroys the universal SSE stream decoder (`SseDecoder`), breaks stream loop
> detection, and creates N×M duplicate byte-reassembly code across disparate
> provider implementations."*

Every clause of that premise is now false, and it is worth being precise about
which ones, because "the old ADR was wrong" is not an argument:

- **`SseDecoder` was not destroyed by this decision — it had no users.** The
  decoder that ships is `nuo_provider::transport::sse::data_payloads`, which every
  provider calls; `nuo-inference-wire`'s copy had zero external references.
  ADR-0088's clause assumed the crate's decoder was the universal one. It was not.
- **Stream loop detection is untouched.** `loop_detector` survives, with
  `nuo-harness` as its consumer.
- **N×M duplication already happened, in the direction the ADR did not predict.**
  Providers duplicate byte reassembly today because they never adopted the shared
  codec. Deleting the codec removes nothing that was preventing that; keeping it
  only preserved a second unexercised copy.

`ContentBlock` also survives, so "discarding canonical wire IR" is not what
happened — a fact that argues the deletion was narrower than ADR-0088 feared.

### 4. Delete `protocol::google_interactions` too, inline the parsers into antigravity
- **Why considered**: it would empty `protocol/` entirely and make the crate
  smaller still.
- **Why rejected**: `parse_response` (298 lines) and `parse_chunk` are genuinely
  shared decoding of a vendor frame format, and `nuo-provider-google-antigravity`
  calls them. Moving them would relocate working code to satisfy a tidiness
  preference — and `[INV-AP-03]` admits any codec with a real caller.

### 4. Keep compatibility shims for the removed public items
- **Why considered**: the items were `pub`, so an out-of-tree consumer could
  exist.
- **Why rejected**: the standing rule is no backward compatibility, and the
  measured internal consumer count was zero for every removed symbol. A shim
  would preserve exactly the surface the decision removes.

## Links

- [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md) — the aperture this completes; its S2/S3 and `WireProvider` claims are corrected here
- [ADR-0086](0086-dual-lane-inference-wire-topology-and-nuo-decision-retirement.md) — whose discriminative lane and `[INV-AP-04]` this supersedes in part
- [ADR-0142](0142-transcoding-gateway-surfaces-portable-mode-and-dual-form-reasoning-replay.md) — the audit that surfaced the drift
- `docs/dev/subsystems.md` §`nuo-inference-wire` — restated to the surviving surface
