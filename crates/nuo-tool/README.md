# nuo-tool

Canonical tool specification, typed results, and execution contracts for Nuo.

## Overview

`nuo-tool` is the dependency leaf every capability crate implements against. It
has one execution entry point, one result vocabulary, and no agent-runtime
dependency.

## Core abstractions

- **`Tool`** — one contract: metadata accessors plus
  `execute(&ToolContext, serde_json::Value) -> Result<ToolOutput, ToolError>`.
  There is no string-based second entry point. `execute_json` is a convenience
  that runs the same parse/coerce/validate pipeline and then calls `execute`.
- **`ToolOutput`** — typed results: `Text`, `Error`, `PermissionDenied`,
  `Shell`, `Code`, `Listing`, `Matches`, `Patch`, `Subagent`, `Image`,
  `Structured`, `Media`, `WebSearch`, `WebArticle`. Consumers never string-sniff.
- **`ToolContext`** — per-call identity, cooperative cancellation, workspace
  roots, an incremental stream/progress sink, and a type-keyed service map.
- **`ToolStream::Progress`** — structured, ephemeral progress ticks (message,
  current, total) for long-running tools; never written to the transcript.
- **`ToolInvocation`** — validated arguments, input contract, and the runtime
  input supervisor for streaming/command tools.
- **`ToolExposure`** — `Direct` (declared every request), `Deferred`
  (`tool_search` discoverable), or `Hidden` (harness-internal).
- **`validation`** — compiled JSON Schema validation plus a bounded,
  schema-directed coercion pass. The dispatcher compiles each tool's input and
  output schema once per request snapshot.
- **`ToolAccesses`** — declarative filesystem access and conflict detection for
  the concurrency scheduler.
- **`ToolArgs` / `ToolSchema`** derive — compile-time draft-07 JSON Schema with
  a cached parsed value.

## Execution pipeline

1. The model request captures an immutable tool snapshot (definitions +
   compiled schemas + aliases).
2. Dispatch resolves the call **inside that snapshot**, parses/coerces/validates
   the arguments once, then runs the tool with a deadlined future.
3. A declared output schema is validated before the structured result reaches a
   scripted consumer.
4. Results are bounded at the session boundary; oversized output spills to a
   private spool file and remains readable through `read`.

## Invariants

- A tool that declares a filesystem access must also opt into `path_bound()`.
- Deferred tools are never declared until activated, and never callable outside
  the request snapshot.
- Permission denial is an observation (`PermissionDenied`), not a hard error.
- No tool may be invoked with arguments that violate its declared top-level
  schema.
