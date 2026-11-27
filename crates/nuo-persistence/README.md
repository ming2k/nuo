# nuo-persistence

Durable storage, per-session journals, SQLite projections, configuration, and memory subsystems for Nuo.

## Overview

`nuo-persistence` owns all durable state required by the Nuo daemon and frontends. Conversation state is written journal-first to per-session append-only logs (ADR-0236), and projected asynchronously into a pure catalog database (`nuo.db`) plus fixed-`K` versioned content shards (`shards/content-XX.db`, ADR-0242, ADR-0255).

## Subsystems

- **`journal`**: Per-session append-only, CRC32C-framed logs with group-commit `fdatasync` lanes and snapshot compaction. Sole durable source of truth for transcripts, branch DAGs, command acts, workspace effects, path aliases, and deletion tombstones.
- **`db`**: Supervised single-writer SQLite actor for `nuo.db` (authoritative local state + catalog projection), fixed-`K` versioned content shards (`shards/content-XX.db`), schema-agnostic migration/verification runner, reader pool, and online rolling `VACUUM INTO` backups.
- **`thread`**: `ThreadSession` single-transcript session facade with undo-on-append-failure, bounded fair projection pool, and startup catch-up.
- **`workspace_effects`**: Append-only filesystem mutation evidence supporting `/undo` and `/diff`, journal-first and projected into content shards.
- **`role_memory`**: Long-term role-scoped dialogue memory (`role_memory.db`) powered by SQLite FTS5 and the Ebbinghaus forgetting curve.
- **`config` & `config_check`**: Strict TOML domain configuration parsing, environment overrides, and schema validation.
- **`connections`**: Credential and token storage (`connections.toml`, `credentials.toml`, `auth.toml`) for model providers and OAuth integrations.
- **`blobs`**: Content-addressed artifact storage (`blobs/`) with ledger-governed, grace-windowed mark-and-sweep garbage collection.
- **`usage_stats`**: Attempt-keyed token usage ledger and recoverable contribution projections inside `nuo.db`.
