# acp (Agent Coordination Protocol)

Canonical inter-agent communication, addressing, envelopes, and collaborative channels.

This crate defines *how agents find each other, address each other, and
communicate* — and nothing else. It contains no cognitive logic, no model client,
and no I/O beyond in-process channels.

## Two communication modes

The protocol standardizes two deliberately distinct modes. Conflating them is
what makes multi-agent systems either lossy or noisy.

| Mode | Shape | Mechanism | Use for |
|------|-------|-----------|---------|
| **Agent-to-agent** | 1:1 request/reply | `Room::request` — correlated reply | Handing a task to the peer that owns it |
| **Agent-to-channel** | N:M publish/read | `Channel` — ordered log + subscriptions | Keeping a group informed without a reply each |

An agent-to-agent call needs *this* answer delivered to *this* caller. A channel
needs an ordered record that any number of readers consume at their own pace,
without the author knowing or caring who is listening.

The asymmetry is deliberate. There is no "channel-to-agent" request/reply mode,
because a channel post has no single answerer: what comes back is a *notification*
that new traffic exists, and the log — not the envelope — is the source of truth.
An `AgentEnvelope` therefore always addresses exactly one agent, and channel
fan-out is resolved by the room where subscriptions live. Modelling fan-out as a
recipient variant would make the router claim it delivered to N agents while
carrying a single target.

## Channel standard

A channel is a named, ordered, many-to-many surface. The standard covers:

**Identity** — `ChannelId` enforces its rules at construction, so a name that
cannot be mistyped cannot silently create a second, empty channel:

- 1–64 characters
- lowercase ASCII letters, digits, `-`, `_`, and `/` as a hierarchy separator
- must begin with a letter or digit
- no empty segments (`a//b`, `/a`, `a/` all rejected)

**Ordering** — every message carries a gapless, monotonic `seq` starting at 1.
This is what makes catch-up possible: a reader at cursor `n` asks for everything
after `n` and gets exactly what it missed.

**Publication** — `publish` always appends. It never fails for lack of an
audience, because a channel is a record, not a delivery guarantee. It does
require that the author, and anyone mentioned, are members: a mention naming a
stranger is a notification nobody can receive, which from the author's side is
indistinguishable from being ignored.

**Subscription** — controls *notification*, never *visibility*:

| Mode | Behaviour |
|------|-----------|
| `All` | Woken on every message. For low-traffic channels of record, e.g. `incidents`. |
| `MentionsOnly` | Woken only when mentioned. The default — an unconfigured subscriber gets the conservative choice. |
| `Manual` | Never woken; read on demand. For monitoring and audit. A mention does not override this. |

The cursor is preserved across mode changes, so switching from `All` to
`MentionsOnly` never replays or skips history.

The decision is made **once**, by the room, at publication time, and the reason
travels in the notification. Re-deriving it at the receiver would be unsound: the
message may already have been evicted by retention, and any scan over the log is a
bound that silently drops mentions beyond it.

**Retention** — bounded (default 512 messages). Sequence numbers keep advancing
as entries are evicted, so a stale cursor correctly reads as "nothing new" rather
than replaying.

**Scope** — a channel log lives in the room that owns it, and a room is a single
trust domain. Routing delivers a *copy* of an envelope to one addressee, so a room
whose members span hosts would give each host its own divergent log and a reader
would silently see only part of the discussion. Rather than pretend otherwise,
`subscribe` admits only current members. Replicating a channel across hosts is a
consensus problem — ordering, partitions, conflicts — and belongs in a room
implementation backed by a replicated log, not in the message router.

## Point-to-point

Every request/reply exchange is correlated by envelope id. A `Mailbox` keeps
in-flight request subscriptions **separate** from unsolicited traffic, so
awaiting a reply can never consume — or discard — an unrelated delegation
arriving at the same agent. Replies for unknown or expired requests are dropped
deliberately rather than leaking into the inbox as phantom tasks.

## Delegation depth

`DelegationBudget::remaining_hops` is decremented on each hand-off and carried
through replies, so agents delegating to agents cannot recurse without bound.

## Cross-host routing

`AgentRouter` resolves local members first, then falls back to the longest
matching remote gateway. Implement `TransportSender` / `TransportReceiver` for a
new wire protocol (Unix socket, WebSocket, NATS, …) and attach it with
`attach_outbound` plus `TransportBridge::spawn_inbound`.
