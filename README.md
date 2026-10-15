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

### 2. Start the Server (Optional / Headless & Container)

```bash
# Start background server
./target/release/nuo server start

# Or run foreground server service (containers / systemd)
./target/release/nuo server start --fg

# Inspect system status and active sessions
./target/release/nuo status

# Print connection bearer token
./target/release/nuo server token
```

### 3. Launch the Terminal Interface

```bash
# Launch interactive TUI (auto-starts/attaches to server)
./target/release/nuo

# Open the interactive thread picker, or resume a thread by id
./target/release/nuo -t
./target/release/nuo -t <thread_id>

# Run a headless one-shot prompt
./target/release/nuo --headless -p "Explain the Nuo project structure"
```

---

## CLI Cheatsheet

| Command | Description |
| :--- | :--- |
| `nuo` | Open interactive terminal interface (default) |
| `nuo -t [thread_id]` | Open the interactive thread picker, or attach to a thread by id |
| `nuo --resume` | Resume the most recent matching thread |
| `nuo --headless -p "<prompt>"` | Execute a headless one-shot agent task and stream the result |
| `nuo status [--watch\|--json\|--all\|--diagnostic]` | Check health, hosted threads, endpoint discovery, and providers |
| `nuo server start [--fg] [--port <n>]` | Start the session server (detached background, or `--fg` for foreground) |
| `nuo server stop` | Gracefully drain and stop the session server |
| `nuo server reload` | Soft reload configuration and MCP/skills |
| `nuo server restart` | Restart or replace the session server instance |
| `nuo server status` | Show session server status and active sessions |
| `nuo server token` | Display current bearer authentication token |
| `nuo thread ls` | List hosted threads and state |
| `nuo thread open [id]` | Open the interactive thread picker or attach by id |
| `nuo thread rm <id>` | Terminate a hosted thread |
| `nuo completions <shell>` | Print a shell completion script (bash, zsh, fish) |

---

## Documentation

- **[Getting Started](docs/tutorials/getting-started.md)**: build the workspace and run your first agent turn.
- **[How-To Guides](docs/how-to/index.md)**: connect a provider, manage threads, and inspect usage.
- **[Architecture Overview](docs/explanation/architecture-overview.md)**: entity tiers, crate topology, and design rationale.
- **[Reference](docs/reference/lifecycle-and-action-semantics.md)**: lifecycle and action semantics.
- **[Terminal UI Reference](docs/reference/tui/index.md)**: viewport layout, visual language, modals, and input pipeline.
- **[Subsystem Architecture](docs/dev/subsystems.md)**: workspace topology, crate boundaries, and architectural invariants.
- **[Architectural Decision Records (ADR)](docs/adr/index.md)**: design decisions, trade-offs, and evolutionary history.
