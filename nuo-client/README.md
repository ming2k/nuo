# nuo-client

Standalone Rust Client SDK and wire communication protocols for the Nuo daemon.

## Overview

`nuo-client` provides the programmatic client interface for interacting with a running `nuo` daemon. It serves as an architectural firewall preventing frontends (`nuox`, `web`) and automation tools from depending on internal daemon server or storage logic.

## Capabilities

- **Daemon Discovery**: Automatically discovers active local daemons via `$XDG_RUNTIME_DIR/nuo/daemon.json`, resolving socket paths, TCP ports, and authentication tokens.
- **Wire Codec & Framing**: Implements zero-overhead async framing over Unix Domain Sockets, Windows Named Pipes, and WebSockets.
- **Protocol Negotiation**: Enforces version handshakes and protocol compatibility checks.
- **Session Streaming**: Asynchronously connects to hosted sessions, streams execution events (`DaemonEvent`, `SessionEvent`), and submits agent requests.
- **Command Completion**: Provides synchronous and asynchronous slash command and subcommand completion helpers for terminal composers.
