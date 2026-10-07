---
id: ADR-0031
title: "Domain-Separated Configuration Matrix and Legacy Invariant Retirement"
status: accepted
date: 2026-10-11
scope: architecture/configuration, server/nuo-server, tui/nuo-tui, client/nuo-client, governance/invariants
superseded_by: null
negative_knowledge: true
---

# 0031. Domain-Separated Configuration Matrix and Legacy Invariant Retirement

- Status: Accepted
- Date: 2026-10-11
- Deciders: Nuo Architecture Working Group
- Consulted: Core Runtime, Infrastructure, Interface, and Governance Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md)
- Supersedes: Legacy monolithic `config.toml` structure and runtime translation shims

---

## Context and Problem Statement

Nuo evolved from an all-in-one prototype into a modular microkernel workspace (ADR-0001, ADR-0005, ADR-0011). However, its configuration topology retained substantial architectural drift and technical debt:

1. **Catch-All Monolith**: A single `config.toml` file housed orthogonal responsibilities:
   - Server daemon lifecycle and network isolation (`[daemon]`).
   - Terminal presentation preferences (`[default_expanded]`, partially split into `tui.toml`).
   - Client session defaults and model routing preferences (`default_connection`, `favorites`, `hidden_models`).
   - Agent cognitive rules, security boundaries, and tool integrations (`[agent]`, `[context]`, `[permissions]`, `[bash_policy]`, `[web]`, `[mcp]`, `[[hooks]]`).

2. **Persistent Legacy Shims**:
   - Runtime aliases (`default_provider`, `provider_retry_*`, `master.*`, `[websearch]`) blurred the canonical schema.
   - Retired context compaction structures (`CompactionPolicy`) remained hanging as dead fields (`#[serde(skip)]`) while active runtime subsystems had not cleanly consolidated onto the versioned `context.*` policy (ADR-0280).
   - Ephemeral migration adapters (`web_migration.rs`) were executed on every configuration load.
   - Diagnostic tools (`nuo config check`) carried 24 historical tombstone keys rather than providing strict, schema-driven contracts.

To ensure long-term maintainability, zero cognitive debt, and clean containerized/headless deployments, Nuo requires a clean-break, domain-separated configuration topology with zero legacy tolerance.

---

## Decision Drivers

- **Zero Legacy Burden (`[INV-CONF-01]`)**: No backwards-compatibility aliases, deprecated spelling shims, or dead structures in active schemas.
- **Concentric Domain Alignment (`[INV-CONF-02]`)**: Every configuration file maps strictly to its owning subsystem layer:
  - `server.toml`: Service daemon hosting & infrastructure (`nuo-server`).
  - `terminal.toml`: Interactive presentation and human interface (`nuo-tui`).
  - `client.toml`: Client session orchestration and output preferences (`nuo-client`).
  - `agent.toml`: Cognitive parameters, context admission, and tool governance (`nuo-harness`, `nuo-agent`).
- **Secret & State Firewall (`[INV-CONF-03]`)**: Absolute physical boundary between user configuration (`$XDG_CONFIG_HOME/nuo`), dynamic runtime state (`$XDG_STATE_HOME/nuo`), and 0600 secrets (`credentials.toml`, `auth.toml`).
- **Fail-Fast Schema Enforcement (`[INV-CONF-04]`)**: Unknown keys, type errors, and deprecated keys reject load explicitly rather than silently decaying to defaults.

---

## Decision Outcome

Chosen Architecture: **Four-Pillar Domain-Separated Configuration Topology**.

```text
$XDG_CONFIG_HOME/nuo/          <--- 100% PURE CONFIGURATION (DOTFILES-SAFE)
├── server.toml          ──> nuo-server (Daemon lifecycle, listener sockets, local auth)
├── terminal.toml        ──> nuo-tui (Themes, keybindings, view collapse states)
├── client.toml          ──> nuo-client / CLI (Connection defaults, retries, model favorites)
└── agent.toml           ──> nuo-harness / nuo-agent (Cognition, context policy, security, MCP, web)

$XDG_STATE_HOME/nuo/           <--- 0600 PROTECTED LOCAL STATE & CREDENTIALS (ADR-0032)
├── credentials.toml     ──> 0600 Static API Keys & credentials firewall (ADR-0032)
├── connections.toml     ──> Dynamic endpoint registrations & model catalog bindings
└── auth.toml            ──> 0600 OAuth token refresh grants & PKCE credentials
```

### Invariants & Behavioral Boundaries

- **`[INV-CONF-01] Strict Vocabulary Standard`**:
  All historical aliases (`default_provider`, `provider_retry_*`, `master.*`, `[websearch]`, `tui.toml`) are eliminated from parsing contracts. Unrecognized fields are rejected.
- **`[INV-CONF-02] Domain Boundary Isolation`**:
  `nuo-server` must never read `terminal.toml`. `nuo-tui` must never parse `server.toml`.
- **`[INV-CONF-03] Compaction Retirement`**:
  `nuo_wire::CompactionPolicy` is completely removed from active configuration structs. All context lifecycle logic binds exclusively to `nuo_wire::context_lifecycle::ContextPolicy`.
- **`[INV-CONF-04] Deterministic Offline Migration`**:
  A deterministic upgrade command `nuo config migrate` translates legacy monolith `config.toml` + `tui.toml` installations into the new matrix without runtime fallback shims.

---

## Rejected Alternatives & Negative Knowledge

### 1. Retaining `config.toml` as an Aggregator with Sub-tables
- *Idea*: Keep a single file but rename sections to `[server]`, `[terminal]`, `[client]`, `[agent]`.
- *Why Rejected*: Fails the containerization boundary. A remote/headless daemon container would still require mounting client/terminal configs, and file locks on `config.toml` during TUI preference changes would contend with daemon state flushing.

### 2. Runtime Backward-Compatibility Alias Decoding
- *Idea*: Accept `server.toml` or fallback to `[daemon]` in `config.toml` indefinitely.
- *Why Rejected*: Violates the "No Legacy Burden" directive. Dual-resolution paths cause subtle bugs where edits to the modern file are masked by forgotten legacy files, and inflates codebase complexity.

### 3. Merging `client.toml` and `terminal.toml`
- *Idea*: Have a single `client.toml` containing both retry/connection defaults and terminal keybindings.
- *Why Rejected*: Headless invocation (`nuo run`, `nuo -p`, CI/CD pipelines) is a first-class citizen that requires client routing policies without importing or linking terminal presentation concerns.

---

## Migration Strategy

1. **Phase 1: Canonical Path Specification**:
   Add `server_config_file()`, `terminal_config_file()`, `client_config_file()`, and `agent_config_file()` to `nuo-host::paths`.
2. **Phase 2: Core Domain Schema Separation**:
   Define independent domain structs in `nuo-persistence` and deprecate legacy monolithic aggregator.
3. **Phase 3: Clean-Break Removal**:
   Eradicate legacy compaction fields, web connection migration bridges, and alias fallbacks.
4. **Phase 4: Automated Offline Migration Utility**:
   Provide `nuo config migrate` to convert legacy setups cleanly.
