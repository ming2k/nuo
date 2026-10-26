# Engineering Workflow & Verification Gates

This document codifies the operational workflows, PR review gates, and verification criteria for changes to Nuo.

---

## 1. Pull Request Review Gates

Before any pull request is merged into `main`, it must clear the following verification gates:

```text
┌─────────────────────────────────────────────────────────────┐
│ 1. Compilation Gate: `cargo check --workspace`              │
│    Zero warnings, zero unresolved symbols, all crates green │
├─────────────────────────────────────────────────────────────┤
│ 2. Automated Test Gate: `cargo test --workspace`            │
│    All unit, integration, and regression tests pass         │
├─────────────────────────────────────────────────────────────┤
│ 3. Architecture Gate: Subsystem Boundary Compliance         │
│    No reverse dependencies (e.g. server -> client)          │
├─────────────────────────────────────────────────────────────┤
│ 4. Code-to-Doc Synchronization Gate                         │
│    API/CLI changes matched with documentation updates       │
├─────────────────────────────────────────────────────────────┤
│ 5. Documentation Integrity Gate: `cargo doc`                │
│    Broken/private intra-doc links and invalid HTML fail     │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. Code-to-Doc Synchronization (Atomic Commits)

Any pull request that introduces or modifies developer-visible or operator-facing surfaces must update the corresponding documentation within the **same atomic pull request**:

| Modified Surface | Required Documentation Synchronization |
| :--- | :--- |
| CLI commands, arguments, or flags (`nuo`) | `docs/reference/commands.md` |
| IPC protocol schemas (`crates/nuo-protocol`) | Relevant ADR and protocol reference |
| Model provider contracts (`providers/*`) | `docs/how-to/connect-a-provider.md` |
| Major architectural restructuring | New or amended ADR in `docs/adr/` |

---

## 3. Pre-Release Quality Checklist

Prior to creating a release tag:

1. **Workspace Compilation**: Run `cargo check --workspace` and verify zero errors.
2. **Full Test Suite**: Run `cargo test --workspace` across all default members.
3. **Changelog Integrity**: Ensure user-facing features, breaking changes, and migrations are documented in `CHANGELOG.md`.
4. **Clean Git State**: Verify no untracked experimental files or temporary debug assets remain in the tree.
   Because a release is cut from a *commit*, not from a working directory, this also covers the inverse and more
   damaging case: **new source files must be tracked**. `git status --porcelain` must show no `??` entries under
   `crates/`, `providers/`, `tools/`, `nuo/`, or `docs/` — an untracked module compiles locally and is absent from
   every tag, so the release artifact fails for everyone but the author. Use `git status --porcelain | grep '^??'`
   and stage the module, ADR, or test the change introduced.
5. **Formatting & Lints**: `cargo fmt --all --check` and
   `cargo clippy --workspace --all-targets -- -D warnings` both silent. The
   workspace denies `unwrap_used`/`expect_used`/`panic`/`todo`/`dbg_macro` for
   shipped code; test targets exempt themselves at the test boundary.
6. **ADR Lineage**: `cargo test -p nuo --test doc_governance -- test_adr_lineage`
   clean — every `ADR-NNNN` citation resolves inside `docs/adr/` (`docs/dev/standards/adr.md` §4).
7. **Documentation Integrity**: citations are checked on both surfaces, because
   prose and code drift for different reasons.
   - `cargo doc --workspace --no-deps` clean — every Rust intra-doc link
     resolves, no public doc reaches a private item, and no invalid HTML. The
     workspace denies these three lints
     (`Cargo.toml` `[workspace.lints.rustdoc]`); rustdoc is the only compiler
     pass that evaluates them.
   - `cargo test -p nuo --test doc_governance -- test_doc_links` clean — every
     relative link across `docs/**` and the root `*.md` resolves to a file, every
     `#anchor` matches a heading, no absolute or `file://` locator, and no public
     doc crosses into `docs/dev/**` (`docs/dev/standards/style.md` §1, `taxonomy.md` §2).

   ADR-0027 and ADR-0102 relocated whole modules; roughly forty Rust citations
   and one Markdown link rotted silently until these gates existed.
8. **Version Coherence**: `[workspace.package].version`, the Cluster C entries in
   `[workspace.dependencies]`, and the top `CHANGELOG.md` section agree, and the
   changelog section for the version being tagged describes *that* code — not the
   previous tag's. `nuo --version` after a release build must print the version
   being released.
9. **Release Build**: `cargo build --release -p nuo` succeeds locally, since the
   binary is the primary artifact.

All nine run in CI (`.github/workflows/ci.yml`) on every push and pull request;
the release job additionally builds the tagged binary for each platform.
