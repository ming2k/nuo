---
id: ADR-0233
title: "A Slash Command Is Never a Conversation Message"
status: accepted
date: 2026-12-20
scope: tool/message, view/document, nuo/shell, persistence/thread, server/handlers
superseded_by: null
supersedes:
  - "`InjectionKind::CommandEcho`"
  - "`Message::command_echo`"
  - "`Message::is_command_echo`"
  - "`UserMessageOrigin::Slash`"
negative_knowledge: true
---

# 0233. A Slash Command Is Never a Conversation Message

- **Status:** Accepted
- **Date:** 2026-12-20
- **Amends:** [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)
  (its "record the invocation, project it out before the wire" shape is retired),
  [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md)
  §5 (its data-shape tolerance for "pre-ledger `CommandEcho` rows" protected a
  population that never existed)
- **Related:** [ADR-0140](0140-command-effect-classes-readings-are-receipts-not-records.md),
  [ADR-0232](0232-a-classifier-is-only-closed-if-its-members-have-producers.md),
  [ADR-0231](0231-one-instruction-region-and-no-ghost-extension-subsystem.md)

## Context and Problem Statement

`InjectionKind::CommandEcho` described a non-driving command echo: a visible
user-role message recorded for resume/export but projected out before the wire.
Six readers depended on it, `Message::command_echo` constructed it, and ADR-0117
§5 declared that *"data-shape tolerance for pre-ledger rows (historical
`CommandEcho` user messages) stays confined to the persistence read boundary as a
migration concern"*.

Every part of that was false. Verified against the full history:

1. **No production writer ever existed.** Across every commit in
   `git log --all`, `InjectionOrigin::new(InjectionKind::CommandEcho)` appears
   only inside the body of `Message::command_echo`, and
   `Message::command_echo` is called by no production code — only by tests.
2. **The migration it was "tolerating" never existed.** The claim first appears in
   `protocol/src/command.rs` in the initial commit: *"Legacy sessions that still
   carry `Message::command_echo` rows fold into this ledger at schema migration
   time."* No migration, in any file, ever did that.
3. **A command cannot reach `messages` at all.**
   `AgentRequest::SlashCommand → handlers_slash::dispatch → record_command`
   writes a `CommandRecord` (the durable ledger) and emits
   `RoundEvent::CommandResult`. It constructs no `Message`. The transcript row is
   `TranscriptMessage::command_result`, synthesized from the ledger, exactly as
   ADR-0140 designed.

So the variant protected nothing, the readers filtered nothing, and the
compatibility argument described an empty set. It was a **tombstone over a grave
that was never dug**.

### The regression this exposed

While auditing it, the `is_command_echo` predicate had been widened to also match
the *live* slash shape — a `display_content` whose text begins with `/`. That
widening was wrong in a way worse than the dead variant:

- `display_content` is set on **every** `AgentRequest::Steer` and `FollowUp`
  (`event_loop/actions/commands.rs`), carrying the user's literal text beside the
  expanded `content`.
- `is_command_echo` was consulted at the **wire funnel**
  (`Agent::prepare_request_messages`), where it is a `retain(|m| !is_command_echo())`.

Therefore a genuine user prompt beginning with `/` — `/etc/hosts is wrong, please
look` — would have been **silently deleted from every provider request** for any
client that does not route `/`-prefixed input to a slash dispatcher first (ACP,
the web client, an embedding). The TUI escaped only because it intercepts leading
`/`, which is a client convention, not an invariant. Verified: such a `Message`
reported `is_command_echo() == true`.

## Decision

### 1. A command is a ledger row, never a message

`InjectionKind::CommandEcho`, `Message::command_echo`, `Message::is_command_echo`,
and `UserMessageOrigin::Slash` are all deleted. The durable command record is
`CommandRecord`; the rendered row is `TranscriptMessage::command_result`; nothing
in between is a `Message`.

### 2. No shape heuristic may delete a message

The following filters are removed, because each one could drop a real user turn:

| Site | Was | Is |
| :--- | :--- | :--- |
| `Agent::prepare_request_messages` | `retain(!is_command_echo)` | nothing to retain against |
| `causal_compactor` | `!message.is_command_echo()` | a user message opens a turn |
| `thread::last_user_prompt` | skip echo provenance | skip `hidden` only |
| `recovery` round opener | skip echo provenance | skip `hidden` only |
| `transcript_messages_from_core` | fold echo + `/`-shape out | no fold |
| `opens_round` | skip echo rows | skip `UserSteer` inserts only |

The rule this ADR encodes: **a projection may omit a message because of its
provenance, never because of its text.** `Message.hidden` is provenance. A leading
`/` is text.

### 3. The reader-side display distinction is not needed

`UserMessageOrigin` keeps `Chat`, `Steer`, `FollowUp`. It existed to let the
Activity modal avoid mistaking a command for the round's prompt — but a command is
not in the transcript's dialogue at all, so the case cannot arise. Its label arm
(`"cmd"`) and `prompt_kind`'s fallthrough go with it.

### 4. `[INV-INJECT-01]`'s exception list is now empty

The producer scan no longer names an exemption. `InjectionKind::ALL` has twelve
members and every one has a production constructor. A future tombstone must add
itself to the scan out loud; there is no wildcard and no residue.

## Invariants

- **`[INV-CMDMSG-01]` A Command Is Not a Message** — the slash dispatcher writes a
  `CommandRecord` and a `RoundEvent`; it constructs no `Message`. Enforced by the
  wire funnel having nothing to filter and by `[INV-INJECT-01]` (no echo variant
  exists to construct).
- **`[INV-CMDMSG-02]` No Text-Shaped Projection** — a wire or transcript projection
  may skip a message for its provenance or its `hidden` flag, never for its text.
  A `/` or `!` prefix carries no structural meaning.
- **`[INV-CMDMSG-03]` A Retired Admission Path Is Deleted, Not Tolerated** — a
  "legacy rows" argument must name the migration that produced them. `CommandEcho`
  had neither a writer nor a migration; the tolerance outlived the fiction.

## Consequences & Trade-offs

- **Positive**: one admission path for commands (the ledger); a `/`-prefixed user
  prompt can no longer be lost; six readers and a predicate that could silently
  drop input are gone; `InjectionKind` has no tombstone; `UserMessageOrigin` has
  three members that each mean something.
- **Positive (security)**: the deleted heuristic was a *model-visibility* filter
  driven by a string prefix. Any peer able to write a `QueuedMessage` could choose
  whether their own text was delivered by prefixing it with `/`.
- **Negative & incurred**: a session file carrying a `command_echo` provenance
  would no longer decode. No such file can exist — there was never a writer — and
  the durable schema version is the lever if one is ever found.
- **Negative & accepted**: restored sessions from the pre-ledger era show no
  command rows in the dialogue. They never did; the ledger is where commands live.
- **Neutral**: the command ledger, `CommandRecord`, `RoundEvent::CommandResult`,
  `merge_command_rows`, the command-output pager, and `[INV-CMD-07]`'s
  completeness net are unchanged.

## Discarded Alternatives

### A — Keep the variant as an inert tombstone

- **Why considered**: zero risk, and ADR-0232 permits a documented tombstone.
- **Why rejected**: ADR-0232's permission was for a residue with a real
  population. This one has no writer, no migration, and no data. A tombstone with
  nothing under it is pure tax on every exhaustive `match`.

### B — Keep the `display_content` heuristic but confine it to the TUI

- **Why considered**: the TUI genuinely needs to *display* a restored slash turn
  as a command row.
- **Why rejected**: the TUI does not have that shape to display — it stages
  `pending_command` rows in the view layer and the ledger supplies the durable row.
  Keeping the heuristic was the source of the wire-filter bug.

### C — Add a `CommandEcho` migration now, to justify the tolerance

- **Why considered**: makes the ADR-0117 story true after the fact.
- **Why rejected**: there is nothing to migrate. Writing a migration for a row
  shape that was never produced is a ceremony that pretends history happened.

## Compliance and Verification

- `crates/engine/src/tests/scheduling.rs::a_slash_prefixed_prompt_reaches_the_wire`
  — a `/`-prefixed user prompt survives model-request assembly.
- `crates/nuo/src/shell/tests/history_recall/restore.rs::restored_user_message_origin_is_never_inferred_from_text`
  — four text shapes (including `/etc/hosts …`) all restore as `Chat`.
- `tests/integration/governance/context_engineering.rs::inv_inject_01_every_injection_kind_has_a_producer`
  — twelve variants, no exemption list.
- `crates/tool/src/message/tests.rs::every_injection_kind_serialises_distinctly`
  — iterates `InjectionKind::ALL`.
