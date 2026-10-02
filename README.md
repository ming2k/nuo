# Nuo (诺)

Nuo is a high-performance AI session daemon, semantic terminal (`nuox`), and web host system powered by the **Agent Coordination Protocol (ACP)** and native cognitive agent architecture.

## Architecture

Nuo is structured as a self-contained, flat Cargo workspace consisting of three primary tiers:

### Protocols & Substrates
- **`acp`** — **Agent Coordination Protocol (ACP)**: Canonical inter-agent communication, collaborative channels, URI addressing (`agent://`, `acp://`), zero-trust HMAC signature envelopes, peer-to-peer delegation, and standard collaboration tools (`acp/`).
- **`nuo-agent`** — Autonomous cognitive loops, two-tier context hygiene, hybrid RRF memory, and claim-check token compaction (`nuo-agent/`).
- **`nuo-tool`** & **`nuo-tool-derive`** — Zero-runtime tool specification, dynamic scopes, risk profiles, and compile-time JSON schema derivation (`nuo-tool/`, `nuo-tool-derive/`).
- **`nuo-model-codec`** — Multi-vendor model API dialect translation, Session IR projection, and streaming SSE codecs (`nuo-model-codec/`).
- **`nuo-mcp`** — Native Model Context Protocol (MCP) client and server transport (`nuo-mcp/`).
- **`nuotc`** — High-performance retained-mode 2D terminal canvas, minimal escape-code diffing, and Flexbox layout engine (`nuotc/`).

### Applications & Frontends
- **`nuo`** — The session daemon host and management CLI (`nuo/`). Manages daemon lifecycle (`nuo start / stop / status / token`), hosts multi-client sessions, and serves HTTP / WebSocket / UDS control planes.
- **`nuox`** — Semantic terminal client and rich interactive TUI (`nuox/`). High-performance interactive harness and headless one-shot runner (`nuox run`).
- **`web`** — Responsive SvelteKit web interface for Nuo session monitoring, chat, and tool approval (`web/`).

### Subsystems & Library Crates
- **`nuo-client`** — Independent client communication SDK for frontends, tests, and CLI tools (`nuo-client/`).
- **`nuo-contracts`** — Pure domain contracts, types, events, and zero-I/O protocols (`nuo-contracts/`).
- **`nuo-harness`** — Host execution harness: permissions, sandboxing, tool scheduling, and causal context compaction (`nuo-harness/`).
- **`nuo-persistence`** — SQLite session store, migrations, configuration, credentials, role memory, and memory recall (`nuo-persistence/`).
- **`nuo-providers`** — Multi-vendor model catalog, OAuth authentication engine, and concrete provider implementations (`nuo-providers/`).
- **`nuo-host`** — Host execution environment, sandboxing, process supervision, OS abstraction, and canonical host tools (`nuo-host/`).

## Quick Start

### Build

```bash
# Build daemon and terminal client
cargo build --release -p nuo -p nuox
```

### Run Daemon

```bash
# Start the background session daemon
./target/release/nuo start

# Inspect running daemon status
./target/release/nuo status

# Print bearer token for client connections
./target/release/nuo token

# Stop the daemon
./target/release/nuo stop
```

### Launch Frontends

```bash
# Launch interactive terminal UI (automatically attaches to daemon)
./target/release/nuox

# Run a headless one-shot prompt
./target/release/nuox run "Explain the Nuo crate architecture"
```

## Documentation

- **[Subsystem Architecture](docs/architecture/subsystems.md)**: Architectural blueprints, layer boundaries, and data flows.
- **[Architectural Decision Records (ADR)](docs/adr/index.md)**: Rationale, negative knowledge, and evolution history.
- **[Versioning & Release Guide](docs/dev/release-and-versioning.md)**: Federated Cluster SemVer model and release operations.
- **[Manual Testing Runbook](docs/dev/manual-testing.md)**: End-to-end verification and release sign-off checklists.
