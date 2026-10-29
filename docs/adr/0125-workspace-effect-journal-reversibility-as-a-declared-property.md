---
id: ADR-0125
title: "Workspace Effect Journal: Reversibility as a Declared Property, Write-Ahead Capture, and Honest Refusal"
status: accepted
date: 2026-10-29
scope: protocol/nuo-protocol, protocol/nuo-tool, tools/nuo-tool-fs, persistence/nuo-persistence, harness/nuo-harness, server/nuo-server, presentation/nuo-view
superseded_by: null
negative_knowledge: true
---

# 0125. Workspace Effect Journal: Reversibility as a Declared Property, Write-Ahead Capture, and Honest Refusal

- Status: Accepted
- Date: 2026-10-29
- Deciders: Nuo Architecture Working Group, Runtime Engineering, Persistence, Agent Alignment
- Consulted: Harness, Tool Infrastructure, Server, Presentation
- Amends: [ADR-0008](0008-single-tool-contract.md) (adds the effect axis to the tool contract), [ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md) (the dispatch pipeline gains a bracketing stage)
- Establishes: `INV-EFFECT-01`..`INV-EFFECT-07`

---

## Context and Problem Statement

Before this ADR, the repository documented three capabilities it did not have:

1. `/diff` — "view workspace modifications made in this thread" — emitted a fixed
   acknowledgement string and read nothing.
2. `/undo` — "undo the last thread turn and file changes" — moved the session
   tree's active leaf to its parent. It restored **no file content at all**; the
   user's working tree was untouched while the reply said the turn was rolled back.
3. `HazardTier::Tier1Mutation` — "sandboxed within workspace; **commit journal
   undo**" — described a journal that did not exist.

These were not merely missing features. They were a class of defect the project
already recognizes: a documented promise with no mechanism behind it. A user who
trusts `/undo` and finds their file unchanged has been actively misled, which is
worse than a tool that says it cannot help.

Two structural facts made the gap hard to close:

- **The structured result was never persisted.** `ToolOutput::Patch { old, new, .. }`
  existed only on the live event stream; the durable transcript received
  `Message::tool_result(call, text)`. After a resume there was nothing to diff
  against, because the pre-image was gone.
- **The tools declared no file accesses.** `write_file` and `edit_text` did not
  override `Tool::accesses`, so both fell back to the trait default derived from
  `scope_target` — which they also did not override. Two concurrent writes to the
  same path therefore did not serialize in the scheduler, and the workspace-jail
  middleware had to recover the target by string-matching argument key names.

The naive remedy — "snapshot the workspace, restore it on `/undo`" — is wrong in a
way that is invisible until it destroys work: between a thread's write and the
user's `/undo`, the user edits the file. A snapshot restore overwrites that edit.
Git-based snapshots make this worse, not better: `git checkout` to undo an agent
turn silently discards the user's own uncommitted changes, and the project's own
bash policy already classifies `git restore`/`git checkout --` as commands
requiring confirmation for exactly this reason.

## Decision Drivers

- **No promise without a mechanism.** A command must either do what it says or say
  it cannot.
- **Never destroy work the journal did not produce.** A reversal that overwrites a
  user edit is worse than a refusal.
- **Reversibility is a property of the effect, not of the tool's self-description.**
  `write_file` calls itself an "idempotent mutation" while truncating whatever was
  there; `execute_command` is genuinely irreversible and no heuristic changes that.
- **Zero architectural rot.** The repository has hard tier gates and a
  zero-legacy policy; the design must fit the existing layers rather than adding a
  parallel one.
- **No overclaiming to the user.** `/diff` and `/undo` must distinguish what they
  can reverse from what they cannot, in the UI and in the data model.

## Considered Options

### A. Git-backed snapshots (rejected)

Commit the workspace each round and `git checkout` to undo.

*Why rejected*: (a) it fights the user's own git state — index churn, branch
switches invalidating the journal, `.gitignore`d paths (build output) invisible to
the snapshot, so "undo succeeded" while the build tree diverged; (b) it pollutes
user history with agent commits, breaking rebase and bisect; (c) reverting via
`git checkout` destroys the user's uncommitted edits, the exact failure the design
must prevent; (d) it covers only files, so it cannot represent a shell or network
effect at all — and would therefore *look* complete while silently lying about the
half of the surface the user most worries about.

Git is used only as an **observer**: the journal takes no dependency on it.

### B. Per-tool journaling (rejected)

Let each mutating tool record its own pre-image before writing.

*Why rejected*: this makes the journal a **self-report**. The one property that
makes it trustworthy — that a tool cannot claim to have changed less than it did —
is lost, because the tool controls both the mutation and the record. A buggy tool
would produce a journal that agrees with its bug.

### C. Filesystem snapshot of the workspace (rejected)

Copy the whole tree per round (or use a copy-on-write filesystem).

*Why rejected*: unbounded storage proportional to workspace size rather than to
change, and the restore path has the same "clobbers user edits" defect as (A)
unless combined with the three-way check this ADR adopts anyway — at which point
the snapshot machinery buys nothing over per-path pre-images.

### D. Effect-class taxonomy + write-ahead capture + three-way reversal (chosen)

Classify each call's reversibility as a **declared, closed property**; capture
pre-images from the filesystem *before* the mutation; reverse only after proving the
file has not moved on.

## Architectural Decisions

### 1. `EffectClass`: reversibility as a first-class, closed axis (`nuo-tool`)

`RiskProfile` answers a *pre-execution* question ("must a human authorize this?").
Reversibility is a different axis and conflating them produces a lie in one
direction or the other.

```rust
pub enum EffectClass {
    None,            // no state outside the conversation changed
    FileBytes,       // a declared, locatable path; reversible from a pre-image
    ExternalEffect,  // a system outside this process/filesystem
    ProcessEffect,   // a started/signalled/reaped process
    Opaque,          // arbitrary execution, or an unlocatable mutation
}
```

The enum is closed so every new tool must pick a class, and every consumer that
switches on it must handle all of them. It carries `is_reversible`,
`requires_capture`, `irreversibility_reason`, and `join` (the latter taking the more
severe of two classifications, so severity may be *promoted* and never laundered).

`Tool::effect_class()` derives from `RiskProfile` by default;
`Tool::effect_class_for(arguments)` reconciles that ceiling with the tool's declared
`accesses`, with exactly two possible transitions, both upward:

- a `FileBytes` ceiling with **no** mutating access becomes `Opaque` — the journal
  cannot capture a pre-image for a path it was never told about, so claiming
  file-reversibility would produce an `/undo` that silently does nothing;
- a `None` ceiling with a declared write becomes `FileBytes` — an observed write
  cannot hide behind a weaker self-description.

### 2. The fs tools must declare their accesses (`nuo-tool-fs`)

`write_file` and `edit_text` now override both `scope_target` (permission) and
`accesses` (scheduler plus effect derivation). The two are deliberately different:
reads keep `ScopeTarget::Unspecified` so they stay auto-admitted — reading is not an
authority a user should be asked to grant — while declaring a real `Read`/`Search`
access so the scheduler can serialize them against a concurrent write. A single
`path_argument()` helper is the one mapping from arguments to path, so a tool cannot
declare one path to the permission gate and another to the scheduler.

### 3. The journal is write-ahead, immutable, and append-only (`nuo-persistence`)

`workspace_effects` (migration 26) records one row per observed effect. A file
mutation is journaled in three phases:

```
append   (before_hash set, completed_at_ms NULL)   ← pre-image durable, mutation not yet run
   ↓
mutate   (the tool writes the file)
   ↓
complete (after_hash set, completed_at_ms set)     ← frozen observation
   ↓
reverse  (reversed_at_ms set)                      ← one-shot, terminal
```

The order is load-bearing: appending *after* the mutation would leave changed bytes
unrecorded after a crash, which is the exact failure the journal exists to prevent.
An incomplete row is therefore not evidence that the mutation happened — it is
evidence that it *may* have — and reversal refuses it rather than guessing.

Immutability is enforced in SQL, not by convention, mirroring `facts`:

- `INV-EFFECT-01` — observations are write-once and rows cannot be deleted. A
  journal a bug can rewrite is not evidence, and `/undo` reads `before_hash` as its
  sole authority on prior content.
- `INV-EFFECT-02` — reversal is one-shot and monotonic. Replaying `/undo` cannot
  write stale bytes back over newer content.
- `INV-EFFECT-03` — completion is one-shot and the post-image is write-once, so an
  incomplete row stays interpretable forever.
- `INV-EFFECT-04` — an unconfirmed effect cannot be reversed.

Content lives in the existing `BlobStore` CAS; the bytes are written before the row
is appended, so a row never references content that was not durably stored. Writes
go through the session's **single-writer door** (five new `PersistenceCommand`
variants), never a second connection.

### 4. Capture is an observation, not a self-report (`nuo-harness`)

`runtime::workspace_journal` reads pre- and post-images from the filesystem, not
from `ToolOutput::Patch`. A tool's self-report can be incomplete, stale, or simply
wrong; the journal's promise is to the user's bytes, so it records what the disk
actually held. Paths are resolved to absolute, lexically normalized form without
symlink resolution, so a retargeted symlink cannot redirect a later `/undo`.

### 5. The bracket is middleware in dispatch (`nuo-harness`)

A tool must not journal itself. `execute_tool` brackets the invocation:

- `begin` captures and appends **before** the tool runs. Its error is a **veto**:
  the tool is not invoked and the user is told why, because an uncaptured mutation
  would be silently irreversible.
- `finish` observes post-images and completes the rows **after** the tool returns,
  including on failure — a tool that errored halfway may still have written bytes,
  and leaving the row unconfirmed would make them permanently un-undoable.

Journaling is installed per session (via `Agent::set_workspace_journal`), so a
subagent with no session of its own journals nothing: its effects belong to its
parent's round.

Non-reversible calls still get a **receipt** — class, irreversibility reason, and
post-state where a single path applies. `/diff` shows them under "cannot undo" and
`/undo` refuses them with a real explanation. Recording only the reversible calls
would make `/diff` a list of pleasing news.

### 6. Reversal is a three-way check (`nuo-harness`)
```
current == after_hash   →  nobody changed it since; restore before_hash
current != after_hash   →  the file moved on; refuse and say so
file missing, created   →  already removed; nothing to do
file missing, edited    →  deleted out from under us; refuse
```

Effects are reversed **newest first**, so two writes to one path unwind through
their own history rather than refusing a file that was perfectly undoable. A
refusal is the correct outcome, not a failure: it is what lets `/undo` be trusted.

### 7. The wire type carries the distinction (`nuo-protocol`)

`CommandResult::WorkspaceChanges { reversible, irreversible, unconfirmed }` and
`CommandResult::UndoOutcome { restored, refused, nothing_to_undo }` are structured so
a frontend **cannot** render an irreversible change as though it were reversible.
`to_text` owns the rendering for every consumer (live display, resume, `/export`),
and the three groups are always present so the user learns the vocabulary once.

### 8. Liveness is derived, and retention expires bytes rather than rows

Two follow-on decisions that the first implementation got wrong, recorded here because
both are invariants a future change must not undo.

**Liveness is a union over the tables that can reach the bytes** (`INV-EFFECT-06`). The
CAS collector derives its live set from `blob_refs` **and** from the journal's
`before_hash` / `after_hash`. The first cut derived it from `blob_refs` alone, which
meant the first maintenance pass deleted every pre-image and `/undo` reported "the
recorded pre-image is no longer available" for a row that still looked undoable — a
broken promise with no signal. Registering journal hashes *into* `blob_refs` was
rejected: the ledger's key is `(session_id, hash)`, so two owners sharing a row means
pruning either one silently disowns the other's bytes.

**Retention expires bytes, never rows** (`INV-EFFECT-05`). A pre-image older than the
window (`DEFAULT_PREIMAGE_RETENTION_DAYS`) gains `expired_at_ms`; the row keeps its
observation and stops contributing to the live set. Pruning the row instead would mean
a change that happened stops having happened, and the journal is evidence. Expiry is
one-way for the same reason reversal is: an expired pre-image must not be resurrected
after the collector has reclaimed its bytes.

### 9. Previews come from the tool, are pure, and surface at the approval gate

`Tool::preview` returns a `PreviewResult` (ADR-0125 §9) for the calls whose effect can
be computed without causing it. Three constraints make it trustworthy rather than
decorative:

- **One function, two callers.** `edit_text` plans the edit in `plan_edit`, used by both
  `preview` and `execute`, so an approved diff cannot differ from the bytes written. A
  preview with its own match logic would drift and become the lie instead of the
  safeguard.
- **The preview carries the file, not the needle.** It shows the current bytes against
  the post-call bytes. Diffing `old_string` against `new_string` would hide the context
  that makes an edit safe or dangerous.
- **`None` is a real answer.** A shell command, a network fetch, or a subagent cannot
  describe its effect without causing it. The approval sheet renders that absence as an
  explicit line, because "no description is possible" is exactly what an operator needs
  before granting authority to something unpredictable.

It is placed on `PermissionRequest` rather than on a separate event so the diff and the
question cannot be decoupled, and the sheet renders it in both states: a prompt whose
most decision-relevant content hides behind a toggle is a prompt that gets approved
unread.

**Turn scope.** The preview is also emitted at preflight, next to the `ToolCall` events
that already are, as `AgentEvent::ToolPreview`. With the diff attached only to
permission prompts, a batch of five authorized writes gives the operator five
separable decisions and no view of the aggregate — they approve each line without ever
seeing the total. Emitting up front makes every mutating card carry its diff before
anything runs, which is what makes a **batch-level** decision possible at all.

`ToolPreview` is classified `EventClass::Ephemera` by the exhaustive `class()` audit,
not by convention: it describes an *intention* ("this call would write these bytes"),
so a resumed transcript must never replay it. That classification is the mechanism —
a new variant cannot be added without choosing its class, and choosing `Dialogue` or
`TurnOutcome` would persist a diff nothing applied.

The preview rides into the step's existing `structured` field (the same one a completed
diff renders from), so a preview card and a result card are drawn by one renderer and
cannot disagree about what the change looks like. The terminal `ToolResult` overwrites
it with what actually happened.

### 10. Capture is gated on residencency: the store is not a secret store (`INV-EFFECT-07`)

The journal captures pre-images so `/undo` can restore them, which is only legitimate
if the captured bytes are **allowed to persist**. A `.env` holding a live API key, an
`id_rsa`, a `.git-credentials` — copying any of these into a content-addressed store
creates a second, long-lived, unencrypted copy of a credential the user never asked to
have copied, in a place with no expiry of its own. ADR-0032's `[INV-SEC-01]` already
forbids exactly this class of leakage for nuo's own state; this is the same discipline
applied to project content.

[`nuo_tool::residency`] classifies a path as `Eligible` or `Withheld`, and `prepare`
consults it **before** reading bytes. A withheld path is:

- never written to the store (neither pre- nor post-image),
- still journaled, as a `file_bytes` row carrying the reason and no pre-image, so
  `/diff` shows the change and `/undo` explains its refusal rather than omitting the
  call or failing on a missing blob,
- excluded from the undo candidate set by the existing
  `irreversible_reason IS NULL` predicate — no new filter was needed because the
  exclusion was already expressed correctly.

**Classification is by path, not by content.** Scanning bytes for key-shaped strings
was rejected: reversibility would then depend on a file's contents at a particular
moment, which is unpredictable exactly where a safety property must not be — a
three-character edit elsewhere could silently change what is undoable, with no way for
the user to discover why. A path is stable and explainable. The asymmetry settles it:
over-classification costs an undo the user can live without; under-classification
copies a credential into durable storage. Content scanning still happens where it
belongs — `SecretScrubMiddleware` over tool *output*, so a secret echoed by a command
is redacted before it reaches the model. That channel is text in flight; this one is
bytes at rest.

Classification is deliberately **total and free of configuration**: the same path
yields the same answer everywhere, so the harness, the journal, and any future
inspection surface agree without a shared mutable registry. An operator who must edit
a `.env.example` still can — templates (`.env.example`, `.sample`, `.template`,
`.dist`, `.tpl`) are eligible, because the one file in the family a user legitimately
rewrites must not be the one file that cannot be undone.

### 11. Shell effects remain irreversible, and that is final (`Opaque`)

The obvious "phase 4" — make `execute_command` reversible via OS-level snapshots — was
implemented far enough to evaluate and then **rejected as an overclaim**, not deferred
for effort.

The mechanism is feasible in principle but not on the current runtime: bubblewrap is
available and probes clean, yet unprivileged `mount -t overlay` inside its namespace
fails here (no `CAP_SYS_ADMIN`, no AppArmor userns exemption), so an overlayfs-backed
copy-on-write workspace is unavailable while a bare userns mount is not.

The decisive objection is not feasibility but truthfulness. A filesystem snapshot
restores **files**. A shell command can also publish a package, push a commit, call an
API, or mutate a database. Presenting a snapshot as "undo" would restore the tree while
the world stayed changed — a *worse* failure than refusing, because it is a plausible
lie the user acts on. Correctness would require a sandbox with network denial that still
permits the development workflow, which is a capability boundary, not a snapshot, and
one that belongs to `[INV-ARCH-TIER-01]` scope rather than here.

So `execute_command` is `Opaque` permanently under this ADR: recorded as a receipt,
never offered for reversal. `EffectClass::irreversibility_reason` carries the sentence
the user reads, and `/undo` prints it. If a future sandbox makes *whole-world* effects
undoable, that is a new ADR amending this one — the classification, the receipt, and
the refusal already have the right shape to accommodate it without redesign.

### 12. Documentation follows mechanics`docs/reference/commands.md` and the `/undo` summary in `startup.rs` are updated to
describe what the commands now do, including the refusal. The `Tier1Mutation`
comment in `nuo-tool/src/hazard.rs` is now true.

## Consequences

### Positive

- `/diff` and `/undo` are real, and resume-safe (the journal is durable, not
  in-memory).
- Reversal never destroys work it did not produce; the refusal is explicit.
- The scheduler now serializes concurrent writes to one path, closing a real
  concurrency hole that predated this work.
- Shell and network effects are recorded and honestly classified, so the user sees
  the whole surface rather than the comfortable half.
- The tool contract gains a property that is compile-time exhaustible and
  testable in isolation.

### Negative

- Pre-images consume CAS storage proportional to changed bytes, bounded by expiry
  (`DEFAULT_PREIMAGE_RETENTION_DAYS`) rather than left to grow. Expired rows keep
  their observation and stop contributing to the live set, so `/undo` refuses them
  with a real reason instead of failing on a missing blob.
- A capture failure vetoes the tool call. This is the intended trade (no silent
  irreversibility) but it means a journal outage degrades write tools rather than
  silently unwarranting them.
- `execute_command` remains permanently irreversible on ordinary filesystems. The
  only honest route to full coverage is OS-level snapshotting (bubblewrap plus
  overlayfs/btrfs), which `nuo-host::workspace_sandbox` already probes for; it is
  out of scope here and would not change the design, only widen `FileBytes`.

### Neutral

- Secrets are withheld, not scrubbed (§10). A credential-bearing path is never
  captured; its row records the change and the refusal reason. This closes a claim an
  earlier revision of this ADR made without a mechanism behind it — the exact defect
  class the ADR exists to eliminate.
- Shell effects stay irreversible (§11). `execute_command` is permanently `Opaque`:
  a filesystem snapshot cannot undo a publish, a push, or an API call, and presenting
  one as "undo" would be a plausible lie the user acts on. OS-level copy-on-write was
  evaluated and rejected on this runtime (unprivileged overlayfs mount is refused
  inside bwrap here), but the rejection is on truthfulness, not feasibility.

## Verification

- `nuo-tool`: `EffectClass` ordering, join, token round-trip, and reconciliation
  (`unlocatable_mutation_fails_closed_to_opaque`,
  `reconciliation_never_demotes_strong_classes`).
- `nuo-persistence`: all four invariants asserted against the live SQLite triggers,
  plus write-ahead ordering, undo-set filtering, and the unknown-op boundary
  (`observations_are_write_once`, `completion_is_one_shot_and_the_post_image_cannot_be_rewritten`,
  `reversal_is_one_shot_and_cannot_be_cleared`,
  `unconfirmed_effect_cannot_be_reversed`, `journal_rows_cannot_be_deleted`).
- `nuo-harness`: capture semantics (`edit_captures_the_pre_image_from_disk`,
  `unreadable_pre_image_is_a_capture_failure`), bracket ordering
  (`begin_appends_the_pre_image_before_the_mutation`,
  `finish_completes_even_when_the_call_failed_partway`, `reads_are_not_journaled`),
  and the full refusal matrix (`refuses_when_the_file_changed_since_this_thread_wrote_it`,
  `refuses_to_remove_a_created_file_that_was_later_edited`,
  `refuses_to_resurrect_a_file_deleted_after_the_edit`,
  `multiple_writes_to_one_path_unwind_newest_first`).
- `nuo-tool-fs`: `edit_preview_matches_the_applied_edit` (the preview equals what is
  written), `edit_preview_is_absent_when_the_edit_would_be_rejected`,
  `rewriting_identical_content_previews_as_unchanged`.
- Retention and liveness (`nuo-persistence`): `journal_preimages_are_live_for_the_blob_collector`,
  `shared_content_is_live_for_both_owners`, `expiry_reclaims_bytes_but_keeps_the_record`,
  `expired_rows_leave_the_undo_candidate_set`, `expiry_is_one_way`.
- Capture residency (`nuo-tool`, `nuo-harness`): `credential_files_are_never_written_to_the_store`,
  `key_material_is_withheld_in_every_conventional_location`,
  `a_credential_file_does_not_taint_its_neighbours`,
  `withheld_paths_are_journaled_with_a_reason_and_no_preimage`.
- Turn-scoped preview ordering (`nuo-harness`):
  `previews_are_emitted_up_front_for_every_mutating_call` (every preview precedes the
  first result) and `read_only_calls_are_not_previewed`.

## Related

- [ADR-0008](0008-single-tool-contract.md) — the single tool contract this extends.
- [ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md) — the dispatch pipeline this brackets.
- [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) — why effects are not transcript columns.
- [ADR-0120](0120-thread-lineage-graph-persisted-fork-provenance-and-browser-grade-history-topology.md) — the thread/round coordinates the journal reuses.
- [ADR-0032](0032-relocation-of-credentials-to-state-store-and-dotfiles-secret-leaks-prevention.md) — the secret-residency rule the capture path must respect.
- [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md) — the precedent for deleting a promise the system could not keep.
