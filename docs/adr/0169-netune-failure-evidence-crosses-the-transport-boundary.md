---
id: ADR-0169
title: "Netune Failure Evidence Crosses Nuo's Transport Boundary"
status: accepted
date: 2026-11-10
scope: [workspace]
superseded_by: null
negative_knowledge: true
---

# ADR-0169: Netune Failure Evidence Crosses Nuo's Transport Boundary

- **Status:** Accepted
- **Date:** 2026-11-11
- **Related:** netune's *diagnostic side table on the trace* and *one trace
  identity and sink contract* decisions (netune 0.0.26, documentation-corrected
  by 0.0.27), [ADR-0037](0037-bifurcation-of-session-telemetry-into-session-stats-and-session-trace.md),
  [ADR-0108](0108-provider-substrate-consolidation-and-satellite-crate-retirement.md)
- **Amends:** ADR-0108 (the direct netune substrate now includes its diagnostic contract, not only its timings)

## Context

Nuo derives the latency timeline for every model attempt from netune's event
log. That part of the relationship was already honest: `dns_us`, `tcp_us`,
`tls_us`, stream readiness, and pool reuse are pure derivations of recorded
events.

Failure was the gap. `netune` returned a `NetError`, and Nuo did what a caller
without a record has to do:

- `netune::Client::send_traced` returned a bare error, so the internally owned
  recorder was dropped at exactly the moment it mattered;
- Nuo's `TransportError` carried only `message: String` and a `retryable: bool`;
- the attempt-level trace sink was invoked only when the response body ended
  successfully, so no sink ever saw a failed attempt;
- the transport hand-built a `RequestTrace` with `TraceId::new("transport")`,
  a placeholder rather than an identity;
- a caller-side deadline discarded the collector, so the phase the attempt
  reached survived only in a prose string.

netune v0.0.26 (documentation-corrected by v0.0.27) closes the substrate gap.
A failed public attempt returns `AttemptError` with a generated trace id, a
bounded `DiagnosticTable`, and a `TraceSnapshot`; `RequestTrace` schema v2
carries `TraceMeta` and diagnostics; and `TraceSink` v2 carries the envelope
with every event. Failure evidence survives `Observation::Off`.

The remaining decision is Nuo's, not netune's: where does that evidence enter
Nuo's own error and telemetry contracts, and how much of it crosses each
boundary?

## Decision Drivers

- **A failed attempt is an attempt.** Nuo's `/trace` timeline and its retry
  policy already treat every attempt as a first-class record; a failure that
  discards its record is the one inconsistent case.
- **No display-string classification.** Retry and phase decisions must read a
  stable code, never `format!("{error}")`.
- **One identity.** The trace id on the error must be the id on the trace the
  sink received, not a second identifier minted by Nuo.
- **Bounded error values.** `ProviderError` is cloned, serialized, and carried
  through the harness; it must not embed an unbounded event log.
- **No wire change.** Trace propagation changes HTTP header order/sets, which
  are fingerprint-relevant. Nuo does not need it to explain its own attempts.
- **Evidence must be useful at every call site.** Provider adapters see
  `TransportError`; the harness sees `ProviderError`; control-plane and MCP
  callers return strings. Each boundary keeps the stable code.

## Decision

### 1. A typed diagnostic crosses the transport boundary

`nuo_provider::transport::TransportDiagnostic` carries the cold-path evidence
that belongs on an error:

```rust
pub struct TransportDiagnostic {
    pub code: DiagnosticCode,
    pub phase: DiagnosticPhase,
    pub retry: RetryVerdict,
    pub message: String,
    pub trace_id: TraceId,
    pub dropped_events: u32,
    pub dropped_diagnostics: u32,
}
```

`TransportError::{Transport, Timeout, Stream}` carry
`Option<Box<TransportDiagnostic>>`; `ProviderError` carries the same value and
exposes `diagnostic()`. Its rendered form appends the stable code
(`[NET-E-TCP-FAILED]`) so a transcript or log names the classification without
the reader parsing prose. The full `EventLog` stays in the trace sink; the error
keeps only the bounded account.

### 2. `AttemptError` is consumed at the boundary that owns the attempt

`NetTransport::send` is the only place that sees netune's `AttemptError`. It:

1. clones the `TraceSnapshot` and publishes it to `TraceSink`, even on failure;
2. converts the terminal `DiagnosticRecord` into `TransportDiagnostic`;
3. returns a `TransportError` whose trace id equals the published trace's
   `meta.id`.

A body failure arrives as a bare `NetError` because netune's attempt boundary is
the response head. The collector still owns the phase timeline, so Nuo records
the error's own diagnostic (unless the protocol already recorded a more
specific one), snapshots the collector, and publishes that snapshot. The sink
therefore sees exactly one terminal trace per attempt on both paths.

### 3. A caller-side deadline speaks the same vocabulary

Nuo's whole-request deadline is outside netune's attempt; cancelling the future
leaves no producer-recorded terminal diagnostic. Nuo asks netune's pure
`diagnose` for the last recorded phase (no clock, no I/O) and records a synthetic
`NET-E-TIMEOUT` with that phase before snapshotting. The error and the sink
share the same trace id. The control-plane `Http` handle uses the same phase
derivation to name a timed-out exchange and appends the code and trace id to its
string error.

### 4. Trace schema v2 replaces the hand-built envelope

The transport no longer constructs `RequestTrace` with a placeholder id. A
successful body still uses `BodyStream::trace()`, which carries the negotiated
`ConnectionIdentity` and the fidelity its tap produced. A failed or merely
timed-out attempt reconstructs an envelope from the collector's `TraceSnapshot`
with `ConnectionInfo`/`ConnectionIdentity` at their unknown defaults: an
uncompleted attempt must not claim a socket it cannot prove. `AttemptRef`
remains zero because `RequestParts` does not carry the harness round/turn/attempt
ordinal; inventing one inside the transport would be a false attribution. The
generated trace id is the join key, and plumbing attempt ordinals through the
provider contract is a separate change.

netune's event-level `TraceSink` v2 is not installed by Nuo. Nuo's sink is
attempt-level because it joins the provider/model endpoint metadata; it is a
test and future-integration seam today, and it now receives failures as well as
successes. The netune ring remains the record of truth either way.

### 5. Trace propagation stays off

Nuo does not enable `TracePropagation::Header`. Adding `x-netune-request-id`
would change HTTP/1 header order and the HTTP/2 header set, which are part of
the wire identity Nuo deliberately controls. Nuo's egress traces have real,
locally generated ids; joining them to an ingress peer is a future decision
that must be a named policy, not a default side effect.

## Invariants Ratified

- **`[INV-PROV-14]` Failure Evidence Crosses the Boundary**: every failed
  attempt emitted by `nuo_provider::transport` carries a registered,
  non-`Unclassified` `DiagnosticCode` and the trace id of its snapshot. A
  caller never has to parse `Display` to classify a transport failure.
  *Enforced by*: `a_failed_attempt_carries_its_diagnostic_and_publishes_its_trace`,
  `a_body_failure_carries_the_phase_and_publishes_its_trace`, and the
  `DiagnosticRecord` registry's own `INV-DIAG-01` test in netune.

- **`[INV-PROV-15]` One Identity per Attempt**: the trace id on
  `TransportError::diagnostic()` and the id on the trace delivered to
  `TraceSink` are the same value; the transport never mints a placeholder.
  *Enforced by*: both failure tests assert `&trace.meta.id == diagnostic.trace_id()`.

- **`[INV-PROV-16]` A Deadline Is Classified, Not Narrated**: a caller-side
  timeout records `NET-E-TIMEOUT` with the last observed phase and the same
  trace id before returning, so a timeout is reviewable like any substrate
  failure. *Enforced by*: `non_streaming_timeout_fires_and_is_retryable` and
  `timeout_reports_headers_or_body_and_redacts_query`.

## Rejected Alternatives

### Option 1 (Rejected): Add `tracing` records beside the trace

This reintroduces netune's rejected second record. The diagnostic is attached
to the same attempt and travels with its id; a logging facade would drift from
it and put provider strings on a path that is not the record of truth.

### Option 2 (Rejected): Parse the `Display` message to recover the phase

The string is not stable, not bounded, and not available offline. It also
cannot distinguish "the peer refused the connection" from "the TLS handshake
expired" without vendor-specific prose. The registry code exists precisely to
avoid this.

### Option 3 (Rejected): Attach the whole `TraceSnapshot` to `ProviderError`

`ProviderError` is cloned through the harness and serialized into session
state. A full event log would make every error proportional to the stream it
failed on. The bounded diagnostic plus a trace id is enough to join the error
to the sink's record.

### Option 4 (Rejected): Enable trace-header propagation by default

It changes the wire fingerprint and would make every provider request carry a
header no provider asked for. Propagation remains an explicit policy for a
future ingress-joining feature.

### Option 5 (Rejected): Keep the sink success-only and classify failures from
the retry loop

The retry loop sees the synthetic `ProviderError` after conversion; it cannot
prove the phase or publish the failed trace. The owner of the attempt must
publish the evidence once, at the boundary, before the error is flattened.

## Consequences

- **Positive**:
  - A failed model, control-plane, MCP, or remote-skill request now names a
    stable netune code, a phase, a retry verdict, and the trace it belongs to.
  - `/trace` and any future sink consumer see failed attempts, not only
    successful bodies.
  - Retry policy can read `DiagnosticCode` instead of inferring from prose.
  - The transport no longer fabricates a trace id or a schema-v1 envelope.
- **Negative & Trade-offs**:
  - `ProviderError` gains an optional serialized field; old records decode
    unchanged through `#[serde(default)]`.
  - A failure allocates one bounded diagnostic and, for traced attempts, one
    snapshot. This is cold-path by construction.
  - `TransportError`'s structured variants gained a field; every constructor
    and matcher was updated in the same change.
  - Nuo depends on netune's code registry staying stable. That is the same
    contract it already relies on for event kinds.

## Validation

- `cargo test -p nuo-provider`: the transport suite asserts the error/sink
  identity, the body-failure path, and the timeout classification.
- `TransportDiagnostic` round-trips through `ProviderError`'s serialized form,
  so a persisted failure keeps its code and trace id.
- The netune migration guide (`docs/reference/migration-0.0.26.md`) is the
  source for the schema-v2 and error-boundary changes.
