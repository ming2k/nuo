# nuo

The session daemon host and management CLI for the Nuo system.

## Overview

`nuo` provides both the command-line operator interface and the long-lived daemon runtime. It hosts multi-client sessions across projects, manages background tasks and MCP integrations, multiplexes client requests, and serves dual control planes:

- **Local IPC Control Plane**: Unix Domain Sockets (`$XDG_RUNTIME_DIR/nuo/daemon.sock`) on Unix or Named Pipes on Windows for local CLI tools and terminal clients (`nuox`).
- **WebSocket Control Plane**: Streaming JSON RPC endpoint (`ws://127.0.0.1:9800`) for remote frontends, browser panels (`web`), and monitoring agents.

## Architecture

- **`main.rs` & `cli.rs`**: CLI argument parsing, daemon lifecycle control, subcommands, and diagnostic tables.
- **`lib.rs` & `host.rs`**: Daemon process detachment, graceful drain budgets, and signal handling.
- **`registry.rs`**: `SessionRegistry` managing concurrent hosted sessions across projects with per-session locks and event multiplexing.
- **`session_driver.rs`**: Request processing loop routing client requests (`AgentRequest`) to appropriate handlers.
- **`handlers_*`**: Request handlers for chat, permissions, model providers, slash commands, and session lifecycle.
- **`serve.rs` & `wire_channel.rs`**: Async WebSocket and IPC server engines with token-based local authentication.
- **`background_jobs.rs`**: Background process supervision and async exploration tasks.

## Commands

```bash
# Daemon Lifecycle
nuo start [--fg] [--port <PORT>]   # Start daemon (detached or foreground)
nuo stop                          # Gracefully drain and terminate daemon
nuo status [--watch]              # Inspect running daemon and active sessions
nuo token                         # Print active bearer authentication token

# Management Subcommands
nuo session rm <id>               # Terminate a hosted session
nuo mcp ls                        # List configured Model Context Protocol servers
nuo mcp probe <name>              # Connect once and inspect advertised tools
nuo skill ls                      # List discovered skills
nuo config check                  # Validate config.toml against schema
nuo completions <shell>           # Generate shell autocompletion script
```

## Configuration

The daemon resolves configuration from `.nuo/config.toml` (project-scoped) and `~/.config/nuo/config.toml` (user-scoped). Run-state discovery metadata is saved to `$XDG_RUNTIME_DIR/nuo/daemon.json`.
