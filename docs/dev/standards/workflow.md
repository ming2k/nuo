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
