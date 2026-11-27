---
id: ADR-0269
title: "Workspace Crate Identity: Every Library Crate Carries the nuo- Prefix"
status: accepted
date: 2026-11-27
scope: workspace/topology, cargo, governance, docs
superseded_by: null
amends:
  - "[ADR-0181](0181-cognitive-runtime-identity-nuo-engine-is-the-only-crate-name.md) (its 2026-11-25 amendment named the runtime crate `engine`; the crate is `nuo-engine` again)"
  - "[ADR-0187](0187-documentation-governance-is-mechanical.md) (the retired-crate-name scan matches whole tokens and retires the unprefixed `crates/<name>` paths)"
negative_knowledge: true
---

# 0269. Workspace Crate Identity: Every Library Crate Carries the nuo- Prefix

- **Status**: Accepted
- **Date**: 2026-11-27
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0181](0181-cognitive-runtime-identity-nuo-engine-is-the-only-crate-name.md), [ADR-0187](0187-documentation-governance-is-mechanical.md)
- **Related**: [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0109](0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md), [ADR-0178](0178-layered-test-architecture-and-hermetic-e2e-boundary.md)

---

## Context and Problem Statement

Commit `5c442a3` (2026-11-15) removed the `nuo-` prefix from every library
package as part of a layout change. No ADR recorded the rename or its
rationale; ADR-0181's amendment only noted after the fact that it had
happened. The result was a workspace of 37 crates with generic names:
`engine`, `protocol`, `server`, `host`, `tool`, `view`, `provider`, and
`persistence` among them.

Those names cost more than they save:

1. **They collide with the code inside the workspace.** Several crates hold
   a local module with the same name as a sibling crate (`engine::runtime::host`,
   `nuo::shell::palette::view`, `engine::…::subagent_tool::tool`,
   `server::…::commands::provider`, the integration suites `mod engine;` /
   `mod host;` / `mod tool;`). In those scopes `host::X` names a local module
   and the crate has to be reached as `::host::X`. That is how
   `::tool::ToolOutput` and its kind ended up spelled out in about 520 places. Whether a
   bare `tool::` means the crate or the module depends on the surrounding
   scope, so a reader cannot tell without resolving imports.
2. **They cannot leave the repository.** The workspace already designs for
   outside consumers: `protocol` and `acp` are wire contracts, `tool` and
   `tool-derive` are the tool SDK, and ADR-0015 inverts provider dependencies
   so third-party provider crates are possible. Names like `engine`,
   `protocol`, `tool`, `server`, and `view` are taken on crates.io. Publishing
   any of these crates would force a rename under time pressure and leave a
   workspace with half prefixed and half unprefixed names.
3. **A rename only gets more expensive.** Every new import, path, script,
   and document adds to the cost. This change touched about 10,700 code paths
   and 800 files. Deferring it would make it larger, not smaller.

## Decision

### 1. One identity per crate

Every package in the `crates/` tier is either the application `nuo` or a
library named `nuo-<role>`. The package name is the crate's only identity:

| Surface | Form | Example |
| --- | --- | --- |
| Package / `-p` flag | `nuo-<role>` | `cargo nextest run -p nuo-engine` |
| Directory | `crates/<package>` | `crates/nuo-engine` |
| Library path in code | Cargo-derived, `-` → `_` | `use nuo_engine::…` |
| Dependency key | the package name, never an alias | `nuo-engine = { workspace = true }` |

The root dev-only test host keeps its existing name, `nuo-tests`.

There is no `[lib] name` override, no `package = "…"` rename of a workspace
crate in any dependency table, and no `extern crate … as …` alias
(`[INV-ENGINE-02]`). The one remaining `extern crate self as nuo_tool` exists so
that `#[derive(ToolArgs)]` output (`::nuo_tool::ToolArgs`) resolves inside
`nuo-tool`'s own tests. It names the crate by its own name, so it is not an
alias.

### 2. Clean break, no compatibility layer

The old names are gone, and nothing re-exports, aliases, or redirects them.
Source, manifests, governance gates, CI, living docs, rustdoc, and crate
READMEs use the new names. Historical ADRs and `CHANGELOG.md` entries keep the
names that were current when they were written (ADR-0187 §3); only ADR link
targets were rewritten, so they still resolve.

The integration suites under `tests/integration/` keep their subsystem module
names (`mod engine;`, `mod host;`, …) on purpose. A suite named `nuo_engine`
would shadow the extern crate `nuo_engine` at the integration root, which is
exactly the kind of collision this decision removes.

### 3. Names are matched exactly, never by substring

Gates keyed on crate names compare exact package names. They do not use
substring search, and none of them may pass vacuously:

- The presentation firewall (`nuo-test-support::dev::firewall`) fails with
  `R0` if a governed crate (`nuo`, `nuo-server`, `nuo-ui-primitives`,
  `nuo-view`) is missing. Without that check, a renamed crate would turn its
  rule into a silent no-op. The lean-link gate reads `cargo tree --format {p}`
  and compares package names exactly.
- Manifest edge gates (`protocol_layering`, `guardrails_has_no_persistence_edge`,
  `filesystem_tools_have_no_document_parser_edge`) read dependency tables
  through `tests/common::manifest_dependencies`, which parses the TOML, so a
  comment or description can neither trip a gate nor satisfy one.
- The living-doc and rustdoc retired-name scans (ADR-0187 `[INV-DOC-03]`)
  match whole tokens, so the retired `nuo-ui` is never found inside the
  current `nuo-ui-primitives`. They also list every unprefixed
  `crates/<name>` path as retired.
- A comment that cites `nuo_<crate>::<name>` must name something that crate
  defines. rustdoc checks intra-doc links only on documented public items, so
  a backticked path, or a link on a private item, is otherwise unchecked.
  The rename itself showed the risk: a mechanical prefix had turned
  `host::run` (the server's own `runtime::host` module) into `nuo_host::run`,
  a function the `nuo-host` crate does not have.

### 4. Paths are relative unless hygiene requires `::`

Because no local item can be called `nuo_*`, a path that starts with a crate
root needs no leading `::`. The ~520 `::tool::…` / `::provider::…` paths that
existed only to step around a same-named local module are now plain
`nuo_tool::…`. Only code that expands in someone else's scope keeps the
absolute form: `macro_rules!` bodies (`nuo_protocol::register_tool!`) and the
`#[derive(ToolArgs)]` output (`::nuo_tool::ToolArgs`), where `::` guards the
expansion against the caller's own items.

### 5. Invariant

- **`[INV-NAME-01]` Prefixed, Directory-Matched, Unaliased Crate Identity**:
  every package in `crates/` is `nuo` or starts with `nuo-`; it lives in
  `crates/<package name>`; it keeps the library name Cargo derives from the
  package name; every in-house dependency edge uses the crate's own name; the
  root `[workspace.dependencies]` entry points at `crates/<package name>`;
  and member manifests reach in-house crates through `{ workspace = true }`
  instead of a `path` of their own; and a comment that cites
  `nuo_<crate>::<name>` names something that crate defines. Gated by
  `tests/integration/governance/crate_naming.rs` (CI step `crate naming`).

## Consequences & Trade-offs

- **Positive**: A path reads the same in every scope. `nuo_host::paths`
  is always the crate and `host::…` is always local, so the `::tool::`
  disambiguation idiom is no longer needed. Any crate can be published
  without a second rename. `-p` flags, directories, and imports all share one
  name.
- **Negative & Incurred Costs**: Paths and `-p` flags are longer
  (`cargo test -p nuo-provider-google-antigravity`). The change is one
  mechanical, workspace-wide diff, so open branches will conflict on imports
  and paths.
- **Wire compatibility**: The change does not alter behaviour or the wire
  format. The wire census hashes declaration *text*, so the two shapes whose
  field types are path-qualified (`AgentResponse`, `RoundEvent`) were renewed.
  Their declarations differ only by the `nuo_` qualifier, so `PROTOCOL_VERSION`
  stays at 37.
- **Observability**: The default tracing target of every event is its module
  path, which now starts with `nuo_`. `RUST_LOG` directives must use the new
  crate names (`nuo_engine=debug`). The explicit targets `nuo_inference_wire::provider`
  and `nuo_protocol::agent` were renamed to match.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep the unprefixed names
- **Why considered**: No churn, and `publish = false` everywhere means no
  crates.io collision today.
- **Why rejected**: It does nothing about the module/crate collisions above,
  which cost readers every day. It also keeps the rename cost growing until
  the first publish forces the change anyway.

### Alternative B — Prefix only the crates that get published
- **Why considered**: Only publishable crates strictly need a unique name.
- **Why rejected**: It leaves two naming rules in one workspace, and every
  publish decision becomes a rename. The one-name rule (ADR-0181) is easier
  to state and to enforce.

### Alternative C — Rename packages but alias them back in dependency tables
- **Why considered**: `engine = { package = "nuo-engine", … }` would leave
  every `use engine::…` untouched.
- **Why rejected**: The ambiguous identifier survives in code while the
  manifest claims otherwise. That is the dual-name failure mode ADR-0181
  removed for `nuo_harness`, and `[INV-NAME-01]` now forbids it.

### Alternative D — Prefix packages but keep the short directory names
- **Why considered**: Smaller diff and shorter paths.
- **Why rejected**: `crates/engine` holding `nuo-engine` breaks the one-name
  rule. Every `-p`, path citation, and governance gate would need a mapping
  between the two.
