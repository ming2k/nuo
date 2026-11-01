# Command Records and Receipts

This article explains the boundary Nuo draws between a slash command that **does** something and one that
only **reports** something, and why only the first is remembered. It is about the model, not about a
keybinding: read it to understand what a session's history is a record *of*.

The decision record is [ADR-0140](../adr/0140-command-effect-classes-readings-are-receipts-not-records.md).

> **One consequence worth stating up front.** Sessions written before this boundary existed mixed the two
> kinds in one ledger, and the kind was never recorded, so no filter can separate them after the fact. Those
> ledgers are retired on load (a schema-major break): the acts a thread took are reconstructible from its own
> state, while a stale measurement is useful to nobody. See §6.

---

## 1. A transcript's strongest claim is not "this happened"

A thread's transcript is ordered, append-only, and replayed from durable state. Those properties are easy to
state and easy to get wrong, because they describe *shape*. The claim that actually earns a transcript its
value is narrower:

> Everything in it was said, in this order — **and it was true when it was said**.

The second half is what a snapshot cannot satisfy. A round of dialogue is a claim about a conversation at a
moment, and the moment is intrinsic to it: re-reading turn 4 does not ask you to believe the model is still
mid-thought. An operational aside such as "unattended execution was switched on at 14:02" behaves the same
way — it is a past act, and past acts stay true.

A token count does not. Neither does a list of running background jobs, a workspace diff, or a provider
quota table. Each is a measurement of the world at the instant it was requested, and each is expected to
give a different answer tomorrow. Stored beside dialogue and replayed by timestamp, such a row asserts
something false the moment the world moves — and it carries no way to signal its own staleness, because
freshness is not a property a stored line can have.

## 2. Two kinds of command, one ledger

The vocabulary therefore splits along that line:

| | **Act** | **Reading** |
| :--- | :--- | :--- |
| Examples | `/unattended on`, `/role ops`, `/trust deny`, `/compact`, `/fork`, `/undo`, `/init`, `/jobs kill` | `/usage`, `/stats`, `/jobs`, `/jobs logs`, `/diff`, `/search`, `/skills list`, `/trust status`, `/help`, `/export`, `/tree` |
| What it changes | durable state the thread must remember | nothing |
| Answer after a week | still the history | may be entirely different |
| Remembered | yes — a record | no — a receipt |

The distinction is declared once, next to each command's summary, in the single table that already generates
completion, help, the catalog, and dispatch. That matters more than it sounds: a rule about *which* commands
are remembered, spread across dozens of handler functions, is a rule that quietly stops being true the
first time someone adds a command. Declared in the table, the class is a property of the vocabulary — and a
command that does not state one cannot be added, because the build fails.

## 3. A reading still answers

Ephemeral is not the same as silent. When you ask a reading, you get its answer immediately, in full, on the
same screen. What you do not get is a permanent row in the conversation asserting that the answer is still
current.

Where the answer goes instead is the client's receipt log — the same retained, scrollable, searchable record
that already holds every other disclosure the client surfaces: a failed clipboard write, a workspace-trust
refusal, a provider retry receipt. That tier exists precisely for content that is worth reading now and
worth remembering for the length of the session, but is not dialogue. A `/diff` reply and a link failure are
the same category of thing: *what came back when you asked*. They live in the same place.

This is why an act and a reading are not merely filed differently but produce visibly different outcomes:
an act settles a row into the thread at the moment it happened, so the conversation's shape reflects the
decisions taken during it; a reading leaves the thread exactly as it was.

## 4. The audit trail is narrower, and worth more

An audit trail earns trust by being complete *about a well-defined subject*. "Every command you typed" is
a poor subject: it mixes decisions with measurements, so the reader has to re-derive which rows were
decisions before they can reason about what happened.

"Every act that changed this session" is a subject you can rely on. If a resumed thread shows
`/unattended on`, the posture is on (or was turned off later — and that row is there too). If it shows a
`/trust` grant, the workspace was trusted at that point. Nothing in the record needs re-dating, because an
act's date is part of the act.

The trail also stops leaking. A record is not private to the transcript: it is transported in the full-attach
snapshot, rendered into `/export`, and ranked as history by `/search`. A static help listing that gets
replayed into every export and scored as recall is not a small inefficiency — it is content in the wrong
tier, which is exactly the defect the notice architecture fixed for diagnostics.

## 5. Failing is a record; asking and failing is not

Two failures look similar and belong on opposite sides:

- A `/undo` that could not read the workspace journal is an **act that was attempted and refused**. That
  belongs in the record: "I tried to undo and it refused" is what a later reader needs.
- A `/diff` that could not read the same journal is a **reading that could not be produced**. Nobody's
  history changed; the answer simply was not available.

A mistyped command is a third case, and it belongs on neither side. Nothing ran, nothing reached the model,
and the draft you typed is handed straight back to your composer. It leaves no trace — which is what makes
fixing a typo cost one keystroke rather than a permanent scar in the session's history.

## 6. What this costs

A reading is gone when the process ends. If you want last week's job table or quota figures, you ask again —
and the honest answer may differ, which is the point. A stale answer presented beside live dialogue, with no
way to tell it is stale, is worse than no answer at all: it invites you to reason about the present using
the past.

The trade is deliberate and one-directional. Nuo would rather have a short history that is entirely true
than a long one you must audit before trusting.

## 7. Related

- [Interrupt Semantics](interrupt-semantics.md) — the same principle for a stopped round: a control-plane
  fact is recorded for the human and never confabulated for the model.
- [Architecture Overview](architecture-overview.md) — where the session store, the transcript projection,
  and the receipt log sit relative to each other.
- [ADR-0116](../adr/0116-content-residency-and-surface-layer-topology.md) — one home per content, and why the
  transcript is not a destination.
- [ADR-0121](../adr/0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) — the
  receipt log as the home, with the bubble as a pointer to it.
