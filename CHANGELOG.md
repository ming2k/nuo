# Changelog

All notable changes to this project are documented in this file.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/), and
the project adheres to the federated SemVer model described in
[ADR-0004](docs/adr/0004-federated-cluster-semver-and-release-topology.md).

## [0.0.3] - 2026-10-05

### Added

- **Daemon dev-drift detection by content identity (ADR-0021):** the daemon
  publishes a bounded digest of its own executable image in the discovery
  record, so a client detects a rebuilt (same-version, same-protocol) daemon by
  comparing executable content rather than the Linux-only inode probe.
- **Idle-gated self-heal:** when the running daemon's image differs from the
  installed one and it is provably idle (no active sessions or daemon tasks),
  the client reclaims it through the canonical drain pipeline and respawns the
  fresh build. A busy daemon is refused with a message naming the work that
  would be interrupted.
- `nuo status --diagnostic` now renders a `Core Image` block (installed vs.
  daemon digest) and a rebuilt-binary drift diagnosis.

### Changed

- Comment and documentation drift swept: legacy crate names (`muta-*` → `nuo-*`),
  broken ADR links, and stale module docs.
- User-facing strings aligned with the `nuo` product name (`nuo stop`,
  `nuo client` update recommendations, daemon banner and log prefixes).

### Removed

- Dead duplicate modules `nuo-server/src/{client,identity,supervisor}.rs` and the
  orphan `nuo/src/client.rs`.
- The dead `discovery_path` helper and the unused `supervise` alias.

### Fixed

- An orphaned integration test (`nuo/tests/it/daemon_spawn.rs`) that was never
  compiled; it is now declared and exercises daemon spawn isolation.

## [0.0.2] - 2026-10-04

Initial tagged release: the unified `nuo` binary, the extracted `nuo-server`
container runtime, decoupled capability tools, and the `providers/` namespace.
