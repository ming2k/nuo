# Answer Language

Declare which language the agent answers in. One setting, one block in the
prompt, and it holds for the rest of the session.

```toml
# agent.toml
response_language = "zh-CN"
```

or, mid-session:

```
/language zh-CN      # declare
/language            # show the declaration
/language off        # remove it
```

## Why this exists

Without a declaration, a session that starts in Chinese drifts into English. The
harness prompt is English, every tool result and guard note is English, and a
compaction rewrites the visible history into an English skeleton — so the only
language signal the model receives is a majority vote, and English always wins
it. Nothing was ever wrong with the model: nothing had told it which language the
*answer* is in.

The declaration renders one block (`agent.language`) that does three things:

1. Names the language for prose — explanations, plans, summaries, questions, and
   the final answer.
2. Exempts verbatim material: code, identifiers, file paths, command lines, shell
   output, log lines, commit messages, and quoted source keep their own language.
   Translating those would corrupt them.
3. Declares the surrounding English **non-authoritative for language**. The
   harness prompt, tool results, injected notes, and compaction summaries are
   context, not a style instruction, and they never change the language of the
   answer.

Point 3 is what makes the setting work. "Answer in Chinese" alone would be one
more voice in the same vote the English injections were already winning.

## What it is not

- **Not a persona or a style tip.** Personal tone tuning is
  [`[[instructions.user]]`](user-instructions.md). The answer language is a
  session fact: it survives `/model` switches, applies to every subagent, and is
  visible in `/context` as its own block.
- **Not a language list.** Any language name (`Chinese`, `中文`, `Brazilian
  Portuguese`) or BCP-47 tag (`zh-CN`) is accepted. Models speak far more
  languages than any list this build could compile.
- **Not a paragraph field.** The value is rendered into the prompt, so it is
  validated as a single-line tag: at most 48 characters, and no newline, control
  character, or angle bracket. A value that fails is refused at load, at
  `nuo config check`, and by `/language`, with the offending character named.

## Undeclared means undeclared

Leave the key out and nothing renders — no default, no "auto", no English
assumption. `/context` shows `agent.language` as an `inactive` row, so "why is it
answering in English" is answerable from the inspector instead of guessed at.

## Subagents

A spawned subagent inherits the session's language by handle, not by copy: change
it with `/language` while a background subagent runs and its next request follows.
The child's own task prompt is English, so without inheritance it would report
back in English to a human who asked in Chinese.

## Compaction

A `/compact` keeps the language: the summarizer is required to write the
checkpoint body in the conversation's language (the section headings stay English
scaffolding). Compaction is the point where an English-only history used to be
re-authored, which no prompt block can undo after the fact.

## Related

- [ADR-0222](../adr/0222-the-answer-language-is-a-declared-session-fact.md) — the decision and its invariants.
- [Context Engineering](../explanation/context-engineering.md) — where a block like this sits, and why.
