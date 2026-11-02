# Reference

Technical descriptions of APIs, command surfaces, entity schemas, state machines, and interface contracts without tutorial prose. Consult these documents for exact, authoritative information.

---

## Specifications & Catalogs

| Reference | Scope & Coverage |
| :--- | :--- |
| [Command Line & Verbs](commands.md) | CLI command catalog, subcommands, flags, effect classes (mutations vs. readings), and exit codes. |
| [Lifecycle and Action Semantics](lifecycle-and-action-semantics.md) | Architectural entity tiers (Client, Thread, Server), state synchronization, and the cross-domain action matrix. |
| [State Model & Transitions](state-model.md) | Session and thread lifecycle state machine, transitions, event boundaries, and persistence rules. |
| [Reasoning Effort & Quota Tiers](effort.md) | Cognitive reasoning budget model, effort levels, dynamic token quotas, and provider tiering. |
| [Terminal UI Reference](tui/index.md) | Visual language, design tokens, viewport layout, modal presets, and keyboard interaction pipelines. |

---

## Upstream Model Wire Schemas

Raw OpenAPI 3.0 contracts and specifications for model provider wire protocols reside in the [`openapi/`](openapi/) catalog, covering Antigravity, CodeBuddy, DeepSeek, and OpenCode wire variants.
