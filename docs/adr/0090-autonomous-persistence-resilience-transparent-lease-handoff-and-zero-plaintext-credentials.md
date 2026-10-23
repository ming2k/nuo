---
id: ADR-0090
title: "Autonomous Persistence Resilience, Transparent Lease Handoff, and Zero-Plaintext Credentials"
status: accepted
date: 2026-11-26
scope: persistence/nuo-persistence, security/credentials, server/nuo-server, architecture/storage
superseded_by: null
negative_knowledge: true
---

# 0090. Autonomous Persistence Resilience, Transparent Lease Handoff, and Zero-Plaintext Credentials

- Status: Accepted
- Date: 2026-11-26
- Deciders: Nuo Architecture Working Group & Core Runtime Team
- Consulted: Persistence, Security, Server, and Storage Governance
- Amends: [ADR-0032](0032-relocation-of-credentials-to-state-store-and-dotfiles-secret-leaks-prevention.md) (advances static credential protection to OS Keyring primacy)
- Complements: [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md) (extends canonical thread persistence with autonomous disaster recovery)
- Complements: [ADR-0045](0045-dynamic-port-sharing-palette-orthogonal-control-plane-and-server-lifecycle-handoff.md) (unifies control-plane handoff with persistence lease transparent attach)

---

## Context and Problem Statement

Following the ratification of Canonical Thread Persistence Schema and Zero-Legacy Clean Break ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)) and the relocation of credentials to `$XDG_STATE_HOME/nuo` ([ADR-0032](0032-relocation-of-credentials-to-state-store-and-dotfiles-secret-leaks-prevention.md)), the SQLite storage substrate in `nuo-persistence` successfully eliminated schema drift and prevented dotfiles Git leaks.

However, real-world deployment across multi-terminal developer workstations, ephemeral CI jobs, and hardened enterprise environments exposed four remaining structural vulnerabilities:

1. **Brittle Owner-Lease Contention**:  
   To prevent SQLite busy deadlocks, `PersistenceHandle` enforces a single-writer lease via `nuo.db.owner.lock`. When a second terminal process launched without connecting to an existing daemon, a hardcoded 2000ms timeout caused abrupt process termination with `failed to acquire database owner lease`, rather than smoothly handshaking or attaching to the active daemon.
2. **At-Rest Plaintext Exposure**:  
   While `credentials.toml` and `auth.toml` were restricted to mode `0600`, secrets remained serialized as plaintext TOML. Any unprivileged process running under the same user UID could read bearer tokens without security gate approval.
3. **Fragmented Disaster Recovery**:  
   Although SQLite online backup (`VACUUM INTO`) was supported at the low-level connection tier, there was no autonomous rolling backup lifecycle, no automatic retention pruning, and no non-destructive pre-flight restore validation.
4. **Daemon-Coupled Storage Reclamation**:  
   Garbage collection for Content-Addressed Storage (CAS) Blobs and zero-reference transcript entries was strictly bound to `nuo-server`'s periodic ticker (`STORAGE_MAINTENANCE_INTERVAL`). Ephemeral CLI runs accumulated unreferenced Blobs indefinitely without deterministic cleanup.

---

## Decision Drivers

- **Driver 1: Zero Unmanaged Storage Growth**: Point-in-time snapshots, WAL write logs, and CAS Blobs must be autonomously bounded and pruned under strict retention quotas.
- **Driver 2: Transparent Multi-Process Cohesion**: Concurrent commands must never crash with raw lock timeout errors; processes must transparently delegate to the primary daemon or coordinate gracefully.
- **Driver 3: Hardware-Rooted Secret Protection**: API keys and OAuth tokens must be managed with OS Keyring primacy, retiring reliance on plaintext filesystem files.
- **Driver 4: Non-Destructive Disaster Recovery**: Restoring any snapshot must unconditionally verify database integrity first and preserve pre-restoration state in an emergency sentinel file.
- **Driver 5: Zero Legacy Burden**: Eliminate historical workarounds without preserving deprecated aliases or backward-compatibility shims.

---

## Architectural Decisions

```text
┌────────────────────────────────────────────────────────────────────────┐
│                          nuo (CLI / TUI)                               │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                        Lease Coordinator & IPC                         │
│  - Try acquire db.owner.lock (2s lease)                                │
│  - If busy: Transparently degrade to IPC Client Attach ([INV-LOCK-01]) │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │ Owner                          │ Client Attach
                    ▼                                ▼
┌──────────────────────────────────────┐  ┌──────────────────────────────┐
│  nuo-persistence (Single Writer)    │  │  nuo-server (Active Daemon)  │
│  ┌────────────────────────────────┐  │  └──────────────────────────────┘
│  │   BackupManager Subsystem      │  │
│  │   - VACUUM INTO snapshots      │  │
│  │   - Rolling retention pruning  │  │
│  │   - Pre-flight verify & rescue │  │
│  └────────────────────────────────┘  │
│  ┌────────────────────────────────┐  │
│  │   Keyring & Credentials        │  │
│  │   - 12-factor ENV (1st)        │  │
│  │   - OS Keyring (2nd)           │  │
│  │   - 0600 state file (Fallback) │  │
│  └────────────────────────────────┘  │
└──────────────────────────────────────┘
```

### 1. Autonomous Rolling Backup Manager (`nuo_persistence::backup`)

A dedicated `BackupManager` provides deterministic snapshot lifecycles:
- **Online Hot-Backup**: Generates standalone, defragmented snapshots via SQLite `VACUUM INTO` under `nuo_backup_{timestamp_ms}_{uuid_short}.db`.
- **Rolling Retention Pruning**: Enforces `DEFAULT_BACKUP_RETENTION_LIMIT` (default 7 snapshots). When a fresh snapshot is generated, older backups exceeding the quota are automatically pruned.
- **Non-Destructive Atomic Restore**:
  1. Executes `verify_backup` (checks `PRAGMA quick_check == 'ok'` and validates `PRAGMA user_version == 24`).
  2. Preserves active state into `<target_db>.pre_restore.bak` before any mutation.
  3. Evicts orphaned `.db-wal` and `.db-shm` sidecars and atomically replaces target state via temporary atomic file rename.

### 2. Transparent Multi-Process Lease Handoff

When a CLI instance fails to acquire `db.owner.lock` within the timeout window:
- It must inspect the daemon identity registry (`Discovery::load()`).
- If an active daemon holds the lease, the secondary instance automatically falls back to **IPC Attach Mode**, routing operations over domain sockets rather than crashing.
- Non-owner instances are strictly prohibited from attempting direct SQLite writes or engine reconstructions.

### 3. Platform Keyring Primacy & Plaintext Retirement

Credential resolution transitions to a three-tier priority hierarchy:
1. **Tier 1 (Explicit Invocations)**: 12-factor environment variables (`api_key_env`).
2. **Tier 2 (Interactive Workstations)**: OS native secure credential store (macOS Keychain, Linux Secret Service / Keyring, Windows Credential Manager).
3. **Tier 3 (Headless / Containerized Fallback)**: Obfuscated `$XDG_STATE_HOME/nuo/credentials.toml` restricted to mode `0600`.

**Implementation status (as shipped in 0.0.47): Tier 1 and Tier 3 only.** Tiers 1 and 3 are live:
`nuo-provider::credentials` resolves the environment first and persists an owner-only (`0600`) state file, and
`nuo_host::SecretString` keeps the material out of `Debug`/`Serialize` paths. **Tier 2 is not yet implemented** —
no keyring backend is linked and `credential_store_is_keyring_pristine` does not exist. `[INV-PERSIST-SECRET-01]`
is therefore recorded as a *ratified target*, not an enforced invariant: it carries no verification mechanism today,
so under the admissibility rules in `docs/dev/standards/invariants.md` §3 it may not be claimed as active. The
mechanism that will make it enforceable is a `keyring`-backed tier selected before the state-file fallback, plus a
test proving that a resolvable keyring entry is never written to disk. Until that lands, the operator-facing
guarantee is file permissions and directory isolation, not hardware-backed storage.

### 4. Deterministic Storage Maintenance Lifecycle

Storage maintenance is decoupled from the long-lived daemon loop:
- `BackupManager` provides synchronous and asynchronous programmatic entrypoints (`create_rolling_backup`, `prune_excess_backups`, `restore_backup`).
- Ephemeral CLI sessions register a process-exit hook to purge unreferenced CAS Blobs and execute `PRAGMA optimize` when orphan thresholds are crossed.

---

## Normative System Invariants Ratified

- **[INV-PERSIST-BACKUP-01] Atomic Hot-Backup via VACUUM INTO**: Under no circumstances shall an external file copy (`cp`, `fs::copy`) be performed on a live WAL-mode SQLite database. All backups MUST be created via SQLite's atomic `VACUUM INTO` command.
- **[INV-PERSIST-BACKUP-02] Non-Destructive Restore with Pre-Flight Sentinel**: A backup restore MUST pass structural integrity checks (`PRAGMA quick_check`) prior to disk writes and MUST persist existing state into `<target_db>.pre_restore.bak`.
- **[INV-PERSIST-LOCK-01] Transparent Multi-Process Cohesion**: Components attempting direct storage access MUST NOT fail silently or panic upon lock collision; they must fall back to IPC proxying or emit structured actionable diagnostics.
- **[INV-PERSIST-SECRET-01] OS Keyring Primacy** *(target — not yet enforced; see §3 implementation status)*: Persistent storage of credentials must prioritize system-level secure credential vaults over unencrypted on-disk TOML files. **Mechanism pending**: no keyring backend is linked in 0.0.47, so this clause is a design commitment rather than an active invariant.
- **[INV-PERSIST-GC-01] Ledger-Governed Blob Reclamation**: CAS Blob deletion MUST be governed strictly by membership in the `blob_refs` relational ledger. Directory scanning heuristics without ledger confirmation are prohibited.

---

## Consequences

### Positive Consequences
- **Zero Data Loss on Hardware Glitches**: Developers can recover corrupted databases using verified rolling hot backups.
- **Elimination of Lock Errors**: Eliminates terminal crashes when launching concurrent CLI sessions in multiple terminal tabs.
- **Enhanced Enterprise Security**: API keys and OAuth tokens are protected behind OS-level hardware security chips (TPM / Secure Enclave).
- **Disk Cleanliness**: Automatic prune loops keep snapshot directories, WAL journals, and CAS Blob stores compact without manual operator maintenance.

### Negative Consequences & Mitigations
- *Disk Quota for Backups*: Storing 7 snapshots increases disk usage by several megabytes.  
  *Mitigation*: Backups are defragmented single-file SQLite databases with no WAL overhead, and `retention_limit` is fully configurable.
- *OS Keyring Availability*: Headless CI environments may lack a D-Bus / SecretService daemon.  
  *Mitigation*: Seamless zero-config fallback to Tier 3 mode `0600` local state files.

---

## Negative Knowledge & Discarded Alternatives

- **Discarded: Live Encryption via SQLCipher on Writer Hot-Path**:  
  Introducing SQLCipher on every ReAct-turn hot path added significant CPU overhead and complicated compilation matrices. We decided to keep SQLite pure and apply at-rest protection at the system keyring tier.
- **Discarded: Multi-Writer SQLite Configuration**:  
  Permitting multiple local OS processes to write concurrently to WAL mode triggered unresolvable `SQLITE_BUSY` spikes and live-lock starvation under parallel agent ReAct loops. The supervised single-writer actor model remains the mandatory invariant.
- **Discarded: Plain cp / rsync Database Snapshots**:  
  Capturing `nuo.db` while transactions are committed into `-wal` produces split-brain corrupted backups. `VACUUM INTO` is the only safe mechanism.
