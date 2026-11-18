---
id: ADR-0208
title: "One Tool Execution Boundary, Request Snapshots, and Deferred Discovery"
status: accepted
date: 2026-11-17
scope: tool/, protocol/tool_registry, engine/dispatch, tool-fs, tool-exec, tool-web, tool-ast, server/runtime/mcp, guardrails
superseded_by: null
supersedes:
  - the legacy string entry point and duplicate tool surfaces removed by ADR-0008's migration
  - the top-level-only argument shape check in `tool::validation`
negative_knowledge: true
---

# 0208. One Tool Execution Boundary, Request Snapshots, and Deferred Discovery

- Status: Accepted
- Date: 2026-11-17

## Context and Problem Statement

The tool layer had accumulated three classes of debt:

1. **Two execution contracts.** `Tool::execute(Value)` and the legacy
   `Tool::call(&str)`/`call_structured` bridge let malformed JSON and mistyped
   model output reach individual tools, where each one produced a different
   error. The bridge also kept dead implementations (`CommandTool`, `DynamicTool`,
   `McpTool`, `ToolRegistry`, `ApprovalHandler`) and a second, string-based
   command tool alive in tests.
2. **Live registry dispatch.** Model requests were assembled from one registry
   read and executed against another. A tool installed or removed mid-request
   (MCP connect/disconnect, `/reload`, a scoped hook) could be called without
   having been advertised, or advertised but no longer resolvable.
3. **Unbounded or flattened tool results.** Errors bypassed the context cap,
   previews carried whole files into events, MCP results were flattened to text,
   and every MCP schema was declared in every prompt.

## Decision

### 1. One execution boundary

`Tool::execute(&ToolContext, serde_json::Value) -> Result<ToolOutput, ToolError>`
is the only implementation entry point. `parameters()` is renamed
`parameters_schema()`; `call`, `call_structured`, and
`call_structured_with_events` are removed. Streaming tools override
`execute_streaming`, whose default delegates to `execute`.

The dead surfaces are deleted, not shimmed: `CommandTool`, `DynamicTool`,
`McpTool` (the leaf adapter), `ToolRegistry`, and `ApprovalHandler`.
`nuo-tool` no longer depends on `tokio`'s process/io features; `command
execution` lives only in `tool-exec`.

### 2. One argument boundary

Dispatch parses, coerces, and validates arguments **once** against a compiled
JSON Schema:

- `jsonschema` validates a recursive subset including local `$ref`,
  `oneOf`/`anyOf`/`allOf`, `enum`, `required`, `additionalProperties`, and
  nested arrays/objects. External refs are rejected.
- A bounded coercion pass repairs only unambiguous model mistakes: numeric
  strings for numeric fields, `"true"`/`"false"` for booleans, a single value
  for an array field, a JSON-encoded object for an object field, and optional
  `null` removal.
- Unparseable or still-invalid arguments never reach the tool. Errors are
  model-actionable: bounded issue list, dot-path locations, and a bounded
  argument echo.
- Schemas are compiled through a process-wide cache keyed by serialized schema.

### 3. Immutable request snapshots

`ToolSnapshot` captures the exact declaration set, compiled input/output
schemas, and name/alias index for one model request. Dispatch resolves **only**
within that snapshot; an unadvertised name is refused even if the live registry
contains it. The snapshot is refreshed when the next request is assembled.

### 4. Deferred exposure and `tool_search`

`Tool::exposure()` is `Direct`, `Deferred`, or `Hidden`. Deferred tools are not
declared until a `tool_search` call activates them. The ranker is BM25F over
four weighted fields — name 5, aliases 3, schema properties 1.5, description 1 —
so term frequency saturates and a long description cannot bury an exact name.
Exact and phrase-name matches retain a small additive bonus, and an empty or
whitespace-only query is rejected rather than activating arbitrary tools. An
activation appends to the stable direct prefix so prompt-cache prefixes do not
churn. MCP tools default to deferred; a server can opt out with
`defer_tools = false`.

### 5. Typed structured and media results

`ToolOutput::Structured { content, value, failed }` carries a
schema-validated machine payload alongside model-facing text.
`ToolOutput::Media { parts, structured, failed }` preserves ordered MCP
text/image/resource content instead of flattening it. Declared output schemas
are compiled and validated at the dispatch boundary; structured payloads are
capped at 512 KiB.

### 6. MCP parity

MCP tool annotations map to risk and scheduling:

- `readOnlyHint` → `ReadOnly` + no accesses,
- `destructiveHint` → `Destructive` + global-exclusive,
- `idempotentHint` → `IdempotentMutation` + global-exclusive,
- no annotations → `NetworkAccess` + global-exclusive.

`structuredContent`, images, resources, `outputSchema`, and `isError` are
preserved. `resources/list` and `resources/read` are exposed as read-only
tools (probed at connect time, deferred like the server's function tools).
Public names are capped at 64 characters and collision suffixes are a stable
hash of the raw `(server, tool)` identity, not an order-dependent integer.

### 7. Bounded output, previews, and spool

- Error output is capped at 8 KiB and spools the full text.
- Preview diffs are windowed around the change (common prefix/suffix + context)
  with an explicit `truncated` flag; large files no longer enter events whole.
- `Tool::preview` is async. Filesystem previews move their read and diff
  planning to `spawn_blocking` and refuse inputs above 8 MiB, so the dispatch
  pipeline and permission gate never perform an unbounded synchronous read.
  Both choke points bound the wait at 5 seconds and continue without a preview
  on timeout; the blocking worker cannot be force-cancelled on every platform.
- Small files (≤1 MiB) use a SHA-256 content version, so a same-length rewrite
  is detected. Larger files use length, mtime, device, inode, and ctime instead
  of hashing gigabytes on every read; the version is a concurrency token, not a
  security boundary.
- Spool files use `create_new` + `0600` in a `0700` directory, have a process/
  sequence suffix, and are swept by age (7 days) and total size (512 MiB).
- The virtual-file resolver inlines at most 4 MiB; larger artifacts fall back
  to `read`'s streaming path.

### 8. Deadlines and cancellation

`Tool::timeout()` gives the dispatcher a per-call deadline (filesystem/search/
web/code-query 30 s, MCP 120 s). A tool that owns a dynamic per-call timeout
(the shell) returns `None`. On expiry the call future is dropped (shell uses
`kill_on_drop`) and the model receives a typed timeout observation.

### 9. Versioned writes

`read` returns an opaque `version` (length + mtime) in its `Code` result and
prefix. `edit_text` and `write_file` accept `expected_version` and refuse a
write whose current bytes no longer match, closing the read→external-write→edit
race.

### 10. Path truth and deny-first permissions

- `normalize_access_path` resolves `.`/`..` lexically; the scheduler, journal,
  and permission scope all see the same path.
- `Tool::path_bound()` replaces the hard-coded `PATH_BOUND_TOOLS` list; the
  collection audit asserts that any tool declaring a filesystem access also
  opts into the workspace boundary.
- `[permissions].deny` is a hard rule evaluated **before** bash confirmation
  and every allow path. A trailing `*` is a prefix; there is no `?`/`[]`
  wildcard engine.
- Explicit `/auto-approve` (`--auto-approve`, with `/unattended`, `/auto`,
  `/delegate`, `/autopilot`, `/yolo` as aliases) bypasses interactive
  confirmations (`BashPolicy::Confirm` and `BrokerPolicy` grants) in-flight
  without persisting rules into `PermissionStore`, while hard denials
  (`[permissions].deny` and `BashPolicy::Deny`) remain enforced.

## Consequences

- **Positive**: one argument/result path for built-ins, MCP, and tests; a model
  call cannot bypass validation or the advertised set; MCP catalogs no longer
  bloat every prompt; previews/errors/spool cannot silently exhaust memory or
  context; writes can detect stale reads.
- **Negative & Incurred Costs**: `jsonschema` adds a real dependency to the
  tool leaf; `Tool` implementors must migrate to `execute` and `&Value`
  metadata; deferred MCP tools require a `tool_search` turn before first use.
- **Neutral**: this is an intentional clean break. No legacy `call`, no
  compatibility shims, no dual schema spellings.

## Discarded Alternatives

- **Keep `call` as a default adapter**: it re-introduces two validation paths
  and lets one tool bypass coercion. Removed instead.
- **Validate only top-level fields**: insufficient for MCP and nested tool
  arguments; it was the source of raw `serde` errors reaching the model.
- **Advertise all MCP tools natively**: prompt bloat and worse tool selection;
  `tool_search` plus a stable direct prefix is cheaper and cache-friendly.
- **Flatten MCP content to text**: loses images and structured data that both
  the UI and scripted consumers need.
- **Order-dependent `_2` name suffixes**: unstable across reconnects; a stable
  hash of the raw identity is deterministic.
- **Persist auto-approved invocations into `PermissionStore`**: conflates
  temporary session-level `/auto-approve` with permanent project allowlist
  grants; only explicit `Always` replies persist specific rules.
