# nuox

Semantic terminal client and interactive TUI for the Nuo (傩) system.

## Overview

`nuox` is the primary terminal frontend for Nuo. Powered by the retained-mode `nuotc` rendering engine, it delivers flicker-free, differential rendering with zero ghost artifacts across terminals and multiplexers.

It connects to the local `nuo` daemon over local IPC or WebSocket, providing both an interactive terminal interface and a non-interactive headless runner.

## Capabilities

- **Retained Terminal Canvas**: Built on `nuotc` with write-time dirty line tracking, run-length packed escape codes, and back-color-erase (BCE) optimization.
- **Ghost-Free CJK & IME**: Perfect wide-character and multi-byte alignment preventing trailing block corruption.
- **Interactive Approval Overlays**: Dedicated interactive sheets for human confirmation of high-hazard tool operations (`Once`, `Always`, `Reject`).
- **Transcript Streaming**: Smooth token-by-token rendering with live tool execution cards and syntax highlighting.
- **Headless Execution**: Fast one-shot CLI execution via `nuox run` for scripting and CI/CD pipelines.

## Usage

```bash
# Launch interactive terminal UI (attaches to active or auto-started daemon)
nuox

# Attach to a specific session
nuox attach <session-id>

# Run a one-shot prompt in headless mode
nuox run "Explain the architecture of nuotc"

# Pass prompt via stdin
cat prompt.txt | nuox run -
```
