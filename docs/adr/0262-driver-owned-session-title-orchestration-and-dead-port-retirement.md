---
id: ADR-0262
title: "Driver-Owned Session Title Orchestration and Dead-Port Retirement"
status: accepted
date: 2026-11-29
scope: engine/cognition, engine/orchestration, engine/runtime, server/runtime/thread_driver, server/composition
superseded_by: null
amends:
  - "[ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) (relocates background title orchestration, cancellation, persistence, and client broadcast out of `engine` into `server`'s `thread_driver` while keeping `[INV-TITLE-01]` intact)"
negative_knowledge: true
---

# 0262. Driver-Owned Session Title Orchestration and Dead-Port Retirement

- **Status**: Accepted
- **Date**: 2026-11-29
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)
- **Related**: [ADR-0154](0154-delete-unwired-spatiotemporal-aspect-engine-and-dead-cognitive-tasks.md), [ADR-0184](0184-server-is-the-application-core.md)

---

## Context and Problem Statement

While thread title distillation itself already ran through `HarnessTaskPipeline` (`ThreadTitleTask`), the surrounding orchestration had drifted into a half-converged boundary leak across `engine` and `server`:

1. **Detached `tokio::spawn` inside `engine` with no lifecycle governance**:
   `engine::orchestration::session_title` spawned fire-and-forget tasks directly from `execute_round`, taking `Arc<persistence::ThreadSession>` without any cancellation handle. Switching sessions, manually renaming a thread, or shutting down a `ThreadDriver` could not cancel in-flight title requests, and back-to-back prompts before the first distillation settled could launch duplicate requests.
2. **Specialized callback hole on `Agent`**:
   Because `engine` could not reach the server's response channel (`resp_tx`), `Agent` carried a dedicated `control.title_established: Mutex<Option<TitleEstablishedFn>>` slot (`set_title_established`) solely so `ThreadDriver::initialize` could inject a closure to broadcast `ThreadsOverview` back to clients.
3. **Unwired dead port abstraction**:
   `engine::runtime::host` defined `TitleSink` and `NoTitles`, and `server::composition::kernel_host` implemented `SessionTitles: TitleSink`, yet zero production call sites ever constructed or invoked them — violating [ADR-0154](0154-delete-unwired-spatiotemporal-aspect-engine-and-dead-cognitive-tasks.md) (`[INV-COG-01]`).

---

## Decision Drivers

- **Single Layer of Ownership ([ADR-0184](0184-server-is-the-application-core.md))**: `engine` owns stateless cognitive execution; `server` (`thread_driver`) owns session persistence, task supervision, cancellation, and client fanout.
- **Occam's Razor over Speculative OOB Frameworks**: With a single detached background cognitive task in production (`thread_title`), introducing a generic out-of-band event bus or lifecycle scheduler would recreate the retired `AspectEngine` ([ADR-0154](0154-delete-unwired-spatiotemporal-aspect-engine-and-dead-cognitive-tasks.md)).
- **Clean Break**: Delete the callback slot (`TitleEstablishedFn` / `set_title_established`), the dead `TitleSink` / `NoTitles` / `SessionTitles` port, and `engine::orchestration::session_title` rather than retaining shims.

---

## Architectural Decisions

### 1. Pure Cognition in `engine`, Supervised Orchestration in `server` (`[INV-TITLE-02]`)

- **`engine`** exposes only pure, stateless title distillation:
  - `HarnessTaskPipeline::generate_title_from_prompt(&self, prompt: &str) -> Option<String>` enforces the prompt excerpt budget (`TITLE_PROMPT_BUDGET_CHARS = 1_000`) and skips blank inputs.
  - `Agent::generate_session_title(&self, prompt: &str) -> Option<String>` delegates directly to `self.cognitive().generate_title_from_prompt(prompt)`.
  - `engine::orchestration::execute_round` no longer triggers titling, `engine::orchestration::session_title` is deleted, `TitleEstablishedFn` / `Agent::set_title_established` are deleted, and the unwired `TitleSink` / `NoTitles` / `SessionTitles` port is deleted.
- **`server` (`runtime::thread_driver::titler::SessionTitler`)** owns the full background lifecycle inside `DriverCtx`:
  - **Deduplication**: tracks at most one active `(session_id, JoinHandle<()>)` per driver; repeated prompts for the same untitled session while distillation is already in flight do not spawn duplicate model calls.
  - **Cancellation**: `SessionTitler::abort()` cancels any in-flight task on `DriverCtx::shutdown()` and on `session_changed` (`/new`, `/resume`, `/fork`), and `abort_if_session(&id)` cancels it on manual `AgentRequest::RenameThread`. `Drop` aborts defensively.
  - **Identity & Lock Re-check**: after the async provider call returns, the task re-checks both `session.id().await == expected_session_id` and `!has_title_after` before persisting (`session.set_title(Some(title), false)`), then calls `handlers_thread::overview(&session, &resp_tx)` directly — the exact same broadcast path manual rename uses.
  - **Admission-Gated Trigger Funnel**: `start_interactive_round` returns a generic `oneshot::Receiver<bool>` resolving to `true` only after `UserPromptSubmit` hooks pass and the opening prompt is durably committed (or `false` if denied). `handlers_chat::start_turn` hands that admission signal to `titler.maybe_spawn_on_admission(...)` for fresh, visible user rounds (`!input.hidden && matches!(input.driver, RoundDriver::Fresh)`), guaranteeing a hook-denied opening prompt never wastes a model request or titles an empty thread while admitted rounds still distill concurrently with streaming.

---

## Normative Invariants

- **`[INV-TITLE-02]` Driver-Owned Background Title Lifecycle**: `engine` MUST remain stateless with respect to thread titling (pure prompt-to-title distillation only, with zero `tokio::spawn`, zero `ThreadSession` title writes, and zero `title_established` observer callbacks). Background task supervision, per-session deduplication, cancellation on session switch / manual rename / driver shutdown, durable persistence, and `ThreadsOverview` broadcast MUST live in `server`'s `thread_driver`.

---

## Consequences & Trade-offs

- **Positive**:
  - Eliminates a cross-crate callback cycle (`server` -> `Agent::set_title_established` -> `engine` -> `server`), an unwired port (`TitleSink` / `SessionTitles`), and detached background task leaks.
  - Prevents duplicate concurrent title requests when multiple prompts/follow-ups arrive before the first title lands.
  - Guarantees an in-flight titler for an old session can never race or overwrite after a session switch or manual rename.
- **Negative & Incurred Costs**:
  - Direct callers of `engine::orchestration::execute_round` outside `ThreadDriver` (e.g. isolated unit tests) no longer get implicit background titling; they call `Agent::generate_session_title` explicitly when desired.

---

## Rejected Alternatives & Negative Knowledge

### 1. Wiring `TitleSink` into `KernelHost` and Keeping `spawn_session_titler` in `engine`
- **Why considered**: `TitleSink` and `SessionTitles` already existed as stubs in `engine::runtime::host` and `server::composition::kernel_host`.
- **Why rejected**: Even with `TitleSink` wired, `engine` would still own detached `tokio::spawn` lifecycle, still lack driver-scoped cancellation across session switches and shutdown, and still need a second port or callback to push `ThreadsOverview` to clients. Moving the 90-line supervisor into `ThreadDriver` deletes both abstractions instead of adding another seam.

### 2. Building a Generic Out-of-Band (OOB) Background Scheduler Framework
- **Why considered**: Generalizing detached post-turn or admission-time cognitive tasks into an event bus / scheduler.
- **Why rejected**: Violates Occam's Razor and [ADR-0154](0154-delete-unwired-spatiotemporal-aspect-engine-and-dead-cognitive-tasks.md) (`[INV-COG-01]`). Only `thread_title` is a genuinely detached background cognitive task (`StreamLoopReviewerTask` and `TrajectoryLoopReviewerTask` are synchronous inline checks). A framework built for one consumer decays into dead surface.
