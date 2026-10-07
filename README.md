# Nuo (傩)

[English](README.md) | [简体中文](README-zh.md)

> The project name **Nuo** derives from the Chinese Pinyin for **傩** (*Nuó*). Nuo was originally a traditional Chinese ritual for expelling perils and disasters, later regarded as a ceremony for communing with the divine. Following traditional Nuo ritual practices, the priest dons a Nuo mask (傩面) to convey intent according to the image and persona embodied by the mask. Nuo draws its core inspiration from this tradition: using a harness system to employ different identities to communicate with intelligence.

Nuo is a high-performance, modular AI session server and semantic terminal system built in Rust. It provides a long-lived background service, an interactive terminal UI, and first-class multi-agent coordination over the **Agent Coordination Protocol (ACP)**.

---

## Features

- **Semantic Terminal UI**: Rich, flicker-free terminal interface powered by `nuo-tui` and the retained-mode `nuotc` engine with sub-millisecond differential updates and flawless CJK alignment.
- **Session Server (`nuo`)**: Detached background server hosting multi-client sessions across workspaces, serving local IPC (Unix Domain Sockets) and WebSocket control planes.
- **Execution Harness (`nuo-harness`)**: Outfits agents with targeted identities and prompts, enforces strict hazard controls (`RiskProfile`), interactive human confirmation checkpoints, and real-time loop detection.
- **Multi-Agent Coordination (ACP)**: Native Agent Coordination Protocol supporting peer-to-peer task delegation (`agent://`) and collaborative multi-subscriber channels (`acp://`).
- **Zero-Runtime Tools**: Zero-I/O contracts with compile-time schema derivation (`nuo-tool-derive`) and native Model Context Protocol (MCP) client/server integration.

---

## Quick Start

### 1. Build

```bash
cargo build --release -p nuo
```

### 2. Start the Server or Foreground Service

```bash
# Start foreground server service (systemd / container)
./target/release/nuo serve

# Or start detached background server
./target/release/nuo start

# Inspect status and active sessions
./target/release/nuo status

# Print connection bearer token
./target/release/nuo token
```

### 3. Launch the Terminal Interface

```bash
# Launch interactive TUI (auto-attaches to server)
./target/release/nuo

# Run a headless one-shot prompt
./target/release/nuo run "Explain the Nuo project structure"
```

---

## CLI Cheatsheet

| Command | Description |
| :--- | :--- |
| `nuo` | Open interactive terminal interface (default) |
| `nuo run "<prompt>"` / `nuo -p` | Execute headless one-shot agent task and print result |
| `nuo serve` | Run the server service in foreground |
| `nuo start [--fg]` | Start the server (detached background or foreground) |
| `nuo stop` | Gracefully drain and stop the server |
| `nuo status [--watch]` | Check server health, hosted sessions, and active endpoints |
| `nuo token` | Display current bearer authentication token |
| `nuo attach` | Join an existing or hosted session in interactive TUI |
| `nuo dashboard` | Open full-screen interactive session dashboard |
| `nuo session rm <id>` | Terminate a hosted session |
| `nuo mcp ls` | List configured MCP servers and probe status |
| `nuo skill ls` | List discovered skills |
| `nuo doctor` | Verify stored session database integrity |

---

## Documentation

- **[Subsystem Architecture](docs/architecture/subsystems.md)**: Workspace topology, crate boundaries, and architectural invariants.
- **[Architectural Decision Records (ADR)](docs/adr/index.md)**: Design decisions, trade-offs, and evolutionary history.
