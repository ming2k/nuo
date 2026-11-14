---
id: ADR-0191
title: "Persistence Failure Is Fail-Fast Without a Database CLI"
status: accepted
date: 2026-11-14
scope: persistence, database, tui, server
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0191. Persistence Failure Is Fail-Fast Without a Database CLI

- Status: Accepted
- Date: 2026-11-14

## Context and Problem Statement

A proposal during the database rewrite added `nuo db status`, `nuo db check`,
`nuo db backup`, and `nuo db repair`. These are operator tools for a database
product. Nuo ships one embedded SQLite file used by one application; the user
is a developer running an agent, not a DBA. A command surface that mirrors
Flyway's administration model would be permanent API and documentation weight
for an event that should be rare and actionable.

## Architectural Decisions

### 1. No `nuo db` subcommand

The application does not expose database administration verbs. There is no
`status`, `check`, `repair`, `migrate`, `baseline`, or `backup` command. The
database opens itself before serving and either succeeds or returns one clear
error.

### 2. One actionable failure message

A refused open names:

- what is wrong (retired pre-baseline file, checksum mismatch, missing/changed
  schema object, newer database, integrity failure);
- the database path;
- the action: move the file aside to start fresh, restore a backup, or upgrade
  the binary.

The message travels through the existing session-preparation failure path, so
the TUI shows it instead of an empty session.

### 3. Recovery is file-level

The single-writer lock and the one-file database make backup and restore a file
operation. The application never deletes, renames, or rewrites a user's
database to "repair" it. Starting fresh is an explicit user action (move the
file aside). This is also why the clean-break cut refuses pre-baseline files
rather than converting them (ADR-0189).

### 4. Future migrations own their backup story

The baseline cut has no pending migration, so there is nothing to snapshot yet.
A future numbered migration MUST add an upgrade test from the previous release
and a tested backup/restore path before it ships. The migration transaction
itself is atomic: a failure leaves the database at the previous version. The
runner will not grow a generic repair mode; a migration that cannot be applied
is a release blocker, not an operator puzzle.

## Consequences

- **Positive**: no permanent command surface for a rare event; no second schema
  authority; no code path that mutates a database outside the one writer. The
  error is visible where the user already looks.
- **Negative & Incurred Costs**: users must use the filesystem for backup and
  recovery; there is no guided repair for a corrupted file. A future migration's
  backup behavior must be designed and tested explicitly rather than inherited
  from a generic tool.
- **Neutral**: logging still records the refused open and the database path;
  support can diagnose from the log without a CLI.

## Discarded Alternatives (Negative Knowledge)

- **A `nuo db` command family**: overdesign for an embedded single-file store;
  it duplicates the migration runner's knowledge behind a second interface.
- **Automatic repair of the ledger or schema**: turns a detected inconsistency
  into silent data mutation; the ledger's value is that disagreement is loud.
- **Automatic rename/delete of an incompatible database**: destroys or hides
  user data without consent.
- **Auto-migrating pre-baseline databases**: keeps the retired chain alive and
  contradicts the clean break.

## Links

- [ADR-0189](0189-immutable-baseline-plus-numbered-migrations.md)
- [ADR-0190](0190-schema-integrity-is-verified-structurally-at-open.md)
