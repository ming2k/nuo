---
id: ADR-0266
title: "Observable Migration Progress, Watch-Only Lifecycle Delivery, and Honest Server Readiness"
status: accepted
date: 2026-11-30
scope: persistence/db, protocol/monitor, server/runtime, server/transport, presentation/view, host/nuo
superseded_by: null
amends:
  - "[ADR-0191](0191-persistence-failure-is-fail-fast-without-a-database-cli.md) (wires PersistenceHandle::ensure_ready into ServerInitStage::Storage so open/migration errors fail fast in ServerPhase::Failed)"
  - "[ADR-0255](0255-versioned-content-shards-pure-catalog-plane-and-durability-boundaries.md) (moves whole-server projection catch-up out of first-thread bootstrap to post-Ready background execution)"
  - "[ADR-0260](0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md) (adds optional StorageInitDetail on Initializing, enforces watch-only phase delivery, retires dual gate forwarding and 50ms failure sleep, and aligns /readyz and diagnostics)"
negative_knowledge: true
---

# 0266. Observable Migration Progress, Watch-Only Lifecycle Delivery, and Honest Server Readiness

- **Status**: Accepted
- **Date**: 2026-11-30
- **Deciders**: Nuo Architecture Working Group
- **RFC**: [RFC-0006](../rfc/0006-observable-migration-progress-and-honest-server-readiness.md)
- **Amends**: [ADR-0191](0191-persistence-failure-is-fail-fast-without-a-database-cli.md), [ADR-0255](0255-versioned-content-shards-pure-catalog-plane-and-durability-boundaries.md), [ADR-0260](0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md)
- **Related**: [ADR-0189](0189-immutable-baseline-plus-numbered-migrations.md), [ADR-0190](0190-schema-integrity-is-verified-structurally-at-open.md), [ADR-0232](0232-a-classifier-is-only-closed-if-its-members-have-producers.md)

---

## Context and Problem Statement

[ADR-0260](0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md) established Listen-First startup and a typed `ServerPhase`, yet auditing the full startup-to-readiness path exposed five concrete production gaps:

1. **Silent Storage Failure Promotion**: `ThreadRegistry::initialize_storage` called `let _ = persistence::db::get_persistence_handle();` without invoking `ensure_ready()`. Because `PersistenceHandle::spawn` records open/migration errors in `startup_error` rather than panicking, a corrupt or incompatible database still transitioned the server to `ServerPhase::Ready`, deferring the error until the first thread entered `Assembler::new` and surfacing it as `ThreadPhase::Failed`.
2. **Opaque Heavy Migrations**: On a schema upgrade, `apply_pending()` runs a mandatory pre-migration `VACUUM INTO` backup ([ADR-0191](0191-persistence-failure-is-fail-fast-without-a-database-cli.md)), N numbered migrations, and post-migration `foreign_key_check` / `quick_check` / `sqlite_master` verification behind a static `Initializing(Storage)` label. On large databases this looked like a hang and tripped fixed 10 s / 30 s client timeouts in `attach.rs` and `control.rs`.
3. **Mislocated Whole-Server Catch-Up**: `persistence::thread::catch_up_projections_once().await` ran inside the first thread's `assemble_with_store`, disguising server-wide shard replay as single-thread assembly while skipping it completely on Dashboard-only launches.
4. **Split Lifecycle Plumbing**: `set_server_phase` published `ServerPhaseChanged` onto both `ServerLifecycle`'s `watch` channel and the lossy `MonitorBus` broadcast channel (forcing `last_sent_phase` deduplication in `serve/connection.rs`); `start_server` constructed a second `ShutdownGate` bridged via a background `handle_gate()` task; and startup failures slept for a blind `50ms` hoping connected peers read `Failed` before cancellation.
5. **Dishonest Readiness Probes**: HTTP `/readyz` returned `200 OK` even when `PersistenceHealth::Down`; `nuo status --diagnostic` never sent `Wire::Hello` (and performed blocking I/O inside `async fn`), reporting `"Server is running and healthy"` for `Initializing`, `Failed`, or degraded servers.

Applying Occam's Razor and KISS principles, we break clean at `PROTOCOL_VERSION = 37` to add minimal, zero-fast-path-overhead migration progress while deleting every redundant plumbing layer.

---

## Architectural Decisions

### 1. Minimal Closed `StorageInitDetail` on `ServerPhase::Initializing`

We extend `ServerPhase::Initializing` with two optional fields:

- `detail: Option<StorageInitDetail>` (`BackingUp { target_version }`, `Migrating { step, total, name }`, `Verifying`)
- `started_at_ms: Option<u64>`

`detail` is populated **exclusively** inside `persistence::db::migrations::runner::apply_pending` when pending migrations actually execute, bridged via a zero-dependency `MigrationObserver` callback. Normal up-to-date boots leave `detail == None` and emit zero extra events.

### 2. Fail-Fast Storage Gate & Post-`Ready` Background Catch-Up

- `persistence::db::init_global_persistence(observer)` enforces `PersistenceHandle::ensure_ready()` and is called by `ThreadRegistry::initialize_storage`. Any open, backup, migration, or verification failure terminates startup in `ServerPhase::Failed { code: Some("storage_init_failed"), message }` and never advances to `Ready`.
- `persistence::thread::catch_up_projections_once()` moves out of `bootstrap::assemble_with_store` to an asynchronous background spawn immediately after `ServerPhase::Ready` in `host::run_inner` (single-thread opens during catch-up self-heal on demand via [ADR-0255](0255-versioned-content-shards-pure-catalog-plane-and-durability-boundaries.md)'s `catch_up_single_session`). The duplicated `ensure_app_roots` loop in `Assembler::new` is deleted.

### 3. Watch-Only Phase Delivery, Single Gate, and Deterministic Failure Flush

- `ThreadRegistry::set_server_phase` updates `ServerLifecycle`'s `watch` channel only; `ServerPhaseChanged` is no longer broadcast onto `MonitorBus`, and `serve/connection.rs` deletes its duplicate branch and `last_sent_phase` deduplication.
- `host::run_inner` passes its `Arc<ShutdownGate>` directly in `ServeOptions::gate`, deleting `host::handle_gate()`.
- When `phase_rx` delivers `ServerPhase::Failed`, `run_session_loop` flushes `ServerPhaseChanged(Failed)` and closes cleanly; `host::run_inner` releases discovery and calls `ConnTable::wait_empty_or_cancel(250ms)` instead of `sleep(50ms)`.

### 4. Progress-Aware Client Watchdog, Hysteresis Preservation, and Honest Probes

- `nuo::transport::ready::await_server_ready` replaces the duplicated `Initializing` wait loops in `attach.rs` and `control.rs`. Each `ServerPhaseChanged` resets the stall timer (`60s` normal, `120s` while `detail.is_some()`, `5min` hard cap), and headless CLI surfaces concise stderr progress only when initialization exceeds 300 ms.
- `apply_server_phase` preserves `app.session.server_phase_since` across sub-step transitions inside `ServerPhase::Initializing` so rapid steps never reset the 120 ms anti-flicker hysteresis.
- HTTP `/readyz` returns `503` whenever `!phase.admits_work()` or `PersistenceHealth::Down`, and `/healthz` + `/readyz` both include `persistence_health` and `elapsed_ms`.
- `diagnose_server()` runs purely async I/O and issues one bounded 500 ms `Wire::Hello` probe so `nuo status --diagnostic` surfaces `Initializing`, `Draining`, `Failed`, or degraded persistence instead of falsely declaring `"running and healthy"`.

### 5. Invariants

- **`[INV-SRVPHASE-06]` Fail-Fast Storage Startup Gate**: `ThreadRegistry::initialize_storage` MUST enforce `PersistenceHandle::ensure_ready()` and terminate startup in `ServerPhase::Failed` on any database open, backup, migration, or verification failure, flushing connected peers via `ConnTable::wait_empty_or_cancel` without advancing to `ServerPhase::Ready`.
- **`[INV-SRVPHASE-07]` Watch-Only Phase Delivery and Zero Fast-Path Noise**: `ServerPhaseChanged` MUST stream exclusively from `ServerLifecycle`'s watch channel (never duplicated on `MonitorBus`). `StorageInitDetail` MUST be emitted only when pending migrations execute, and sub-step transitions within `ServerPhase::Initializing` MUST NOT reset the client's 120 ms display hysteresis timer.
- **`[INV-SRVPHASE-08]` Honest Readiness Across All Probes**: HTTP `/readyz` MUST return `503` when `ServerPhase` does not admit work or `PersistenceHealth` is `Down`; `nuo status --diagnostic` MUST reflect live `Wire::Hello` phase and durability state rather than inferring health from socket reachability alone.

---

## Consequences & Trade-offs

- **Positive**: Database upgrades give immediate, calm progress across TUI, CLI, `nuo status`, and `/readyz`; corrupt databases fail fast at the server boundary; normal warm boots remain zero-overhead and flicker-free; duplicate phase delivery, gate forwarding, sleep races, and duplicated client wait loops are eliminated.
- **Negative & Incurred Costs**: Clean wire break advancing `PROTOCOL_VERSION` from `36` to `37` with a renewed wire-surface census; clients and servers must match protocol `37`.

---

## Negative Knowledge & Rejected Alternatives

### Rejected Alternative A: Adding a Wire-Level `ServerWork` Subsystem for Background Maintenance and Shard Catch-Up
Introducing a third watch channel, `MonitorEvent::ServerWorkChanged`, a 5-variant `MaintenanceStage` enum, and a new TUI status-bar slot for daily 24-hour storage maintenance and sub-second shard catch-up was rejected under Occam's Razor. Daily maintenance belongs in structured logs, and moving `catch_up_projections_once()` to post-`Ready` background execution solves the mislocation bug in 10 lines with zero wire surface expansion.

### Rejected Alternative B: Fine-Grained Sub-Millisecond Init Variants (`AcquiringLock`, `OpeningDatabase`, `LoadingRuntimeState`)
Emitting wire variants for operations that complete in 1–2 ms (below the 120 ms hysteresis floor) or duplicate `ServerInitStage::Runtime` adds protocol noise without user benefit. Restricting `StorageInitDetail` to the three genuinely disk-proportional migration steps (`BackingUp`, `Migrating`, `Verifying`) keeps the fast path allocation-free and every variant empirically meaningful.

### Rejected Alternative C: Bundling Orthogonal `project_root` and `LinkState` Cleanups into This Cut
Removing legacy `project_root` fields and `LinkState::{link_down, relink_requested}` spans 30+ unrelated call sites and tests; keeping them out of this ADR preserves review clarity and narrow blast radius.
