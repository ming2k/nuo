# ADR-0164: Instructions Are One Pipeline over System and Project Sources

- **Status:** Accepted
- **Date:** 2026-11-08
- **Related:** [ADR-0052](0052-unified-instruction-pipeline-pure-role-topology-and-end-to-end-cache-fidelity.md), [ADR-0130](0130-model-guidance-dynamic-prompt-engineering-and-pure-cognitive-demarcation.md), [ADR-0162](0162-stream-restarts-are-a-separate-budget-and-visible-attempts-are-discarded.md)
- **Amends:** ADR-0130's placement of user route tuning; the terminology of ADR-0052's instruction registry

> **2026-11-08 Amendment:** [ADR-0165](0165-user-instructions-absorb-model-guidance-as-the-user-source.md)
> adds the **user** source and removes `[model_guidance]` entirely. The
> "two sources", "no user layer", and "`[model_guidance]` remains" statements
> below are superseded; the pipeline, selector, ordering, and attestation
> decisions stand. The final source order is system (Base) → user (Base) →
> project (Session).

## Context

The instruction engine had a naming split: `SystemPromptRegistry` composed
`InstructionTier` / `InstructionSlice` / `InstructionBundle`. "System prompt"
is a **wire artifact** — Anthropic's `system` blocks, OpenAI's top-level
`instructions`, Gemini's `systemInstruction`, a Chat Completions system
message — while "instruction" is the semantic unit the user authors. Calling
the composition registry a "system prompt" made every discussion drift between
the artifact and the content.

The customization surface was also incomplete:

- The compiled sections (identity, provider mechanics, workspace topology) are
  the **system** source, versioned with the binary.
- `AGENTS.md` was the only **project** source: one unscoped blob in the Session
  tier, with no way to target a role or a model.
- `[model_guidance]` could target a provider type and a model pattern, but it is
  **user** configuration; it cannot express team-reviewed project guidance, and
  it has no role dimension.
- Roles carried an identity preamble but no way for a repository to add
  role-specific instructions that adapt to the project.

## Decision

### 1. Terminology

- **Instruction(s)** is the primary noun: the semantic content that narrows
  agent behavior. The config, the file format, the types, and the UI use it.
- **System prompt** is reserved for the assembled wire artifact. Code that
  projects an `InstructionBundle` onto a provider protocol may say "system
  prompt"; code that composes content must not.
- The composition types are renamed with no aliases: `InstructionContext`,
  `InstructionRegistry`, `InstructionSection`, `InstructionRegistryError`,
  `default_instruction_registry`, `register_instruction_section`. The module is
  `cognition/model_request/instruction.rs`.

### 2. One pipeline, two sources

There is **one** composition pipeline: registered sections render into one
`InstructionBundle`, memoized by the context hash, tiered Base → Session → Task
→ Ephemeral, and projected per provider protocol.

Two **sources** feed it:

1. **System** — compiled sections and role identities, versioned with the
   binary. Trusted by construction.
2. **Project** — repository files under `.nuo/instructions/`, reviewed and
   versioned with the code, admitted only behind the Instructions trust domain.

A general **user-level instruction layer is deliberately not added**. It would
force a precedence decision (does a personal style preference outrank a project
convention?) with no clear answer, for a feature users are unlikely to need.
User route tuning still exists as `[model_guidance]`; folding it into the same
selector engine is a future user-source decision, not a silent third layer.

### 3. Project instructions

- Location: `.nuo/instructions/**/*.md`. Every Markdown file is one entry.
  Non-Markdown files are ignored. Symlinks are rejected.
- Frontmatter (YAML) is optional; absent frontmatter means project-global.

  ```md
  ---
  role: reviewer
  provider: commandcode-goat
  connection: cc-fox
  model: "*deepseek*"
  ---
  Review rule body.
  ```

- Selectors: `role`, `provider`, `connection`, `model`. `role`, `provider`, and
  `connection` are exact, case-insensitive, and must name a configured entity
  (a missing fact never matches). `model` is **always a glob** (`*`, `?`;
  `/` is ordinary text, so `*` spans a vendor prefix); an exact model id is a
  pattern with no wildcards. All set selectors must match.
- **No `priority`.** Matching entries are additive. Composition order is:
  1. scope rank broad → narrow: global → role → provider → connection → model;
  2. within one rank, fewer selectors first;
  3. path order (the loader returns repository-relative paths sorted).

  A reader sees the most specific guidance last; there is no hidden numeric
  knob to learn.
- Rendering: all matching entries form **one** Session slice
  (`system.project_instructions`) after the AGENTS.md rules, with the source
  path as `## <path>` headings for attribution. Entry text is Markdown prose.
- Authoring errors fail loudly at load: unknown frontmatter keys, unterminated
  frontmatter, an empty selector value, a malformed model pattern, and an empty
  body are all rejected with the file path. A typo must not silently change
  agent behavior.

### 4. Roles

`role` is a first-class selector, not a special case of model guidance. The
active staffing role is part of the instruction context; a subagent's role is
its profile name (`explore`, `code`, `debug`, …), so a repository can ship
role-specific guidance that adapts to the project. Subagents inherit the
parent's declaration set by handle and resolve against it with their own role.

### 5. Attestation and trust

`.nuo/instructions` is part of the **Instructions** trust-domain digest
(`nuo_host::PROJECT_INSTRUCTION_PATHS`), alongside `AGENTS.md`,
`.cursorrules`, and `.windsurfrules`. The domain label is now "Project
instructions" rather than "AGENTS.md". A trusted project instruction can only
be loaded after the domain digest matches; editing any file flips the domain to
`Changed`, and the trust gate re-prompts. The loader re-checks symlinks at read
time so a link cannot be swapped in between attestation and load.

### 6. AGENTS.md and model guidance

- `AGENTS.md`, `.cursorrules`, and `.windsurfrules` remain an **unscoped**
  project-rules slice with their own source delimiters. They are not merged into
  `.nuo/instructions`: different provenance, different attribution, and the
  legacy files are already trusted as a set.
- `[model_guidance]` remains the **user-level route-tuning** source (per
  provider type / model pattern). It is not folded into this ADR. When a user
  source is designed, its entries should use the same `InstructionSelector`
  matcher rather than a second glob engine.

## Invariants

- `[INV-INST-01]` **One pipeline.** System and project content render through
  the same registry, tiers, memoization, and protocol projection.
- `[INV-INST-02]` **No numeric scope priority.** `model` is uniformly a glob;
  composition order is scope rank, selector count, then path.
- `[INV-INST-03]` **A missing fact never matches a set selector.** A route that
  cannot name its role/provider/connection/model does not admit scoped content.
- `[INV-INST-04]` **Project instructions are attested.** The digest covers every
  path the loader reads; symlinks are rejected by both.
- `[INV-INST-05]` **Roles are first-class.** The active role is part of the
  context; subagents use their profile name and inherit the parent's set.
- `[INV-INST-06]` **Authoring errors fail loudly.** Unknown keys, malformed
  frontmatter/patterns, and empty bodies are load errors with the file path.

## Consequences

- A repository can adapt the agent to itself: project-global rules plus
  role-specific instructions, without touching compiled prompts.
- Provider/connection/model-scoped guidance can be committed and reviewed with
  the project; `model` patterns cover relay and local id variance uniformly.
- All project instructions render as one Session slice: editing one file
  invalidates the Session prefix but never the Base tier that carries identity
  and provider mechanics.
- `[model_guidance]` and `.nuo/instructions` are intentionally distinct sources
  today (user route tuning vs project guidance). Unifying them into one
  user-source pipeline is a future ADR, not an implicit behavior.
- The registry rename is a hard break: no `SystemPrompt*` aliases remain.

## Validation

- `instruction_selector_matches_route_facts_and_model_patterns`,
  `instruction_selector_ordering_is_broad_to_narrow`,
  `instruction_selector_validation_rejects_empty_values_and_bad_patterns`.
- `scoped_instructions_parse_frontmatter_and_body`,
  `instruction_frontmatter_rejects_unknown_keys`,
  `unterminated_frontmatter_is_rejected`, `empty_instruction_body_is_rejected`,
  `non_markdown_instruction_files_are_ignored`, `symlinked_instruction_is_rejected`.
- `project_instructions_render_matching_role_entries_in_scope_order`.
- `scoped_project_instructions_are_attested`,
  `the_attested_rule_list_is_the_loaded_rule_list`.
