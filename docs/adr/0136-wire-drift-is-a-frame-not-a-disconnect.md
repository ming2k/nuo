---
id: ADR-0136
title: "Wire Drift Is a Frame, Not a Disconnect: Typed Undecodable Frames, Link-Loss Disclosure, and a Census-Gated Protocol Number"
status: accepted
date: 2026-12-15
scope: protocol/nuo-protocol, server/nuo-server, host/nuo, tui/event-loop, dev/governance
superseded_by: null
negative_knowledge: true
---

# 0136. Wire Drift Is a Frame, Not a Disconnect: Typed Undecodable Frames, Link-Loss Disclosure, and a Census-Gated Protocol Number

- Status: Accepted
- Date: 2026-12-15
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0123](0123-attach-failure-typing-stale-tab-retirement-and-identity-addressed-session-rebasing.md) (extends `[INV-ATTACH-02]` from attach refusal to post-handshake decoding), [ADR-0133](0133-attach-readiness-boundary-network-free-handshake-typed-session-phases-and-authoritative-late-hydration.md) (`Welcome` remains the first frame; a refusal taxonomy now also covers an unreadable first frame), [ADR-0134](0134-two-track-notice-architecture-sovereign-notice-center-dialog-and-footer-dock-eradication.md) (the commit that exposed this defect by deleting `RetryResolved` without a protocol bump)

---

## Context and Problem Statement

A user repeatedly observed `nuo` print one line —

```text
nuo: connecting to the session server…
```

— and then close its window, with no error, no notice, and no way to find out what happened. It reproduced "from time to time", which made it look like a race.

It was not a race. The client log recorded it eight times in a single day, identically:

```text
WARN nuo::transport: attach: wire recv failed
     error=Failed to deserialize Wire payload: unknown variant `RetryResolved`, ...
INFO nuo::shell::event_loop: app exiting reason="should_quit_flag"
```

Three facts, established by inspection of the running system:

1. `/proc/<server-pid>/exe` was `/usr/bin/nuo (deleted)` — the server had been running since before the binary on disk was replaced.
2. The installed client contained **zero** occurrences of `RetryResolved`; the running server image contained **twelve**.
3. `git show 1ba2961` (ADR-0134) deleted the variant and touched `PROTOCOL_VERSION` **not at all** — it stayed at 18.

So a server one revision behind answered the same protocol number, negotiated successfully, and then emitted a variant the client no longer modelled. The variant is only produced *after a provider retry recovers*, which is why it appeared intermittently rather than always: the fault was deterministic but latent, armed by ordinary model-side 429/503 jitter.

### Why it killed the client

The chain had three independent defects, each of which alone would have been survivable:

**D1 — Encoding conflated a frame with a stream.** `NativeWireCodec` returned `Result<Wire, io::Error>`, so "this payload names a variant I do not know" and "the socket is dead" were the same value. But both transports are *self-delimiting*: native IPC carries a 4-byte length prefix and WebSocket carries message boundaries. By the time parsing ran, the frame's bytes had already been consumed — the stream was perfectly aligned on the next frame. The decoder reported a recoverable per-frame condition as unrecoverable stream death.

**D2 — Semantics conflated a link death with a session end.** When the reader task stopped, `transport.rs` pushed `AgentResponse::Exit` into the response channel. `Exit` is a *server instruction* — `/exit`, a killed thread, a drain. Synthesizing it locally meant the client asserted "the server told me to stop" when all it knew was "my own reader stopped". The TUI's handler for `Exit` is `should_quit`, so the window closed.

**D3 — UX suppressed the one line that explained it.** The detach epilogue was gated on `info.port > 0`. A local server is UDS-only (`port: 0`), so the `Reconnect: nuo` hint never printed. Combined with D2's silent quit, the operator saw the connect line and then a vanished window.

### Why the stale server was never replaced

`local_pair_compatible` read:

```rust
let same_version = version == Some(env!("CARGO_PKG_VERSION"));
return !same_version || server_image_is_current;
```

Any product-version difference was accepted outright. `is_dev_drift` carried a mirror-image guard requiring `same_version`, so it could never fire for the very case that mattered. An upgraded client therefore kept attaching to a server whose binary no longer existed on disk, for as long as that process lived.

### The structural gap

Nothing in the repository connected "the wire surface changed" to "the protocol number moved". `PROTOCOL_VERSION` is a hand-maintained `u32`, and deleting an enum variant is a one-line edit that no test objected to. D1–D3 are *containment*; without a gate on the number, the same class of drift remains available to the next commit.

## Decision Drivers

- **A frame the peer cannot parse is information, not a fault of the connection.** Self-delimiting transports make this provable rather than hopeful.
- **Only the server can end a session.** The client must never synthesize the instruction that terminates it.
- **A silently disappearing window is the worst possible outcome.** Every terminal path must leave a retained, answerable record.
- **Mechanisms over exhortations.** The rule "bump the protocol when the wire moves" must be enforced by something that fails, not by a comment.
- **No backward compatibility shims.** The wire envelope is typed; unknown variants are surfaced, never silently coerced.

## Architectural Decisions

### 1. `WireFrame`: undecodable is a frame outcome (`[INV-WIRE-01]`)

`nuo-protocol` gains:

```rust
pub enum WireFrame {
    Wire(Wire),
    Undecodable { envelope: Option<String>, detail: String },
}

pub fn decode_wire_payload(payload: &[u8]) -> WireFrame;
```

Both codecs (`NativeWireCodec` here and in `nuo-server`) now decode to `WireFrame`. `Err` is reserved for framing that genuinely cannot be resynchronized — an impossible length prefix. A payload that names an unknown variant returns `Undecodable` and the stream keeps its alignment.

`envelope` names the outer discriminator (`"Request"`, `"Response"`, …) because serde's message for a `#[serde(flatten)]`ed variant reports only the inner enum, leaving an operator unable to tell which direction drifted.

Every consumer must handle the variant explicitly:

- **Attach handshake**: an unreadable first frame is still a refusal, and is reported as such — no thread was claimed, and the protocol number is exactly what the peers disagreed about.
- **Attached reader**: disclosed as a notice; **the connection stays open**. A live session is not discarded because one frame was unreadable.
- **Control / monitor**: reported as the specific skew rather than a timeout.

Each of the three consumers has its own end-to-end test over a socket pair,
because their correct behaviour differs:

| Consumer | On an unmodelled frame | Pinned by |
| :--- | :--- | :--- |
| Attach | disclose, keep the connection | `an_unmodelled_frame_after_welcome_does_not_end_the_attachment` |
| Control | fail with a refusal naming the skew (one verb, one reply) | `an_unmodelled_frame_on_the_control_plane_names_the_skew` |
| Monitor | skip and keep streaming | `an_unmodelled_frame_on_the_monitor_plane_is_skipped_not_fatal` |

The monitor test asserts the event *behind* the skewed frame still arrives,
which is what proves the reader kept going rather than merely not panicking.

### 2. `LinkLost` is not `Exit` (`[INV-WIRE-02]`)

`transport.rs` no longer synthesizes `AgentResponse::Exit` when its reader task ends. `AgentResponse::Exit` is reserved for server instruction.

The client gained `AppMutation::LinkLost { reason }`, which:

- latches `App::link_down` (the existing dead-link chrome state, D6);
- retains an `Error` notice naming what was lost **and stating the session is still running**;
- **does not** set `should_quit`.

`LinkLost` is emitted when the response stream ends **with a recorded transport failure**. The reader task stores its own error in a `LinkLostMarker` shared with the consumer, so the disclosure repeats the real cause (`IPC read error: Connection reset by peer (os error 104)`) rather than a canned sentence. A clean close records nothing: the server's `Exit` already said what happened, and inventing a fault on top of it would contradict the instruction.

The mutation latches `App::link_down` **and** sets `App::relink_requested`. The event loop ends the current *attachment* on that flag, and `runner::run_attached` re-resolves the server (discovering, replacing, or spawning as needed) and attaches again with the transcript and tabs carried across in `TuiOutcome`. So a transient socket failure costs the operator nothing: the chrome states what broke, then the client puts itself back.

### 3. The detach epilogue always speaks (D3)

The `port > 0` gate is removed. The epilogue prints for a UDS-only server too, naming the endpoint the server actually advertises.

### 4. Compatibility requires the image to agree (D4)

`local_pair_compatible` now checks the image **first**:

```rust
if !server_image_is_current { return false; }
if let Some(proto) = protocol { return protocol_accepts(proto); }
version.is_some_and(|v| v == env!("CARGO_PKG_VERSION"))
```

An in-window protocol number can always be outrun by an in-window wire change, so the number alone cannot certify compatibility. `is_dev_drift` drops its `same_version` precondition for the same reason: a replaced image is drift whatever version it reports.

### 5. The census gates the protocol number (`[INV-WIRE-03]`)

`crates/nuo/src/dev/wire_census.rs` recomputes the variant census of `Wire`, `AgentRequest`, `AgentResponse`, and `RoundEvent` from source and compares it against the checked-in `wire-census.txt`, which records the protocol number it was taken at:

| Observed | Verdict |
| :--- | :--- |
| Same number, different variants | **Failure** — names every added/removed variant |
| Number advanced, surface unchanged | **Failure** — a bump that says nothing |
| Number advanced, surface changed, census renewed | Clean |

Renewing is `nuo dev census --bless`; checking is `nuo dev check wire` and the enforced test `test_inv_wire_census_matches_protocol_version`.

The census is a **file in the tree**, not a regenerated artifact, so "the wire changed" is reviewable in the diff of the commit that changes it. A self-check test reproduces the ADR-0134 shape in miniature and asserts the checker names the removed variant.

## Consequences

### Positive

- A protocol skew now yields a notice naming the failed enum and envelope, with the session intact — instead of a vanished window.
- A transport failure names its real cause and the client re-dials itself, so a socket blip no longer ends the session.
- A one-shot headless run whose link dies mid-run exits non-zero instead of printing partial output and reporting success.
- Upgrades that leave a stale server process are detected and replaced on the next attach, which removes the *source* of the drift rather than only its symptom.
- The "delete a variant, forget the bump" class of defect fails CI with the variant names in the message.
- The detach epilogue is unconditional, so every exit path leaves the operator something to act on.

### Negative

- Every `WireFrame` consumer must handle `Undecodable`; the exhaustive `match` forces this at compile time, which is the point. The three consumers also carry their own end-to-end tests, because "handle it" means three different things (disclose / refuse / skip).
- The census must be renewed in the same commit as a wire change. This is a deliberate second step that a reviewer sees.
- An in-window protocol number no longer implies compatibility on its own, so a dev-build server image that differs from the installed one is replaced on attach instead of reused.
- A link loss now re-attaches rather than merely reporting, so a server that is permanently unreachable produces an attach attempt per loss. The failure surfaces through the normal attach error path (`[INV-ATTACH-01]`), not a silent loop.

### Neutral

- Protocol version unchanged at 19 by this ADR: `WireFrame` is a client-internal type and the wire bytes are identical. The census was baselined at the current number in the same commit.

## References

- [ADR-0123](0123-attach-failure-typing-stale-tab-retirement-and-identity-addressed-session-rebasing.md) — typed attach refusal, `[INV-ATTACH-01]`/`[INV-ATTACH-02]`
- [ADR-0133](0133-attach-readiness-boundary-network-free-handshake-typed-session-phases-and-authoritative-late-hydration.md) — network-free handshake, `[INV-ATTACH-03]`
- [ADR-0134](0134-two-track-notice-architecture-sovereign-notice-center-dialog-and-footer-dock-eradication.md) — the variant deletion that exposed the gap
