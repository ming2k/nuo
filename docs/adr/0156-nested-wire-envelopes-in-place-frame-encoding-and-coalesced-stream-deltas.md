---
id: ADR-0156
title: "Nested Wire Envelopes, In-Place Frame Encoding, and Coalesced Stream Deltas"
status: accepted
date: 2026-11-07
scope: protocol/wire, server/ipc, client/transport, performance/streaming
superseded_by: null
negative_knowledge: true
---

# 0156. Nested Wire Envelopes, In-Place Frame Encoding, and Coalesced Stream Deltas

- Status: Accepted
- Date: 2026-11-07
- Deciders: Nuo Architecture Working Group
- Consulted: Protocol, Server Transport, TUI Runtime, Performance Engineering
- Amends: [ADR-0136](0136-wire-drift-is-a-frame-not-a-disconnect.md) (the per-frame outcome taxonomy stands; the envelope shape and codec cost model change)
- Complements: [ADR-0122](0122-provider-retry-is-chrome-typed-state-machine-and-turn-outcome-anchors.md)

---

## Context and Problem Statement

The client/server wire is the hottest serialization path in the product: a
single answer streams a delta per provider SSE event, and every delta crossed
the following path.

1. **`#[serde(flatten)]` on the envelope.** `Wire::Response { #[serde(flatten)]
   response }` (and `Request` / `Monitor`) makes every *decode* buffer the frame
   into an intermediate map before it can name the inner enum. Flatten is a
   serializer convenience; on a stream it is a per-frame allocation and copy.
2. **One allocation and one copy per frame on encode.** The codec called
   `serde_json::to_vec(&item)` into a fresh `Vec`, then copied that into the
   framed `BytesMut` behind the 4-byte length prefix.
3. **One frame, one flush, one write syscall per delta.** The forwarding loop
   called `SinkExt::send` per `AgentResponse`, and `send` flushes. A provider
   that emits word-sized deltas produced hundreds to thousands of writes and
   JSON documents per answer.
4. **Per-frame thread identity.** `round_response` clones the session id into
   every event, including every delta.

None of these is individually fatal; together they are the fixed cost of
streaming, paid once per few characters.

## Decision

### 1. Payloads are nested, never flattened

```json
{"type":"Response","response":{"Round":{…}}}
```

`#[serde(flatten)]` is removed from `Wire::Request`, `Wire::Response` and
`Wire::Monitor`. The inner enum decodes directly from its named field.

This is a **hard wire break**. `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION`
move to 23 together: there is no compatibility lane for the flattened shape and
no dual decoder. A peer on the old shape is refused by protocol negotiation, not
silently half-served.

### 2. Frames are encoded in place, by exactly one codec

There is **one** `NativeWireCodec` and one `MAX_WIRE_FRAME_SIZE`, both owned by
`nuo-protocol`. The server-facing copy (a byte-for-byte duplicate that had to be
edited in lockstep) is deleted; `nuo-server::wire_channel` is transport
adaptation only. The codec reserves the 4-byte prefix, serializes with
`serde_json::to_writer(&mut dst.writer(), &item)`, then backfills the length.
On a serialization error or an oversize frame the buffer is truncated back to
the frame start, so a failed encode can never shift the stream's alignment.

### 3. Streamed deltas coalesce, barriers flush

The per-connection forwarding loop holds a pending run of consecutive deltas of
the same channel (`StreamDelta` or `StreamReasoningDelta`) for at most
`STREAM_COALESCE_WINDOW` (8 ms). The run is flushed immediately when:

- a non-delta event arrives (so `StreamStart`, `ToolCall`, `StreamEnd`, … keep
  their ordering);
- the channel or thread changes (the displaced run is sent before the new one);
- the event channel lags, closes, or the client sends `EndThread`;
- the coalescing deadline expires.

The client's contract is unchanged: it receives the same deltas in the same
order, in fewer frames.

## Invariants

1. **[INV-WIRE-05] Nested envelope.** A wire payload names its direction in
   `type` and carries its body in the named field. No field of the body is
   hoisted to the envelope.
2. **[INV-WIRE-06] One encode, one buffer.** A frame is serialized directly into
   the frame buffer; no intermediate `Vec` and no copy.
3. **[INV-WIRE-07] Alignment survives failure.** Every encode error leaves `dst`
   exactly as long as it was before the frame started.
4. **[INV-WIRE-08] Coalescing is order-preserving.** A delta is never reordered
   across a barrier. The only observable difference is that several deltas of
   one channel may arrive in one frame.

## Consequences

- A streamed answer costs one frame per 8 ms of deltas per channel, instead of
  one per provider event.
- Decode no longer buffers each frame into an intermediate map; the client pays
  one direct deserialization.
- The raw shape changed. Every fixture that hand-framed a wire payload was
  updated (`nuo-protocol`, `nuo-server`, `nuo` transport and integration tests);
  the unknown-variant tests now assert that the failure names the offending
  inner variant.
- `StreamStart` still precedes the first delta: the coalescer only batches
  deltas, and `StreamStart` is a barrier.

## Negative Knowledge

- **Do not** reintroduce `#[serde(flatten)]` for envelope ergonomics. The
  ergonomic win is at the call site; the cost is on every frame.
- **Do not** coalesce across channels or threads "because deltas look alike".
  Reasoning and text deltas are different surfaces, and a connection can carry
  more than one thread.
- **Do not** delay a barrier to fill a coalescing window. Ordering is the
  contract; batching is the optimization.
- **Do not** keep a legacy decoder for the flattened shape. Two wire shapes in
  one protocol number is the drift this ADR removes.

## Verification

- `nuo-server::serve::tests::{stream_deltas_coalesce_until_the_channel_changes,
  take_delta_matches_only_stream_events,
  end_thread_serializes_as_a_nested_unit_variant}`
- `nuo-protocol::readiness_protocol_tests::{undecodable_frame_is_a_frame_outcome_not_a_stream_error,
  the_stream_stays_aligned_after_an_undecodable_frame,
  an_oversize_frame_is_not_left_in_the_buffer}`
- `nuo::shell::tests::link_resilience::an_unknown_wire_variant_does_not_end_the_stream`
- `nuo` integration: a raw-framed client attaches, chats, and ends the thread
  using the nested envelope.
