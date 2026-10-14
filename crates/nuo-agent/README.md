# nuo-agent

Cognitive agent runtime for the Nuo (傩) system: the think → act → observe loop, session lifecycle,
identity & intent projection (the Nuo Mask), token-pressure management, tool dispatch, and first-class agent-to-agent
collaboration.

## Shape of the crate

`Agent` is the single entry point. It owns a model `Provider`, a `ToolRegistry`,
a `TokenBudget`, and — only when built into a `Room` — a `Collaboration` binding.
The cognitive loop is a private implementation detail, so provider, tool, and
budget configuration has exactly one owner.

## Purity: agents are not inherently networked

A standalone agent has **no protocol surface**:

- no collaboration tools registered,
- no peer directory injected into its prompt,
- no protocol branches in its cognitive loop.

Collaboration arrives exclusively as ordinary tools with ordinary JSON schemas,
installed by `AgentBuilder::in_room`. An agent's behaviour therefore cannot
depend on the transport it is or is not attached to. This boundary is enforced by
tests in `tests/purity_test.rs`, including advertisement parity — every tool
advertised to the model must be executable, and vice versa.

## Built-in collaboration tools

When an agent joins a room, seven tools are registered — in two families that
mirror the protocol's two communication modes:

**Agent-to-agent** (1:1, awaits an answer):

| Tool | Purpose |
|------|---------|
| `delegate_to_peer` | Hand a self-contained task to a peer and await its result |
| `list_peers` | Enumerate reachable peers and their specialties |

**Agent-to-channel** (N:M, expects no reply):

| Tool | Purpose |
|------|---------|
| `publish_to_channel` | Post to a channel, recorded for all subscribers |
| `read_channel` | Read recent history, including what arrived while busy |
| `list_channels` | Find where a discussion is happening |
| `open_channel` | Create a channel; idempotent |
| `subscribe_to_channel` | Choose notification policy (`all` / `mentions_only` / `manual`) |

Keeping the families distinct prevents the common failure where an agent
"broadcasts" a task and waits forever for an answer, or delegates a status update
nobody asked for.

An agent does not need to be taught the protocol: it discovers peers through its
system prompt and `list_peers`, and hands off work with `delegate_to_peer` in
natural language. Rejections surface as tool errors so the model can adapt its
plan rather than treating them as infrastructure failure.

## Channel conversation

How a channel notification is handled is decided by the policy the subscriber
chose, and only the subscriber's own policy can wake it:

| Policy | On a notification |
|--------|-------------------|
| `All` | Woken on every message, and replies only if mentioned |
| `MentionsOnly` | Woken only when mentioned |
| `Manual` | Never woken; reads on demand |

A channel turn drains what the agent has not yet read into its session, so a
member that was busy or joined late still sees the discussion in order. The
channel log is the source of truth: notifications are hints, and a suppressed one
costs latency, never content.

An automatic reply is reserved for *mentions*. An `All` subscriber is woken by
every message, but posting a reply to each one would have two such subscribers
answering each other without bound — so a subscriber that was merely keeping up
decides for itself whether to contribute, and normally does so on its next turn.

Cursor-based reading (`Channel::drain_for`) makes catch-up idempotent and resumable:
a turn reads a bounded window, the cursor advances only over what was actually
returned, and the prompt states when the backlog is deeper than what it shows.
Nothing is lost to timing, and nothing is spent on messages that did not matter.
Bursts need no special handling — later notifications find the backlog already
drained and cost nothing.

## Session lifecycle

- `Session` holds state, history, and token accounting.
- `SessionEvent` streams `RoundStarted`, `ToolCallStarted`, `ToolCallFinished`,
  `Steered`, `Compacted`, and `Done` for frontends to render progress.
- `SessionStore` persists and reloads sessions (`InMemorySessionStore` included).

### Concurrency model

Two guarantees, enforced in code rather than by convention:

**Many sessions per agent, one per conversation surface.** An agent never has a
single session, and never a session per message. The key names the *surface* the
conversation happens on:

| Surface | Key | Used for |
|---------|-----|----------|
| 1:1 with an agent | `SessionKey::Peer` | Task delegation, queries |
| Threaded 1:1 | `SessionKey::Thread` | Independent continuity with one peer |
| Multi-party | `SessionKey::Channel` | A shared channel with N participants |

A channel is a surface in its own right. When A, B and C discuss something in
`#ops`, that discussion has its own continuity: folding it into B's private 1:1
history with A would either fragment the shared conversation into pairwise copies
or force every private exchange to carry unrelated group chatter.

**One writer per session.** `Agent::run_turn` and `Agent::run_channel_turn` hold a
per-session lock for the duration of a turn, so turns in one conversation are
strictly FIFO with no interleaving, while different sessions proceed in parallel.

**Scoped context.** A conversation only sees its own surface. A 1:1 prompt never
carries channel traffic, and a channel conversation sees that channel's
discussion rather than one peer's private history. This keeps context clean and
avoids paying the same group message's token cost once per private session.

**Fresh prompts on restore.** The system prompt is rebuilt on every session load
rather than persisted with the history. Room membership and channel traffic change
while a conversation is dormant, so a prompt frozen at creation would permanently
blind that conversation to the world moving on. A regression test covers exactly
this.

**Session isolation.** A session's history is private to its conversation. Peer
A's content never appears in peer B's context. Sharing is explicit — via a
channel — never implicit through a shared session.

## Steering: modifying work in flight

Inference is atomic: a prompt already dispatched cannot be amended. So "apply
immediately" can only honestly mean **at the next round boundary**. That
distinction is why `Agent::steer` is a separate mechanism from queueing a turn:

| Intent | Meaning | Consumed |
|--------|---------|----------|
| `Inbound::Turn` | New work: becomes its own turn | After the current turn finishes (FIFO) |
| `Inbound::Steer` | Guidance for work in flight | At the next round boundary |

Only the sender knows which is meant, so the sender chooses. Getting it wrong
fails in both directions: queueing "stop, wrong file" makes it useless, and
injecting an unrelated task mid-turn contaminates the running task's reasoning.

`SteerAction` chooses the effect:

| Action | Effect |
|--------|--------|
| `Note` | Appended before the next round; the model continues with the correction |
| `Redirect` | Discards the round's planned tool calls and re-plans |
| `Cancel` | Stops the turn and settles with the partial result |

A `Cancel` settles rather than abandons, so a counterpart is never left waiting
until timeout. Steering a turn that already finished returns `false` rather
than pretending success. `SteeringHandle` is keyed by request id, so guidance
for one turn is never applied to another running on the same agent.

## Token management

- `TokenCounter` estimates cost locally (CJK-aware) before dispatch.
- `TokenBudget` defines watermarks: warning → compaction → hard limit.
- `Compactor` offloads oversized tool outputs into claim-check storage (`Partial` or `Full`), then summarizes mid-history while
  preserving the system prompt and the most recent turns.

## Tools

Three sources, one namespace: `DynamicTool` (closures), `Tool` implementations,
and `McpTool` (Model Context Protocol bridges). All are validated against the
same rules: object schema, declared properties, no unknown arguments.
