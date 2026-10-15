# Interrupt Semantics

This article explains how Nuo stops a running round: what an interrupt targets, why a stopped round is recorded rather than erased, and why omission — not a synthetic message — is the model-facing signal.

---

## 1. An Interrupt Targets a Round, Scoped to Its Scene

`Esc Esc` is the universal "stop the running round" chord, and it is a *windowed* gesture: the first press arms a short confirmation window ("Esc again interrupts"), the second fires. A scene's chord stops only that scene's own view — a zoomed subagent's interrupt stops the subagent's round, not the entire outer turn it runs inside (see [ADR-0025](../adr/0025-scene-scoped-interrupt-chord.md)).

This scoping is the router's contract, not an afterthought. The chord resolves per scene, so interrupting inside a subagent cannot penetrate the scene boundary the user is standing in.

---

## 2. Why a Round Stopped Is Recorded

When a round stops before its natural terminal path, the harness writes a durable **`RoundInterrupt`** record — the *why* and *when* of the stop, at the moment it stopped. A resumed session can then show that the round stopped, and why, instead of silently losing it.

The reason is a closed classifier (`RoundInterruptReason`), each carrying a single user-facing label shared by the TUI, the Web app, and headless output:

| Reason | Label | Cause |
| :--- | :--- | :--- |
| `User` | `Esc Esc` | The user explicitly stopped the round (double-Esc, or the equivalent control-plane `Interrupt`). |
| `Superseded` | `new message` | A newer round replaced this one: the user sent a new message or a follow-up while the round was live, or switched sessions. |
| `Terminated` | `process exited` | The host process terminated with the round still in flight (server stop, signal exit, or crash). |
| `Error` | `error` | The round ended in a fatal error (rate limit, network failure, provider quota exhaustion). |

---

## 3. A Projection Record, Not a Conversation Message

`RoundInterrupt` is deliberately **not** a conversation message. It:

- never enters the model window or the archived transcript,
- never reaches the model,
- never costs context tokens.

It rides in the session store beside the command ledger and is re-projected into the transcript on resume by timestamp seam. **Omission stays the model-facing signal**: the model sees that the round ended, not a fabricated "the round was interrupted" message injected into its context. This is the design's central decision — a control-plane fact is recorded for the human, never confabulated for the model.

---

## 4. Generation Semantics

Two stop paths differ in whether the round's own cleanup runs:

- The plain **interrupt** path (`User`) does not bump the generation, so the round unwinds and emits its own cleanup.
- The **superseded** path's stale round has its cleanup generation-suppressed — before the record existed, this left no trace at all, which is precisely why the record was introduced.

The success-side mirror is `RetryResolution`: where an interrupt records "this round stopped", a retry-resolution records "this round hit N retryable faults and then completed", written once when the round reaches its natural terminal path after at least one retry.

---

## 5. Related

- [Lifecycle and Action Semantics Reference](../reference/lifecycle-and-action-semantics.md): the interrupt entry in the cross-domain action matrix.
- [ADR-0025](../adr/0025-scene-scoped-interrupt-chord.md): the scene-scoped interrupt chord.
- [ADR-0058](../adr/0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md): granular causal forking and aside retirement.
