---
id: ADR-0265
title: "Supply-Chain Time Modernization (chrono to jiff 1.0) and Advisory Governance"
status: accepted
date: 2026-11-29
scope: workspace/deps, acp, protocol, engine, server, nuo, ui-primitives, view, providers, ci/supply-chain
superseded_by: null
negative_knowledge: true
---

# 0265. Supply-Chain Time Modernization (chrono to jiff 1.0) and Advisory Governance

- **Status**: Accepted
- **Date**: 2026-11-29
- **Deciders**: Nuo Architecture Working Group
- **Related**: [ADR-0193](0193-pdf-reads-are-bounded-text-extraction-through-a-host-document-seam.md)

---

## Context and Problem Statement

Enabling `cargo deny check` as a hard CI supply-chain gate surfaced two RustSec advisories in `Cargo.lock`:

1. **`RUSTSEC-2026-0543` (`chrono 0.4.45`, unmaintained)**: Direct workspace dependency across 11 in-house crates (`acp`, `engine`, `model-routing`, `nuo`, `protocol`, `provider-google-antigravity`, `provider-opencode-go`, `server`, `tool-web`, `ui-primitives`, `view`), with zero external transitive consumers.
2. **`RUSTSEC-2026-0192` (`ttf-parser 0.25.1`, unmaintained)**: Transitive dependency pulled solely through `host -> pdf-extract 0.12.1 -> lopdf 0.42.0 -> ttf-parser 0.25.1` (pure-Rust font glyph metric parser with no known vulnerability).

Silently suppressing `chrono` in `deny.toml` when zero third-party dependencies require it would carry an unmaintained direct dependency into production indefinitely, while `jiff 1.0` (the RustSec-recommended successor) also eliminates Unix `localtime_r` environment-variable unsafety on multi-threaded servers.

---

## Decision Drivers

- **Zero Unmaintained Direct Dependencies (`[INV-SUPPLY-01]`)**: Every direct workspace dependency must be actively maintained; unmaintained crates with clean replacements are migrated rather than waived.
- **Sound Multi-Threaded Local Time**: Server and TUI processes are multi-threaded; local timezone resolution must not rely on unsafe `localtime_r` / `TZ` environment reads.
- **Audited, Minimal Transitive Waivers**: Any `deny.toml` advisory ignore must apply strictly to an unavoidable transitive non-CVE notice, name the exact dependency edge, and document its exit condition.
- **Lean Crate Dependency Graphs**: Crates that only compute epoch milliseconds (`model-routing`) should use `std::time::SystemTime` rather than linking any datetime crate.

---

## Architectural Decisions

### 1. `[INV-SUPPLY-01]` Replace `chrono` with `jiff 1.0` across the workspace

- Remove `chrono` from `[workspace.dependencies]` and all 11 workspace crate manifests, eradicating `chrono` from `Cargo.lock`.
- Add `jiff = { version = "1.0", features = ["serde"] }` to `[workspace.dependencies]`:
  - `acp`: replace `chrono::DateTime<Utc>` with `jiff::Timestamp` on `ChannelMessage`, `Subscription`, `AgentEnvelope`, `PendingRequest`, and `AgentPresence` (wire-compatible RFC 3339 serde serialization).
  - `protocol`, `ui-primitives`, `nuo`, `server`, `engine`, `view`, `tool-web`, `provider-google-antigravity`, `provider-opencode-go`: migrate RFC 3339 parsing, UTC formatting, and local timezone formatting to `jiff::Timestamp`, `jiff::ZonedDateTime`, and `jiff::tz::TimeZone::system()`.
  - `model-routing`: remove the datetime crate dependency altogether and compute epoch milliseconds directly from `std::time::SystemTime`.

### 2. Bounded transitive waiver for `RUSTSEC-2026-0192` (`ttf-parser`)

- Record one audited ignore entry in `deny.toml` for `RUSTSEC-2026-0192`, strictly scoped to the transitive `host -> pdf-extract 0.12.1 -> lopdf 0.42.0 -> ttf-parser 0.25.1` edge.
- PDF parsing remains isolated behind the `host::document::DocumentExtractor` capability seam ([ADR-0193](0193-pdf-reads-are-bounded-text-extraction-through-a-host-document-seam.md)); this waiver is to be removed as soon as `pdf-extract` upgrades `lopdf` off `ttf-parser`.

---

## Consequences & Trade-offs

- **Positive**: `chrono` and `RUSTSEC-2026-0543` are completely eliminated from `Cargo.lock`.
- **Positive**: Local timezone rendering (`day_key_from_epoch_ms`, `sent_time_label`, log rotation, session timestamps) uses pure-Rust tzdb resolution without `localtime_r` data-race hazards.
- **Positive**: `model-routing` drops one external dependency edge.
- **Positive**: `cargo deny check` passes all four gates (`advisories`, `bans`, `licenses`, `sources`).
- **Negative & accepted**: Public Rust types in `acp` (`ChannelMessage.at`, `AgentEnvelope.created_at`, `AgentPresence.*`) change from `chrono::DateTime<Utc>` to `jiff::Timestamp` (a clean internal/API break; JSON wire format remains RFC 3339).

---

## Discarded Alternatives (Negative Knowledge)

- **Waive `RUSTSEC-2026-0543` in `deny.toml` to keep `chrono`.** Rejected: `chrono` had only 16 small call sites across in-house crates and zero external transitive dependents; waiving an unmaintained direct dependency would normalize supply-chain rot on day one of the `cargo-deny` gate.
- **Migrate to `time` v0.3 instead of `jiff 1.0`.** Rejected: `time` refuses local-offset lookup in multi-threaded Unix processes unless `unsound_local_offset` is enabled, which conflicts with Nuo's multi-threaded Tokio server and strict safety posture (`unsafe_code = "deny"`).
- **Drop PDF extraction or replace `pdf-extract` pre-release to avoid `RUSTSEC-2026-0192`.** Rejected: `ttf-parser` has no security vulnerability (unmaintained notice only) and is already isolated behind `DocumentExtractor` (ADR-0193); swapping PDF parsers right before production release introduces real functional regression risk for zero security gain.
