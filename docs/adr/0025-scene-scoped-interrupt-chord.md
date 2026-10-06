---
id: ADR-0025
title: "Scene-Scoped Interrupt Chord: A Scene's Esc Esc Stops Only Its Own View"
status: accepted
date: 2026-10-07
scope: tui/nuo-tui, interaction/input, interaction/affordance, protocol/wire, server/nuo-server, harness/subagent
superseded_by: null
negative_knowledge: true
---

# 0025. Scene-Scoped Interrupt Chord: A Scene's Esc Esc Stops Only Its Own View

- Status: Accepted
- Date: 2026-10-07
- Deciders: Nuo Architecture Working Group
- Consulted: TUI, Interaction, Wire-Protocol, and Runtime maintainers
- Informed: System Architects
- Amends: scene-scoped interrupt routing, aside interrupt generalization, and Esc boundary invariants

---

## Context and Problem Statement

`Esc Esc` is the universal "stop the running round" chord. It is a *windowed*
gesture: the first press arms a 2s confirmation (the "Esc again interrupts"
toast), the second fires (`App::ESC_ARM_WINDOW`).

Under the Stage-Scene-Overlay router the TUI stands in exactly one
**scene** at a time — Conversation, Dashboard, Settings, TaskInspection (a
zoomed subagent), or Aside (a `/btw` view) — with overlays floating above it.
The Aside scene had long owned a scene-scoped interrupt (`InterruptSide`,
[ADR-0017](0017-concurrent-aside-execution-substrate-and-isolated-round-routing.md)) that stops the *aside's* round and leaves the primary running. The
TaskInspection scene, however, inherited the Conversation's `Esc` arm verbatim:
`resolve_subagent_key` delegated straight to the shared chat core, whose `Esc`
resolves to the scene-agnostic `InputAction::Interrupt`, and `Interrupt` was
hard-wired to the **primary** session.

Two coupled defects followed:

1. **Scope leak.** `Esc Esc` inside a zoomed subagent issued
   `AgentRequest::Interrupt` for the *primary* session. On the server that
   cancels the primary round's token, which the harness executor propagates down
   into **every** in-flight child (`Tool::request_cancel`). So a chord pressed
   to stop one subagent stopped the entire outer turn — it penetrated the scene
   boundary the user was standing in.

2. **Dishonest liveness.** The scene's run state was read from the **primary's**
   chrome (`app.viewed_chrome().responding` special-cases only the aside;
   `running_sessions` holds sessions, and a subagent is not a session — it runs
   *inside* the parent's round). The zoom therefore advertised an interrupt
   whenever the *parent* ran, even for a child that had already finished.

The root cause is conceptual: the chord/`SceneKeys`/`InputAction` model never
defined **what an interrupt targets per scene**. It modeled "interrupt" as one
global verb that happened to mean "primary", plus one ad-hoc special case for
the aside. That is a scope-and-ownership gap, not a bug in any single arm.

## Decision Drivers

- **Scene scope is the router's contract.** A chord resolved inside a
  scene must act on that scene's view, never on a sibling or an ancestor surface.
  `[INV-TUI-CLEAN-02]` already forbids Esc from *leaving* a scene; the same
  boundary must forbid it from *reaching across* one.
- **Honest advertisement ([ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md)).** A rendered chord ("Esc Esc
  interrupt") must be live: it may appear only when the thing it stops is
  actually running.
- **Nested work is first-class.** Subagents are the primary nested-work surface;
  stopping one must be possible without discarding the enclosing round's other
  work (siblings, the parent's own turns).
- **One contract, three views.** Conversation, Aside, and Subagent interrupt
  chords must share one confirmation policy and one priority ladder; only the
  *target* differs.

## Considered Options

- **Option 1 (chosen):** Make the interrupt chord explicitly scene-scoped. The
  resolver passes the scene's own liveness **and** its interrupt target into one
  shared ladder; add a subagent-scoped wire verb so the TaskInspection scene
  stops only the viewed child.
- **Option 2:** Make `Esc Esc` inert in the Subagent scene; require the user to
  return to the Conversation to interrupt.
- **Option 3:** Keep interrupting the primary from every scene; only fix the
  advertising so the chord is documented as primary-scoped.

## Decision Outcome

Chosen option: **Option 1**, because it is the only option that makes the
chord's behavior follow the router's scene boundary, keeps the affordance
available where the user is standing, and gives honest liveness — at the cost of
one new wire verb and a scene-local liveness read.

Concretely:

- `InputAction::InterruptSubagent` is the TaskInspection scene's Esc target;
  `Interrupt` (primary) and `InterruptSide` (aside) are its siblings.
- The shared Esc priority ladder
  (`session::resolve_esc(keys, responding, interrupt)`) is parameterized on the
  scene's own `responding` and `interrupt` target. Conversation passes
  `keys.is_responding` + `Interrupt`; Aside passes `is_responding` +
  `InterruptSide`; Subagent passes `SceneKeys::focused_subagent_running` +
  `InterruptSubagent`. The priority order (dismiss completion → clear focus →
  interrupt) is identical in all three, so it cannot drift.
- `AgentRequest::InterruptSubagent { call_id }` carries the parent tool-call id
  of the viewed child (the `SubagentRegistry` key). The driver's 
  handler cancels that one child's token — reachable because the tool registers
  its live `CancellationToken` in the shared registry — and queues a boundary
  `AgentOp::Interrupt` as the idle fallback. The parent round and every sibling
  child are untouched.
- The double-press dispatch is a scene-target enum
  (`InterruptTarget::{Primary, Aside, Subagent}`); the session-level flips
  (`running_sessions`, `clear_responding`, prompt-cancel row) run only for the
  primary/aside paths, because a subagent has no session-level round of its own.
- `App::focused_subagent_running()` derives the viewed child's liveness from its
  transcript step's status (a subagent is not in `running_sessions`).
  `App::tick_esc_arm()` laps the armed window against the scene's target, so a
  child that settles mid-window disarms the toast even while the parent runs.

### Invariants & Behavioral Boundaries

- `[INV-TUI-SCOPE-01]` **No cross-scene interrupt reach.** The interrupt chord
  resolved in scene S acts only on S's own view: the Conversation's targets the
  primary, the Aside's the viewed aside, the Subagent's the viewed child. No
  scene's Esc may emit a target belonging to another scene.
- `[INV-TUI-SCOPE-02]` **Target-scoped liveness.** A scene that advertises (or
  arms) an interrupt must read the *target's* running state — never a
  global/primary-only flag. A finished viewed child under a still-running
  primary advertises no interrupt.
- `[INV-TUI-SCOPE-03]` **One ladder per view.** The three interrupt scenes share
  `resolve_esc`'s priority ladder; they differ only in the `responding`/`interrupt`
  arguments. A change to dismissal priority lands in exactly one place.
- `[INV-WIRE-SCOPE-01]` **Addressed cancellation is single-target.** A
  `InterruptSubagent { call_id }` cancels exactly the child registered under
  `call_id`; an unknown or finished id is a graceful no-op. It must never fall
  back to the primary interrupt.

### Positive Consequences

- `Esc Esc` in a zoomed subagent stops that subagent and nothing else; sibling
  subagents and the parent's round continue.
- The zoom's interrupt affordance is honest: shown only while the viewed child
  runs, and retracted the instant it settles.
- The three interrupt scenes now share one confirmation policy and one priority
  ladder; the aside's behavior is preserved but expressed as an instance of the
  general rule rather than bespoke code.

### Negative Consequences & Trade-offs

- One more wire verb and a scene-local liveness accessor to maintain.
- The subagent interrupt is cooperative at the turn boundary when the child is
  idle/queued (the `AgentOp::Interrupt` fallback); it is prompt when the child is
  mid-request (the shared cancellation token races the model call). This mirrors
  the primary's own two-path interrupt.
- A subagent interrupt does not flip any session-level chrome (correctly — there
  is none to flip); the nested view repaints from the child's own
  `SubagentEvent`s. A future nesting depth > 1 inherits the same rule per level.

## Rejected Alternatives & Negative Knowledge

### Option 2 (Rejected): Make Esc Esc inert in the Subagent scene
- Why considered: Smallest change — one resolver arm returns `None`; no wire
  verb, no harness work.
- Why rejected: Removes a primary affordance exactly where the user is looking
  at the running work. Stopping a runaway subagent would require leaving the
  zoom first, which is both an extra gesture and a loss of the contextual view.
  It also leaves the *read* defect (dishonest liveness) half-fixed and leaves the
  chord's scope undefined rather than defined.

### Option 3 (Rejected): Keep the primary as the universal interrupt target
- Why considered: Zero behavioral change; only reword the hint so the chord is
  documented as primary-scoped.
- Why rejected: Codifies the scope leak as intended. A single `Esc Esc` would
  still destroy the enclosing round's unrelated work from inside a nested view —
  a surprising, unbounded blast radius for a chord the user aimed at one child.
  It also cements the asymmetry with the aside's already-scoped interrupt,
  leaving two contradictory mental models for the same physical gesture.

### Reusing `AgentRequest::Interrupt` with a scene field (Rejected)
- Why considered: Avoids a new enum variant; a `target: Option<String>` on
  `Interrupt` could carry a call id.
- Why rejected: Overloads one wire verb with two lifetimes — the primary round's
  cancellation (session-scoped, flips the whole session idle) and a child's
  (target-scoped, touches no session state). The server handler would have to
  branch into two dispatch shapes behind one name, which is exactly the scope
  ambiguity this ADR exists to remove. A dedicated variant keeps the server's
  single-target guarantee (`[INV-WIRE-SCOPE-01]`) structural.

### Driving the subagent interrupt only through `AgentOp::Interrupt` (Rejected)
- Why considered: The `SubagentHandle` inbox already exists; no new
  cancellation-token plumbing needed in the registry.
- Why rejected: `AgentOp::Interrupt` is drained only at the next ReAct-turn
  boundary, so a child parked inside a long model request would not stop until
  the *next turn* — unlike the primary, whose `CancellationToken` races the
  in-flight request. Registering the child's token in the shared
  `SubagentRegistry` gives the subagent interrupt prompt parity with the
  primary. The inbox op is retained as the idle/queued fallback, not the sole
  mechanism.

## Links

- Related ADRs: [ADR-0017](0017-concurrent-aside-execution-substrate-and-isolated-round-routing.md) (aside execution substrate),
  [ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md) / [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md) (head-band legend and scene namespace).
- Related PRs or issues: double-`Esc` leaked the primary interrupt through the
  Subagent scene; scene-local liveness read from the primary chrome.
