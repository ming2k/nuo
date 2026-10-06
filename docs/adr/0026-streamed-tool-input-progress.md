---
id: ADR-0026
title: "Streamed Tool-Input Progress: Announce the Tool Call Before Its Arguments Finish"
status: accepted
date: 2026-10-08
scope: tui/nuo-tui, presentation/disclosure, protocol/wire, harness/dispatch, agent/rounds, interaction/affordance
superseded_by: null
negative_knowledge: true
---

# 0026. Streamed Tool-Input Progress: Announce the Tool Call Before Its Arguments Finish

- Status: Accepted
- Date: 2026-10-08
- Deciders: Nuo Architecture Working Group
- Consulted: TUI, Interaction, Wire-Protocol, and Runtime maintainers
- Informed: System Architects
- Complements: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md), [ADR-0008](0008-single-tool-contract.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md)
- Amends: ADR-0008 §"single breathing anchor" (clarifies what a per-step progress signal may *not* be), ADR-0014 §tool-phase vocabulary (carries `tool-start`/`tool-delta` through to the frontend)

---

## Context and Problem Statement

The TUI already streams a great deal live: assistant text and reasoning arrive
as per-delta appends (`nuo-tui/src/lib.rs:961-1026`), shell stdout/stderr arrive
as `ToolStream` frames, the activity bar carries a spinner and an elapsed timer,
and the retained canvas repaints on a ~100 ms frame budget (ADR-0003,
`nuo-tui/src/event_loop/mod.rs:423-425`). None of that is "everything at the
end".

There is, however, **one leg of the round that produces no observable signal
until it is completely finished: the model's tool-call arguments.**

Providers stream a tool call's arguments incrementally — Anthropic emits
`input_json_delta` frames, OpenAI emits `choices[].delta.tool_calls[].function.arguments`
fragments. In this codebase those fragments are **consumed and buffered, never
forwarded**:

- `nuo-harness/src/agent/rounds.rs:715-736` appends every
  `ProviderStreamEvent::ToolCallDelta` into `calls[index].arguments` and emits
  no frontend event.
- Only after the stream ends does
  `nuo-harness/src/dispatch/pipeline.rs:221-234` emit one
  `AgentEvent::ToolCall { id, name, arguments }` per call — the whole JSON object
  at once.
- The wire vocabulary has **no arguments-delta variant at all**: `RoundEvent`
  carries `ToolCall`/`ToolResult`/`ToolStream` but no `ToolInputDelta`
  (`nuo-wire/src/events.rs:1149-1188`).

The user-visible consequence is worst for exactly the tools whose arguments are
the *point* — `edit_text`, `write_file` — where the argument payload can be an
entire file and stream for many seconds. During that window:

1. **The step does not exist yet.** The transcript entry is created on
   `ToolCall` (`nuo-tui/src/lib.rs:1116-1123`), so there is nothing to see — not
   even an empty expanded step.
2. **The bar is mislabelled.** `StreamStart` sets `Phase::Answering`
   (`nuo-tui/src/lib.rs:948-958`) and tool-call argument deltas never move it, so
   the bar reads `answering` while the model is in fact *planning a tool call* —
   the label is untrue, and its duration is a real elapsed counter that keeps
   climbing, which is what reads as "frozen".
3. **Non-shell results appear only at completion.** The edit diff, search
   results, and subagent summaries exist only once `ToolResult` lands
   (`nuo-tui/src/disclosure/renderers/tools.rs:185-203`); the step is created
   expanded (`nuo-tui/src/tools/edit_text.rs:38-40`) but its body is empty until
   the call has already run.

The design intent behind the buffering is *correct and must be preserved*: the
stream-finalization guard (`nuo-harness/src/agent/rounds.rs:755-797`) treats a
stream that ends mid-tool-call, or with truncated arguments, as a retryable
transport failure rather than committing a partial response. A tool must never
execute on half a JSON object.

The defect is not that partial arguments are withheld from **execution** — that
is a safety property. The defect is that they are withheld from **observation**:
progress and executability were conflated into one signal, so "not yet
executable" silently became "not yet visible".

## Decision Drivers

- **Progress-honesty ([ADR-0020](0020-interactive-component-registry-single-source-of-truth.md) §"advertised keys must be live").** Any
  moment of a round must let the user answer: whose turn is it (model / tool /
  human), what is it doing, for how long, and is it alive. The tool-argument
  phase currently answers none of the first three.
- **Preserve the execution gate.** No change may allow a partial argument object
  to reach a tool. The buffer-then-parse discipline is a safety invariant, not
  an implementation detail.
- **No new breathing anchors (ADR-0008).** Per-step liveness rides on hue alone;
  the activity bar is the single animated glyph. Decorative per-step motion is
  prohibited, so progress signalling must be structural (counts), not animated.
- **Respect the frame budget (ADR-0003).** High-frequency deltas are coalesced
  onto the heartbeat; the new signal is high-frequency and must join that class
  rather than force a per-delta wake.
- **Carry the existing vocabulary through, don't invent one (ADR-0014).** The
  upstream streaming vocabulary already defines `tool-start` / `tool-delta` /
  `tool-end`; the decision is to *forward* those phases to the frontend, not to
  design a parallel notion of tool progress.

## Considered Options

- **Option 1 (chosen):** Introduce a two-tier frontend-visible tool-input
  signal — an early `ToolCallStarted` announcement (name known, arguments still
  streaming) plus a lightweight, count-only `ToolInputProgress` tick — while the
  full argument object remains withheld from execution until it is complete and
  parseable.
- **Option 2:** Do nothing on the wire; only fix the *label* so the bar stops
  reading `answering` (a TUI-local phase correction).
- **Option 3:** Stream the raw partial JSON arguments into the transcript body
  as they arrive (a live "arguments so far" view).
- **Option 4:** Commit and execute the arguments as soon as the bytes so far
  parse as valid JSON.

## Decision Outcome

Chosen option: **Option 1.** It is the only option that removes the invisibility
of the tool-argument phase while keeping the execution gate exactly as strict as
it is today, and it does so by forwarding vocabulary that already exists upstream
rather than inventing a new concept.

Concretely:

- **`RoundEvent::ToolCallStarted { index, id, name }`** is emitted by the harness
  as soon as a tool-call slot has a **name** — i.e. at the first fragment that
  carries one (`content_block_start` for Anthropic; the first `tool_calls[]`
  delta for OpenAI). It carries the slot index and, once known, the provider id.
  The TUI creates a `Running` transcript step and, in the same act, writes the
  tool phase so the bar leaves `Answering` and reads the tool verb (e.g.
  `making edits`). The step's summary is rendered from the `name` and the
  incremental string fields as they become extractable (see below); its body is
  legitimately empty until output arrives.
- **`RoundEvent::ToolInputProgress { index, id, bytes }`** is a **count-only**
  tick emitted at a bounded cadence while arguments stream. It carries the
  number of argument bytes accumulated so far — **never the bytes themselves**.
  The step renders this as a static, non-animated counter/timer clause in its
  summary (e.g. `receiving input · 3.2 KB`).
- **Incremental field extraction is permitted, not required.** A presenter may
  parse *complete, closed* string fields out of the partial JSON (e.g. once
  `"path": "…"` is terminated) to refine a summary or grow a diff preview, but
  the executable `arguments` used for dispatch is still the whole object parsed
  after the stream ends. Extraction is an optimization layered on top of the
  count-only signal, never a substitute for the execution gate.
- **`ToolCall` retains its current meaning for providers that deliver arguments
  in one shot.** A provider that never streams argument fragments emits
  `ToolCall` directly; the TUI treats a `ToolCall` for a slot not previously
  announced as a `ToolCallStarted` immediately followed by the completed call, so
  both lanes converge on one frontend state machine.

### Invariants & Behavioral Boundaries

- `[INV-STREAM-TOOL-01]` **Executability gate.** A tool executes only against a
  complete, `serde_json`-parseable argument object. No partial argument stream,
  partial-field extraction, or progress tick may be dispatched, persisted, or
  otherwise treated as the call's arguments. The finalization guard in
  `nuo-harness/src/agent/rounds.rs:755-797` is the enforcement point and must not
  be weakened.
- `[INV-STREAM-TOOL-02]` **Early announcement.** A tool call becomes visible to
  the frontend — a running step plus its tool phase — as soon as its name is
  known, strictly before its arguments finish streaming. Progress must never be
  discoverable only at completion.
- `[INV-STREAM-TOOL-03]` **Progress is count-only.** The streaming progress
  signal carries a byte count, never the raw argument bytes; the transcript must
  never render a partial-JSON body for an in-flight call.
- `[INV-STREAM-TOOL-04]` **Coalescing class.** `ToolCallStarted`,
  `ToolInputProgress`, and tool `ToolStream` frames are coalescible stream
  updates: they ride the existing frame budget and must not force a per-delta
  wake of the event loop (ADR-0003). Stream-begin/end and the tool *lifecycle*
  (`ToolCall`, `ToolResult`, `ToolCancelled`) remain coalescing-exempt.
- `[INV-STREAM-TOOL-05]` **One breathing anchor.** A per-step progress indicator
  uses no spinner glyph, pulse, or other animation; it is a static text clause.
  The activity bar remains the sole animated anchor (ADR-0008).
- `[INV-STREAM-TOOL-06]` **Phase honesty.** While a tool call's arguments stream,
  the bar's phase must reflect the tool verb, never `Answering`. The phase
  reaches its tool value no later than the `ToolCallStarted` announcement.

### Positive Consequences

- The tool-argument window is observable: a running `Edit <path>` step appears
  and the bar reads `making edits` while the model is still emitting the call,
  so the round is never visually indistinguishable from a stall.
- The signal is additive and lane-independent: a provider that streams fragments
  and one that delivers whole arguments both fall out of one frontend state
  machine.
- The execution gate is untouched, so the safety property the buffering exists
  to protect (no execution on truncated input) is preserved verbatim.

### Negative Consequences & Trade-offs

- One new wire pair (`ToolCallStarted`, `ToolInputProgress`) and a matching
  pair of frontend mutations to maintain; the harness stream loop must emit at a
  bounded cadence rather than per byte.
- The count-only signal is coarse: it tells the user *how much* input arrived,
  not *what* it says. That is the deliberate price of `[INV-STREAM-TOOL-03]`;
  richer readability is bought per-presenter via permitted field extraction.
- Two producers (streamed vs whole-argument) must agree on the announced-then-
  completed collapse; a frontend that announces twice would double-insert a step.
  A regression test pins the single-step outcome.

## Rejected Alternatives & Negative Knowledge

### Option 2 (Rejected): Fix only the bar's label, no wire change
- Why considered: Smallest possible change — correct the TUI-local phase so the
  tool-argument window does not read as `answering`; no new events.
- Why rejected: It fixes the *lie* without fixing the *absence*. The step still
  does not exist until completion, so the user watching the transcript still
  sees nothing appear for the duration — the perceived stall survives. It also
  cannot recover the case the user actually complained about (a large edit),
  because it carries no per-call information at all. Correcting the label is
  necessary but not sufficient, and is subsumed by `[INV-STREAM-TOOL-06]`.

### Option 3 (Rejected): Stream the raw partial JSON into the transcript body
- Why considered: Maximally transparent — the user watches the arguments take
  shape character by character.
- Why rejected: A half-written JSON object is not readable information; it is
  noise that grows, wraps, and reflows on every frame, dominating the viewport
  for a payload the user will see properly as a diff one moment later. It also
  collides with the frame budget (a body that changes every delta forces a
  re-measure) and with the reading-surface principle (ADR-0020 §Rejected 4): the
  transcript presents outcomes, not wire traffic. The count-only signal gives the
  "something is happening, and here is its size" reassurance without the noise.

### Option 4 (Rejected): Execute as soon as the bytes so far parse as JSON
- Why considered: Would let short tool calls start marginally earlier, shaving
  latency off the tail of the stream.
- Why rejected: This is a correctness hazard, not an optimization. A prefix of
  arguments can be syntactically valid JSON while being semantically incomplete
  (a truncated write, a missing `new_string`), so "parses" does not imply
  "intended". It directly attacks the guarantee `[INV-STREAM-TOOL-01]` exists to
  preserve and would re-open the truncated-stream failure mode that
  `nuo-harness/src/agent/rounds.rs:764-797` was written to prevent. The latency
  saved is a fraction of a second; the cost is executing a call the model never
  finished expressing.

### Forcing a per-delta repaint for tool input (Rejected)
- Why considered: The simplest way to make progress "feel" live is to wake and
  repaint on every fragment.
- Why rejected: It abandons the coalescing discipline that keeps a 60+ Hz token
  stream from thrashing the canvas (ADR-0003), and buys nothing — a byte counter
  is just as informative at 10 Hz as at the fragment rate. The signal joins the
  coalescible class instead (`[INV-STREAM-TOOL-04]`).

### A per-step spinner or progress glyph (Rejected)
- Why considered: A small animation on the running step reads as "alive".
- Why rejected: ADR-0008 makes the activity bar the single breathing anchor and
  carries per-step liveness on hue alone, precisely so a transcript full of
  running steps does not flash in unison. A per-step animation reintroduces that
  visual competition for a signal the count already conveys
  (`[INV-STREAM-TOOL-05]`).

## Compliance

- `nuo-harness` must emit `ToolCallStarted` before (or with) the first argument
  fragment that names the call, and may emit `ToolInputProgress` only at a
  bounded cadence — never per byte, never carrying argument bytes.
- The finalization guard (`nuo-harness/src/agent/rounds.rs:755-797`) and
  `[INV-STREAM-TOOL-01]` are the enforcement point for the execution gate; a test
  must fail if dispatch is reachable with a non-parseable argument object.
- `nuo-tui` must treat `ToolCallStarted` and a first-seen `ToolCall` as the same
  announcement (single step insertion), render `ToolInputProgress` as a static
  clause, and never render a partial-JSON body.
- A rendered per-step progress indicator must be non-animated
  (`[INV-STREAM-TOOL-05]`); a test asserting the absence of an animated per-step
  glyph is the regression net.

## Links

- Related ADRs: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md) (retained canvas / coalescing budget),
  [ADR-0008](0008-single-tool-contract.md) (single tool contract, single breathing anchor),
  [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md) (TUI presentation split),
  [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md) (upstream `tool-start`/`tool-delta`/`tool-end` vocabulary),
  [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md) (disclosure defaults and live-chord honesty).
- Related code: `nuo-harness/src/agent/rounds.rs:715-736` (buffering site),
  `nuo-harness/src/dispatch/pipeline.rs:221-234` (completion emit),
  `nuo-wire/src/events.rs:1149-1188` (`RoundEvent`),
  `nuo-tui/src/lib.rs:948-958` / `:1099-1127` (translator),
  `nuo-tui/src/phase.rs:91-103` (tool verbs),
  `nuo-tui/src/disclosure/renderers/tools.rs:44-58` (per-step hue-only rule).
