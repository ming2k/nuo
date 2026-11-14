---
id: ADR-0178
title: "One Capability, One Producer, and No Ghost Builtins"
status: accepted
date: 2026-11-14
scope: protocol/tool-identity, tools/registration, harness/assembly, presentation/tool-components
superseded_by: null
negative_knowledge: true
---

# 0178. One Capability, One Producer, and No Ghost Builtins

- Status: Accepted
- Date: 2026-11-14
- Deciders: Nuo Architecture Working Group
- Consulted: Tool Runtime, Agent Assembly, Skill Registry, Persistence, Presentation
- Complements: [ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md),
  [ADR-0008](0008-single-tool-contract.md), [ADR-0064](0064-developer-tool-scope.md)

---

## Context and Problem Statement

A tool can be wrong in three different ways without any test failing:

1. **Advertised but unproducible.** `BuiltinTool` names a capability, role
   allowlists admit it, a presenter draws it, and `ToolOutput` has a variant for
   it — but no factory or assembly path ever creates it. `read_image` was this
   shape after the workspace split; `use_skill` and `list_skills` were this
   shape from the beginning.
2. **Produced twice.** Two code paths build the same `(name, variant)`:
   a self-registered factory and a direct `toolset.upsert(...)` in the session
   bootstrap. `ToolSet::upsert` silently makes the later one win, so the
   factory, its injected service, and its tests can be dead while the assembly
   looks healthy. `recall_memory` and `code_query` both had this shape.
3. **Produced but never admitted.** The tool is real and installed, but no
   built-in role grants it. This is not a defect when it is a deliberate scope
   decision (`ask_user`, `todo`, `code_query`, `recall_memory`); conflating it
   with (1) leads to deleting working tools or, worse, "fixing" it by admitting
   them everywhere.

The cost is not just dead code. A ghost emitted in a prompt teaches the model to
call something that cannot run; a duplicate registration makes the service
injection, tests, and the actual production implementation disagree. Both are
architecture rot that a green build does not catch.

## Decision

### 1. `BuiltinTool` is a production manifest, not a wish list

Every `BuiltinTool` variant must have exactly one production assembly path:

- a `register_tool!` factory collected by `collect_toolset`, or
- an explicit agent-owned insertion that genuinely depends on state the
  registry cannot see (`TodoTool` is tied to the live `TodoList`; `SubagentTool`
  needs the rest of the toolset; fs/exec tools are constructed with the
  session's `SystemToolContext`/`ExecToolContext`).

A variant with no producer is removed from the enum. It may remain a
programmatic utility with a literal name, but it does not enter `BuiltinTool`,
role allowlists, prompt schemas, or the tool presenter registry.

### 2. One producer per `(name, variant)`

A capability is either factory-registered or explicitly assembled, never both.
The bootstrap's direct insertions are limited to the cases above and must not
overlap a collected factory. `ToolSet::upsert` remains the mechanism for the
explicit cases; it is not a place to shadow a factory for convenience.

When an existing direct insertion duplicates a factory, the factory wins: it is
the path that receives injected services and is exercised by the registry
integration test. The duplicate is deleted, not kept as a fallback.

### 3. Programmatic utilities are not model-facing tools

`UseSkillTool` and `ListSkillsTool` remain useful library types. `/skills`
calls `ListSkillsTool` directly; tests construct both. They are not built-ins,
so their names are literals, `Tool::builtin()` returns `None`, and no prompt
schema advertises them. The model reaches skills through mention injection and
the `skill` subagent role, which is the implemented design.

### 4. Scope is not production

`ask_user` and `todo` are installed/registered and deliberately excluded from
the developer/ops allowlists ([ADR-0064](0064-developer-tool-scope.md)).
`code_query` is admitted only to the explore/debug subagents; `recall_memory`
only to the philosophist role. These are scope policies, not ghosts. Removing a
tool because a role does not admit it would confuse the two axes.

### 5. Output variants follow the same rule

A `ToolOutput` variant with no producer is either wired to the tool that
naturally produces it or removed. `Matches` was a formatted string in
`search_text`; it now returns the structured variant, so the renderer and the
model see one shape.

### 6. The registry test names the ghosts

The integration test asserts both directions:

- every factory-backed capability is present in `collect_toolset`;
- `use_skill`, `list_skills`, `read_text`, and `read_image` are absent from the
  collected set.

The absence assertions are the regression fence: a future "helpful" alias that
reintroduces a ghost fails the test before it reaches a prompt.

## Invariants

1. **Every builtin has one producer.** A `BuiltinTool` variant with no
   assembly path is a defect.
2. **A `(name, variant)` has one producer.** Factory and direct insertion do
   not overlap.
3. **No ghost names in the collected set.** Old spellings are absent, not
   aliased.
4. **Scope does not delete capabilities.** A role allowlist omission is a
   policy, not evidence of a ghost.
5. **Programmatic utilities stay out of the manifest.** A utility can exist
   without being a model-facing tool.
6. **Output variants are produced.** A declared `ToolOutput` shape has a tool
   that returns it.

## Consequences

- `read_image` and `use_skill`/`list_skills` are gone from `BuiltinTool`; the
  latter remain as programmatic utilities.
- `recall_memory` has one implementation: the port-based factory. The
  persistence tool module and its `nuo-tool` dependency are deleted.
- `code_query` has one producer: the factory. The bootstrap's direct
  `create_ast_tools` insertion is gone.
- `search_text` returns `ToolOutput::Matches`; the TUI uses the structured
  pattern instead of re-parsing the raw arguments.
- `ask_user`, `todo`, `code_query`, and `recall_memory` remain real tools with
  deliberate role scopes; they are not deleted by this ADR.
- The registry integration test now fences ghost names and asserts the
  factory-backed capabilities are present.

## Negative Knowledge

- **Do not** add a `BuiltinTool` variant "for discoverability" before a
  producer exists. The enum is a manifest, not a roadmap.
- **Do not** keep a direct insertion as a "fallback" for a factory. Two
  producers means the service injection and the tests can be testing a path
  production never runs.
- **Do not** treat a role allowlist omission as a ghost. `ask_user`/`todo` are
  intentionally out of the developer scope.
- **Do not** re-add `use_skill`/`list_skills` to the enum without installing
  them. Programmatic utilities use literal names and `Tool::builtin()` returns
  `None`.
- **Do not** remove `ToolOutput::Matches` instead of wiring `search_text` to
  it; the structured shape is the point.
