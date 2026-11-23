---
id: ADR-0232
title: "A Classifier Is Only Closed If Its Members Have Producers"
status: accepted
date: 2026-12-20
scope: tool/message, tool/envelope, engine/cognition
superseded_by: null
supersedes:
  - "`InjectionKind::SessionReviewInput`"
  - "`InjectionKind::SystemPrompt`"
  - "`Tag::SystemReminder`"
negative_knowledge: true
---

# 0232. A Classifier Is Only Closed If Its Members Have Producers

- **Status:** Accepted
- **Date:** 2026-12-20
- **Amends:** [ADR-0226](0226-one-renderer-and-one-reader-for-message-envelopes.md)
  (the envelope vocabulary shrinks), [ADR-0228](0228-untrusted-content-has-one-framing.md)
  (the `Foreign` origin finally has its producer)
- **Related:** [ADR-0229](0229-every-context-element-has-a-production-owner.md),
  [ADR-0231](0231-one-instruction-region-and-no-ghost-extension-subsystem.md),
  [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md)

## Context and Problem Statement

ADR-0229 established the rule for *context* declarations: a `Topic` or an
`InputKey` without a production source is a test failure. The same pattern was
live, unnoticed, in the two **message** vocabularies — `InjectionKind` (provenance)
and `Tag` (envelope region). Their doc comments asserted coverage:

> Each variant maps 1:1 to a concrete injection site in the harness; the doc-link
> in each arm is the single source of truth for "where does this come from".

That claim was false for three members, and no test could see it:

| Member | Claim | Reality |
| :--- | :--- | :--- |
| `InjectionKind::SessionReviewInput` | *"the variant stays so old session files carrying injected rows still deserialize"* | `Message.origin` is `Option<…>` with `#[serde(default)]`, so an unknown/missing origin already loads. The variant bought no compatibility and taxed every `match`. |
| `InjectionKind::SystemPrompt` | *"the harness rebuilt the head system message"* | Its only constructor was `prepare_request_messages_debug` — a **preview** function. A live request carries instruction text in `ModelRequest::instructions`; it never becomes a `Message`. The name described a pipe this variant is not on. |
| `Tag::SystemReminder` / `Tag::Checkpoint` | *"the closed tag vocabulary of model-facing envelopes"* | No door produced them. `Checkpoint` in particular sat next to a **hand-written** `[Conversation checkpoint]` prose header in `protocol`, which is exactly the second-spelling defect ADR-0226 exists to remove. |

Two more asymmetries were real but undocumented, so a reader could not tell them
from the same rot:

- `Tag::SubagentResult` has a writer and no reader.
- `Origin::Foreign` had no producer at all: `InjectionKind::UntrustedDirective`
  was documented as the framing for "foreign task data" and constructed nowhere.
  The one door that *does* deliver foreign-authored text — an inter-agent note off
  the ACP fabric — wrote it into the user role **unframed**.

The lesson is not "these five members were wrong". It is that a closed vocabulary
with no producer scan is a declaration surface with no enforcement, and its doc
comment is the only thing holding the line. ADR-0229 already learned this for
`Topic`; this ADR applies it to messages.

## Decision

### 1. Delete the members with no producer

- `InjectionKind::SessionReviewInput` — removed. The "old files" argument was
  false (`origin` is optional with a serde default); a retired diagnostic does not
  get to live in a live enum.
- `InjectionKind::SystemPrompt` — removed. The debug preview now emits a plain
  `Role::System` message with **no** injection origin: the role is the whole fact,
  and stamping a provenance would claim this row was injected into a conversation
  when it is a preview-only projection.
- `Tag::SystemReminder` — removed. Nothing produced it.

The names `DebugPreview` and `InjectionKind::Unknown` were both considered and
rejected: `DebugPreview` would have described a site that no longer stamps, and
`Unknown` buys a lossy fallback for a field whose whole job is classification.

### 2. Give `Tag::Checkpoint` and `Origin::Foreign` their producers

- `checkpoint_message` now renders through the one envelope renderer:
  `<checkpoint status="durable" note="…">summary</checkpoint>`. `CHECKPOINT_HEADER`
  is deleted. This is the sixth and last hand-written model-facing format from
  ADR-0226's table.
- An inter-agent note is framed as `<untrusted origin="foreign">…</untrusted>`.
  The provenance stays `InjectionKind::InterAgent` (who spoke) and the framing says
  what the bytes are (data). Without it, any ACP peer — including one relaying a
  tool argument a third model wrote — could place a directive in another session's
  user role.

### 3. Document the real asymmetry instead of hiding it

`Tag::SubagentResult` has no reader. That is correct and now says so in
[`tool::envelope::subagent_result`]: a one-shot queued follow-up has no address to
de-duplicate and no "is this a fold" question to answer, but its two
model-authored strings still need escaping and terminator neutralization.

### 4. Exhaustiveness becomes data, and the data is scanned

- `InjectionKind::ALL` and `Tag::ALL` are the single lists; `Tag::from_name` and
  the serialisation test iterate them instead of a hand-written copy. (The
  hand-written copy in the test is *how* the two dead variants stayed tested —
  meaning not at all.)
- `InjectionKind::tag()` gives one spelling per variant, used by diagnostics and by
  the scan, so a rename is one edit.
- Two governance scans (`[INV-INJECT-01]`, `[INV-ENVELOPE-07]`) fail the build if a
  member has no production constructor. They strip every `pub const … ALL …`
  block before scanning: a constant that lists the vocabulary by definition cannot
  be allowed to vouch for it. A third test asserts the scan corpus contains the
  known producers, so narrowing the directory list fails loudly instead of
  certifying a dead vocabulary.

## Invariants

- **`[INV-INJECT-01]` No Unproduced Injection Kind** — every `InjectionKind::ALL`
  member has a production constructor outside the declaration and the tests.
- **`[INV-ENVELOPE-07]` No Unproduced Envelope Tag** — every `Tag::ALL` member has
  a production constructor.
- **`[INV-INJECT-02]` No Backwards Compatibility Variant** — a retired injection
  path is deleted, not retained "so old rows deserialize". `Message.origin` is
  optional; the durable schema version is the compatibility lever.
- **`[INV-INJECT-03]` Provenance and Trust Are Different Axes** — `InjectionKind`
  answers *who wrote this*; the envelope answers *how it must be read*. A variant
  does not imply trust, and a frame does not imply provenance.
- **`[INV-ENVELOPE-08]` A Writer-Only Tag States Why** — a tag with no reader
  documents the reason at its constructor, so asymmetry is a decision rather than
  an omission.

## Consequences & Trade-offs

- **Positive**: two dead injection variants and one dead tag are gone; the sixth
  hand-written model-facing format (`CHECKPOINT_HEADER`) becomes an envelope; an
  inter-agent note can no longer masquerade as a user directive; a future dead
  member fails a test instead of a review.
- **Negative & incurred**: a persisted row whose `origin.kind` was `system_prompt`
  or `session_review_input` no longer deserializes. No live producer could write
  either after ADR-0231, and `origin` is `Option` with `serde(default)`, so the
  affected surface is a historical debug preview and a retired diagnostic — both
  out of the durable path. `THREAD_SCHEMA_VERSION` is the lever if a real session
  file is ever found to carry one.
- **Negative & accepted**: the checkpoint message's bytes change
  (`[Conversation checkpoint]` → `<checkpoint …>`). It is model-facing text with no
  parser; the transcript asks the payload, not the prose.
- **Neutral**: `Rank` placement, the untrusted framing itself, and every other
  envelope are unchanged.

## Discarded Alternatives

### A — Keep the variants and add `#[serde(other)] Unknown` to `InjectionKind`

- **Why considered**: one line, and forward-compatible.
- **Why rejected**: `InjectionKind` exists to classify. A fallback makes every
  unknown provenance indistinguishable, which is the one failure the type was built
  to prevent. `Topic` has no `Unknown`; neither should this.

### B — Rename `SystemPrompt` to `DebugPreview`

- **Why considered**: keeps a useful debug marker.
- **Why rejected**: the variant would then describe a site that no longer stamps
  it — `Role::System` already says everything a preview row needs. A renamed
  unproduced variant is still an unproduced variant.

### C — Give `Tag::SubagentResult` a reader for symmetry

- **Why considered**: "every tag must round-trip" is satisfying.
- **Why rejected**: there is nothing to look up. The envelope's job there is
  escaping and neutralization, both writer-side. Inventing a reader to satisfy a
  symmetry rule is the same defect as inventing a producer to satisfy a scan.

## Compliance and Verification

- `tests/integration/governance/context_engineering.rs` —
  `inv_inject_01_every_injection_kind_has_a_producer`,
  `inv_envelope_07_every_tag_has_a_producer`,
  `the_producer_scan_corpus_contains_the_declarations`.
- `crates/tool/src/message/tests.rs::every_injection_kind_serialises_distinctly`
  now iterates `InjectionKind::ALL` and asserts `tag()` agrees with the serde
  spelling.
- `crates/tool/src/envelope.rs::every_tag_has_exactly_one_spelling` iterates
  `Tag::ALL`.
- `crates/engine/src/cognition/agent/steering.rs::an_inter_agent_note_is_framed_as_untrusted_data`.
- `crates/tool/src/envelope.rs::a_subagent_report_cannot_reshape_the_message`.
- `tests/integration/engine/it/compaction.rs` asks the envelope parser which tag a
  projected row carries, instead of matching the retired prose header.
