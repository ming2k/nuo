---
id: ADR-0032
title: "Relocation of Credentials to State Store and Dotfiles Secret Leaks Prevention"
status: accepted
date: 2026-10-11
scope: security/credentials, architecture/paths, governance/invariants, persistence/secrets
superseded_by: null
negative_knowledge: true
---

# 0032. Relocation of Credentials to State Store and Dotfiles Secret Leaks Prevention

- Status: Accepted
- Date: 2026-10-11
- Deciders: Nuo Architecture Working Group & Security Review Board
- Consulted: Security Engineering, Interface, Runtime, and Persistence Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0031](0031-domain-separated-configuration-matrix-and-legacy-retirement.md)
- Amends: Secret location contract in [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md) §"Secret Storage Firewall"

---

## Context and Problem Statement

Historically, Nuo separated API keys from behavior configuration by writing `credentials.toml` beside `config.toml` under `$XDG_CONFIG_HOME/nuo/credentials.toml` (ADR-0014). While file mode was forced to `0600` (owner read/write only), this layout created severe security hazards and architectural inconsistencies:

1. **The Dotfiles Exposure Trap**:  
   Modern developers routinely version-control their `$XDG_CONFIG_HOME` (`~/.config/`) using automated dotfiles managers (`chezmoi`, `yadm`, Git bare repositories). Placing static bearer tokens and provider API keys inside `~/.config/nuo/` resulted in frequent accidental pushes of live secrets to private or public Git repositories.

2. **Architectural Asymmetry**:  
   OAuth refresh tokens and dynamic authentication state were already located under `$XDG_STATE_HOME/nuo/auth.toml` (0600). Housing API keys under `config/` while housing OAuth tokens under `state/` created an arbitrary and indefensible dual-track secret hierarchy.

3. **Violation of the Clean Configuration Principle**:  
   `$XDG_CONFIG_HOME/nuo/` must contain purely declarative, non-sensitive, idempotent configuration files (`server.toml`, `client.toml`, `terminal.toml`, `agent.toml`) that can be safely shared, audited, and versioned without sanitization hurdles.

---

## Decision Drivers

- **Zero Secret Ingestion in Config (`[INV-SEC-01]`)**: `$XDG_CONFIG_HOME/nuo/` must contain zero plaintext keys, API tokens, or secrets.
- **Unified Secret Co-location (`[INV-SEC-02]`)**: All physical credential files (`credentials.toml`, `auth.toml`) must reside strictly under `$XDG_STATE_HOME/nuo/` with mode `0600`.
- **Transparent Migration (`[INV-SEC-03]`)**: Seamlessly detect and promote any legacy `$XDG_CONFIG_HOME/nuo/credentials.toml` into `$XDG_STATE_HOME/nuo/credentials.toml`, purging the legacy file from the config tree.
- **No In-Memory Secret Spillage**: Invariant that credentials remain shielded behind runtime accessors.

---

## Decision Outcome

Chosen Architecture: **Total Relocation of Credentials to State Store (`$XDG_STATE_HOME/nuo/credentials.toml`)**.

```text
$XDG_CONFIG_HOME/nuo/          <--- 100% SAFE FOR DEDICATED DTOFILES VERSIONING
├── server.toml
├── client.toml
├── terminal.toml
└── agent.toml

$XDG_STATE_HOME/nuo/           <--- PROTECTED 0600 LOCAL MACHINE STATE & SECRETS
├── credentials.toml           <--- [RELOCATED] Static API Keys (mode 0600)
├── auth.toml                  <--- Dynamic OAuth Token Grants (mode 0600)
└── connections.toml           <--- Dynamic Provider Registrations
```

### Invariants & Behavioral Boundaries

- **`[INV-SEC-01] Config Directory Sanitization`**:
  No routine command or persistence layer writes any credential file under `$XDG_CONFIG_HOME/nuo/`. Any remaining file at `$XDG_CONFIG_HOME/nuo/credentials.toml` is treated as a deprecated migration artifact.
- **`[INV-SEC-02] Canonical State Location`**:
  `nuo-host::paths::credentials_file()` resolves canonically to `$XDG_STATE_HOME/nuo/credentials.toml`.
- **`[INV-SEC-03] Automatic One-Way State Promotion`**:
  Upon boot or credentials load, if a legacy file exists at `$XDG_CONFIG_HOME/nuo/credentials.toml` and the state destination is absent, the secrets are migrated to `$XDG_STATE_HOME/nuo/credentials.toml` and the legacy config-dir file is removed or truncated.

---

## Rejected Alternatives & Negative Knowledge

### 1. Retaining `credentials.toml` in `$XDG_CONFIG_HOME` with a `.gitignore` Advice
- *Why Rejected*: Relies on fallible human discipline. Automated dotfiles tools often bypass standard `.gitignore` or copy directory contents recursively. Defense-in-depth requires architectural isolation.

### 2. Merging API Keys into SQLite Database (`nuo.db`)
- *Why Rejected*: API keys must be readily inspectable, editable, and provisionable by human operators via simple Unix text editors (`nano`, `vi`) or standard secret injection scripts without requiring SQLite binaries.

### 3. Merging `credentials.toml` directly into `auth.toml`
- *Why Rejected*: `auth.toml` is written dynamically by async background OAuth refresh engines across multiple concurrent sessions, whereas `credentials.toml` holds operator-managed static secrets. Isolating dynamic OAuth lease updates from static API keys prevents concurrent write contention.

---

## Verification & Impact

- `paths::get().credentials_file()` points to `state_dir.join("credentials.toml")`.
- `Credentials::load()` inspects both modern and legacy paths, atomically promoting to state store.
- Dotfiles management on `$XDG_CONFIG_HOME/nuo/` becomes safe for public git tracking.
