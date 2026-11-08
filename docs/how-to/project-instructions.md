# Project Instructions

Repository-owned instructions adapt the agent to this project. They live in
`.nuo/instructions/`, are reviewed with the code, and are admitted only after
the **Project instructions** trust domain is approved.

## File format

Every `.md` file under `.nuo/instructions/` is one instruction entry. The body
is Markdown. A file without frontmatter applies to the whole project and every
role.

```md
---
role: reviewer
provider: commandcode-goat
connection: cc-fox
model: "*deepseek*"
---
When reviewing this repository:
- Treat `crates/nuo-harness` as the behavior boundary.
- Reject changes that add a second instruction pipeline.
```

All four frontmatter fields are optional. A set selector must match; unset
selectors are wildcards.

| Field | Match | Example |
| :--- | :--- | :--- |
| `role` | exact, case-insensitive | `reviewer`, `ops`, `explore` |
| `provider` | exact, provider type | `commandcode-goat` |
| `connection` | exact, connection name | `cc-fox` |
| `model` | glob, case-insensitive | `*deepseek*`, `deepseek/deepseek-v4-flash-fast` |

`model` is always a pattern. An exact id is a pattern without wildcards, so one
rule can cover relay prefixes (`*deepseek*`) or local names. There is no
`priority`: matching entries compose additively, broad to narrow — global →
role → provider → connection → model — and then by path order. The most
specific guidance appears last.

## What happens to matching entries

All matching entries render as one **Project Instructions** section after the
`AGENTS.md` rules, each under a heading with its source path:

```
# Project Instructions

## .nuo/instructions/global.md
...

## .nuo/instructions/reviewer.md
...
```

Subagents inherit the same declaration set and resolve it with their own role
(`explore`, `code`, `debug`, …).

## Authoring errors

The loader fails loudly with the file path on:

- unknown frontmatter keys (typos are not ignored),
- unterminated frontmatter (missing closing `---`),
- empty selector values or malformed `model` patterns,
- an empty body,
- symlinked files or directories.

## Trust

`.nuo/instructions/` is part of the Project instructions trust domain, together
with `AGENTS.md`, `.cursorrules`, and `.windsurfrules`. Adding or editing any of
them invalidates the grant; approve it again with `/trust instructions` (or the
attach prompt). A trusted workspace is the only state in which the loader runs.

## Scope boundaries

- `AGENTS.md` and the legacy rule files remain an unscoped project-rules block.
  They are not merged into scoped instructions.
- User-level, cross-project tuning lives in `agent.toml` as
  `[[instructions.user]]`; see [User Instructions](user-instructions.md). It
  uses the same selectors and ordering, renders in the Base tier, and is
  additive with project instructions.
