# Explanation

Discursive documentation that explains how and why Nuo is built the way it is. Read these to understand design rationale and boundaries, not to complete a task.

---

## Articles

| Article | Explains |
| :--- | :--- |
| [Architecture Overview](architecture-overview.md) | The entity tiers, crate topology, and subsystem boundaries behind the CLI and terminal. |
| [The Terminal UI](tui.md) | The rendering philosophy, retained canvas diffing, and zero-layout-jitter zone design. |
| [Terminal Shortcuts and Conflicts](terminal-shortcuts-and-conflicts.md) | Keyboard shortcut design trade-offs, POSIX line discipline collisions, and terminal emulator configurations. |
| [Interrupt Semantics](interrupt-semantics.md) | Signal propagation, turn interruption, and cooperative cancellation lifecycle. |
| [OpenCode Provider Integration](opencode-provider-integration.md) | Protocol contracts, session continuation, and tool execution topology. |

For the *why* behind each ratified decision, consult the [Architectural Decision Records](../adr/index.md).
