# Reasoning Effort Reference

Reference for how Nuo models reasoning **depth** ("how hard should the model think before answering?"). The implementation lives in `crates/nuo-provider/src/effort.rs`; this page is its prose companion for users and contributors.

---

## 1. Two Layers

Reasoning depth is split across two layers that are easy to conflate:

- **Layer A — the abstraction** (`nuo-provider::effort`): the universal [`Effort`] vocabulary and the translation onto each public API specification. This is the only depth concept in the codebase; no caller outside the module ever sees a provider-specific depth shape.
- **Layer B — model capability ladders** (`nuo_provider::effort_ladders`): which rungs a given model actually accepts.

---

## 2. The `Effort` Vocabulary

`Effort` is a closed, `Copy` enum ordered ascending by depth:

```text
None < Minimal < Low < Medium < High < Xhigh < Max < Ultra
```

| Rung | Wire string | Meaning |
| :--- | :--- | :--- |
| `None` | `none` | Disable reasoning where the provider supports an explicit off value. |
| `Minimal` | `minimal` | Minimal reasoning; some providers expose a tier below `low`. |
| `Low` | `low` | Fastest and cheapest; simple tasks may skip thinking. |
| `Medium` | `medium` | Middle ground; may omit thinking on simple queries. |
| `High` | `high` | The default depth; equivalent to omitting `effort`. |
| `Xhigh` | `xhigh` | Deeper-than-high with extended exploration (Fable / Opus-4.7+ tier). |
| `Max` | `max` | Maximum reasoning, no depth cap. |
| `Ultra` | `ultra` | Ultra-deep with automatic task delegation (OpenAI GPT-6 / Astra). |

---

## 3. The Open Companion Type

`EffortLevel` is the **open** companion carried on the runtime model view:

- `Known(Effort)` — a rung of the vocabulary.
- `Other(String)` — a provider-advertised wire string the vocabulary does not name.

`EffortLevel::parse` never drops a value: an unrecognized provider tier is preserved as `Other` and stamped through to the wire verbatim. This lets the vocabulary stay closed (`Effort` is `Copy`) while live discovery can still surface a tier Nuo has not named.

---

## 4. Resolution Precedence

A model's accepted rungs resolve through one precedence chain:

```text
live discovery (a preset whose RemoteCatalogSource carries effort tiers)
       ↓  only the providers that advertise tiers land here
static baseline  ←  model capability ladders in `nuo_provider::effort_ladders`
       ↓  the compiled-in fallback when upstream advertises nothing
COMMON_LADDER / &[]  (generic conservative fallback / non-reasoning model)
```

`COMMON_LADDER` (`low`/`medium`/`high`) is the conservative fallback for a reasoning model whose full depth capability is unknown. A non-reasoning model declares `&[]`.

---

## 5. Translation onto Wire Specifications

The protocol layer maps a chosen `Effort` onto each public API specification, not onto a brand:

| API specification | Wire field | Form |
| :--- | :--- | :--- |
| OpenAI Responses | `reasoning.effort` | enum string |
| OpenAI chat completions | `reasoning_effort` | enum string |
| Anthropic Messages | `output_config.effort` | enum string |
| Google generateContent | `thinkingConfig.thinkingLevel` / `.thinkingBudget` | **closed upper-case enum** `LOW`/`MEDIUM`/`HIGH` / int tokens |

Providers that implement the OpenAI Responses / chat-completions specification (xAI Grok, Moonshot Kimi, DeepSeek, Z.AI GLM) reuse the OpenAI translation verbatim. Google is the outlier: a Gemini model declares an `Effort` ladder like any other, and the Google protocol maps each rung onto `thinkingLevel` (3.x) or a `thinkingBudget` bucket (2.5).

**The Google rung is not the generic effort spelling.** `Effort::as_str` produces lower-case vocabulary words (`"high"`, `"minimal"`, …), but `thinkingLevel` is a closed, case-sensitive enum of exactly `LOW`/`MEDIUM`/`HIGH` (verified against the live Antigravity capture, `docs/reference/openapi/antigravity.yml` → `ThinkingConfig.thinkingLevel`). Sending `"high"` — or `"minimal"`, a rung the *catalog* advertises but the *field* does not accept — is rejected as `400 INVALID_ARGUMENT` with the content-free body `Request contains an invalid argument.`, which names no field and is therefore very hard to attribute. The translation is provider-owned wire knowledge and lives in one place per provider (`nuo_provider_google_antigravity::request::thinking_level_wire`); a provider that stamps `as_str()` directly onto a closed enum field is a defect, not a mapping choice.

Because that class of `400` cannot explain itself, the Google provider also folds the **shape of the request it actually sent** into the error (routed model id, the stamped `thinkingConfig`, tool and content counts) — see `RequestShape`. That is evidence travelling with the failure, not a guess at the cause.

A dialect may also *decline* a field the specification defines. DeepSeek's Responses surface defines `reasoning` as `{effort}` only — it has no summarized-CoT channel and returns plaintext `reasoning_text` items — so the builder emits neither `reasoning.summary` nor `include: ["reasoning.encrypted_content"]` there (ADR-0083).

---

## 6. Clamping

A model accepts only a subset of the rungs (its `Model::effort_levels`). Callers must **clamp** a requested level down to what the model supports rather than sending an unsupported value, which the upstream rejects with HTTP 400. `EffortLevel::Other` rungs cannot be ranked, so they participate only by exact name match.

The ladder is therefore the **accepted set**, not the set of rungs a model *distinguishes*. This matters when a provider alias-maps tiers: DeepSeek accepts `minimal`/`medium`/`xhigh`/`ultra` and serves them as `low`/`high`/`high`/`max`. A ladder narrowed to the three distinct rungs would make `none` unreachable (the clamp snaps "no thinking" **up** to `low`) and would clamp an explicit `medium` **down** to `low` — shallower than the `high` upstream would have served. Where a provider advertises only its distinct rungs (`/models` → `effort.supported_levels`, which excludes `none` because it is the off switch), the catalog layer expands the advertisement back to the accepted set; compiled baselines declare the same set so a live refresh never changes the menu (ADR-0083).

---

## 7. Who Decides the Default, and Who Decides the Switch

Depth and the on/off switch are **provider declarations**, not framework rules
(ADR-0126). Nothing in the framework branches on a vendor or model-family name.

- **Default depth** — `Model::default_effort`. The framework's entire role is
  `Effort::channel_default(declared, ladder)`: clamp the provider's declared
  preference onto the provider's declared ladder. A provider that declares no
  preference gets the vendor-neutral `High`, still clamped onto its own ladder.
  A route with no ladder has no default at all, and the request omits the field.
- **Whether a separate on/off switch exists** —
  `ReasoningSupport::has_thinking_switch()`, derived from the route's already
  declared reasoning encoding. The adaptive and manual encodings take a field
  that can be omitted or explicitly disabled; the content/summary encodings are
  always-on streams with nothing to switch. A depth-only surface expresses "off"
  with its shallowest rung.
- **The user's choice** is stored in the route's open tuning map
  (`TunableSettings`), keyed by the knob id (`"effort"`, `"thinking"`). It is
  resolved in exactly one place (`Channel::effective_effort` /
  `Channel::thinking_state`), and the wire and the UI both read that result — the
  UI never re-clamps and never infers a knob's presence from a protocol string.

---

## 8. Baseline Ladders

Specific family ladders are constants in `nuo_provider::effort_ladders`:

| Constant | Rungs |
| :--- | :--- |
| `COMMON` | `low`/`medium`/`high` |
| `CLAUDE_FULL` | `low`/`medium`/`high`/`xhigh`/`max` |
| `CLAUDE_NO_XHIGH` | `low`/`medium`/`high`/`max` |
| `OPENAI_GPT` (≤ 5.5) | `none`/`minimal`/`low`/`medium`/`high`/`xhigh` |
| `OPENAI_GPT_5_6` | adds `max` |
| `OPENAI_GPT_6` | `low`/`medium`/`high`/`xhigh`/`max`/`ultra` |
| `XAI_GROK` | `none`/`low`/`medium`/`high` |
| `LOW_HIGH_MAX` (Kimi K3, CommandCode DeepSeek lanes) | `low`/`high`/`max` |
| `DEEPSEEK_NATIVE` (DeepSeek platform + pass-through relays) | `none`/`minimal`/`low`/`medium`/`high`/`xhigh`/`max`/`ultra` |
| `GLM_5` | `low`/`high`/`xhigh`/`max` |
| `GEMINI_LEVEL` (3.x) | `minimal`/`low`/`medium`/`high` |
| `GEMINI_BUDGET` (2.5) | `minimal`/`low`/`medium`/`high`/`max` |
| `QWEN_MIXED` (QianwenAI) | `none`/`low`/`medium`/`high`/`xhigh`/`max` |
| `DEEPSEEK_V4_PLAN` (QianwenAI) | `low`/`medium`/`high`/`xhigh`/`max` |

Notes on the DeepSeek rungs: the DeepSeek platform accepts the whole closed vocabulary and maps the tiers it does not distinguish — `minimal`→`low`, `medium`/`xhigh`→`high`, `ultra`→`max` — with `none` as the thinking off switch and `high` as the default (integer efforts are rejected upstream with HTTP 400). Its `/models` advertisement lists only the three distinct rungs plus the default, so both the compiled `DEEPSEEK_NATIVE` ladder and the discovered expansion carry the full accepted set. Only surfaces whose accepted strings are *unverified* beyond the three presets — Kimi K3, the CommandCode `provider/v1` DeepSeek lanes — keep `LOW_HIGH_MAX`.

On the QianwenAI Token Plan surface, `none` is a first-class rung (the endpoint answers with zero reasoning content — the thinking off switch), while its DeepSeek V4 Pro snapshots reject `none` and `minimal` outright, so "reasoning off" is inexpressible there. That route-specific floor cannot be expressed on a shared wire id — capabilities are keyed by model id, not by route — so the DeepSeek ids carry their home platform's vocabulary on every route (ADR-0083).
