---
id: ADR-0212
title: "Shell-Aware Command Permissions and Role-Scoped SQLite Store"
status: accepted
date: 2026-11-18
scope: tool/events, guardrails/command_parse, guardrails/permission_store, guardrails/permission_policy, persistence/db/permissions, engine/runtime/permission_backend, server, nuo
superseded_by: null
supersedes:
  - per-project `permissions.json` permission storage and `ProjectPaths`
  - whole-string-only `execute_command` `Always` grants
negative_knowledge: false
---

# 0212. Shell-Aware Command Permissions and Role-Scoped SQLite Store

- **Status**: Accepted
- **Date**: 2026-11-18
- **Complements**: [ADR-0188](0188-guardrails-depend-on-contracts-not-persistence.md), [ADR-0189](0189-immutable-baseline-plus-numbered-migrations.md), [ADR-0208](0208-tool-contract-validation-snapshots-and-deferred-discovery.md)

## Context

Three structural weaknesses existed in the previous permission model:

1. **Exact whole-string command matching**: `ExecuteCommandTool::scope_target` emitted the entire shell string as `ScopeTarget::Command(cmd)`, and `PermissionDecision::Always` persisted that exact string. Running `echo 1` and pressing `Always` still prompted on `echo 2`, pushing operators toward blanket `/auto-approve`.
2. **Unsafe naive prefix wildcards**: Hand-authored `[permissions].allow` rules ending in `*` (e.g., `"git status *"`) performed raw `starts_with` matching without shell tokenization. A compound command like `git status && curl ...` matched `"git status *"`, and `"git status"` (no trailing space) failed to match `"git status *"`.
3. **Workspace-only JSON persistence (`permissions.json`)**:
   - `PermissionStore` stored rules in a per-project JSON file (`permissions.json`) using full-file overwrite, losing concurrent updates across threads.
   - Workspace-free roles such as `ops` and `philosophist` had `workspace_root = None`, so `PermissionStore::set_project_root(None)` disabled persistence entirely — `ops` could never persist `Always` approvals.
   - Sharing one JSON bucket per workspace leaked approvals across distinct roles in the same repository.

## Decision

### 1. Shell-Aware Command Parsing and Safe Prefix Grants (`guardrails::command_parse`)

- Parse `execute_command` inputs with a quote-aware shell tokenizer before evaluating `PermissionStore` rules:
  - Split compound pipelines and chains (`&&`, `||`, `;`, `|`, newlines) into individual segments. **Every** segment must pass `is_allowed` (and not match `is_denied`) for a compound command to pass.
  - Detect shell hazards per segment: output redirections (`>`, `>>`, `>|`, `&>`), command/process substitutions (`$(...)`, `` `...` ``, `<(...)`, `>(...)`), subshells (`(...)`, `{ ... }`), and execution-hijacking environment overrides (`LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_*`, `PATH`, `IFS`, `BASH_ENV`, `ENV`, `GIT_SSH_COMMAND`, `GIT_EXEC_PATH`, etc.).
  - A trailing-`*` prefix rule (e.g., `"cargo test *"`) **only** matches simple hazard-free segments on word boundaries (both `"cargo test"` and `"cargo test ..."`). Compound commands, redirections, substitutions, subshells, and unsafe env overrides never match prefix rules; they require every sub-segment to pass or an exact/full-tool rule.
- On `PermissionRequest`, `BrokerPolicy` populates `suggested_scope: Option<String>` for safe, simple commands while preserving the literal exact `rule`:
  - Known multi-word subcommand tools (`git`, `cargo`, `npm`, `pnpm`, `yarn`, `bun`, `docker`, `kubectl`, `helm`, `terraform`, `gh`, `rustup`, `pip`, `uv`, `go`, `systemctl`, `brew`, `nuo`, etc.) suggest `<program> <subcommand> *` when a plain subcommand token is present.
  - Other safe single-verb commands (`echo`, `rg`, `ls`, `cat`, `make`, `pytest`, etc.) suggest `<program> *`.
  - Shells, interpreters, privilege escalators, and network fetchers (`sh`, `bash`, `zsh`, `fish`, `pwsh`, `python`, `node`, `ruby`, `perl`, `sudo`, `env`, `xargs`, `find`, `curl`, `wget`, `ssh`, `eval`, `exec`) **never** auto-suggest a wildcard prefix (`suggested_scope = None`); `Always` persists only the exact command.
- **Explicit Prefix vs Exact Grant Choice (`PermissionDecision::AlwaysExact`)**:
  - When `suggested_scope` is `Some(...)` (and `!one_off`), the TUI confirmation sheet and headless prompt expose **both** `Always` (persists `suggested_scope`, e.g. `cargo test *`) and `AlwaysExact` (persists the literal exact command, e.g. `cargo test --lib`). When `suggested_scope` is `None`, only the standard single `Confirm always` / `Cancel` flow is shown.
- **Zero-Prompt Safe Read-Only Shell Classifier (`is_segment_safe_readonly`)**:
  - Within `BrokerPolicy` (strictly *after* `HookPolicy`, `DisabledPolicy`, `DenyPolicy`, `BashPolicy`, and `PermissionStore::is_denied`), `execute_command` evaluates `is_command_admitted` across all parsed segments: each segment passes if covered by `session` / `always` **or** classified as provably read-only (`ls`, `cat`, `head`, `tail`, `wc`, `stat`, `file`, `du`, `df`, `rg`/`grep` without `--pre`/`--hostname-bin`/`-f`, `tree` without `-o`, `git status/diff/log/show/rev-parse/ls-files/grep/blame/describe/shortlog` and read-only `branch`/`tag`/`remote` without global `-c`/`--output`/`--ext-diff`, `cargo metadata/tree/pkgid/locate-project`, version queries, `pwd`/`echo`).
  - Only bare executable names (no `/` or `\`) with no shell hazards (`is_prefix_eligible()`) and strictly workspace-relative paths (rejecting leading `/`, `~`, `\`, Windows drive/UNC forms, `$VAR`/`${VAR}`/`%VAR%` expansion, and any `..` traversal component) qualify. Any escape or mutation falls through to normal interactive permission prompts, and explicit `[permissions].deny` rules still hard-deny even read-only commands.

### 2. SQLite Persistence in `nuo.db` (`0003_permission_rules.sql`)

- Replace `permissions.json` with a `STRICT` SQLite table `permission_rules` in `nuo.db` (migration v3), accessed exclusively through `persistence::db::permissions` (`one_door` invariant) and exposed to `guardrails` through a `PermissionBackend` trait (preserving ADR-0188 decoupling).
- Declarative `[permissions].allow` / `[permissions].deny` in `config.toml` remain read-only TOML inputs; runtime `Always` grants and `revoked` suppressions live in SQLite with row-level upserts (`created_at_ms`, `updated_at_ms`, `last_used_ms`).
- **Debounced Asynchronous `last_used_ms` Tracking**: Matched `always` rules enqueue a non-blocking, fire-and-forget `TouchPermissionRuleLastUsed` command (`try_touch_permission_rule_last_used`) to the persistence actor, debounced per `(scope_key, rule)` over a 60-second window inside `PermissionStore` so hot-path checks never block on SQLite writes and failed compound commands never bump timestamps.

### 3. Composite Scope: `role_id + optional workspace`

- Replace `set_project_root(Option<PathBuf>)` with `set_scope(role_id, Option<PathBuf>)`:
  - `scope_kind = 'workspace'`, `workspace_key = <canonicalized workspace path>` for workspace-bound roles (`developer`, custom workspace roles).
  - `scope_kind = 'global'`, `workspace_key = ''` for workspace-free roles (`ops`, `philosophist`, global custom roles).
- Queries for a `(role_id, workspace)` scope load both `(role_id, 'workspace', workspace_key)` and `(role_id, 'global', '')` rules, with workspace-scoped revocations suppressing inherited global config/rules cleanly.
- **Zero-legacy clean break & no `permissions.json` auto-migration**: Remove `permissions.json` persistence, `dirs.project_permissions`, `ProjectPaths` / `ProductProjectPaths` / `NoProjectStorage`, and `Agent::set_project_root`. Legacy per-workspace `permissions.json` files lack `role_id` attribution and safe shell-boundary tokenization; auto-importing them into `(role_id, workspace)` or `global` buckets would silently mis-attribute or over-broaden historical wildcard approvals. Operators start cleanly on explicit SQLite approvals and `[permissions]` config rules.

## Consequences

- **Positive**: `Always` works naturally for `cargo test`, `git status`, `echo`, etc., without opening shell-chaining holes; `ops` and other workspace-free roles gain durable permissions; multi-thread concurrent approvals no longer overwrite each other; `nuo.db` owns all durable runtime state.
- **Negative**: Callers previously using `Agent::set_project_root` migrate to `Agent::set_permission_scope(role_id, workspace_root)` and bind the `PersistenceHandle` backend when persistence is desired.
