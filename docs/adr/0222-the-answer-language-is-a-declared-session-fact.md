---
id: ADR-0222
title: "The Answer Language Is A Declared Session Fact, Not An English Default"
status: accepted
date: 2026-11-19
scope: protocol/language, protocol/context, engine/context, engine/agent, persistence/config, server/slash, docs
superseded_by: null
negative_knowledge: true
---

# 0222. The Answer Language Is A Declared Session Fact, Not An English Default

- **Status:** Accepted
- **Date:** 2026-11-19
- **Related:** [ADR-0215](0215-context-sources-declare-moment-topic-rank-stability-and-budget.md) (context sources), [ADR-0165](0165-user-instructions-absorb-model-guidance-as-the-user-source.md) (user instructions), [ADR-0217](0217-context-projection-is-inspectable-and-prefix-drift-is-reported.md) (inspection)
- **Scope:** `protocol`, `engine`, `persistence`, `server`

---

## Context and Problem Statement

A session that begins in Chinese drifts into English as it runs. The report is
reproducible and the mechanism is not mysterious once the request is measured:

1. **The prompt is English.** Every default context source — precedence, the
   working contract, secrets, exploration, the web boundary, the skill catalog —
   is authored in English, and nothing states which language the *answer* is in.
2. **The harness injects English mid-turn.** Tool results, `@file:` envelopes,
   guard nudges (`[System Directive: …]`), compaction checkpoints, and cleared
   tool-result tombstones are all English, and the hidden ones arrive as
   **user-role** messages after the user's own text — the strongest position in
   the window, under a precedence contract that says the latest user request wins.
3. **Compaction resets the language.** A fold replaces the visible history with an
   English-headed checkpoint whose body was produced by a summarizer prompt with
   no language requirement at all (the one prompt that *did* say "respond in the
   same language" belonged to a summarizer with no callers left).

So the only language signal was a majority vote, and English always wins it: an
English constitution, hundreds of English observation tokens per ReAct turn, and
an English history after the first compaction, against a handful of Chinese
sentences from the user.

There was a sanctioned workaround — `[[instructions.user]]` with
`text = "always answer in Chinese"` — and its existence is the defect. A user
discovering they must hand-write the one rule that decides which language the
model thinks out loud in is a missing product concept, not a missing tip.

## Decision Drivers

- **A language is a session fact, not a style tip.** It survives `/model`,
  applies to every subagent, and must be visible in `/context` like every other
  prompt input. Personal prose tuning is what `[[instructions.user]]` is for.
- **The harness may not author a language choice.** Defaulting the block to
  English would make the harness the author of a decision the user never made,
  and defaulting it to "match the user" would be a rule that costs tokens to say
  nothing.
- **The block must outrank the surrounding English**, or it is one more voice in
  the same majority vote. The load-bearing sentence is not "answer in X" but "the
  English around you is context, not a style instruction".
- **Cache discipline.** A language switch must not move the frozen head of the
  cacheable prefix.
- **A config value that reaches the prompt is untrusted input.** It may not carry
  a newline or the envelope's own angle brackets.

## Considered Options

- **Option 1: A paragraph in `CodingBaselineGuidance`.** Rejected: that block is
  `Frozen`, so it would ship one product's language to every deployment, and a
  per-user value there would invalidate the head of the prefix on every change.
- **Option 2: Only document the `[[instructions.user]]` workaround.** Rejected:
  it leaves the actual defect (nothing states the answer language, and the
  surrounding English is not disarmed) in place, and it asks every non-English
  user to rediscover it.
- **Option 3: A declared answer language with its own input, topic, source and
  validated type (chosen).**
- **Option 4: Force a per-turn transient restatement of the language.** Rejected
  as the primary mechanism: it works but pays tokens on every request, and it
  treats the symptom (drift) rather than the cause (no declared fact). The
  persistent block is the cause; a turn-scoped restatement remains available to
  an embedding that measures drift it cares about.

## Decision Outcome

### 1. A validated type, not a string

`protocol::ResponseLanguage` wraps a single-line tag. `ResponseLanguage::parse`
refuses, with the offending character named: an empty value, a value longer than
`MAX_LANGUAGE_TAG_LEN` (48 characters — a tag, not a sentence), any control
character, and `<` / `>`.

It is deliberately **not** an enum of "supported languages". Every model in the
catalog speaks more languages than this crate could ever enumerate, and a missing
variant would make a working language unusable. The tag is data; the *shape* is
the contract.

Validation runs at three boundaries that must agree: config load (fatal, per
`[INV-CONF-04]`), `nuo config check` (a check that accepted a tag the loader would
refuse would not be a check), and `/language` (refused before anything is
persisted).

### 2. A declared input, topic, and source

- `InputKey::ResponseLanguage`, `Stability::Session`.
- `Topic::OutputLanguage` — its own topic, because folding it into
  `UserInstructions` would make it a rule the user has to write, and one topic has
  one owner.
- `agent.language`, in the session band, `Budget::elide_to_operator(400)`. After
  `user.instructions` and before `skills.catalog` in emission order.

The block names the language, exempts verbatim material (code, paths, commands,
logs, quoted source), and then disarms the surroundings: the harness prompt, tool
results, injected notes, and compaction summaries *may* be English and *never*
change the language of the answer.

An undeclared language renders **nothing** — no block, no default, and the
`inactive` row in `/context` says so.

### 3. Three surfaces, one value

- `agent.toml`: `response_language = "zh-CN"`.
- `/language [tag|off]`: reads the current declaration; a tag validates →
  persists → re-seeds the live agent in that order, so a failed write leaves the
  session on the language the next restart will reproduce.
- Subagents inherit the parent's handle (`bind_response_language`), because the
  human reading a child's report is the same human.

`Config::set_response_language` writes **one key** under the same file lock the
general save takes: load → set → save. Routing it through `Config::save` would
resurrect every other user-owned table from a possibly stale snapshot and make an
explicitly runtime-owned setting look like a selection flush.

### 4. The compaction prompt states its language

`COMPACTION_SYSTEM_PROMPT` now requires the summary in the conversation's
language, and `COMPACTION_USER_INSTRUCTIONS` names the section headings as fixed
English scaffolding with non-English content beneath them. Without this the first
fold re-authored the history in English, which no prompt block can undo because
the drift is already in `messages`.

### 5. Clean break: the retired compaction pipeline is deleted

`persistence::thread` carried a second, fully-formed compaction pipeline
(`run_compaction`, `summarize_with_provider`, `serialize_for_summary`,
`build_excerpt_summary`, `compact_messages`, `CompactionSelection`,
`select_compaction*`, `build_compaction_result`, `SUMMARY_TEMPLATE`, …) with
**no production caller** — the live path is `CausalCompactor` →
`split_compaction::compact_facts`. It was also the pipeline whose
"respond in the same language" prompt made the missing requirement look handled.

Deleted outright, together with its two tests. `ContextProjectionResult` had a
verbatim second definition in the session; the session now re-exports
`protocol::ContextProjectionResult`.

## Invariants

- **`[INV-LANG-01]` A prompt-bound language is a validated tag.** No rendered
  language may carry a newline, a control character, or the block envelope's
  delimiters — enforced by `ResponseLanguage::parse` at load, at
  `config check`, and at `/language`.
- **`[INV-LANG-02]` Undeclared is not English.** An undeclared language
  contributes nothing; the harness never invents one. Pinned by
  `an_undeclared_language_renders_no_block_at_all`.
- **`[INV-LANG-03]` The rule outranks the surrounding context.** The block states
  that English prompt text, tool output, injected notes, and compaction summaries
  are context and do not change the answer language. Pinned by
  `a_declared_language_names_itself_and_disarms_the_english_context`.
- **`[INV-LANG-04]` A language switch moves only the session band.** Pinned by
  `changing_the_language_moves_only_the_session_band`.
- **`[INV-LANG-05]` One declaration, three surfaces, inherited by subagents.**
  Config, `/language`, and the subagent handle share one value.

## Consequences

- A user declares once (`/language zh-CN` or `response_language`) and the rule
  holds across turns, compactions, and model switches, because every surface that
  could outvote it now defers to it explicitly.
- The prompt grows by ~280 tokens **only** when a language is declared; an
  English-default deployment pays nothing.
- `/context` shows `agent.language` like any other block, so "why is it answering
  in English" is answerable from the inspector.
- Dead code removed: ~600 lines of a second compaction pipeline that no path
  called, and a duplicated `ContextProjectionResult`.
