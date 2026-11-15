---
id: ADR-0207
title: "Multi-File Patch Tool, Fullscreen Diff Scene, Universal Message Pager, and Measured-Usage Compaction"
status: accepted
date: 2026-11-15
scope: tools, tui, diff, compaction, telemetry
superseded_by: null
supersedes: []
negative_knowledge: false
---

# ADR-0207: Multi-File Patch Tool, Fullscreen Diff Scene, Universal Message Pager, and Measured-Usage Compaction

- Status: Accepted
- Date: 2026-11-15
- Tags: tools, tui, diff, compaction, telemetry

> **Superseded in part (2026-11-15)**: Decision §1 (`BuiltinTool::Patch` /
> `crates/tool-fs/src/patch.rs`) was retired because native parallel `edit_text`
> / `write_file` calls already carry structured `ToolOutput::Patch` diffs for
> `SceneKind::Diff` without maintaining a third text-patch envelope grammar in
> tool schemas. Decisions §2 (`SceneKind::Diff`, universal message pager) and §3
> (measured-usage compaction anchor, `cache_hit_ratio`) remain active.

## Context

Following the performance, harness, and TUI parity roadmap against OpenCode:

1. **Multi-file atomic editing**: Models had only single-file `edit_text` and `write_file`. Editing multiple coordinated files required sequential tool calls without cross-file preflight validation.
2. **Reading & diff review surfaces**: Pressing `v` on a transcript entry was restricted to command steps (`is_command_body`), leaving long assistant messages, reasoning blocks, and non-command tool outputs without a fullscreen pager; and there was no dedicated fullscreen diff review scene (`SceneKind::Diff`).
3. **Compaction trigger accuracy**: Mid-turn `project_context_if_needed` relied solely on local BPE estimates (`estimate_model_request`), which can undercount system prompts, tool schemas, or multimodal payloads relative to the provider's tokenizer.

## Decision

1. **`BuiltinTool::Patch` (`crates/tool-fs/src/patch.rs`)**:
   - Supports `*** Begin Patch` / `*** End Patch` envelopes with `*** Add File:`, `*** Update File:` (including `*** Move to:`), and `*** Delete File:`.
   - Enforces two-phase execution: all paths are lexically and canonically confined to `workspace_root` and all hunks are matched in memory before any filesystem mutation occurs via `atomic_write`.
   - Declares every affected path in `Tool::accesses` and `Tool::scope_target` so the scheduler and permission broker arbitrate multi-file writes safely.

2. **Universal Message Pager & `SceneKind::Diff` (`crates/nuo/src/shell/`)**:
   - `open_command_log_scene` now accepts any `TranscriptMessage` (command steps via `CommandLog::derive`, non-command tool steps, and text/reasoning messages), eliminating the artificial command-only restriction.
   - `SceneKind::Diff` (`Alt+D`) collects `ToolOutput::Patch` records from the focused transcript and provides hunk-colored unified diffs, `n`/`p` file cycling, `r` per-file reviewed toggles, and standard scene-stack navigation (`Esc`/`q`/`Alt+[`).

3. **Measured-Usage Compaction Anchor & Cache Hit Ratio (`crates/engine/`, `crates/protocol/`)**:
   - `AgentContext::last_measured_prompt` records `(reported_prompt_tokens, estimated_at_report)` whenever a provider turn settles with `prompt_tokens > 0`.
   - `project_context_if_needed` computes `max(local_estimate, reported_prompt_tokens + delta)` and clears the anchor upon compaction replacement.
   - `TurnPerformanceSnapshot::cache_hit_ratio()` computes the fraction of prompt tokens served from the provider's prompt cache.

## Negative Knowledge

- **Do not allow `PatchTool` to declare `ToolAccesses::read_write_file` with `ScopeTarget::Unspecified`**: `debug_assert_safe_targets` in `crates/protocol/src/tool_registry.rs` panics in debug/test builds if a write-declaring tool returns `ScopeTarget::Unspecified` for valid inputs.
- **Do not mutate any file during `PatchTool::execute` before all paths and hunks validate**: Partial multi-file writes on a failing hunk or path-escape check leave the workspace inconsistent.
- **Do not keep `last_measured_prompt` across context replacement**: Once `project_context_if_needed` replaces `messages`, the pre-compaction provider measurement no longer reflects the active window.
