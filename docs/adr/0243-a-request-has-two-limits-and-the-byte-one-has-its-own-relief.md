---
id: ADR-0243
title: "A Request Has Two Limits, and the Byte One Has Its Own Relief"
status: accepted
date: 2026-11-21
scope: protocol/pressure, protocol/error, provider/response, engine/orchestration, engine/cognition/agent
superseded_by: null
negative_knowledge: true
---

# 0243. A Request Has Two Limits, and the Byte One Has Its Own Relief

- **Status**: Accepted
- **Date**: 2026-11-21
- **Related**: [ADR-0019](0019-token-pressure-measurement-and-budget-accounting.md),
  [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md),
  [ADR-0214](0214-context-thresholds-follow-the-model-window-and-are-not-user-configuration.md),
  [ADR-0229](0229-every-context-element-has-a-production-owner.md)
- **Amends**: ADR-0019 (pressure is not token-only), ADR-0214 (a second, byte-denominated ceiling follows the route)

---

## Context and Problem Statement

A 1M-token session accumulated 67 pasted screenshots and stopped working. Every
request answered:

```text
OpenAI HTTP 413 Payload Too Large: [code=request_too_large] request body
exceeds configured limit
```

The session's own telemetry said it was healthy: the projected request was
441 861 tokens against a 1 000 000-token window, well below every threshold.
Both statements were true, because they measured different things:

| Dimension | What it bounds | Cost of one screenshot |
|---|---|---|
| Tokens | what the model can attend to | ≈ 1 600 tokens |
| Bytes | what an edge will carry | ≈ 800 KB of base64 |

Sixty-seven attachments therefore cost ~107 k tokens (9 % of the window) and
~55 MB of body (roughly seven times what a common edge accepts). Three defects
followed from budgeting only the first dimension:

1. **No gate could see the problem.** Pruning and compaction fire on token
   thresholds, which the session never approached. Nothing in the harness
   measured the serialized body at all.
2. **The verdict was misclassified.** `request_too_large` matched no known
   overflow code, so `413` became `InvalidRequest`; and because `413` was also
   in the vision-probe family (`400 | 413 | 415 | 422`), the harness concluded
   the *route* could not take images, stripped all 67, and retried.
3. **The recovery could not stick.** The probe latched a route only when the
   whole round returned `Ok`. That retry *succeeded* — 239 k prompt tokens — but
   the round kept running, and the next turn re-assembled from a history that
   still carried the images. It failed again, and because `image_probe` was
   still armed, the harness reported *"retrying without images did not help"*
   about a hypothesis it had just confirmed. The round died on a duplicate 413.

The underlying model of a request was incomplete. A request is delivered over a
transport, and the transport has a limit of its own. Token accounting is a
budget for the **model**; it is not a budget for the **wire**.

## Decision

**A request is budgeted in both dimensions it is actually measured in, and each
dimension has the relief that fits its physical cause.**

### 1. Byte size is a measured property of the assembled request

`protocol::request_wire_size` reports `body_bytes`, split into `media_bytes`,
`text_bytes`, and `tool_schema_bytes`. Attachments are measured exactly: base64
is ASCII with no JSON-escapable characters, so its serialized length is its
length. Text is counted at UTF-8 length and treated as a lower bound, which is
why relief aims below the ceiling rather than at it.

The budget rides on `ContextBudget::max_request_body_bytes`
(`DEFAULT_REQUEST_BODY_LIMIT_BYTES`, 8 MiB). No vendor publishes a portable
number, so the default is explicitly a conservative default, not a claimed
vendor fact. It is seeded with the token threshold on every route switch
(`[INV-CTXSRC-07]`: one place owns the numbers).

### 2. A refusal is better evidence than a default

When a route answers `413`, the harness records the refused size and lowers that
route's ceiling below it (`Agent::learn_payload_ceiling`, keyed by route
fingerprint like the image-suppression latch). The next round is projected
against the *observed* limit instead of re-uploading the same failing body. The
correction is deliberately decisive — a quarter below the size that failed —
because the transport says only "too large" and a timid step pays another
multi-megabyte upload to learn nothing. Three passes bound the loop
(`MAX_PAYLOAD_RELIEFS`).

### 3. Byte relief is attachment offload, not compaction

`protocol::plan_media_offload` selects attachments by **recency and budget**:

- the newest attachment always survives — the conversation is about it, and a
  request without it answers a different question;
- older attachments survive while they fit inside 90 % of the ceiling;
- the rest are evicted oldest-first until the projected body fits.

Selection is per *attachment*, not per message: a single prompt can carry six
screenshots, and message granularity would protect all six or none. Text is
preserved verbatim; only pixels leave the request.

Compaction is the wrong tool here. It folds history to fit a token window and
would drop the rounds the user is working in, while leaving the newest — and
therefore largest — attachments untouched.

### 4. What leaves the request is persisted before it leaves

The invoice that replaces an attachment prints an `Artifact Path`. A path is a
promise that a producer wrote the bytes there, so
`orchestration::persist_media_artifacts` writes them to the content-addressed
spool **before** the projection is applied (`[INV-SPOOL-01]`, ADR-0093). The
path is also readable: `read` resolves a spool path and attaches images, so an
offloaded screenshot is recoverable by the model on demand rather than lost.

The projection is committed (`ContextProjectionKind::Payload`), so the durable
window and the next round agree instead of re-deriving the same eviction — and
re-hashing the same 40 MB — every turn.

### 5. The three refusal families are disjoint

| Family | Status/code | Relief |
|---|---|---|
| Token overflow | `400` + `context_length_exceeded` | compaction |
| Body too large | `413`, `request_too_large` | attachment offload |
| Payload shape refused | `400`, `415`, `422` | vision-capability probe |

`ProviderErrorKind::PayloadTooLarge` is the byte verdict.
`is_request_refusal` excludes it (`[INV-PAYLOAD-01]`: a size verdict says nothing
about a capability), so `413` can no longer withhold vision permanently. A
`413` is accepted as payload pressure by status even when no code parses, and a
status-less verdict is still recognized by kind.

### 6. Withholding is round-scoped when it is an experiment

The probe now arms a round-scoped `withhold_images` flag, and every later turn
of that round is projected without attachments. The probe's evidence is that
one request succeeded *without* them; letting the next turn re-add them made the
harness disprove its own confirmed hypothesis.

## Invariants

- **`[INV-PAYLOAD-01]` Size and capability are disjoint** — a payload-size verdict
  (`413` / `PayloadTooLarge`) says nothing about whether the route accepts
  images. It never enters `is_request_refusal` and never withholds vision
  permanently. Enforced by
  `crates/protocol/src/error.rs::tests::a_body_too_large_refusal_belongs_to_payload_relief`
  and the image-probe integration suite.
- **`[INV-PAYLOAD-02]` Withholding is round-scoped and outcome-evidenced** — a
  successful attachment-free retry is the only evidence that latches
  `withhold_images`; no vendor error text is parsed, and the flag resets when
  the hypothesis is disproved.

## Consequences & Trade-offs

- **Positive**: a screenshot-heavy session stays deliverable. The newest
  attachments survive, the conversation text is untouched, the durable
  transcript keeps every image, and each offloaded pixel is recoverable at the
  artifact path named in the request.
- **Positive**: a size refusal is now diagnosed by the dimension that caused it.
  `413` no longer masquerades as "this model cannot see images", so a working
  vision route is never latched off by a transport budget.
- **Negative**: the byte ceiling is a configurable-free internal constant. A
  provider that silently accepts more than 8 MiB will still have old
  attachments offloaded until a `413` teaches the harness otherwise — the
  default trades some attachment retention for not uploading bodies that will
  be refused.
- **Negative**: the first request on an unknown route can still be refused once.
  The recovery exists precisely because no portable number exists; the learned
  ceiling makes it a one-time cost per route per session.
- **Incurred**: offloaded pixels occupy spool space (RAM on Linux). The spool is
  ephemeral by design; the durable copy remains the session transcript.

## Discarded Alternatives (Negative Knowledge)

### Alternative A: raise the token estimate for images

- **Why considered**: cheapest possible change; `estimate_message_tokens` would
  count bytes instead of a flat 1 600 per image, so the existing token gate
  would fire.
- **Why rejected**: it conflates two independent limits. A 55 MB body inside a
  1M-token window is exactly the case where the token gate is *correct* to stay
  quiet, and inflating the estimate would also make the context meter lie about
  what the model is charged. The model charges ~1 600 tokens for that image
  whether or not the edge accepts the bytes.

### Alternative B: treat `413` as `ContextOverflow`

- **Why considered**: both are "request too big", and the compaction recovery
  already exists.
- **Why rejected**: compaction folds *old* rounds, while the offending bytes are
  usually the *newest* attachments it preserves. Routing `413` to compaction
  would spend a summarization call and still send the same body. The two also
  differ in the report they must produce (`Prune`/`Compact` vs `Payload`).

### Alternative C: keep the vision probe and just latch earlier

- **Why considered**: the probe *did* find a working request; only the latch
  timing was wrong.
- **Why rejected**: the probe's conclusion would be wrong even if it latched.
  Withholding every image forever is not the remedy for a size limit; the newest
  images fit and must stay. It also permanently disables a capability that
  works, for a reason the user cannot see or undo.

### Alternative D: hard-cap attachments at ingest

- **Why considered**: `host::image::normalize_image` already bounds one image
  to 1 568 px and 4 MiB, so bounding the *aggregate* there looks natural.
- **Why rejected**: an ingest cap cannot know the route, and it would refuse
  input the active model can accept. The message also has no way to un-refuse
  later. The request boundary is the only place that knows both the bytes and
  the ceiling, so that is where the decision belongs.

## Links

- [`crates/protocol/src/pressure.rs`](../../crates/protocol/src/pressure.rs) —
  `request_wire_size`, `plan_media_offload`, `payload_relief_target`
- [`crates/protocol/src/error.rs`](../../crates/protocol/src/error.rs) —
  `ProviderErrorKind::PayloadTooLarge`, `is_payload_too_large`
- [`crates/engine/src/orchestration/payload.rs`](../../crates/engine/src/orchestration/payload.rs) —
  artifact persistence and the relief budget
- [`crates/engine/src/orchestration/context_gate.rs`](../../crates/engine/src/orchestration/context_gate.rs) —
  `ContextProjectionGate::project_payload`
