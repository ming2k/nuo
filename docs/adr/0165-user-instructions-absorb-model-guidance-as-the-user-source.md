# ADR-0165: User Instructions Absorb Model Guidance as the User Source

- **Status:** Accepted
- **Date:** 2026-11-08
- **Related:** [ADR-0130](0130-model-guidance-dynamic-prompt-engineering-and-pure-cognitive-demarcation.md), [ADR-0164](0164-instructions-are-one-pipeline-over-system-and-project-sources.md)
- **Supersedes in part:** ADR-0130 (the `[model_guidance]` config, its resolver, and `RouteSettings::model_guidance`)

## Context

ADR-0164 unified system and project instructions but deliberately deferred a
user source, leaving `[model_guidance]` as a separate mechanism: its own config
type, its own hierarchical resolver, its own `InstructionSection`, its own
`Agent` handle and subagent binding, and a route-override field in
`RouteSettings`. That was the only user-level, cross-project tuning surface, but
it duplicated the new selector engine and was becoming an isolated island.

The capability is real: personal model-style tuning ("speak plain Chinese on
DeepSeek", "be terse on a local model") cannot live in a repository, because it
is not team guidance and must not require a commit. The answer is not to delete
the capability, and not to keep a second mechanism: it is to make `model_guidance`
a **source** of the one instruction pipeline.

## Decision

### 1. Three sources

The pipeline now composes from three sources:

1. **System** — compiled sections and role identities.
2. **User** — `[[instructions.user]]` in `agent.toml`, cross-project personal
   tuning.
3. **Project** — `.nuo/instructions/**/*.md`, trusted and attested.

There is still exactly one registry, one selector matcher, one ordering rule,
and one protocol projection.

### 2. User config

```toml
[[instructions.user]]
model = "*deepseek*"
text = "Use plain, natural Chinese. Avoid bureaucratic cliches."

[[instructions.user]]
connection = "cc-fox"
model = "deepseek/deepseek-v4-flash-fast"
text = "Ultra-concise mode."

[[instructions.user]]
role = "reviewer"
provider = "commandcode-goat"
text = "Review against the project architecture."
```

- The selectors are the same `InstructionSelector` as project instructions:
  `role`, `provider`, `connection` (exact; the latter two must name configured
  entities) and `model` (always a glob; an exact id is a pattern without
  wildcards).
- Matching is additive. Ordering is the shared rule: global → role → provider →
  connection → model, then selector count, then declaration order. There is no
  `priority`.
- The config is validated at load: unknown keys, empty selector values, a
  malformed model pattern, and empty text are errors with the entry index.
- A user entry gets provenance `user:<ordinal>` and renders under a
  `user | <selector>` heading.
- Subagents inherit the parent's user-instruction handle and resolve it with
  their profile role, exactly as they inherit project instructions.

### 3. Placement and cache

- `system.user_instructions` is a **Base** section, ordered after tone and
  before provider guidance. It is route-stable content and belongs in the
  cacheable prefix.
- Project instructions remain **Session** content. The final order is system
  (Base) → user (Base) → project (Session), so a personal style instruction
  comes before repository rules and a repository rule can still close the loop.
- One user-instruction set is one slice; changing it invalidates the Base
  prefix, which is intended (personal tuning is session-stable, not per-turn).

### 4. Hard deletion, no aliases

The following are removed outright:

- the `[model_guidance]` table and `ModelGuidanceConfig`;
- `Config::resolve_model_guidance` and its provider-table/prefix/glob/family
  cascade;
- `RouteSettings::model_guidance` (route overrides become `connection` +
  `model` user entries in `agent.toml`);
- `Agent::model_guidance` / `model_guidance_handle` / `bind_model_guidance`;
- the `system.model_guidance` instruction section;
- `SubagentTool::parent_model_guidance` and its binding;
- the `AgentBuilder::with_model_guidance` test/embedding API.

There is no re-export and no fallback. Old `[model_guidance]` is **recognized
only to fail with a precise migration error**:

```
`[model_guidance]` is retired; move its entries to `[[instructions.user]]`
(provider/connection/model selectors, model is a glob)
```

That is migration UX, not compatibility: the old shape never takes effect.

## Invariants

- `[INV-INST-06]` **User is a source, not a subsystem.** User content goes
  through the same registry, matcher, ordering, and projection as system and
  project content.
- `[INV-INST-07]` **One matcher.** Model selection is always a glob; there is
  no second model-guidance cascade.
- `[INV-INST-08]` **No silent fallback.** A retired `[model_guidance]` fails
  loudly; a route override cannot survive as a hidden second prompt path.
- `[INV-INST-09]` **Base before Session.** User instructions are Base; project
  instructions are Session; the cacheable prefix contains the user source.
- `[INV-INST-10]` **Subagents inherit both scoped sources.** User and project
  handles travel together; the child resolves with its profile role.

## Consequences

- One config surface for all authored instructions: user entries in
  `agent.toml`, project entries in the repository. The model selector is the
  same glob in both.
- Personal cross-project tuning survives the removal of `[model_guidance]`;
  per-route overrides move into ordinary `connection` + `model` entries.
- The user source is Base-tier content: it is cached in the prefix, and a
  change invalidates that prefix but not the system identity sections.
- Existing route settings carrying `model_guidance` are no longer read. This is
  an intentional hard break; affected overrides must be re-expressed as user
  entries.

## Validation

- `user_instructions_parse_and_validate`,
  `user_instruction_validation_rejects_empty_text_and_bad_patterns`,
  `retired_model_guidance_is_rejected_with_migration`.
- `user_instructions_are_rendered_dynamically_in_base_tier`,
  `subagent_inherits_parent_user_instructions`.
- `reseed_instructions_populates_agent_from_config`.
