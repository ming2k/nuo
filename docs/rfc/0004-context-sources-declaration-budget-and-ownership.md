# RFC-0004: Context Sources — Declaration, Budget, and Ownership

- **Status:** Migrated to ADR — implemented by
  [ADR-0215](../adr/0215-context-sources-declare-moment-topic-rank-stability-and-budget.md),
  [ADR-0216](../adr/0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md),
  [ADR-0217](../adr/0217-context-projection-is-inspectable-and-prefix-drift-is-reported.md)
- **Revision:** Final
- **Date:** 2026-12-19
- **Related:** ADR-0153 (retention and the escape hatch),
  ADR-0164 (instructions are one pipeline), ADR-0165 (user instructions),
  ADR-0172 (containers are regions), ADR-0173 (thread is the durable SSOT),
  ADR-0179 (one capability, one producer), ADR-0180 (single ownership of shared
  vocabulary), ADR-0182 (no dead context substrate), ADR-0199 (coding baseline
  and skill catalog), ADR-0203 (nested project rules load on first read),
  ADR-0214 (thresholds follow the model window)
- **Supersedes in part:** `InstructionContext` as an open bag of `pub` fields,
  `InstructionSection::is_transient`, `TEMPORARY_CONTEXT_BUDGET_BYTES`, the
  tool-name string branches in the prompt policy, and the unenforced prose
  `agent.precedence` as the repository's only precedence mechanism
- **Implementation note:** all six phases landed. The vocabulary is
  `protocol::context`; the compiler, default policy, and inspector are
  `engine::cognition::context`; `ToolSurface` replaced `tool_names`; the request
  path takes a compiled `Policy` and cannot fail on layout; `cognition/budget`
  and `InstructionSection::is_transient` are deleted; `/context` (with `--json`)
  renders the live plan and `RequestProjection` durably records it with the
  drift classification. See the three ADRs above for what was ratified and what
  was discarded.

## 0. Recommendation (TL;DR)

Keep the pipeline. Make every context element *declare itself*.

1. **`Stability` is the one field that decides region, order, and cache
   expectation.** `Turn` is transient; everything more stable is `fixed` and
   sorts ascending. Delete `is_transient()` — the region becomes derived, so
   "transient but frozen" stops being representable.
2. **`Topic` is a closed vocabulary with exactly one active owner.** Two active
   sources claiming one topic is a build error unless one declares
   `supersedes`. This is ADR-0179/ADR-0180 applied to prompt content, which is
   the one shared vocabulary in this repository that still has no owner.
3. **`Rank` routes placement and is enforced, not labelled.** `Directive` never
   becomes conversation content; `Untrusted` never enters the cacheable prefix.
   Today `BlockRender::Xml("untrusted")` is, by RFC-0002 §6's own admission, "a
   signal, not a hard boundary".
4. **`Budget` is mandatory per source, token-denominated, with a declared
   overflow policy that always emits a notice naming the loss and its recovery
   path.** Plus one model-relative ceiling for the instruction region as a whole,
   which does not exist today.
5. **The registry is a compiler with a validated state.** `Policy::compile()`
   proves ordering, ownership, stability order, and rank placement once; the
   request path then assembles infallibly. The `panic!("invalid instruction
   layout")` on the hot path becomes unreachable by construction.
6. **Five lifecycle moments are enumerated and named.** Every context mutation
   must name one. New features that "just append something" have nowhere to
   hide.
7. **The plan is an artifact.** `/context` renders every block with its topic,
   owner bounds, cost, truncation, and the reason it is active; the persisted
   `prefix_fingerprint` is compared across requests so prefix drift is reported
   instead of silently re-billed.

The core rule:

> **Every element names its moment, its topic, its rank, its stability, and its
> budget — or it does not reach the model.**

---

## 1. Motivation

### 1.1 What exists

Five channels reach the model today, and they are genuinely well-factored:

| Channel | Mechanism | Written by |
| :--- | :--- | :--- |
| Cacheable text | `fixed: Vec<Block>` | `cognition/model_request/policies.rs` |
| Request-local text | `transient: Vec<Block>` | unused |
| Request-local messages | `temporary_context: Vec<Message>` | `HookPhase::ProjectTemporaryContext` |
| Conversation | `messages: Vec<Message>` | thread facts |
| Tool declarations | `tools: Vec<ToolSpec>` | admitted toolset |

The assembly is pure (`InstructionContext` has no `&Agent`), deterministic
(`render_blocks` owns all presentation), and memoized. `RequestBuilder` is the
single builder for live, replay, and branch switching
(`crates/protocol/src/request_builder.rs:1-16`). That substrate is the reason
this RFC is about *declaration* and not about *plumbing*.

### 1.2 Nine verified defects

Every item below is reproducible from the tree at the time of writing.

**D1 — Two spellings of one lifecycle moment.** `InstructionSection::is_transient`
has no production implementation (`crates/engine/src/cognition/model_request/instruction.rs:84`;
its only implementor is a test at `:425`). `HookOutcome::TemporaryContext` has no
production producer either (`crates/protocol/src/extension.rs:137` is a test
dummy). Worse, both are invoked from the *same* moment — `build_regions` inside
`assemble_prepared` (`model_request/mod.rs:70-73`) and the extension loop inside
`model_request` (`cognition/agent/state.rs:657-671`). Two APIs, one moment, zero
producers: a new contributor must guess which one is canonical, and the guess
has no test to confirm it.

**D2 — Topic duplication is already shipping.** Two independent topics are stated
twice in every request:

- native-tool preference: `agent.coding_baseline`
  (`policies.rs:88-92`, "`find_files` with `max_depth=1` instead of `ls`…
  `search_text` instead of `grep`/`rg`… `read` instead of `cat`") and
  `workspace.topology` (`policies.rs:302-316`, the same three paragraphs
  verbatim);
- parallelization: `policies.rs:94` and `policies.rs:297-300`.

Secrets and untrusted data are likewise stated in `policies.rs:99` and again as
`security.web_untrusted` (`policies.rs:349-359`). Nothing detects this. The
registry checks `id` uniqueness only
(`instruction.rs:185-202`); semantic ownership is unowned.

**D3 — The prompt branches on tool names, which the repository forbids.**
`policies.rs` conditions guidance on `name == "find_files"`, `"search_text"`,
`"read"`, `"code_query"`, `"read_url"`, `"search_web"` (`:88`, `:302-317`,
`:345-347`). `[INV-TUNE-01]` and its governance test
(`tests/integration/governance/tuning_firewall.rs:55`) exist precisely to ban
framework-core branches on a vendor/family string; the same defect is present,
unbanned, in the file that writes the prompt. Consequence: renaming a tool
silently deletes its guidance, and no test fails. It also makes `tool_names`
volatile: activating one deferred MCP tool through `tool_search` changes the
rendered `fixed` prefix mid-session.

**D4 — `fixed` is not route-stable, and drift is unobservable.** `fixed`
contains `project.rules` and `workspace.topology`, both of which change when a
workspace file changes — `workspace_topology()` re-reads the root manifest on
mtime (`state.rs:452-457`), and nested project rules load on first read
(ADR-0203). Nothing declares which blocks are stable, nothing orders by
stability, and nothing reports that the provider's cached prefix was
invalidated. `RequestProjection::prefix_fingerprint` is persisted per
`(session, round, turn)` (`crates/protocol/src/request_projection.rs:22`,
`model_request/mod.rs` → `fire_request_projection_persist`,
`agent/rounds/lifecycle.rs:1316`) and **no code compares two consecutive
fingerprints**. The evidence needed to see a cache loss is already stored and
never read.

**D5 — Admission is unbounded; eviction is disciplined.** The eviction path
names what it lost and keeps a handle
(`protocol/src/pressure.rs:594-610`, `cleared_placeholder_with_reason`,
`truncate_middle` with a `call_id`). The admission path does the opposite:
- `skills.catalog` silently drops everything past 64 entries
  (`agent/state.rs:524`);
- `project.rules` (`AGENTS.md`) has no bound at all — RFC-0002 §6 asked for one
  and it never landed;
- project/user instructions have no bound;
- the only bound that does exist is `TEMPORARY_CONTEXT_BUDGET_BYTES = 8 * 1024`,
  **denominated in bytes** (`agent/state.rs:1526`), in a codebase whose
  `pressure.rs:147-150` states in prose that bytes answer a transport question
  and every context budget is token-denominated.

**D6 — A layout error panics on the request path.** `try_register` validates
only id uniqueness; an anchor that names nothing, or a cycle, is discovered by
`build_regions` (`instruction.rs:281-293`) and the caller turns it into
`panic!("invalid instruction layout: {error}")`
(`model_request/mod.rs:73`, `:113`). A third-party section with one mistyped id
takes down the server from inside a request.

**D7 — The render contract is unstated but assumed.** The memo key is
`hash(InstructionContext)` (`instruction.rs:246-248`), so a section that reads
the clock, the filesystem, or a network resource returns a stale cached body and
nothing fails. `INV-FORMAT-02` requires deterministic bytes; the trait does not
require purity, and nothing checks it.

**D8 — Dead context substrate, against the repository's own invariant.**
`cognition/budget/mod.rs` (122 lines) declares `ContextBudgetManager`,
`BudgetTier`, thresholds at 60%/75%, and has **zero consumers** outside its own
test. `InstructionContext::has_subagent_tool` is computed on every request
(`state.rs:535`) and read by nobody (two sites in the tree: the declaration and
the assignment). `[INV-CONTEXT-06]` bans "no dead context substrate"; it is
scoped to a retired module and does not reach a dead `pub` type or a dead field.

**D9 — The extension surface is unproven.** `register_instruction_section`,
`disable_instruction_section`, `order_instruction_section`, and
`with_instruction_registry` (`cognition/agent/mod.rs:986-1022`) have **no
production caller** — the sole composition root builds the agent with
`Agent::builder_from_toolset(...).with_host(...).with_skills(...)`
(`server/src/composition/bootstrap.rs:751-757`) and never customizes the
registry. The API that is supposed to keep this design honest is exercised only
by integration tests, so no real product requirement has ever stress-tested it.

### 1.3 Why these cannot be fixed piecemeal

D2, D5, and D6 are each a missing *proof obligation* on one declaration. Adding
a lint for D2 does not stop D5; adding a bound for D5's catalog does not stop
`AGENTS.md`; moving the panic in D6 to `try_register` still lets a *runtime*
inconsistency (D1, D3, D4) through. What is missing is not a check but a place
to put checks: a source that declares what it is, so that every check has a
field to read. That is the whole of this RFC.

---

## 2. Goals and Non-Goals

### Goals

- One declaration per context element, carrying moment, topic, rank, stability,
  budget, and overflow.
- Structural ownership: a duplicated topic is a compile-time failure, not a
  review comment.
- Stable prefix by construction, with drift reported rather than absorbed.
- Every loss visible to the model and to the operator, with a recovery path.
- An infallible request path.
- An inspectable plan: every block's cost and reason, on demand and in CI.
- An authoring path a new contributor can follow without reading 213 ADRs.

### Non-Goals

- Rewriting the ReAct loop, the thread model, or the provider projectors.
- A general prompt templating language. Bodies stay strings.
- Solving prompt injection. This RFC makes placement enforceable; it does not
  claim an untrusted string can never influence a model.
- A user-facing knob for any of it. Per ADR-0214, these are algorithm constants
  derived from the model window, not preferences.
- Preserving `is_transient`, `ContextBudgetManager`, or `has_subagent_tool`.

---

## 3. Principles

1. **A context element is a claim on a finite budget.** Therefore it has an
   owner, a topic, a lifetime, a rank, and a cost. An element missing any of the
   five is not a context element; it is a bug.
2. **Check once, then be infallible.** Validation belongs to policy
   construction, not to the request path.
3. **Structure decides what reaches the model; prose decides how the model
   arbitrates.** Never conflate them. `supersedes` is structure; "safety wins"
   is prose. Today `agent.precedence` is asked to do both.
4. **Loss is never silent, and every loss names its way back.**
5. **Volatility orders the prefix.** The cheapest cache invalidation is the one
   that only reaches the suffix.
6. **The context is an index; the workspace is the store.** Anything above its
   budget becomes a pointer, not a paragraph.
7. **If it cannot be inspected, it is not designed.**

---

## 4. Normative Invariants

- **`[INV-CTXSRC-01]` Single Topic Owner** — At most one *active* source per
  `Topic`. Two sources may declare the same topic only if one names the other in
  `supersedes`, and the compiler picks exactly one winner. Enforced by
  `InstructionRegistryError::DuplicateTopic` and a golden test over the default
  policy.
- **`[INV-CTXSRC-02]` Declared Budget** — Every source declares a token budget
  and an overflow policy; the assembler enforces both. No source may exceed its
  budget silently, and the instruction region as a whole is bounded by a
  model-relative ceiling.
- **`[INV-CTXSRC-03]` Stability Order** — Within `fixed`, blocks are ordered by
  ascending `Stability`, then by declared order. A source whose declared inputs
  are more volatile than its position implies is a compile error.
- **`[INV-CTXSRC-04]` Rank Is Placement** — `Rank::Untrusted` never enters
  `fixed`. `Rank::Directive` never becomes a conversation message. Both are
  asserted over the rendered request, not documented.
- **`[INV-CTXSRC-05]` Infallible Request Path** — Policy validation happens in
  `compile()`; no request-path function may panic on a layout condition.
- **`[INV-CTXSRC-06]` No Dead Context Declaration** — Every `ContextInputs`
  accessor has a production reader; every `context` submodule is reachable from
  a production path. Supersedes and extends `[INV-CONTEXT-06]`.
- **`[INV-CTXSRC-07]` One Budget Vocabulary** — Context thresholds and
  watermark fractions exist only in `protocol::CompactionPolicy` /
  `protocol::ContextBudget`. A numeric watermark elsewhere is a governance
  failure.
- **`[INV-CTXSRC-08]` Purity** — A `render` implementation is a function of
  `ContextInputs` alone: no clock, no filesystem, no network, no random, no
  interior mutability.

---

## 5. Design

### 5.1 The five lifecycle moments

Every context mutation names one of these. This is the mental model the
repository currently lacks, and the reason new features each invent their own
insertion point.

| Moment | When | May be lossy | Side effects |
| :--- | :--- | :--- | :--- |
| **M1 Assemble** | per request | no | none (pure) |
| **M2 Admit** | before a round, on a durable fact | no | persists a `ThreadFact` |
| **M3 Relieve** | between turns | yes, with a notice | persists a projection checkpoint |
| **M4 Evict** | mid-round | yes, must preserve the uncommitted tail | checkpoint |
| **M5 Resume** | hydrate, replay, branch switch | rebuilds projections | none — never re-runs M2 |

M1 is the moment this RFC governs. M3/M4 already have the right shape
(`orchestration/context_gate.rs:13-36`); the only change there is that the
relief ladder's thresholds come from one vocabulary (`[INV-CTXSRC-07]`).

### 5.2 `Stability` is the region, the order, and the cache claim

```rust
/// When a value changes, and therefore how much cached prefix it can invalidate.
/// Ordering is meaningful: ascending = more stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stability {
    /// Never changes for a given build (a compiled-in contract).
    Frozen,
    /// Changes when identity, role, or provider route changes.
    Route,
    /// Changes on a session-scoped decision (`/model`, `/role`, trust grant).
    Session,
    /// Changes when a workspace file changes (`AGENTS.md`, manifest, skills).
    Workspace,
    /// Changes every request. This is the transient region.
    Turn,
}
```

Three consequences from one field, replacing three declarations:

- **region** — `Turn` is `transient`; everything else is `fixed`. `is_transient()`
  is deleted because the illegal combination ("transient but frozen") becomes
  unrepresentable.
- **order** — `fixed` sorts by ascending `Stability`, so a workspace edit
  invalidates only from the first `Workspace` block onward and the provider's
  cached prefix keeps the `Frozen`/`Route`/`Session` head.
- **cache claim** — the assembler knows what a re-render should and should not
  cost, which is what makes D4 reportable.

**Consequence: the default order is re-derived, not preserved.** Ascending
stability is a dominant constraint, and today's anchor chain is not consistent
with it. Declaring honest stability for the 11 defaults yields:

| Band | Sections |
| :--- | :--- |
| `Route` | `agent.identity`, `agent.precedence`, `agent.coding_baseline`, `security.web_untrusted`, `provider.guidance` |
| `Session` | `user.instructions`, `skills.catalog`, `workspace.roots` |
| `Workspace` | `project.rules`, `project.instructions`, `workspace.topology` |

Five of the eleven blocks change position relative to today. Two readings:

- **Cost.** The prompt order changes once, in a reviewed golden diff. Semantics
  do not: nothing in the prose depends on `project.rules` immediately following
  `user.instructions`.
- **Benefit.** An `AGENTS.md` edit no longer invalidates the security boundary,
  the provider notes, the skill catalog, or the extra roots — it invalidates only
  the three `Workspace` blocks at the end of the prefix.

There is also a correctness argument for the new order: `agent.precedence` states
that instructions run "from general to specific", and the stability bands happen
to implement exactly that (`Route` → `Session` → `Workspace`), which the current
anchor chain does not. The default policy uses no `Frozen` block; that band is for
a compiled-in embedding policy (exactly what
`tests/integration/engine/it/system_prompt_builder.rs` exercises today).

This does not re-litigate ADR-0172. That decision was that the *container* is the
wire region and that `RequestRegion`/`WireShape`/`TextRole`/`Trust` duplicated
information already carried by container membership. `Stability` is not carried
by membership: `fixed` today holds both compiled-in contracts and
mtime-sensitive workspace summaries, and nothing distinguishes them. `Stability`
adds information instead of renaming it — and it removes a second field
(`is_transient`) rather than adding a parallel one.

### 5.3 `Topic` is the ownership vocabulary

```rust
/// What a context element is *about*. One active owner per topic.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Topic {
    Identity,
    Precedence,
    ToolDiscipline,
    Delegation,
    ExplorationProtocol,
    UserAuthority,
    UserInstructions,
    ProjectAuthority,
    ProjectInstructions,
    SkillCapability,
    WorkspaceBoundary,
    WorkspaceTopology,
    UntrustedPolicy,
    ProviderDialect,
    Memory,
    Retrieval,
    TurnDirective,
}
```

Closed on purpose. Adding a topic is a one-line edit to `protocol`, which is the
review gate: an author who wants new prompt text must decide whether it belongs
to an existing topic or is genuinely new. Open `&'static str` topics would let
`ToolDiscipline` be spelled `tool.discipline` and `tool_discipline` in two
crates — the exact failure mode `[INV-VOCAB-02]` already bans for types.

With this, D2 becomes a compiler error:

```
error: two active sources own Topic::ToolDiscipline: "agent.coding_baseline",
       "workspace.topology". Declare `supersedes` or narrow with `is_active`.
```

Topics are declared in `nuo-protocol` because embedders register sources through
the public builder API; the registry that enforces them stays in `nuo-engine`.
That is the existing ownership split (`Block` and `ScopedInstruction` are already
in `protocol`).

### 5.4 `Rank` routes placement

```rust
pub enum Rank {
    /// Shapes model behavior. Always instruction text, never a message.
    Directive,
    /// Data the model may consult. Instruction text if small, a message if large.
    Reference,
    /// Data that must never be interpretable as an instruction.
    Untrusted,
}
```

Placement rules the assembler enforces:

| Rank | `fixed` | `transient` | `messages` |
| :--- | :-: | :-: | :-: |
| `Directive` | yes | yes | **no** |
| `Reference` | yes | yes | yes |
| `Untrusted` | **no** | yes | yes (tool-result shape) |

ADR-0172's decision (option 1) and RFC-0002 §6 both say trust is conveyed by
placement and by the block's boundary choice, and RFC-0002 §6 immediately
qualifies it:
"`BlockRender::Xml("untrusted")` is a signal, not a hard boundary." A signal
that no code reads is a comment. `Rank` turns the signal into the routing
decision plus the assertion (`[INV-CTXSRC-04]`), which is a behaviour change
rather than a new label — the distinction that made the ADR-0172 deletions
correct.

### 5.5 `Budget` and the disclosure ladder

```rust
pub struct Budget {
    /// Token ceiling for this source's rendered body. Not bytes.
    pub tokens: usize,
    pub overflow: Overflow,
}

pub enum Overflow {
    /// Truncate at a structural boundary and append a notice naming the loss
    /// plus how to obtain the rest.
    Elide { resume: Resume },
    /// Replace the body with a one-line index entry the model can dereference
    /// with a tool.
    Index { resume: Resume },
    /// Drop the element entirely. Legal only for a topic whose absence the
    /// prompt models (for example an empty catalog).
    Drop,
}

pub enum Resume {
    /// A workspace file the model can `read` with a line offset.
    File { path: &'static str, from_line: usize },
    /// A tool call that returns the remainder.
    Tool { tool: &'static str },
    /// An `@`-addressable asset.
    Mention { address: &'static str },
}
```

`[INV-CTXSRC-02]` adds the piece that does not exist today: **a ceiling for the
instruction region as a whole**, derived from the model window exactly as the
other thresholds are (ADR-0214), for example

```rust
// protocol::ContextBudget
pub instructions_ceiling_tokens: usize, // min(8% of window, 24_000)
```

Without it, a 200 KB `AGENTS.md` plus 64 skills plus scoped instructions can
consume the window before the conversation starts, and every downstream budget
inherits the error. With it, the assembler drops blocks in reverse stability
order (most volatile first) until the region fits, emitting one notice per drop.

Two live instances become conforming immediately:

- `skills.catalog` (`state.rs:524`) keeps its 64-entry cap but must render
  "(showing 64 of N)" — the model currently cannot tell that a capability
  exists and was hidden from it;
- `TEMPORARY_CONTEXT_BUDGET_BYTES` (`state.rs:1526`) becomes a token budget,
  because in this repository bytes answer a transport question
  (`pressure.rs:147-150`).

### 5.6 The source trait

```rust
pub trait ContextSource: Send + Sync {
    /// Stable identity for override, disable, telemetry, and `supersedes`.
    fn id(&self) -> &'static str;

    /// Exactly one active owner per topic (`[INV-CTXSRC-01]`).
    fn topic(&self) -> Topic;

    /// The inputs this source may read. Declared so the compiler can derive
    /// stability, measure cost, and prove nothing else was read.
    fn inputs(&self) -> &'static [InputKey];

    /// Placement inside one stability band.
    fn order(&self) -> InstructionOrder { InstructionOrder::Tail }

    /// Sources this one replaces when both are active. A same-topic pair must
    /// declare a winner in at least one direction.
    fn supersedes(&self) -> &'static [&'static str] { &[] }

    fn is_active(&self, inputs: &ContextInputs) -> bool { true }

    /// PURE (`[INV-CTXSRC-08]`): a function of `inputs` only.
    fn render(&self, inputs: &ContextInputs) -> Option<Block>;

    /// Required. An unbounded source is the defect this trait exists to prevent.
    fn budget(&self) -> Budget;
}
```

Four deliberate omissions:

- **no `is_transient`** — derived from `Stability`, which is derived from
  `inputs()`;
- **no `stability`** — derived. A source cannot claim more stability than its
  inputs have, so a `Workspace`-volatile body cannot sit in the `Frozen` band;
- **no region parameter** — the region is not a choice a source makes;
- **no default `budget`** — a default would be inherited by every future source
  and would recreate D5.

`Rank` is derived from `topic()` through a total function
(`Topic::rank()`), not declared per source. A source cannot mislabel a `Directive`
as `Reference` to smuggle directive text into a message; a new topic must choose
a rank when it is added to the vocabulary.

`inputs()` is a declaration, not a capability boundary: `ContextInputs` is passed
whole, so a source *can* read an input it did not declare. The declaration is
what the compiler uses to derive stability and cost; the proof that a source read
nothing else is a test-only probe — with the probe enabled `Source<T>`'s `Deref`
records into a thread-local, and the test asserts the recorded set is a subset of
`inputs()`. Two levels, because a real capability boundary would need 12 separate
accessors and would cost the `Deref` ergonomics that keep section bodies small.

The rank mapping has exactly four non-default entries: only `Topic::Retrieval`
may map to `Rank::Untrusted`; `Topic::Memory`, `Topic::SkillCapability`, and
`Topic::WorkspaceTopology` map to `Rank::Reference`; every other topic maps to
`Rank::Directive`. A topic whose content is genuinely untrusted data cannot be
added without a matching `Untrusted` mapping in the same edit — which is the
review the design wants.

### 5.7 `ContextInputs`: a value that carries its own provenance

Today's `InstructionContext` (`instruction.rs:21-49`) is a flat struct of 12
`pub` fields. It works, but a field has no owner, no cost, and no stability, so
the assembler cannot reason about it and a dead field is invisible (D8).

```rust
/// One typed input, with the metadata the assembler needs to order, cost, and
/// report it. `Deref` keeps section bodies unchanged: `inputs.project_rules.trim()`
/// still compiles.
pub struct Source<T> {
    value: T,
    stability: Stability,
    provenance: Provenance,
}

pub enum Provenance {
    Config(&'static str),
    WorkspaceFile(PathBuf),
    Route { provider: String, model: String },
    SessionDecision(&'static str),
    Runtime,
}
```

`InstructionContext` is renamed `ContextInputs`, every field becomes `Source<T>`,
and `tool_names: Vec<String>` becomes:

```rust
pub struct ToolSurface {
    /// Declared native capabilities, not names. Prompt policy branches on these.
    native: BTreeSet<BuiltinTool>,
    /// Deferred tools currently activated. Never consulted by prompt policy.
    activated: BTreeSet<String>,
    parallel: bool,
    /// Whether any admitted tool spawns a subagent. Gives the dead
    /// `has_subagent_tool` an owner (Topic::Delegation) or deletes it.
    spawns_subagent: bool,
}
```

`ToolSurface` is what fixes D3: `policies.rs` may still *write* the string
"`search_text`" in a body, but it cannot *branch* on it, because the raw name
list is no longer reachable from a prompt source. A rename can no longer silently
delete guidance, and a deferred-tool activation can no longer mutate the
instruction prefix.

### 5.8 The compiler, and the infallible request path

```rust
pub struct Policy(/* private */);

impl Policy {
    /// Proves every invariant that can be proven without a live turn.
    pub fn compile(sources: Vec<Box<dyn ContextSource>>) -> Result<Self, ContextPolicyError>;
}

impl Policy {
    /// Total. No `Result`, no `panic!`. Ordering, ownership, and stability are
    /// already decided; only `is_active` and `render` remain.
    pub fn plan(&self, inputs: &ContextInputs) -> ContextPlan;
}

pub struct ContextPlan {
    pub blocks: Vec<PlannedBlock>,   // ordered, measured, with notices
    pub fixed: Vec<Block>,
    pub transient: Vec<Block>,
    pub total_tokens: usize,
    pub drift: PrefixDrift,          // which bands re-rendered since last request
    pub notices: Vec<ContextNotice>,
}
```

`ContextPolicyError` covers `DuplicateId`, `DuplicateTopic`, `UnresolvedSupersede`,
`MissingAnchor`, `Cycle`, `StabilityOrder`, `RankPlacement`, `EmptyBudget`.

The two `panic!("invalid instruction layout")` sites
(`model_request/mod.rs:73`, `:113`) become unreachable because the only route to
a `Policy` is `compile`. `assemble_prepared` and `assemble_thread` lose their
`Result`/panic duality and simply take a `&Policy`. `is_active` remains runtime
dependent, which is why `DuplicateTopic` is proven for *declared* topics plus
`supersedes`, not for liveness.

The memo key changes from `hash(InstructionContext)` to
`(hash_of_declared_inputs, policy_generation)` where `policy_generation`
increments on `register`/`disable`/`set_order`. Today a stale memo after an
ordering change is impossible only because every mutator remembers to call
`invalidate_memo` (`instruction.rs:204-208`); a generation counter cannot be
forgotten.

### 5.9 Drift is reported, not absorbed

At M1 the assembler compares the new `prefix_fingerprint`
(`protocol/src/provider_state.rs:230`) with the previous one for the same thread:

- fingerprint unchanged → `PrefixDrift::None`;
- fingerprint changed and the changed bands are all `>= Stability::Workspace` →
  `PrefixDrift::Suffix { blocks: [..] }`;
- a `Frozen`/`Route` block changed while the route did not → `PrefixDrift::Head`,
  reported as a defect because it should be impossible.

`/context` shows it, the status line can surface it, and one governance test
asserts that the default policy's `Head` band is hash-stable across a workspace
mutation (the test edits a temp `AGENTS.md` and asserts the `Frozen`/`Route`
prefix bytes are identical).

This is the cheapest high-value item in the RFC: the fingerprint already exists
and is already persisted.

### 5.10 Inspection

```
$ nuo context
Region     Tokens   Blocks
fixed       3,142        8
transient       0        0
messages   41,208       58
tools       6,744       23
─────────────────────────────────────────
total      51,094 / 128,000  (40%)   prune@83,200  compact@108,800

fixed                      (ascending stability, then declared order)
  #  id                     topic              rank       stability  tok  note
  1  agent.identity         Identity           Directive  Route       12
  2  agent.precedence       Precedence         Directive  Route       71  structure resolves placement
  3  agent.coding_baseline  ToolDiscipline     Directive  Route      396  merged, was 438 + 42
  4  user.instructions      UserInstructions   Directive  Session    220
  5  skills.catalog         SkillCapability    Reference  Session    318  64 of 91 shown
  6  workspace.roots        WorkspaceBoundary  Reference  Session      0
  7  project.rules          ProjectAuthority   Directive  Workspace  902  AGENTS.md elided 1,204→902
  8  workspace.topology     WorkspaceTopology  Reference  Workspace  642  exploration merged out

  inactive
     security.web_untrusted UntrustedPolicy    Directive  Route          no web tool admitted
     provider.guidance      ProviderDialect    Directive  Route          provider declares none
     project.instructions   ProjectInstr…      Directive  Workspace      `.nuo/instructions` empty

drift: suffix (project.rules, workspace.topology) changed since round 41
```

`nuo context --diff <round>` renders only the changed blocks. `nuo context
--json` is the machine form used by the golden tests. The persisted side reuses
`RequestProjection` (already keyed by `(session, round, turn)`) extended with
`blocks: Vec<PlannedBlockRecord>` and `drift`.

Note the arithmetic the operator can now check: the merged `ToolDiscipline`
(396) plus the shrunk `WorkspaceTopology` (642) is 1,038 tokens where the
duplicated pair cost 1,380, and the diff is visible in review instead of
accumulating silently. `inactive` rows matter as much as active ones: they are
how a contributor sees that a source exists and why it is not contributing.

### 5.11 The authoring path

This is the part the design exists to make obvious. One table answers "where does
my new context go".

| You want to… | Moment | Channel | What you touch |
| :--- | :--- | :--- | :--- |
| Add a standing rule for every request | M1 | `fixed` | one `ContextSource`, a `Topic`, a `Budget` |
| Add a rule for one role/provider/model | M1 | `fixed` | `.nuo/instructions/*.md` or `[[instructions.user]]` — **no new source** |
| Add a rule for this turn only | M1 | `transient` | a source with `Turn` stability |
| Add data fetched for this turn | M1 | `transient` | extension `ProjectTemporaryContext` (budgeted) |
| Record something the user said | M2 | `messages` | `conversation_context`, with an `InjectionKind` |
| Shrink an old tool result | M3/M4 | `messages` | `CompactionPolicy.prune_*` — never a source |
| Shrink history | M3 | `messages` + `Compaction` fact | `CompactionPolicy` |
| Make a large thing discoverable | M1 | index + tool | `Overflow::Index` |
| Change what wins on conflict | — | structure + prose | `supersedes()` for text; `agent.precedence` for behaviour |

And the five steps for the common case, all enforced:

1. **Pick the topic.** If an existing topic fits, you are editing a source, not
   adding one — which is the whole point of D2.
2. **Declare inputs.** The compiler derives stability; you cannot claim more than
   your inputs have (`[INV-CTXSRC-03]`).
3. **Declare a budget.** Required, token-denominated, with a `Resume` for any
   `Elide`/`Index` (`[INV-CTXSRC-02]`).
4. **Render a pure body.** The `context` directory is scanned for clock/fs/net
   calls (`[INV-CTXSRC-08]`).
5. **Update the golden.** `nuo context --json` is pinned; adding prompt text is a
   visible diff in review, never an invisible one.

A `Turn` source is not a special kind of source: it reads a turn-scoped input
(`InputKey::TurnDirective`) that the agent fills per request, which is also why
M1 is the only moment a transient block can be born.

### 5.12 Module layout

`protocol` owns the vocabulary; `engine` owns the registry. No new crate.

```
crates/protocol/src/context/
  mod.rs        // Stability, Topic, Rank, Budget, Overflow, Resume, ContextNotice
  plan.rs       // ContextPlan, PlannedBlock, PrefixDrift  (wire-neutral)

crates/engine/src/cognition/context/
  mod.rs        // the one entry point: Policy, ContextPlan construction
  source.rs     // ContextSource, InputKey
  inputs.rs     // ContextInputs, Source<T>, ToolSurface, Provenance
  registry.rs   // registration, compile(), supersede resolution, memo generation
  builtin.rs    // the default sources (today's policies.rs)
  inspect.rs    // human table, --json, --diff
```

`cognition/model_request/` keeps only the assembler (`ModelRequestAssembler`),
which now takes a `&Policy`. This is a move plus a rename, not a redesign.

---

## 6. Security, Reliability, and Failure Modes

- **Untrusted content in the prefix.** `[INV-CTXSRC-04]` asserts that no
  `Rank::Untrusted` block reaches `fixed`, over the rendered request, for every
  registered source. A new source cannot opt out by declaring `Rank::Directive`,
  because rank comes from the topic.
- **Closing-tag injection.** `render_blocks` escapes attributes but not bodies;
  an `AGENTS.md` containing `</policy>` can end a trusted envelope early. Add a
  per-request nonce for trusted envelopes whose body came from a workspace file,
  or reject a body containing its own closing tag. Pick one; do both if cheap.
- **Budget exhaustion as a denial of service.** A workspace file cannot exceed
  the instruction ceiling: the assembler stops reading at the source's budget,
  so a 200 MB `AGENTS.md` costs a bounded read. The `Elide` notice must not cut a
  code fence (RFC-0002 §6, still open) — truncate on a line boundary and close
  any open fence.
- **Policy compile failure at startup.** `compile()` failing is a configuration
  error and must fail the agent's construction loudly with the source ids and a
  remediation hint, not degrade to "no prompt". A bare session with no system
  message is the exact defect ADR-0199 fixed.
- **Supersede cycles.** `A supersedes B` and `B supersedes A` on the same topic
  is `UnresolvedSupersede`, not "pick the first".
- **Determinism across processes.** The in-process memo may keep
  `DefaultHasher`; the *persisted* identity is `prefix_fingerprint` (SHA-256).
  Persisting a `DefaultHasher` value would be a cross-version hazard, so
  `PlannedBlockRecord` carries the SHA-256 prefix, never the memo key.
- **Partial extension output.** A `ProjectTemporaryContext` producer that
  exceeds its budget is truncated with a notice, as today
  (`agent/state.rs:1531-1542`), but in tokens and attributable to the extension's
  id, so `/context` can say which extension spent the budget.

---

## 7. Migration and Compatibility

Six phases, each independently shippable and each leaving the tree green.

**Phase 0 — delete the dead substrate.** Delete `cognition/budget/mod.rs`
(`[INV-CTXSRC-07]`); delete `InstructionContext::has_subagent_tool` or give it
`Topic::Delegation`. Extend `[INV-CONTEXT-06]` in `docs/dev/subsystems.md` to
reach dead fields and dead modules, and add the governance test that fails on a
`pub` field with no reader and a `pub mod` with no consumer outside its crate.
No behaviour change.

**Phase 1 — vocabulary.** Add `protocol::context::{Stability, Topic, Rank,
Budget, Overflow, Resume, ContextNotice}` and `Topic::rank()`. No consumers yet;
the compiler enforces nothing. Land the golden harness (`nuo context --json`
rendering `InstructionText` for a fixture workspace) so every later phase shows a
byte diff.

**Phase 2 — declaration without behaviour change.** Add `topic`, `inputs`,
`budget`, `supersedes` to `InstructionSection` with defaults that reproduce
today's output, and populate them for the 11 defaults. Run the golden: bytes
must be identical. This is where D2 is discovered: the golden fails on the two
duplicated topics, so merge them (native-tool preference into `ToolDiscipline`,
parallelization into `ExplorationProtocol`) and re-pin the golden — one
intentional, reviewed diff.

**Phase 3 — the compiler.** Introduce `Policy::compile` and `ContextPlan`; move
the two panic sites to compile-time errors; replace the memo hash with
`(inputs_hash, generation)`. Delete `is_transient`, deriving the region from
`Stability`, and switch `fixed` to stability ordering — the phase-3 golden shows
the re-derived default order described in §5.2, which is the one intentionally
reviewed reorder. Move `policies.rs` → `cognition/context/builtin.rs`.

**Phase 4 — budgets and rank enforcement.** Token-denominate
`TEMPORARY_CONTEXT_BUDGET_BYTES`; add `instructions_ceiling_tokens` to
`ContextBudget`; make `skills.catalog` report `64 of N`; assert rank placement
over the rendered request.

**Phase 5 — `ToolSurface`.** Replace `tool_names: Vec<String>` with
`ToolSurface`; rewrite the tool-name branches in `builtin.rs` to match
`BuiltinTool`. Add the test that a deferred activation does not change `fixed`.

**Phase 6 — inspection.** Ship `/context`, `nuo context`, `--json`, `--diff`;
extend `RequestProjection` with `blocks` and `drift`; surface `PrefixDrift::Head`
as a defect in logs. Update the docs: a new
`docs/explanation/context-engineering.md` (the moments, the five declarations,
the authoring path), index entries for the two orphaned how-tos
(`docs/how-to/index.md` omits `project-instructions.md` and
`user-instructions.md`), and fix `docs/how-to/project-instructions.md:21`
(cites the retired `crates/nuo-harness`, ADR-0181) and `:82` (says instructions
render "in the Base tier", deleted by ADR-0172).

**Compatibility.** `register_instruction_section` keeps its signature; the new
methods have defaults, so an out-of-tree embedder compiles unchanged. Section
*ids* are unchanged, so `disable_instruction_section("project.rules")` keeps
working. `Policy::compile` is additive. No persisted schema changes in phases
0–5; phase 6 extends a table that is already versioned (ADR-0192) and adds a
migration. Because ADR-0214 already removed user-facing context configuration,
there is no config surface to migrate.

---

## 8. Work Plan

| Phase | Deliverable | Rough size |
| :--- | :--- | :--- |
| 0 | dead code deleted, `[INV-CTXSRC-06]` governance test | 1 day |
| 1 | `protocol::context` vocabulary + golden harness | 2 days |
| 2 | 11 sources declare topic/inputs/budget; D2 merged | 3 days |
| 3 | `Policy::compile`, `ContextPlan`, infallible path | 4 days |
| 4 | budgets, ceiling, rank assertions | 3 days |
| 5 | `ToolSurface`, string branches removed | 2 days |
| 6 | `/context`, `--json`, `--diff`, drift, docs | 4 days |

Roughly three weeks of focused work for one engineer, with phases 0–2 landing
customer-visible value (a smaller prompt, no duplicate guidance) before the
structural work lands.

---

## 9. Test Plan

**Unit**

- `compile`: duplicate id, duplicate topic with no `supersedes`, supersede cycle,
  missing anchor, cross-source cycle, stability-order violation, rank violation,
  zero budget.
- `plan`: token measurement per block, `Elide` notice text and resume path,
  `Index` replacement, `Drop` legality, region derivation from `Stability`.
- Memo: generation counter invalidates on register/disable/set_order; a source
  that reads no `Workspace` input cannot land in the `Workspace` band, because
  stability is derived rather than claimed.
- Purity: `[INV-CTXSRC-08]` scan over `cognition/context/`.

**Golden**

- `nuo context --json` for a fixture workspace: 11 declared sources, 8 active
  blocks, exact bytes, exact per-block tokens.
- Byte stability: two compilations of one input are identical.
- Head stability: mutating `AGENTS.md` changes only `Workspace`-band blocks.
- No tool schema in text; no message body in text (RFC-0002 §5.6, still worth
  pinning).
- No duplicate topic among active default sources.

**Governance**

- `[INV-CTXSRC-06]`: every `ContextInputs` accessor has a production reader;
  every `context` submodule is reachable.
- `[INV-CTXSRC-07]`: no watermark fraction literal outside
  `protocol/{pressure,tuning}.rs`; `cognition/budget` stays deleted.
- No prompt source branches on a tool-name string literal (compiles today only
  through `ToolSurface`, so the test is "`tool_names` is not reachable from
  `context`").
- Living docs cite current crate names (extend
  `tests/integration/governance/architecture_convergence.rs:396-403` to
  `docs/how-to/` and `docs/reference/`).

**Behavioural evals**

- Untrusted web content in `transient` cannot redirect the agent; `fixed` never
  contains it.
- Truncated `AGENTS.md` still lets the model retrieve the remainder from the
  named file offset.
- A 64-of-91 catalog still lets the model discover a skill past the cap.
- Same behaviour across Anthropic / OpenAI / DeepSeek for the same inputs.

---

## 10. Alternatives Rejected

- **Keep `is_transient` and make it work.** It encodes one bit of what
  `Stability` encodes, cannot express a cache claim, and can contradict the band
  it sits in. Deriving the region removes an illegal state instead of testing
  for it.
- **A `stability()` method instead of deriving it from `inputs()`.** A source
  would be free to claim `Frozen` while reading a mutable workspace file — the
  precise defect D4 describes. Derivation makes the claim unfalsifiable.
- **Open `&'static str` topics.** Cheaper to add, and recreates D2 with
  different spellings. `[INV-VOCAB-02]` already rejects this shape for types.
- **Per-source `rank()`.** Lets a `Directive` be labelled `Reference` to reach a
  message channel, and vice versa. Rank is a property of the topic.
- **A default `budget()`.** Every future source inherits it, and the default is
  either unbounded (D5 returns) or arbitrary (silently truncates a contract).
- **`Trust` as a separate axis from `Rank`.** Rank *is* the placement decision;
  a second axis would need a mapping table and could disagree with it.
- **A prompt templating language.** Bodies are prose for a model, not a program.
  Determinism comes from the assembler and the golden test.
- **Keeping the panic and adding `catch_unwind`.** Turns a config error into a
  request-time failure with a worse diagnostic.
- **A new `nuo-context` crate.** The registry depends on `Agent` state
  (`is_active` reads turn state), so it belongs below `Agent` but inside
  `engine`. A crate boundary would force a trait-object hop per block for no
  isolation gain.
- **Making the whole instruction region `Session`-stable by rebuilding only on
  route change.** Would require re-reading `AGENTS.md` on every route change and
  would still drift mid-session; ordering by stability is strictly better and
  free.
- **Skipping the golden snapshot in favour of structural tests.** Structural
  tests would have missed D2's three duplicated paragraphs. Only a rendered,
  reviewed diff catches text.

---

## 11. Open Questions

1. **Should `skills.catalog` be `Session` or `Workspace`?** `Session` (as
   proposed) buys a longer stable prefix; `Workspace` is more honest, because a
   skill edit is a workspace change. No measurement of the trade-off yet.
2. **Should `instructions_ceiling_tokens` be 8% of the window, or a fixed 24 k
   with a percentage cap?** The second is safer for small windows; the first
   matches ADR-0214's "derive from the window" rule. Needs one measurement pass
   over the provider matrix.
3. **`Elide` on a code fence:** truncate on a line boundary and close the fence,
   or refuse to start a fence that cannot be closed? The first is simpler; the
   second never produces invalid Markdown.
4. **Closing-tag injection:** escape for trusted workspace documents, or use a
   per-request nonce for every XML envelope? Escaping is cheaper for the common
   case but changes `AGENTS.md` bytes, which some users will notice.
5. **Does `Topic` belong in `protocol` or in `engine`?** In `protocol` today
   because embedders construct sources. If the eventual embedding story turns out
   to be server-side only, `engine` is the narrower home and the vocabulary
   becomes private.
6. **Should `supersedes` be directional per topic or a global priority number?**
   Directional is precise and needs review attention; a priority number is
   compact and invites silent reordering.
7. **Do subagents share `Policy` or each compile their own?** They already share
   inputs and add a role selector (ADR-0164). Sharing one `Policy` with
   role-scoped `is_active` is cheaper; a child that disables a source needs its
   own. Unresolved.
8. **Should the drift report be user-visible by default?** It is a cost signal for
   a cache miss the user did not cause. Loud by default teaches the cost; quiet by
   default avoids noise on every `AGENTS.md` save.
