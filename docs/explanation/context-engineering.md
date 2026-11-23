# Context Engineering

How the model's context is declared, measured, and kept honest. Read this before
adding anything to the prompt, injecting anything into a conversation, or
changing a compaction threshold.

The binding decisions are [ADR-0215](../adr/0215-context-sources-declare-moment-topic-rank-stability-and-budget.md)
(declaration and compile), [ADR-0216](../adr/0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md)
(tool capability, not names), and [ADR-0217](../adr/0217-context-projection-is-inspectable-and-prefix-drift-is-reported.md)
(inspection and drift). The exploratory record is
[RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md).

---

## 1. The one rule

> **Every element names its moment, its topic, its rank, its stability, and its
> budget — or it does not reach the model.**

An element that cannot state all five is not a context element; it is a bug
looking for a place to hide. `Policy::compile` refuses the ones it can see at
construction, and the governance suite refuses the rest.

---

## 2. Five lifecycles, and which one you want

Every context mutation happens at exactly one of these. Naming the moment is the
first design decision, and getting it wrong is how a "small feature" ends up
rewriting the prompt every request.

| Moment | When it runs | Lossy? | Side effects |
| :--- | :--- | :--- | :--- |
| **M1 Assemble** | every model request | never | none — pure function of `ContextInputs` |
| **M2 Admit** | before a round, on a durable fact | never | appends a `ThreadFact` |
| **M3 Relieve** | between ReAct turns | yes, with a notice | persists a projection checkpoint |
| **M4 Evict** | mid-round | yes; must preserve the uncommitted tail | checkpoint |
| **M5 Resume** | hydrate, replay, branch switch | rebuilds projections | none — never re-runs M2 |

M1 is the moment the compile-checked policy governs. M3/M4 are the compaction
ladder (`crates/engine/src/orchestration/context_gate.rs`) and derive their
thresholds from `protocol::CompactionPolicy` only.

---

## 3. Where a new piece of context goes

| You want to… | Moment | Channel | What you touch |
| :--- | :--- | :--- | :--- |
| Add a standing rule for every request | M1 | `fixed` | one `ContextSource`, a `Topic`, a `Budget` |
| Add a rule for one role, provider, connection, or model | M1 | `fixed` | `.nuo/instructions/*.md` or `[[instructions.user]]` — **no new source** |
| Declare which language the answer is in | M1 | `fixed` | `/language` or `response_language` — a session fact on `agent.language` |
| Add data for one request only | M2 | `messages` | a hidden message with its own `InjectionKind` and envelope (ADR-0226) |
| Add a rule that must survive every turn | M1 | `fixed` | a new `Topic`+`InputKey`+source, in one change (ADR-0229) |
| Record something the user said | M2 | `messages` | `cognition::conversation_context`, with an `InjectionKind` |
| Shrink an old tool result | M3/M4 | `messages` | `CompactionPolicy::prune_*` — never a source |
| Shrink history | M3 | `messages` + a `Compaction` fact | `CompactionPolicy` |
| Make something large discoverable | M1 | index entry + a tool | `Overflow::Index` |
| Change what wins on conflict | — | structure *and* prose | `supersedes()` for text; `agent.precedence` for behaviour |

Most "add prompt context" requests are the second row: a scoped instruction, not
a new source. A new source is the answer only when the content is not
user-editable configuration.

---

## 4. Adding a source: the five enforced steps

1. **Pick the topic.** If an existing `Topic` fits, you are editing a source, not
   adding one. Two active sources on one topic is `DuplicateTopic`
   (`[INV-CTXSRC-01]`).
2. **Declare the inputs.** `inputs()` returns the `InputKey`s you read — in
   `is_active` *and* in `render`. Stability is derived as the maximum over those
   keys, so you cannot claim more stability than your inputs have
   (`[INV-CTXSRC-03]`).
3. **Declare a budget.** Required, token-denominated, with an `Overflow` and a
   `Resume` (`[INV-CTXSRC-02]`). A directive may not use `Overflow::Drop`.
4. **Return a pure body.** `block` and `is_active` read `ContextInputs` only:
   no clock, no filesystem, no network, no interior mutability
   (`[INV-CTXSRC-08]`). The render memo is keyed on the inputs hash, so an impure
   read serves a stale body silently.
5. **Update the golden.** `nuo-tests --test integration engine::it::context_prefix`
   pins the rendered prefix and its per-block cost. A prompt change is a reviewed
   diff, never an invisible one.

```rust
struct MyRule;

impl ContextSource for MyRule {
    fn id(&self) -> &'static str { "agent.my_rule" }
    fn topic(&self) -> Topic { Topic::ToolDiscipline }   // 1
    fn inputs(&self) -> &'static [InputKey] { &[InputKey::ToolSurface] } // 2
    fn is_active(&self, inputs: &ContextInputs) -> bool {                // 4
        inputs.tool_surface.has_native(tool::BuiltinTool::Todo)
    }
    fn block(&self, _inputs: &ContextInputs) -> Option<Block> {          // 4
        Some(Block::new("agent.my_rule", "…").with_title("…"))
    }
    fn budget(&self) -> Budget { Budget::elide_to_operator(200) }        // 3
}
```

Register it with `AgentBuilder::add_context_source`, which compiles the whole
declaration set immediately and returns the error at the call site.

### Why there is no second region

A request has one instruction region and one conversation. It has no third,
per-request text channel: the previous design declared a `transient` region for
"this turn only" text, no producer ever wrote to it, and every provider branched
on a string that was always empty. The region, the `Turn` band, the topic, the
input key, and the per-provider branch are deleted together (ADR-0231). A
request-local payload is a message with its own provenance, not a second
instruction channel.

### Why `Position::Frame` exists

`Topic::Identity` and `Topic::Precedence` are not "the most stable blocks"; they
are what the request *is*. They are emitted first, in topic order. A frame source
may declare only `Route`-or-more-stable inputs, because a volatile frame block
would invalidate the whole prefix on every workspace edit (`FrameVolatility`).

`agent.precedence` is therefore unconditional: making the arbitration contract
appear only when a policy file happens to exist is a false economy.

### Why the order looks the way it does

Frame first, then ascending stability, then each source's declared order. The
practical consequence is that the three `Workspace`-stable blocks
(`project.rules`, `project.instructions`, `workspace.topology`) are the tail of
the prefix, so editing `AGENTS.md` does not invalidate the working contract, the
security boundary, or the tool surface.

---

## 5. Reading the plan

```
/context            # open the inspector for the most recent request
/context --json     # the machine form the golden pins
```

Bare `/context` is intercepted locally and opens `DialogKind::ContextPlan`, a
session-scoped inspector that reads the same `ContextPlanView` a non-terminal
client receives over `AgentRequest::QueryContextPlan` (ADR-0220). It answers, in
one screen: what the request costs by layer, what each declared source costs and
*is*, what was cut and how to get it back, and whether the cacheable prefix
moved.

```
Request
Instructions        2,431 tokens (1.2% of window)
Fixed prefix        2,431 tokens · 13 blocks
Model window        200,000 tokens
Instruction ceiling  16,000 tokens
Request total       53,738 tokens
  History           41,207 tokens
  Instructions       2,431 tokens
  Tool schemas       9,412 tokens
Attribution          -6 tokens vs measured region (tokenizers are not additive)

Sources · emission order
  Route 18 tokens · 1 active
 ▸ agent.identity           identity            directive      18  ▏
  Frozen 583 tokens · 3 active
   agent.precedence         precedence          directive     152  ▊
   agent.coding_baseline    tool_discipline     directive     332  ▊▊▊
   security.secrets         secrets             directive      99  ▏
  …
  Workspace 524 tokens · 3 active
   project.rules            project_authority   directive     440  ▊▊▊▊  elided 1204 · read `AGENTS.md` from line 0 for the rest
   project.instructions     project_instructions directive     45  ▏
   workspace.topology       workspace_topology  reference      39  ▏
   security.web_untrusted   untrusted_policy    untrusted       —  inactive for this request

Cacheable prefix
Drift  suffix moved · project.rules
```

`Enter` expands the selected block's **rendered body** in place — the exact
bytes that block contributes, not a re-render — so "is `project.rules` elided at
a sensible boundary?" is answerable without reading source. `Esc` closes the
body first, then the inspector.

Three details are deliberate and are the reason the surface exists:

- **The row order is the request's order.** Nothing sorts by cost: a reader
  scanning down the list is reading the prompt top to bottom
  (`[INV-CTXPLAN-05]`).
- **`inactive` rows are present and explain themselves**, because "why is this
  block not in my prompt" is the most common debugging question and an absent row
  cannot answer it.
- **The attribution residual is a row, not a rounding.** The per-block column is
  an attribution across sources, not a partition of the region: tokenizers are
  not additive, so the sum can differ from the measured instruction cost by a few
  tokens. Showing the difference is the honest alternative to a total that looks
  exact and is not.

`Drift` is where a defect is announced: `DEFECT — a frame/frozen block moved with
no route change` is impossible by construction, so it always means one of three
things — an impure source, an unstable ordering, or a workspace value classified
too stably. It is logged at `warn` every time it happens.

`/context --json` serializes the same view, bodies included. There is exactly
one machine form: a body-less projection beside it would be a second spelling of
the same facts and the beginning of a divergence.

---

## 6. What "one topic, one owner" buys

The defect this design exists to prevent had already shipped twice: the native-tool
preference and the parallelization rule each appeared in two sections, in
near-identical wording, so a model read the same instruction twice and a
maintainer had two places to keep in sync.

Now:

- **Structurally**, `DuplicateTopic` fails compilation and names both owners.
- **Textually**, `no_two_blocks_state_the_same_rule` fails on a repeated
  paragraph, which catches duplication that owns two different topics.

If you genuinely need to replace another source's text, declare `supersedes` and
name it; the compiler picks exactly one winner.

---

## 7. Budgets, loss, and the recovery route

Eviction has always been disciplined (a pruned tool result keeps its handle).
Admission used to be unbounded: the skill catalog silently dropped everything
past 64 entries and project rules had no bound at all.

Every source now declares `Budget { tokens, overflow }`:

| `Overflow` | Emits | Use for |
| :--- | :--- | :--- |
| `Elide { resume }` | a truncated body plus a notice naming the loss and the route back | a document the model can re-read |
| `Index { resume }` | a one-line pointer to a tool that returns the content | regenerable data (topology, a large catalog) |
| `Drop` | nothing | only where the prompt models the absence; never a directive |

`Resume` is `File { path, from_line }`, `Tool { tool }`, `Mention { address }`,
or `Operator` (no in-workspace route exists, so the notice asks a human to
shorten the source).

`ContextBudget::instructions_ceiling_tokens` is the region-wide backstop: 8% of
the model window, clamped to 2k–24k. When the instruction region exceeds it, the
assembler evicts body blocks from the tail — most volatile first — and never the
frame.

### The second limit: bytes

A request is delivered over a transport, and the transport has a ceiling of its
own. Tokens bound what the model can attend to; bytes bound what an edge will
carry. They disagree most on attachments — a screenshot is ~1 600 tokens and
~800 KB of base64 — so a session can sit at 9% of a 1M-token window and still be
a 55 MB body that every request is refused with `413 request_too_large`
(ADR-0243).

Both limits are now budgeted:

| Dimension | Measured by | Relief |
| :--- | :--- | :--- |
| Tokens | `estimate_model_request` | prune stale tool bodies → compact |
| Bytes | `protocol::request_wire_size` | offload the oldest attachments → prune → compact |

Byte relief keeps the newest attachments (the conversation is about them),
preserves all text, and persists each dropped pixel to its content-addressed
artifact path before the invoice names it. The invoice survives in the request,
so the model can read an offloaded image back. A `413` lowers that route's
learned ceiling, so the cost is paid once per route per session rather than on
every turn.

---

## 8. Tool capability, not tool names

The prompt policy used to branch on `name == "find_files"` and five siblings.
Renaming a tool silently deleted its guidance, and activating a deferred MCP tool
moved the cacheable prefix mid-session.

`ContextInputs::tool_surface` is a `ToolSurface`: the declared `BuiltinTool` set,
whether the channel parallelizes, and whether anything spawns a subagent.
`deferred_active` is diagnostic and is excluded from the surface's identity, so
activation cannot invalidate the render memo (`[INV-TOOLSURFACE-03]`).

A third-party tool cannot contribute prompt guidance. That is intentional: it
describes itself in its own `description`, which is the protocol field designed
for it.

---

## 9. Where the code lives

```
crates/protocol/src/context/
  mod.rs        Stability, Topic, Rank, Position — the declaration vocabulary
  plan.rs       Budget, Overflow, Resume, ContextPlan, PrefixDrift

crates/protocol/src/language.rs
  ResponseLanguage — the validated answer-language tag (ADR-0222)

crates/engine/src/cognition/context/
  source.rs     ContextSource, SourceOrder, SourceMeta (derived facts)
  inputs.rs     ContextInputs, InputKey, ToolSurface
  registry.rs   Policy::compile / plan, ordering, ownership, budgets, ceiling
  builtin.rs    the 14 default sources
  inspect.rs    the operator table and the JSON form
  tests.rs      the compiler and plan suite

crates/engine/src/cognition/model_request/
  mod.rs        ModelRequestAssembler::plan → build (the only request path)
```

The request path takes a compiled `Policy` and is total: no `Result`, no panic on
a layout condition (`[INV-CTXSRC-05]`). A broken composition fails agent
construction with the offending ids.

---

## 10. Invariant index

| Invariant | Statement | Enforced by |
| :--- | :--- | :--- |
| `[INV-CTXSRC-01]` | one active owner per topic | `DuplicateTopic` + `no_two_default_sources_share_a_topic` + `inv_ctxtopic_01_*` |
| `[INV-REGION-01]` | one instruction region, one conversation | `InstructionText` is a newtype (ADR-0231) |
| `[INV-CTXSRC-02]` | every source declares a budget and an overflow policy | `EmptyBudget`, `SilentDirectiveDrop`, overflow tests |
| `[INV-CTXSRC-03]` | stability is derived, never claimed | `stability_is_derived_from_the_declared_inputs` |
| `[INV-CTXSRC-04]` | rank is placement | `Topic::rank` + plan assertions |
| `[INV-CTXSRC-05]` | the request path cannot fail on layout | `Policy::compile` is the only constructor |
| `[INV-CTXSRC-06]` | no dead context declaration | `inv_ctxsrc_06_*` governance tests |
| `[INV-CTXSRC-07]` | one budget vocabulary | `inv_ctxsrc_07_one_budget_vocabulary` |
| `[INV-CTXSRC-08]` | `block`/`is_active` are pure | review + the memo-key argument |
| `[INV-BOUNDARY-01]` | a boundary is not named `render` | `Boundary` (ADR-0225) |
| `[INV-ENVELOPE-01]` | one renderer for message envelopes | `inv_envelope_01_*` (ADR-0226) |
| `[INV-TOOLHEADER-01]` | one producer/reader for the tool-result header | `inv_toolheader_01_*` (ADR-0227) |
| `[INV-UNTRUSTED-01]` | one framing for untrusted content | `inv_untrusted_01_*` (ADR-0228) |
| `[INV-CTXTOPIC-01]` | every declared topic/key has a producer | `inv_ctxtopic_01_*` (ADR-0229) |
| `[INV-TOOLSURFACE-01]` | no prompt branch on a tool name | `inv_toolsurface_01_*` |
| `[INV-TOOLSURFACE-03]` | activating a deferred tool cannot move the prefix | `activating_a_deferred_tool_does_not_move_the_instruction_prefix` |
| `[INV-CTXPLAN-01]` | the plan is the request | `the_plan_and_the_request_are_the_same_bytes` |
| `[INV-CTXPLAN-02]` | a `Head` drift is reported, never swallowed | `a_head_drift_is_rendered_as_a_defect` |
| `[INV-CTXPLAN-03]` | persisted identity is stable across builds | `inv_ctxplan_03_*` |
| `[INV-CTXPLAN-04]` | a block's body costs what the block is reported to cost | `the_bodies_are_the_plans_own_rendered_regions` |
| `[INV-CTXPLAN-05]` | one plan, one order (no client-side re-sorting) | `emission_order_is_preserved_verbatim`, `the_rows_follow_the_plans_emission_order_and_name_every_block` |
| `[INV-CTXPLAN-06]` | a rendered body is live only, never durable | `inv_ctxplan_06_*` governance test |
| `[INV-CTXPLAN-07]` | one measurement, many readers | `the_context_plan_dialog_queries_the_plan_on_every_open` |
| `[INV-LANG-01]` | a prompt-bound language is a validated tag | `ResponseLanguage::parse` at load, `config check`, and `/language` |
| `[INV-LANG-02]` | undeclared is not English | `an_undeclared_language_renders_no_block_at_all` |
| `[INV-LANG-03]` | the language rule outranks the surrounding context | `a_declared_language_names_itself_and_disarms_the_english_context` |
| `[INV-LANG-04]` | a language switch moves only the session band | `changing_the_language_moves_only_the_session_band` |
| `[INV-LANG-05]` | one declaration, three surfaces, inherited by subagents | `reseed_instructions` + `bind_response_language` |
