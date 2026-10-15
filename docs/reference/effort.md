# Reasoning Effort Reference

Reference for how Nuo models reasoning **depth** ("how hard should the model think before answering?"). The implementation lives in `crates/nuo-model-codec/src/effort.rs`; this page is its prose companion for users and contributors.

---

## 1. Two Layers

Reasoning depth is split across two layers that are easy to conflate:

- **Layer A — the abstraction** (`nuo-model-codec::effort`): the universal [`Effort`] vocabulary and the translation onto each public API specification. This is the only depth concept in the codebase; no caller outside the module ever sees a provider-specific depth shape.
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
| Google generateContent | `thinkingConfig.thinkingLevel` / `.thinkingBudget` | enum string / int tokens |

Providers that implement the OpenAI Responses / chat-completions specification (xAI Grok, Moonshot Kimi, DeepSeek, Z.AI GLM) reuse the OpenAI translation verbatim. Google is the outlier: a Gemini model declares an `Effort` ladder like any other, and the Google protocol maps each rung onto `thinkingLevel` (3.x) or a `thinkingBudget` bucket (2.5).

---

## 6. Clamping

A model accepts only a subset of the rungs (its `Model::effort_levels`). Callers must **clamp** a requested level down to what the model supports rather than sending an unsupported value, which the upstream rejects with HTTP 400. `EffortLevel::Other` rungs cannot be ranked, so they participate only by exact name match.

---

## 7. Baseline Ladders

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
| `LOW_HIGH_MAX` (DeepSeek V4) | `low`/`high`/`max` |
| `GLM_5` | `low`/`high`/`xhigh`/`max` |
| `GEMINI_LEVEL` (3.x) | `minimal`/`low`/`medium`/`high` |
| `GEMINI_BUDGET` (2.5) | `minimal`/`low`/`medium`/`high`/`max` |
| `QWEN_MIXED` (QianwenAI) | `none`/`low`/`medium`/`high`/`xhigh`/`max` |
| `DEEPSEEK_V4_PLAN` (QianwenAI) | `low`/`medium`/`high`/`xhigh`/`max` |

Notes on the DeepSeek rungs: the DeepSeek platform and its OpenAI-compatible relays accept only the `low`/`high`/`max` presets (integer efforts are rejected upstream with HTTP 400). On the QianwenAI Token Plan surface, `none` is a first-class rung (the endpoint answers with zero reasoning content — the thinking off switch), while its DeepSeek V4 Pro snapshots reject `none` and `minimal` outright, so "reasoning off" is inexpressible there.
