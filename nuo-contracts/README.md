# nuo-contracts

Shared domain contracts, types, and wire events for the Nuo (傩) system.

## Principles

`nuo-contracts` serves as the **zero-I/O domain core**:
- **No I/O**: Completely free of filesystem access, network sockets, or external process execution.
- **Dependency Inversion Boundary**: Defines shared vocabulary, capability traits, and data transfer objects (DTOs) used across providers, persistence, harnesses, and client frontends.
- **Contract-Only Admission**: Code is admitted here only when exchanged across multiple layers or required to prevent architectural dependency cycles. Implementation logic lives in capability or subsystem crates.

## Core Modules

- **`capability`**: Abstract `Provider` and `Tool` capability traits, plus the normalized `ModelRequest` exchanged between cognitive agents and model providers.
- **`events`**: Canonical wire event envelopes (`DaemonEvent`, `SessionEvent`, `RoundEvent`) streaming session progress, tool output, and thinking deltas to frontends.
- **`session_ir`**: Abstract Session Intermediate Representation (IR), turn models, and execution facts.
- **`subagent`**: Subagent preset definitions, roles, and child-task parameter structures.
- **`policy`**: Capability scopes, hazard risk profiles, and context token budgets.
- **`nuo_tool_bridge`**: Bidirectional adapter bridging zero-runtime `nuo-tool` specifications into application runtime capabilities.
