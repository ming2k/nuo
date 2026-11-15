# nuo-persistence

Durable storage, SQLite persistence, configuration, and memory subsystems for Nuo.

## Overview

`nuo-persistence` owns all durable state required by the Nuo daemon and frontends. It provides an asynchronous, single-writer SQLite actor engine, transaction management, migration pipelines, and memory retrieval tools.

## Subsystems

- **`db`**: Supervised single-writer SQLite persistence actor operating in WAL mode. Manages transactional session persistence, delta streaming, and execution fact graphs.
- **`role_memory`**: Long-term conversational and philosophical memory index powered by SQLite FTS5 and the Ebbinghaus forgetting curve.
- **`tools`**: Exports the canonical `recall_memory` tool conforming to `nuo-tool::Tool` for memory retrieval.
- **`config` & `config_check`**: TOML configuration parsing, environment overrides, and schema validation.
- **`connections`**: Secure credential and token storage (`auth.toml`) for model providers and OAuth integrations.
- **`blobs`**: Content-addressable file and artifact storage with mark-and-sweep garbage collection.
- **`usage_stats`**: Day-partitioned usage and token consumption telemetry.
