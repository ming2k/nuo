# RFC-0006: Observable Migration Progress and Honest Server Readiness

- **Status:** Accepted — implemented; ratified by
  [ADR-0266](../adr/0266-observable-migration-progress-watch-only-lifecycle-and-honest-readiness.md)
- **Revision:** Final
- **Date:** 2026-11-30
- **Related:** [ADR-0189](../adr/0189-immutable-baseline-plus-numbered-migrations.md),
  [ADR-0190](../adr/0190-schema-integrity-is-verified-structurally-at-open.md),
  [ADR-0191](../adr/0191-persistence-failure-is-fail-fast-without-a-database-cli.md),
  [ADR-0255](../adr/0255-versioned-content-shards-pure-catalog-plane-and-durability-boundaries.md),
  [ADR-0260](../adr/0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md)
- **Non-Goals (explicitly excluded by Occam's Razor / KISS):**
  - No `ServerWork` / background maintenance wire protocol (daily storage maintenance stays in logs).
  - No eager `role_memory.db` open during global startup (remains lazy until a role uses it).
  - No orthogonal `project_root` / `LinkState` refactors bundled into this cut.

---

## 0. Recommendation (TL;DR)

Apply Occam's Razor to [ADR-0260](../adr/0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md)'s
listen-first server lifecycle:

1. **Enforce fail-fast storage startup**: `ThreadRegistry::initialize_storage` calls
   `nuo_persistence::db::init_global_persistence`, enforcing `PersistenceHandle::ensure_ready()` so
   database open or migration failures terminate immediately in `ServerPhase::Failed` rather than
   promoting to `ServerPhase::Ready`.
2. **Surface real migration work with zero fast-path overhead**: `ServerPhase::Initializing` gains
   optional `detail: Option<StorageInitDetail>` (`BackingUp`, `Migrating { step, total, name }`,
   `Verifying`) and `started_at_ms: Option<u64>`, populated exclusively when `apply_pending()` runs
   migrations. Up-to-date boots emit zero extra events and stay silent under the 120 ms hysteresis.
3. **Delete split-brain lifecycle plumbing**: Deliver `ServerPhaseChanged` exclusively through
   `ServerLifecycle`'s `watch` channel (removing duplicate `MonitorBus` broadcast and `last_sent_phase`
   dedup); share one `ShutdownGate` directly with `start_server` (deleting `nuo_host::handle_gate()`);
   replace `sleep(50ms)` startup-failure waits with deterministic connection drain
   (`ConnTable::wait_empty_or_cancel`); and move whole-server `catch_up_projections_once()` out of
   first-thread `assemble_with_store` to post-`Ready` background execution.
4. **Make every readiness surface tell the truth**: Unify client `Initializing` wait loops into one
   progress-aware watchdog (`nuo::transport::ready`), make HTTP `/readyz` return `503` when
   `PersistenceHealth::Down`, and make `nuo status --diagnostic` issue a bounded `Wire::Hello` probe
   before claiming the server is healthy.

---

## 1. Defects Resolved

| # | Defect | Evidence | Consequence |
|---|---|---|---|
| D1 | Silent storage failure promotion | `initialize_storage()` discarded `get_persistence_handle()` without calling `ensure_ready()` | Corrupt or incompatible DB advanced to `ServerPhase::Ready` and failed later as `ThreadPhase::Failed`. |
| D2 | Opaque multi-second migrations | `apply_pending()` ran `VACUUM INTO` backup, N migrations, and `foreign_key_check`/`quick_check` behind static `"storage…"` | Upgrades looked hung and tripped fixed 10 s / 30 s client timeouts. |
| D3 | Server-wide catch-up trapped in first thread | `assemble_with_store` called `catch_up_projections_once().await` before `PrepareStage::Thread` | First user thread paid for server-wide shard repair under `"Preparing thread store…"`; Dashboard-only boot skipped it. |
| D4 | Duplicate phase delivery & gate forwarding | `set_server_phase` wrote both `watch` and `MonitorBus`; `nuo_host::handle_gate()` bridged two `ShutdownGate`s; startup failure used `sleep(50ms)` | Double wakeups, ad-hoc dedup, extra background task, and socket EOF races on startup failure. |
| D5 | Dishonest health probes | `/readyz` ignored `PersistenceHealth::Down`; `diagnose_server()` never sent `Hello` and used blocking I/O inside `async fn` | `nuo status --diagnostic` declared `"Server is running and healthy"` for `Initializing`, `Failed`, or `Down` servers. |
| D6 | Duplicated client init wait loops | `attach.rs` and `control.rs` duplicated `ServerPhase::Initializing` wait loops with blind wall-clock deadlines | Large legitimate `VACUUM INTO` backups aborted CLI commands prematurely. |

---

## 2. Architecture Summary

### 2.1 Wire Contract (`PROTOCOL_VERSION = 37`)

- `StorageInitDetail`:
  - `BackingUp { target_version: u32 }` — mandatory pre-migration `VACUUM INTO` snapshot ([ADR-0191](../adr/0191-persistence-failure-is-fail-fast-without-a-database-cli.md)).
  - `Migrating { step: u32, total: u32, name: String }` — 1-based step inside `apply_pending_transaction`.
  - `Verifying` — post-migration `foreign_key_check`, `quick_check`, and structural verification ([ADR-0190](../adr/0190-schema-integrity-is-verified-structurally-at-open.md)).
- `ServerPhase::Initializing { stage: ServerInitStage, detail: Option<StorageInitDetail>, started_at_ms: Option<u64> }`.

### 2.2 Persistence Observer Seam

`nuo_persistence::db` exposes `MigrationProgress` and `MigrationObserver = Arc<dyn Fn(MigrationProgress) + Send + Sync>`
with zero dependency on `nuo-protocol` or `nuo-server`. `init_global_persistence(observer)` spawns the
supervised writer with the observer and enforces `handle.ensure_ready()?`.

### 2.3 Server Lifecycle Simplification

- `ThreadRegistry::set_server_phase` writes exclusively to `ServerLifecycle`'s `watch::Sender`.
- `ServeOptions::gate` accepts the host's `Arc<ShutdownGate>` directly, retiring `nuo_host::handle_gate()`.
- On startup failure, `run_session_loop` flushes `ServerPhaseChanged(Failed)` and closes cleanly;
  `nuo_host::run_inner` releases discovery and awaits `ConnTable::wait_empty_or_cancel(250ms)` instead of
  sleeping 50 ms.
- `nuo_host::run_inner` spawns `nuo_persistence::thread::catch_up_projections_once()` asynchronously right
  after `ServerPhase::Ready`; `bootstrap::assemble_with_store` removes both that call and its
  duplicated `ensure_app_roots` loop.

### 2.4 Client Watchdog, Honest Diagnostics & TUI Projection

- `nuo::transport::ready::await_server_ready` replaces both wait loops in `attach.rs` and
  `control.rs`, resetting its stall budget (`60s` normal / `120s` heavy I/O, `5min` hard cap) on
  every `ServerPhaseChanged` and printing single-line stderr progress after 300 ms.
- `diagnose_server()` uses pure async I/O and performs one bounded 500 ms `Wire::Hello` probe to
  capture `live_phase` and `live_persistence_health`.
- `nuo_view::empty_state::server_phase_status` renders exact migration steps and `(Ns)` elapsed once
  `>= 2s`. `apply_server_phase` preserves `server_phase_since` across sub-step transitions inside
  `ServerPhase::Initializing` so the 120 ms anti-flicker hysteresis never resets mid-boot.
