---
id: ADR-0149
title: "Command Error Routing: Transcript Purity, Recoverable Draft Restoration, and Top-Right Notice Toast"
status: accepted
date: 2026-12-20
scope: protocol/nuo-protocol, server/nuo-server, presentation/nuo-view, presentation/nuo-ui-primitives, host/nuo, tui/composer, tui/event-loop
superseded_by: null
negative_knowledge: true
---

# 0149. Command Error Routing: Transcript Purity, Recoverable Draft Restoration, and Top-Right Notice Toast

- Status: Accepted
- Date: 2026-12-20
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Substrate, Interface Architecture, Protocol, Server Core
- Amends:
  - [ADR-0116](0116-content-residency-and-surface-layer-topology.md) — reaffirms 7-tier z-model; command errors route to L4 toast and L2 notice log, never L0 transcript
  - [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) — extends admission purity to reject command execution faults and modal dispatch artefacts
  - [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md) — decoupled draft restoration from transcript settling; rejection is now completely silent on L0 while restoring draft in composer
  - [ADR-0140](0140-command-effect-classes-readings-are-receipts-not-records.md) — classifies unknown command outcomes as `Reading` (non-durable, zero mutation) across the wire

---

## Context and Problem Statement

Three interaction and architectural defects compromised the purity of the thread transcript (L0) and the ergonomics of slash command dispatch:

1. **Operational Scars in Conversation History**: When a user mistyped a command (e.g. `/modles` instead of `/models`), or entered invalid arguments, the dispatch path staged an optimistic pending command row in the transcript. Upon server response (`UnknownCommand` or `Error`), the frontend settled this row into a permanent, highlighted error card embedded directly in the message flow. A typo is neither dialogue nor a durable state mutation; leaving an error block in the transcript permanently defaced the chat log and polluted the narrative continuity of the thread.
2. **Modal Invocations Leaking into Transcript**: Typing a modal-opening command (such as `/models`, `/tools`, `/mcp`, `/skills`, or `/help`) into the composer was designed to open an interactive L3 dialog. However, an opportunistic loop stage staged and immediately cancelled a synthetic command row in the transcript (`TranscriptMessage::pending_command` followed by `cancel_pending_command`), permanently depositing a dead `⌘ /cmd [cancelled]` row on screen every time a dialog was opened via slash command.
3. **Draft Recovery Tied to Transcript Settling**: ADR-0119 introduced *Recoverable Rejection* (`[INV-CMD-03]`) so typos wouldn't force a full retype. However, draft restoration was coupled solely to `TranscriptEdit::SettleCommandResult`. As a result, cleaning up the transcript by withdrawing the row risked discarding the user's input, forcing a false choice between a clean transcript and an ergonomic draft restore.

## Decision Drivers

- **Transcript Admission Purity (`[INV-TRANSCRIPT-01]`, `[INV-TRANSCRIPT-02]`)**: L0 is strictly reserved for human-model dialogue facts and successful causal state mutations (acts). Operational failures, syntax errors, and client navigation must never occupy space in the dialogue time axis.
- **Single-Home Content Residency (`[INV-LAYER-01]`, ADR-0116)**:
  - Dialogue + turn anchors &rarr; `L0` (Transcript).
  - Receipts and diagnostic history &rarr; `L2` (Surface Log / Notice Center).
  - Spatial config surfaces &rarr; `L3` (Dialog).
  - Transient feedback &rarr; `L4` (Toast Bubble).
- **Zero Friction on Input Mistakes**: A typo is a transient physical accident. It must not cost a retype, nor should it require reading back through permanent scars in the thread. Feedback must be immediate, visible, and localized without disrupting context.

## Architectural Decisions

### 1. Command Errors Never Settle in Transcript (`[INV-CMD-09]`)

A command result that evaluates to `CommandResult::UnknownCommand` or `CommandResult::Error` **MUST NEVER** be settled or retained in the L0 transcript:

- Any optimistically staged pending command row is **withdrawn** from `messages` (`TranscriptEdit::WithdrawStagedCommand`), clearing layout height caches and restoring clean dialogue geometry.
- No historical record or fallback message is inserted into L0. Live transcript and resumed transcript remain identical (`[INV-TRANSCRIPT-04]`).

### 2. Modal Commands Emit Zero Transcript Rows (`[INV-CMD-10]`)

Modal-opening slash commands (`/models`, `/tools`, `/permissions`, `/mcp`, `/skills`, `/stats`, `/trace`, `/threads`, `/help`, etc.) intercept locally to display an L3 Dialog. They consume the composer text and record it into the composer input history for Arrow-Up/Down recall, but emit **zero** transcript messages. The synthetic `cancel_pending_command` push is completely abolished.

### 3. Command Failures Route to Top-Right Notice Toast (`[INV-CMD-11]`)

When a slash command is unrecognized or fails execution:

- The shell dispatches `AppMutation::NoticeDisclosure` carrying a `ClientNotice::CommandReceipt` with `failed: true`.
- When `failed: true`, `CommandReceipt::render()` extracts the concise error headline from the body into `title`, rather than leaving the title as a bare invocation string. This ensures the toast immediately informs the user of the exact failure cause.
- The presentation substrate displays an elevated `ToastBubble` positioned in the top-right corner (`x = width - toast_width - 2`, `y = 1`) with `NoticeSeverity::Error`.
- The toast displays the error explanation or closest fuzzy match suggestion (e.g. `Unknown command: /modles · Did you mean /models?`), and the navigational pointer (`Alt-o · all notices`).
- The receipt is permanently logged in the L2 Surface Log (Notice Center, accessible via `Alt-o`), ensuring full auditability without polluting the conversation canvas.

### 4. Recoverable Rejection Integrated with Withdrawal (`[INV-CMD-12]`)

`TranscriptEdit::WithdrawStagedCommand` gains a boolean parameter `restore_draft: bool`. When true:

- If the composer (`app.input`) is empty, the invocation string is restored to `app.input`.
- The cursor is positioned at the end (`app.set_cursor_end()`), primed for immediate `Backspace` typo correction.
- Autocomplete popup open state is suppressed until the next keystroke, preventing intrusive layout jumps over the restored text.

### 5. Optimistic Staging Suppression for Unrecognized Commands (`[INV-CMD-13]`)

`handle_send_slash` stages an optimistic pending row (`stages_row`) **only** when the command is recognized in `command_catalog` and declared as an Act (`is_known && Mutation`). Unrecognized names (which are almost universally typos) do not stage an optimistic row in L0. This eliminates the visual jitter of a pending row flashing onto the transcript and disappearing a few milliseconds later.

### 6. Wire Outcome for Unknown Commands is `Reading`

`nuo-server`'s command effect resolver classifies `CommandResult::UnknownCommand` as `CommandEffect::Reading`. Because an unrecognized command executed zero business logic and mutated zero session state, emitting `CommandEffect::Mutation` was a category error. As a `Reading`, it is non-durable in persistence (`session.commands` remains empty) and announces its ephemeral nature across the protocol wire.

---

## Invariant Census

| Invariant | Scope | Rule | Verification |
| :--- | :--- | :--- | :--- |
| `[INV-CMD-09]` | `shell::event_loop` | A command error or unknown command is withdrawn from the transcript and never settled. | `withdrawal_with_restore_draft_returns_invocation_to_empty_composer` |
| `[INV-CMD-10]` | `shell::event_loop` | Modal-opening slash commands emit zero transcript rows. | `text_modal_commands_resolve_and_consume_composer` |
| `[INV-CMD-11]` | `shell::notices` | Command failure disclosures route to top-right notice toast with `NoticeSeverity::Error` and log to L2. | `unknown_slash_command_never_stages_and_surfaces_error_headline` |
| `[INV-CMD-12]` | `shell::event_loop` | Withdrawing a rejected slash command restores the draft to an empty composer with caret at end. | `rejected_command_returns_the_draft_to_the_composer` |
| `[INV-CMD-13]` | `shell::event_loop` | Unrecognized slash commands never stage an optimistic pending row in L0 transcript. | `unknown_slash_command_never_stages_and_surfaces_error_headline` |

---

## Consequences

- **Positive**: The chat transcript remains pristine dialogue and verified mutations. Users encounter zero visual clutter from command typos.
- **Positive**: Typo recovery is completely frictionless: Backspace corrects the error immediately while the top-right toast provides the exact suggestion.
- **Positive**: Opening configuration dialogs like `/models` leaves zero trash rows behind in the thread.
- **Negative / Neutral**: Because notifications appear in the top-right corner rather than inline, users viewing ultra-wide terminals rely on the restored text in the composer for immediate visual confirmation of the failure.
