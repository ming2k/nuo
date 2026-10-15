# Parked Request State Model Reference

Reference for the lifecycle of a request that parks an agent mid-round on a human decision. Cited by the shared vocabulary module (`crates/nuo-wire/src/human_request.rs`) and the broker that enforces it (`crates/nuo-harness/src/interop/human_broker.rs`).

---

## 1. The Three Parked Protocols

Three protocols park an agent mid-round on a human decision:

| Kind | Request | Reply payload |
| :--- | :--- | :--- |
| `Permission` | Approve a write/execute, or confirm a dangerous command | `PermissionDecision` (`Once` / `Session` / `Always` / `Reject`) |
| `Question` | A structured multiple-choice `ask_user` question | `UserQuestionReply` (one array of selected option labels per question) |
| `Stdin` | A line of stdin for an interactive `bash` command | `StdinReply` (raw text) |

---

## 2. The Lifecycle

Every parked request settles by one rule:

```text
requested/parked ──► replied | cancelled
```

- **park** — register the request, obtain its receiver and park timestamp.
- **reply** — settle exactly once from a wire-originated (human) reply.
- **cancel** — settle everything still parked (turn aborted, session end, interrupt), recording the kind so metrics can tell a user cancel from a teardown cancel.

A single broker (`nuo_agent::human_broker`) owns all three so the rule is enforced once.

---

## 3. Provenance (Anti-Fabrication)

The axis a naive implementation lacks is **provenance**: who actually settled the request.

| Provenance | Source |
| :--- | :--- |
| `User` | A reply that crossed a client connection. A human (or something acting with the human's authority, such as a scripted `PermissionReply`) decided. |
| `Policy { policy }` | Generated agent-side because no human channel existed. The payload names the policy. |

Only wire-originated replies resolve as `User`. A policy settlement is always labeled, so the model can never mistake a labeled recommendation for a human decision. A non-TTY headless client used to answer `ask_user` with the first option and the model received "User answered the question(s)" — a fabricated human decision; this model forbids that.

---

## 4. Autonomy Posture

Whether a session can park at all depends on its human-channel posture, accounted per attached client (the session's effective channel is the OR over all attached clients):

| Posture | Meaning |
| :--- | :--- |
| `Interactive` | A human is watching and can answer parked requests. The default for TUI and Web clients. |
| `Autonomous` | No human is reachable (headless, CI, cron). Requests must not park: they resolve per `AutonomousFallbackPolicy`, and every resolution is labeled with its true source. |

The session is autonomous only while zero interactive clients are attached. One interactive watcher is enough to make it interactive. Attach/detach bookkeeping is monotonic and lock-free (`HumanChannelAccountant`); a disconnect can only remove interactivity, never add it.

---

## 5. Autonomous Fallback Policy

When a session is autonomous and a question arrives:

| Policy | Behavior |
| :--- | :--- |
| `FailClosed` | Refuse: the tool returns an "unavailable" result and the model resolves the ambiguity itself. The default — a missing human is an error, not an opinion. |
| `RecommendedLabeled` | Answer with each question's first option (the schema's "recommended" convention), labeled `[answered by policy, not by user]`. |

---

## 6. Interactions

- **Subagents** inherit their parent's posture, so a question that would flow up to a nonexistent human fails fast in the child instead of parking forever.
- **Legacy clients** that predate the posture field default to `Interactive`.
