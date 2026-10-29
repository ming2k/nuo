---
id: ADR-0126
title: "Provider-Owned Model Tunables: An Open Knob Envelope With Framework-Owned Depth Semantics"
status: accepted
date: 2026-10-29
scope: provider/nuo-provider, protocol/nuo-protocol, routing/nuo-model-routing, persistence/nuo-persistence, server/nuo-server, tui/event-loop, presentation/nuo-view, providers/*
superseded_by: null
negative_knowledge: true
---

# 0126. Provider-Owned Model Tunables: An Open Knob Envelope With Framework-Owned Depth Semantics

- Status: Accepted
- Date: 2026-10-29
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Model Routing, Server Core, Presentation
- Extends: [ADR-0113](0113-provider-owned-wire-dialects-route-derivation-never-infers-a-dialect-from-a-model-id.md), [ADR-0114](0114-provider-owned-response-statefulness-remote-continuation-requires-a-declared-stored-surface.md) — those gave the provider ownership of its *wire shape* and *request sequence*; this gives it ownership of the *knobs a user may turn on it*

---

## Context and Problem Statement

Adding a single tuning knob to one provider required edits in six layers. The
knob set — reasoning depth, the extended-thinking switch, and whatever a future
surface exposes (edit format, safety thresholds, image-detail budget) — was
encoded as **named fields on structs owned by the framework**:

| Layer | Legacy carrier of a knob |
|---|---|
| Transport (how a request is sent) | `Transport::OpenAiResponses { effort, … }` |
| Resolved capabilities | `ModelCapabilities` (no knob field at all) |
| Wire DTO to the UI | `ProviderModelInfo { effort, thinking }` |
| Persistence | `RouteSettings { effort, thinking }` |
| Picker projection | five per-protocol `match` arms re-deriving the value |
| Presentation | `focused_field: u8` with hard-coded field numbers |

Two consequences followed, and both were already visible in the tree.

**The framework core learned vendor and model-family names.** The default
reasoning depth branched on a family string:

```rust
let preferred =
    if family == "gpt" || family == "openai-subscription" || family.starts_with("gpt") {
        Effort::Medium
    } else {
        Effort::High
    };
```

and the universal `Effort` vocabulary carried a **Google wire encoder**
(`gemini_thinking_budget`), whose bucket fractions are one surface's encoding
rather than a property of "depth". `docs/reference/effort.md` already stated the
opposite rule — that this translation is per-provider wire knowledge — so the
code and its own reference disagreed.

**The presentation layer re-derived authority it did not own.** The composer
footer and the server's connection detail both decided *whether* to show depth by
sniffing the protocol string:

```rust
let show = match m.protocol.as_str() {
    "anthropic" => m.thinking == Some(true),
    _ => m.effort.is_some(),
};
```

and the model editor invented a fallback the server never sent
(`unwrap_or("medium")`, plus a client-side `clamp_to_levels`). Each of these is a
second implementation of a server rule, and the file's own comments record the
drift it caused.

The structural cost is the same in every layer: **the axis of variation is data
(which knobs this route has), but it was written as structure (a field per
knob)**. Every new provider knob re-opened the same six files.

### Why this is not a "just make it generic" ADR

A reflexive answer — erase all knobs into `BTreeMap<String, String>` — is
*also* wrong, because reasoning depth is genuinely special:

- it has a **total order**, so a request can be clamped to what a route accepts;
- it has a **shared vocabulary**, so `"high"` means the same thing across
  vendors and a selector caption can be written once;
- it needs a **default-selection rule**, because a route that exposes depth but
  has no stored choice must still send something.

Those three capabilities are exactly what an opaque string cannot provide. The
decision therefore has two halves, and the ADR's value is the boundary between
them.

---

## Decision Drivers

1. **Provider autonomy without framework edits.** Shipping a new knob on one
   provider must not require touching the framework core, the picker, the
   protocol DTO, or the editor's field numbering.
2. **Mechanism over exhortation** (`AGENTS.md` §3). The boundary must be enforced
   by types and a test, not by a comment asking contributors to behave.
3. **No silent data loss on protocol skew.** A frontend built before a provider
   shipped a knob must not erase the user's setting for it.
4. **One authority per fact.** The tier the UI shows must be the tier the wire
   stamps; neither may re-derive the other's rule.
5. **A defensible promotion gate.** "Depth is framework-owned" must be
   justified by criteria a future contributor can re-apply, not by precedent.

---

## Architectural Decisions

### 1. One open envelope; no speculative framework vocabulary

`nuo_provider::tuning` is deliberately tiny: `TunableSetting`, `TunableSettings`,
and the two framework-owned knob ids. It is a **transport and storage envelope**,
not a declarative capability model.

- **The knob id is provider-owned and open.** It is the provider's own name for
  the knob (`"effort"`, `"thinking"`, `"edit_format"`) and is an opaque map key
  in storage and on the wire. The framework compares it for equality and never
  matches it against a literal outside the knob's own translator.
- **`TunableSettings` is a transparent `BTreeMap`; `TunableSetting` is untagged**
  (`Flag(bool)` / `Rung(String)` / `Opaque(Value)`). That is the entire schema a
  receiving build needs in order to store and re-emit a knob it does not
  understand.

An earlier draft of this decision also introduced a declarative layer —
`TunableId`, `TunableSpec`, `TunableRungs`, `TunableValue`, and a `TunableKind`
enum with `Ladder`/`Enum`/`Toggle`/`Opaque` shapes. It was cut, and the reason is
recorded in Negative Knowledge: **it had no consumer.** Every provider exposes
exactly two knobs, both of which the framework already models as typed data
(`Model::effort_levels`, `Model::thinking`); nothing rendered a
`TunableKind`, and `Enum`/`Opaque` had zero call sites. A speculative vocabulary
with no traffic is not an abstraction, it is a second thing to keep in sync.

The rule that made the cut safe, and that a future contributor should re-apply:
**the framework grows a type only when a shipped consumer needs it.** When a
provider actually exposes a third knob with a shape the current envelope cannot
express, that shape gets added *with* its renderer, in the same change.

### 2. Depth is framework-owned, and the promotion gate is explicit

`Effort` stays a closed, `Copy` enum with a total order and stays in the
framework, because it satisfies **all three** of:

1. **Semantic consensus** — three or more unrelated vendors expose it, differing
   only in encoding (OpenAI Responses `reasoning.effort`, OpenAI chat
   `reasoning_effort`, Anthropic `output_config.effort`, Google
   `thinkingConfig.thinkingLevel` / `.thinkingBudget`).
2. **Total order** — clamping needs a rank, and every vendor's values order
   shallow-to-deep.
3. **Protocol-local translation** — mapping a rung onto the wire is decided by
   which public API specification the route speaks, never by vendor business
### 2. Depth is framework-owned, and the promotion gate is explicit

`Effort` stays a closed, `Copy` enum with a total order and stays in the
framework, because it satisfies **all three** of:

1. **Semantic consensus** — three or more unrelated vendors expose it, differing
   only in encoding (OpenAI Responses `reasoning.effort`, OpenAI chat
   `reasoning_effort`, Anthropic `output_config.effort`, Google
   `thinkingConfig.thinkingLevel` / `.thinkingBudget`).
2. **Total order** — clamping needs a rank, and every vendor's values order
   shallow-to-deep.
3. **Protocol-local translation** — mapping a rung onto the wire is decided by
   which public API specification the route speaks, never by vendor business
   logic.

A knob failing any one criterion is carried as an opaque provider string in the
tuning map and stamped verbatim. This is not a lesser outcome: an opaque knob still inherits
persistence, transport, rendering, and unknown-value preservation. It simply
does not inherit framework semantics it cannot honestly have.

### 3. The route declares; the transport sends

`Transport` variants now carry **only** endpoint shape: base URL, client
profile, dialect, and (for Responses) declared statefulness. They carry no user
choice. A tuning is a setting *on a route*, so it lives on
`Channel::tunings: TunableSettings` — one open map any route can populate with
any knob its provider declares.

This is the single most important structural consequence. Previously a knob was
welded into the variant that happened to speak the right protocol, so a new knob
meant threading a field through every variant and every destructuring site.

The resolved default travels with the ladder it belongs to: `Model::default_effort`
and `ReasoningSupport::has_thinking_switch()` (derived from the declared
encoding) are provider declarations, resolved through the same
precedence as the ladder (remote advertisement over vetted baseline) and surfaced
on `ModelCapabilities`. `Effort::channel_default` now takes the *declared
preference* rather than a family string, so the framework's entire contribution
is "clamp this preference onto this ladder".

### 4. One authority: the server resolves, the client renders

`Channel::effective_effort` and `Channel::thinking_state` are the only places
depth and switch values are resolved. They are consumed by:

- the request boundary (`nuo-server`'s factory), which stamps the wire;
- the picker projection (`channel_model_info`), which fills
  `ProviderModelInfo::effort` / `thinking`. A `None` there is the **only** signal
  a frontend may use to decide a knob is absent — never the protocol string.

A frontend **renders** those values. It may not re-clamp, invent a fallback, or
decide knob existence from the protocol string. The composer footer, the
connection detail, the model detail view, and the model editor were all changed
to plain reads.

### 5. The preservation rule

`TunableSetting` is untagged (`Flag(bool)` / `Rung(String)` / `Opaque(Value)`)
and `TunableSettings` is a transparent `BTreeMap`. An unrecognised knob
round-trips through storage and IPC untouched. `RouteSettings` gains
`tunings: TunableSettings` alongside its typed `effort`/`thinking` mirrors, and
`RouteSettings::tunings()` is the one place the two representations reconcile.

Without this rule, provider autonomy would be fiction: one un-upgraded client
would silently delete a newer provider's setting.

### Subsystem Boundaries & Structural Changes

| Crate | Change |
|---|---|
| `nuo-provider` | New `tuning` module (the envelope). `Model` gains `default_effort`; switch presence derives from `ReasoningSupport::has_thinking_switch()`. `ModelCapabilities` gains `default_effort` + `default_effort()`. `Channel` gains `tunings`, `effective_effort()`, `thinking_state()`. `Transport` loses `effort`/`thinking`. `Effort::channel_default` takes a declared preference; `gemini_thinking_budget` is deleted from the vocabulary. `RouteCapabilities` and vendor-named `ReasoningSupport` variants deleted/renamed. |
| `providers/nuo-provider-google-antigravity` | Gains `thinking_budget_wire`, the Gemini budget encoder it always owned. |
| `nuo-protocol` | Re-exports the envelope; `ProviderModelInfo::effort`/`thinking` are documented as resolved authority, and `protocol` as display-only. |
| `nuo-persistence` | `RouteSettings` gains the open `tunings` map and a reconciling accessor. |
| `nuo-model-routing` | `derive` populates `Channel::tunings`; `channel_model_info` collapses five per-protocol arms into one generic construction. |
| `nuo-server` | `provider_registry` reads tunings instead of transport fields; the family-sniffing `effective_channel_effort` helper is deleted; two protocol sniffs become plain reads. |
| `crates/nuo` | The editor no longer re-derives or re-clamps; the footer and detail view read the server's answer. |

---

## Normative Invariants Established

- **`[INV-TUNE-01]` No provider knowledge in the core vocabulary**: the
  framework's depth vocabulary and its resolution points must carry neither a
  vendor wire encoding nor a model-family branch. Verified by
  `test_inv_tune_01_no_vendor_wire_encoding_or_family_branch_in_core_vocabulary`.
- **`[INV-TUNE-02]` No user choice on a transport**: `Transport` variants carry
  endpoint shape only. Verified structurally — the fields do not exist, so the
  compiler rejects reintroduction.
- **`[INV-TUNE-03]` Unknown knobs are preserved**: a `TunableSettings` round-trip
  drops no key. Verified by `tuning::tests::settings_round_trip_unknown_knobs_verbatim`.
- **`[INV-TUNE-04]` Single resolution point**: a knob's effective value is
  computed in `nuo-provider` and read elsewhere. Verified by
  `test_inv_tune_04_no_client_side_knob_resolution`, which forbids `clamp_to` /
  `channel_default` calls and protocol-string knob inference in
  `crates/nuo/src/shell/**`.

---

## Consequences

- **Positive**: a provider can ship a knob by declaring it where it declares the
  model; no framework edit, no DTO change, no editor renumbering. The wire and
  the UI cannot disagree about depth, because there is one resolver. The
  framework core contains no vendor name and no vendor wire encoding.
- **Positive**: the two shipped knobs (depth and the thinking switch) express
  their differences as *data* — `Model::effort_levels`, `Model::default_effort`,
  `ReasoningSupport::has_thinking_switch()` — rather than as parallel
  per-protocol code paths.
- **Negative & Trade-offs**: a third knob with a value shape the current envelope
  cannot express would need framework work (a storage/rendering story) before a
  provider could use it. That is deliberate: it keeps an unrendered knob out of
  users' faces. It is also why the envelope is untagged `String`/`bool`/`Value`
  rather than a closed shape enum — a provider can already carry the *value*, so
  only its *rendering* is missing.
- **Neutral**: `ProviderModelInfo::effort` / `thinking` remain the projection a
  frontend reads. They are resolved server-side; the `None`/`Some` distinction is
  the sole signal of knob presence, which is why no surface may sniff `protocol`
  for the same answer.

---

## Rejected Alternatives & Negative Knowledge

### Alternative A — one `BTreeMap<String, String>` for every knob (rejected)

- **Why considered**: minimal type surface; a provider could add a knob with zero
  framework involvement.
- **Why rejected**: it destroys the three capabilities that make depth usable — a
  total order (so a request can be clamped), a shared vocabulary (so a caption is
  written once), and a default rule (so a route with no stored choice still sends

### Alternative A — one `BTreeMap<String, String>` for every knob (rejected)

- **Why considered**: minimal type surface; a provider could add a knob with zero
  framework involvement.
- **Why rejected**: it destroys the three capabilities that make depth usable — a
  total order (so a request can be clamped), a shared vocabulary (so a caption is
  written once), and a default rule (so a route with no stored choice still sends
  something). It also loses value *shape*, so a renderer cannot know whether to
  draw a ladder, a switch, or an enum, and would have to re-learn each knob by
  name — reintroducing the coupling this ADR removes.

### Alternative B — a typed field per knob on `RouteSettings`/`ProviderModelInfo` (rejected)

- **Why considered**: simplest to consume; the compiler checks every field.
- **Why rejected**: this is the legacy shape, and it is what made a new knob a
  six-layer edit. It also cannot preserve an unknown knob: a build without the
  field has nowhere to put the value, so it drops it.

### Alternative C — keep the family-name default rule but move it to a table (rejected)

- **Why considered**: it would have preserved the exact shipped behaviour with a
  smaller diff.
- **Why rejected**: it still encodes "which families prefer `medium`" in the
  framework. The preference belongs to the provider that ships the model, so it
  moved to `Model::default_effort` and the framework's role became "clamp what
  was declared".

### Alternative D — leave the Gemini budget encoder on `Effort` (rejected)

- **Why considered**: it was already written, tested, and reachable from the
  provider.
- **Why rejected**: it is Google's wire encoding living in the universal depth
  vocabulary, contradicting `docs/reference/effort.md`'s own rule. It moved to
  `nuo-provider-google-antigravity::request::thinking_budget_wire`, where its
  sibling `thinking_level_wire` already lives, and its test moved with it.

### Alternative E — a declarative "tunable spec" layer (rejected after building it)

- **Why considered**: a `TunableSpec { id, kind, rungs, default, label }` table
  plus a closed `TunableKind` (`Ladder`/`Enum`/`Toggle`/`Opaque`) and an open
  `TunableValue::Other` looked like the *right* abstraction: it would let a
  generic renderer walk a provider's declaration with no per-knob code.
- **Why rejected**: it was built, shipped through `Model`, `ModelCapabilities`,
  `Channel`, `ProviderModelInfo` and the picker — and then measured. `TunableId`,
  `TunableSpec`, `TunableRungs` and `TunableValue` had **one** reference each,
  their own re-export. `TunableKind::Enum` and `TunableKind::Opaque` had **zero**
  call sites anywhere, including tests. `ProviderModelInfo::tunables` was
  populated and serialized over IPC, and **no frontend ever read it**. It was all
  scaffolding in front of two knobs that the framework already models as typed
  data. It was deleted in full.
- **The durable lesson**: an abstraction is justified by its consumers, not by
  its symmetry. A type introduced "in case a provider needs it later" is a second
  thing to keep in sync, and it will drift from the typed path that actually
  works. The measurable before/after is in this ADR's Decision §1: the envelope is
  now `TunableSetting` + `TunableSettings` + two id constants.

### Alternative F — keep typed `effort`/`thinking` mirrors beside the open map (rejected)

- **Why considered**: it would have kept every existing reader working with a
  minimal diff, and the two typed fields are what the editor writes.
- **Why rejected**: it makes one fact storable two ways. `RouteSettings` had
  `effort`, `thinking` **and** `tunings`, so a reader had to reconcile them and a
  writer had to update both — and a mismatched write was silent. The mirrors were
  removed; `RouteSettings` now holds only the map, read through
  `TunableSettings::effort()` / `::thinking()`. The one place the *old on-disk
  spelling* still exists is the migration reader, which explicitly translates
  retired `{ effort, thinking }` JSON into map keys — that translation is real
  work with a real test, not a compatibility layer.

### Alternative G — a `RouteCapabilities` DTO the frontend resolves locally (rejected)

- **Why considered**: the client needed context window, max output, vision and
  tool-call support for the active route, and a small DTO looked like a clean way
  to ship them.
- **Why rejected**: measured, `RouteCapabilities` was carried by **no IPC event**
  at all, and three of its five fields (`max_output_tokens`, `tool_call`,
  `thinking`) had **zero** readers. Its only real effect was that the client
  reconstructed `ReasoningSupport::AnthropicAdaptive` from a `bool` — the
  seventh instance of "presentation re-derives a server rule", the exact defect
  this ADR removes. Worse, its `ModelCapabilities::for_channel(...)` fallback
  resolves against a registry the shipped client does **not** link, so in
  production it reported "undeclared" for every route and the vision gate could
  never refuse a paste. It passed its tests only because the registry is a
  dev-dependency. Deleted; the four callers now read
  `ProviderModelInfo::context_window` / `::vision` directly.

### Alternative H — vendor-named `ReasoningSupport` variants (rejected)

- **Why considered**: `AnthropicAdaptive`, `AnthropicManual`,
  `AnthropicAdaptiveAlwaysOn`, `AnthropicAdaptiveOnByDefault` were descriptive
  and had shipped.
- **Why rejected**: the enum describes **how reasoning is encoded on the wire**,
  not who invented the encoding. A vendor name in the framework core is the same
  defect as the `family == "gpt"` branch — and it becomes a lie the moment a
  second provider adopts the encoding (four non-Anthropic provider crates already
  declare `AnthropicAdaptive`). Renamed to `Adaptive`, `Manual`,
  `AdaptiveAlwaysOn`, `AdaptiveOnByDefault`. The "does this route have an on/off
  switch" question that used to be answered by sniffing
  `protocol == "anthropic"` is now
  `ReasoningSupport::has_thinking_switch()` — derived from the encoding that was
  already declared, so no new field is needed.

### Negative knowledge recorded

- A *structural* change is not a *deletion*. Removing a field from one struct and
  adding an open map is only half the job: the typed fields have to be deleted
  from every carrier, or the two forms coexist and drift. The first pass of this
  work stopped at "moved to a map" and left `RouteSettings.effort`,
  `RouteSettings.thinking`, and a `ResolvedTunable` DTO in place — the exact
  half-measure this ADR now forbids.
- **A guard is only as good as its boundary.** The `[INV-TUNE-03]` check was
  first written to ban `pub thinking:` anywhere, which flagged
  `Model::thinking: ReasoningSupport` — a legitimate capability *declaration*,
  the single source that `has_thinking_switch()` derives from. The check now
  targets only the retired tuning *store* spellings (`pub effort: String`,
  `pub thinking: Option<bool>`, `pub thinking_switch: bool`). Over-broad guards
  get disabled, not obeyed.
- `serde(deny_unknown_fields)` on a migration-read struct is a data-loss trap:
  adding a new field spelling makes the *old* records fail to parse as a whole,
  so a migration silently drops user overrides instead of translating them. The
  retired spelling must be read as a `serde_json::Value` and translated
  explicitly (see `read_legacy_cache_route_settings`).
- `TunableSettings` needs an explicit `IntoIterator for &TunableSettings`; a
  transparent map does not get one for free.

eny_unknown_fields)` on a migration-read struct is a data-loss trap:
  adding a new field spelling makes the *old* records fail to parse as a whole,
  so a migration silently drops user overrides instead of translating them. The
  retired spelling must be read as a `serde_json::Value` and translated
  explicitly (see `read_legacy_cache_route_settings`).
- `TunableSettings` needs an explicit `IntoIterator for &TunableSettings`; a
  transparent map does not get one for free.

---

## Links

- [ADR-0113](0113-provider-owned-wire-dialects-route-derivation-never-infers-a-dialect-from-a-model-id.md) — provider owns its wire dialect
- [ADR-0114](0114-provider-owned-response-statefulness-remote-continuation-requires-a-declared-stored-surface.md) — provider owns its response statefulness
- [ADR-0068](0068-canonical-model-catalog-normalization-and-semantic-lifting.md) — the remote-over-baseline capability resolution and the open capability vocabulary this extends
- [ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md) — the per-model reasoning opt-in this preserves
- Reference: `docs/reference/effort.md`, `crates/nuo-provider/src/tuning.rs`
