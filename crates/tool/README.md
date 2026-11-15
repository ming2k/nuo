# nuo-tool

Canonical tool specification, dynamic capability registry, and risk contracts for Nuo.

## Overview

`nuo-tool` provides zero-agent-runtime tool abstractions enabling capability crates (`nuo-host`, `acp`, `nuo-persistence`) to define native tools without pulling in the entire agent execution engine.

## Core Abstractions

- **`Tool`**: Primary asynchronous trait defining tool name, description, JSON parameters schema, and execution entry point (`execute`).
- **`RiskProfile`**: Declares operational hazard levels (`Safe`, `ReadOnly`, `Mutating`, `Destructive`) for downstream approval policies.
- **`ToolScope`**: Categorizes tool capabilities (`System`, `Filesystem`, `Network`, `Agent`, `Memory`).
- **`ToolContext`**: Carries cooperative cancellation tokens and execution metadata across tool calls.
- **`ToolRegistry`**: Thread-safe dynamic tool registry supporting native tools, dynamic closures (`DynamicTool`), and Model Context Protocol adapters (`McpTool`).
- **`ToolSchema`**: Derive macro (re-exported from `nuo-tool-derive`) generating JSON schemas at compile time.
