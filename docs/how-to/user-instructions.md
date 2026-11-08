# User Instructions

User instructions are personal, cross-project prompt tuning. They live in
`agent.toml` and use the same selectors, matching, and ordering as project
instructions.

```toml
[[instructions.user]]
model = "*deepseek*"
text = """
Use plain, natural Chinese. Avoid bureaucratic cliches.
"""

[[instructions.user]]
connection = "cc-fox"
model = "deepseek/deepseek-v4-flash-fast"
text = "Ultra-concise mode."

[[instructions.user]]
role = "reviewer"
provider = "commandcode-goat"
text = "Review against the project architecture."
```

## Selectors

| Field | Match | Example |
| :--- | :--- | :--- |
| `role` | exact, case-insensitive | `reviewer`, `ops`, `explore` |
| `provider` | exact provider type | `commandcode-goat` |
| `connection` | exact connection name | `cc-fox` |
| `model` | glob, case-insensitive | `*deepseek*`, `deepseek/deepseek-v4-flash-fast` |

Unset selectors are wildcards. `model` is always a pattern; an exact id is a
pattern without wildcards. `provider` and `connection` must name configured
entities.

Matching entries are additive. Order is broad to narrow — global → role →
provider → connection → model — then selector count, then declaration order.
There is no `priority` knob.

## Placement

User instructions render as a **Base** section (`system.user_instructions`)
after tone and before provider guidance. Project instructions stay in the
Session tier, so the order is system → user → project.

Subagents inherit the parent's user-instruction set and resolve it with their
profile role (`explore`, `code`, `debug`, …).

## Migration from `[model_guidance]`

`[model_guidance]` is retired. An old table fails with a precise error; move
each entry to `[[instructions.user]]`:

```toml
# Before
[model_guidance]
"*deepseek*" = "Speak plainly."

# After
[[instructions.user]]
model = "*deepseek*"
text = "Speak plainly."
```

Provider-scoped tables and provider-prefixed keys become `provider` /
`connection` selectors. A per-route `model_guidance` override becomes a
`connection` + `model` entry. There is no compatibility alias.
