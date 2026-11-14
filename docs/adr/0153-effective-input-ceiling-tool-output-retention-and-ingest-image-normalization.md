---
id: ADR-0153
title: "Effective Input Ceiling, Request-Projected Tool-Output Retention, and Ingest Image Normalization"
status: accepted
date: 2026-11-07
scope: protocol/context-lifecycle, cognition/request-assembly, host/image-ingest, server/context-controls
superseded_by: null
negative_knowledge: true
---

# 0153. Effective Input Ceiling, Request-Projected Tool-Output Retention, and Ingest Image Normalization

- Status: Accepted
- Amended: [ADR-0182](0182-context-policy-is-configuration-and-phase-a-substrate-is-deleted.md) (the context policy is a compact configuration module)
- Date: 2026-11-07
- Deciders: Nuo Architecture Working Group
- Consulted: Cognitive Runtime, Context Lifecycle, Host Capabilities, Performance Engineering
- Supersedes: the "watermarks are fractions of the model's declared context window" rule in
  the unversioned predecessor of `ContextPolicy`, and the unwritten "a pasted image is
  whatever the clipboard produced" ingest posture
- Complements: [ADR-0019](0019-token-pressure-measurement-and-budget-accounting.md),
  [ADR-0031](0031-domain-separated-configuration-matrix-and-legacy-retirement.md),
  [ADR-0048](0048-cli-architecture-modernization-resource-symmetry-and-application-observability-topology.md),
  [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md),
  [ADR-0142](0142-transcoding-gateway-surfaces-portable-mode-and-dual-form-reasoning-replay.md)

---

## Context and Problem Statement

Turns were slow — five to thirty seconds before the first token — with no local
CPU mystery. A release-built micro-benchmark over a 6 MB request measured the
entire local path (clone, revision fingerprints, JSON serialization, cache plan)
at single-digit milliseconds. The cost was not serialization. It was the *size of
what was being serialized*.

The local session database made the shape unmistakable:

- the current workspace's `deepseek-v4-flash` sessions carried **130k–240k-token
  prompts**;
- time-to-first-token tracked prompt size almost linearly (≈6–8 s at 140–155k,
  ≈15–30 s at 240k, up to 57 s at 940k on a Gemini route);
- prompt caching was healthy — `cache_read_tokens` was ~99% of `prompt_tokens` —
  so the remaining latency was the unavoidable attention/prefill cost of a
  quarter-million-token context;
- a single user round ran **78–146 ReAct turns**, each turn appending tool output;
- across 1,051 threads and 186,991 usage records, the database held **49 prune
  directives and zero compaction checkpoints**: relief essentially never fired.

Three independent defects produced that state:

1. **The budget was derived from the capability ceiling.** `deepseek-v4-flash`
   advertises a 1,000,000-token window. At the 65%/85% watermarks, pruning began
   at 650k and compaction at 850k. A 240k-token prompt is "healthy" under that
   rule even though it makes every turn slow. A model's maximum window is a
   correctness bound, not a performance target.
2. **Tool output accumulated without a retention horizon.** Oversized results
   were truncated once at record time, but every result below 8 KB was replayed
   verbatim for the rest of the session. Older results were only touched under
   pressure — pressure that, by (1), never arrived.
3. **Attachments were stored at whatever resolution the clipboard produced.** A
   Retina screenshot is routinely several megabytes of PNG, base64-inflated
   again on the wire, and re-uploaded on every turn. The `image` crate was already
   a workspace dependency but had no production caller.

## Decision

### 1. Derive every watermark from an **effective** input window

`ContextPolicy` gains `max_input_tokens` (default **128,000**). `resolve_budget`
computes:

```text
W_eff = min(declared_window or fallback_window, max_input_tokens)
```

and derives `prune`, `compaction`, `target`, and cruise watermarks from `W_eff`.
A 1M-token route therefore prunes at ~83k and compacts at ~109k; a 64k-token
route keeps its own smaller window. The declared window continues to govern
admission/protocol correctness; it no longer governs the *performance* budget.

`[INV-CONTEXT-01]` **Capability is not budget.** No watermark may be derived from
an uncapped model window. A new `ContextPolicy` field that affects pressure must
be expressed against `W_eff` or be rejected in `validate()`.

### 2. Project historical tool output into three retention bands

`project_historical_tool_outputs` runs on the **provider-facing clone** of the
round history (never on durable facts) for every full-replay delivery:

| Band (measured from the tail on original message weights) | Shape |
| --- | --- |
| newest `recent_observation_protect_tokens` (6k) | verbatim |
| next band up to `tool_output_retention_tokens` (24k) | frozen head (12 lines / 800 chars) + re-fetch pointer |
| older | one-line `[cleared tool result: …]` + re-fetch pointer |

The re-fetch pointer (`- Spool File: "…"`, `- Artifact Path: "…"`,
`- Virtual Path: "…"`) is preserved verbatim in both lossy bands. Losing it
would leave the model unable to re-read what the projection elided.

Both lossy shapes are deterministic functions of the content, so the same live
message always projects to the same bytes. Decisions are measured on the
original message weights, which are immutable facts; a result can therefore only
move outward (verbatim → frozen → cleared), never oscillate between bands and
churn the server-side prefix cache.

`[INV-CONTEXT-02]` **Retention is a request projection, not a history rewrite.**
The durable transcript and its blob/spool artifacts are untouched. The
projection is bounded, reproducible, and re-derived on resume.

`[INV-CONTEXT-03]` **Remote continuations are exempt.** A `RemoteStored` route
anchors on `previous_response_id`; freezing an old message changes the local
semantic head and invalidates the cursor every turn, converting a cheap delta
into a full replay. Those routes get relief from the capped budget and
compaction, not from the retention projection.

`[INV-CONTEXT-04]` **A lossy projection preserves its escape hatch.** Any band
that removes bytes must retain the tool result's durable re-fetch pointer.

### 3. Normalize raster images at the ingest boundary

A new host capability (`nuo_host::image`) normalizes clipboard, pasted-file,
tool-produced, and rehydrated-artifact images:

- decode PNG/JPEG and downscale so the longest edge is **1568 px**;
- re-encode as PNG (lossless, alpha preserved) or, when the source was JPEG or
  the PNG exceeds the byte budget, as JPEG quality 82 with transparency
  composited onto white;
- enforce a **4 MiB encoded** budget; if still oversized, shrink the long edge
  in deterministic 0.8 steps until it fits;
- return `None` for images already within both budgets (no decode/re-encode
  churn) and for GIF/WebP (flattening an animation to its first frame would
  silently change meaning).

`[INV-CONTEXT-05]` **Attachments are normalized once, at ingestion.** No
provider request may carry an attachment larger than the normalized budget for
that ingest path.

### 4. Derive request revisions in one pass; never copy a subagent transcript

`request_revisions` derives the semantic head, envelope fingerprint, and
cacheable-prefix fingerprint in a single traversal, serializing each
non-system message exactly once. `ModelRequest` stores the prefix identity at
assembly; `cache_plan()` reuses it instead of re-serializing the conversation.
Any in-place mutation of an assembled request (the image-input probe) must call
`recompute_revisions()`.

The request projection clones messages through `clone_without_children`: a
`task` result's nested subagent transcript is durable/UI state that no provider
ever sees, and copying it per turn copied an entire subagent history for
nothing.

## Rejected Alternatives

- **Lower the watermark fractions globally.** A single 20%/30% rule would
  cripple a 64k-token route (12k/19k) while still allowing a 1M-token route to
  reach 200k/300k. The defect is the uncapped base, not the fraction.
- **Persist freeze directives per turn.** The transcript's projection model
  supports `Freeze`, but committing it every turn rewrites the session's
  projection state and pays an O(history) clone/write in exactly the path we are
  trying to make cheap. Request-scoped projection gets the wire benefit with no
  durability cost; the facts stay intact.
- **Prune tool output on every turn.** Incremental pruning changes the prefix at
  an arbitrary old position, invalidating the cache from that point. The
  tail-relative, monotone three-band projection changes only the message near
  the tail boundary, once per band transition.
- **Normalize GIF/WebP animations.** Losing frames is a semantic change, not a
  size optimization. Those formats are left untouched.
- **Keep the dead `cache_frozen` / `Freeze` record-time mechanism.** The
  documented "freeze at record time" wiring had no production caller; the
  request projection is the single mechanism that actually runs. `freeze_tool_output`
  now has a live caller and an explicit idempotence contract.

## Consequences

- The compaction band for current long-context models moves from 650k–850k to
  ~83k–109k tokens. A single very long ReAct round now compacts (and pays one
  summarization call) instead of running every turn at a quarter-million tokens.
  On the observed 375k-token session, the three-band projection alone removes
  roughly half of the prompt (the 388 KB of accumulated tool bodies collapse to
  a bounded verbatim head plus ~430 short shapes); the capped budget then
  compacts the remainder toward its 64k target.
- Stateless relays that replay the full conversation (e.g. the `opencode-go`
  `Stateless` Responses surface) send a materially smaller body every turn, and
  their server-side prefix cache continues to cover the bulk of the stable head.
- Remote-stored continuations keep their existing `previous_response_id` path
  untouched.
- Pasted and tool-produced screenshots are bounded once and replayed at the
  bounded size for the rest of the session.
- The durable transcript still records full facts (bounded by the existing
  8 KB record-time truncation and the spool/artifact stores); database size is
  not sacrificed for wire size.

## Validation

- `context_lifecycle::policy` tests: capped 1M window resolves to 128k with
  83.2k/108.8k/64k watermarks; a 64k route keeps its own window; a v1 document
  without the new keys receives the documented defaults.
- `pressure` tests: three-band projection from the tail, spool-pointer
  preservation in both lossy bands, idempotent re-projection.
- `nuo-provider` test: the one-pass revision function reproduces the standalone
  semantic/envelope/prefix fingerprints byte-for-byte.
- `nuo-host::image` tests: small images untouched, oversized downscaled to the
  long edge, unsupported formats untouched.
- Full `nuo-harness`, `nuo-server`, `nuo-protocol`, `nuo-provider`, `nuo-tool`
  suites remain green.
