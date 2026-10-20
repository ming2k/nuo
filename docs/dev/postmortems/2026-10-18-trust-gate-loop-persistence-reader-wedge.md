# 2026-10-18 — Trust Gate Confirm Loop: Persistence Reader Wedge

## 1. Executive Summary

- **Impact**: One operator. For ~8 minutes (and every re-attach for the rest
  of the daemon's life), pressing Enter on the `[file asset trust]`
  pre-attach gate silently failed to persist the trust grant; the gate
  re-presented the same dialog on every round-trip. All other daemon
  functionality (sessions, chat, tool runs) was unaffected.
- **Data integrity**: Unaffected. The persisted trust records (written
  before the wedge) remained intact; grants attempted *during* the wedge
  failed closed and left no partial state.
- **Duration**: First manifestation 2026-10-18T02:49:54Z (UTC); no
  self-recovery for the remainder of the daemon process.

## 2. Timeline of Events (UTC)

- **T_impact — 2026-10-18T02:49:54Z**: Immediately after a TUI session end
  (`should_quit_flag` → session teardown), every fresh workspace-security
  read in the daemon began failing:
  `cannot open sqlite db '~/.local/share/nuo/nuo.db': disk I/O error`.
- **T_detect — 2026-10-18T~03:05Z**: Operator report: "Enter to confirm not
  work anymore, still loop the same scene."
- **T_mitigate — 2026-10-18T~03:10Z**: Root cause identified from the
  daemon's open file descriptors: `nuo.db-wal (deleted)` and
  `nuo.db-shm (deleted)` — the SQLite WAL sidecar pair had been unlinked
  under the live daemon. Prescribed mitigation: graceful daemon restart
  (clean close checkpoints the still-open unlinked WAL into the main
  database; `kill -9` would strand it).
- **T_resolve**: Hardening merged (see §5): the wedge now self-heals in
  process, and the TUI renders the attestation outcome instead of looping.

## 3. Detection Gaps

- **No monitor owns the reader path.** The supervisor's writer health
  (`WriterHealth`) stayed `Healthy` throughout: the writer kept serving into
  the unlinked inode. Only *fresh read-only opens* failed, and each failure
  produced nothing but a per-call `WARN` — no aggregate signal, no health
  transition, no respawn.
- **The TUI buried the failure.** The server's persist-failure path *does*
  emit `AgentNotice::trust_changed("Workspace trust failed: …")`, but the
  full-screen PreAttach interstitial hid the transcript where that notice
  lands, and the surface's submitting spinner silently refreshed the
  question. From the operator's seat the loop had no cause attached.
- **No test modeled the wedge.** Nothing exercised "on-disk WAL removed
  under a live writer" — a state unit tests cannot reach through the public
  surface, because every constructor opens its own healthy database.

## 4. Trigger & Underlying Root Cause

- **Trigger**: The on-disk `nuo.db-wal`/`nuo.db-shm` files were unlinked at
  02:49:54Z while the daemon held them open. The external actor is not
  identifiable from available telemetry (no sidecar audit trail exists) —
  recorded blamelessly: the system gap is that *nothing watches the
  sidecar's presence*, not whichever process removed it.
- **Underlying root cause (state-machine failure)**: SQLite read-only opens
  of a WAL-mode database must attach the WAL index (`-shm`); against the
  deleted pair the attach fails (`disk I/O error` at the
  `PRAGMA user_version` probe inside `PersistenceHandle::reader()`), while
  the live writer connection — holding the deleted inodes by fd — continues
  unimpeded. The workspace-security store fails closed on unreadable state
  (correct), the snapshot therefore always reported `Quarantined`
  (correct), and the gate re-mounted on every republish (correct per
) — the *composition* of three individually-correct policies
  produced an inescapable loop with zero user-visible cause.
- **Why it could persist indefinitely**: the failure lived entirely in the
  reader-construction path, which sits outside the supervised actor whose
respawn machinery is the system's only recovery mechanism.

## 5. Action Items & Defense-in-Depth

| # | Action | Kind | Owner | Verifiable completion |
|---|--------|------|-------|-----------------------|
| P1 | `PersistenceCommand::RebuildEngine` + `RetireEngine` handshake: the supervisor retires the live engine — an explicit `wal_checkpoint(FULL)` drains the WAL *while the engine is still functional* (close-time checkpointing is a heuristic that can be skipped or deferred by close-v2 teardown), the engine then closes cleanly, and only afterwards does the fresh generation open. A concurrent fresh open against a live wedged engine is itself a disk-I/O failure, which is why the handshake exists. | Preventative (mechanism) | ming | `supervision::reader_wedge_heals_through_engine_rebuild` passes |
| P2 | Reader-wedge detector in `PersistenceHandle::reader()`: every failed construction sequence counts; at the 3-failure threshold exactly one rebuild is requested (CAS latch, cleared by the next successful open). Non-owner handles never request a rebuild. | Preventative (mechanism) | ming | Threshold + latch asserted in the wedge test |
| D1 | Regression test reproducing the wedge: live writer, WAL-only frames, sidecars unlinked, reader opens fail, rebuild heals, pre-wedge write recovered. | Detection | ming | `db::tests::supervision::reader_wedge_heals_through_engine_rebuild` |
| D2 | Gate lifecycle tests: failed attestation renders on the gate and resumes the decision; retry re-dispatches the full selection; trust notices without a mounted gate stay on their normal flow; the trusted snapshot still unmounts. | Detection | ming | `tests::pre_attach_gate::*` |
