# How-To Guides

Task-oriented guides for competent users. Each guide assumes you have completed [Getting Started](../tutorials/getting-started.md) and know your way around the CLI.

---

## Guides

| Guide | Solves |
| :--- | :--- |
| [Connect a Model Provider](connect-a-provider.md) | Authenticate a model provider over the OAuth-subscription or API-key (BYOK) lane and verify it is usable. |
| [Manage Threads and Sessions](manage-threads.md) | List, resume, and terminate hosted threads; drive the server lifecycle. |
| [Inspect Token Usage](inspect-usage.md) | Read cross-session token consumption, context accounting, and provider allowances. |
| [Project Instructions](project-instructions.md) | Add repository-owned instructions under `.nuo/instructions/`, scoped to a role, provider, connection, or model. |
| [User Instructions](user-instructions.md) | Add cross-project instructions in `agent.toml` with the same selectors and ordering. |
| [Answer Language](answer-language.md) | Declare the language the agent answers in (`/language zh-CN`) so it does not drift into English as the session runs. |

For the full command surface, consult the [README CLI Cheatsheet](../../README.md#cli-cheatsheet).
