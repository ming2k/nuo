# Changelog

All notable changes to this project are documented in this file.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/), and
the project adheres to the federated SemVer model described in
[ADR-0004](docs/adr/0004-federated-cluster-semver-and-release-topology.md).

## [Unreleased]

## [0.0.5] - 2026-10-08

### Added

- **Streamed tool-input progress (ADR-0026):** Announce the tool call before its
  arguments finish streaming. Emits `RoundEvent::ToolCallStarted` (name known,
  arguments still streaming) plus count-only `RoundEvent::ToolInputProgress`
  ticks; the full argument object still gates execution, but the UI reflects
  the tool phase and running step immediately.

### Changed

- **`Esc Esc` (interrupt) is now scene-scoped.** Pressing it inside a zoomed
  subagent stops *that subagent* instead of the enclosing primary round — it no
  longer penetrates the scene boundary and cancels the whole outer turn (and
  every sibling subagent). The Subagent scene reads the *viewed child's* own run
  state for the interrupt affordance and arm window, so a finished child under a
  still-running parent no longer advertises an interrupt. The change adds the
  wire verb `AgentRequest::InterruptSubagent { call_id }` (routed by the driver
  into the one child via the shared `SubagentRegistry`; the parent round and its
  siblings are untouched), an `InputAction::InterruptSubagent`, and a
  scene-target dispatch enum shared by the Conversation / Aside / Subagent Esc
  ladders. Adds `[INV-TUI-SCOPE-01..03, INV-WIRE-SCOPE-01]` (ADR-0025).

- **Search-result blocks are now layered instead of a flat run of rows.** An
  expanded `search_text` step draws a top count band (`Found N matches · M
  files`, bold on a brand-tinted surface), a per-file title band carrying the
  path (bold on a raised surface), and the match rows on the code surface with
  the literal query bolded inside each line. The three tiers each sit on their
  own background (`theme::match_count_surface` > `match_title_surface` >
  `code_surface`), so a file heading and the matched text read as distinct from
  the surrounding content rather than blending into it. The count band appears
  only when the tool emitted its `Found N match(es):` header (a structured
  `Matches` payload drops it, so no band is invented); the literal-query
  highlight is skipped when the search ran as a regex. Listings
  (`find_files`/`list_dir`) are unchanged.

- **`ask_user` steps render as a readable question→answer list.** An expanded
  `ask_user` step previously fell through to the generic code renderer, dumping
  the harness's answer JSON (`User answered the question(s). Selected option
  labels:\n[[…]]`) as a line-numbered blob — the questions were unrecoverable and
  the answers unreadable. The step now declares a dedicated `ResultKind::Questions`
  renderer that rejoins the questions (recovered from the call's `arguments`:
  header chip, text, option count, multi-select flag) with the recorded selection
  from the result, drawing each question as `[header]` / `question (N options[,
  multi-select])` with its picks on dim `↳` rows (multi-select continuations
  align under the first label), and a cancelled request as `↳ cancelled — no
  answer` rather than an empty array. The collapsed header is now count-led
  (`Ask N question(s) · <header chips>`) instead of privileging the first
  question. Parsing is defensive: a restored or truncated call degrades to the
  raw result text instead of panicking.

## [0.0.4] - 2026-10-06

### Changed

- **Unified the legacy `muta` / `mutx` on-disk vocabulary into the `nuo`
  namespace.** TUI preferences moved from `$XDG_CONFIG_HOME/mutx/config.toml`
  to `$XDG_CONFIG_HOME/nuo/tui.toml` — a sibling of the daemon's `config.toml`,
  not a field inside it. Themes and the ASCII logo resolve under
  `$XDG_CONFIG_HOME/nuo/`. Runtime environment variables were renamed from
  `MUTA_*` / `NUOX_*` to `NUO_*` (`NUO_HOME`, `NUO_PORT`, `NUO_LOG`,
  `NUO_LOG_RETENTION`, `NUO_BIN`, `NUO_HEADLESS`, …), the daemon log file is
  now `nuo.log`, and user-facing strings plus MCP / OpenRouter identities and
  provider `referrer` / user-agent values no longer carry the old name. The
  pre-rename locations and the one-shot `history.json` legacy import are
  dropped outright — no compatibility shims.
- **Two-row head band: uniform session identity + scene row (ADR-0024):** the
  head band's two rows now have fixed jobs on every scene. Row 1 is always the
  ambient **session identity** (`SESSION` + id tail + `[ROLE]` badge +
  workspace); row 2 is the **scene row**, naming the scene the user stands in in
  plain lowercase (`conversation`, `dashboard`, `settings`, `subagent`,
  `aside`) followed by its context (the chat title, the fleet summary, the
  settings breadcrumb, …). The session's `UNATTENDED` / `UNCONFINED` run-mode
  flags move from the conversation's row 1 to row 2's right edge, beside the
  standing `C-x menu` namespace pair.
- **Command Palette entry is now `C-x p` (ADR-0023):** the palette's sole
  canonical chord is the `Ctrl+X` scene namespace's switcher verb. The former
  `Ctrl-L` binding — and the router special-case that *disabled* it behind other
  modals — are retired, so the palette opens from every context through one
  discoverable entry. `Ctrl-L` is now inert; a user can still remap `palette` in
  `[keybindings]`.
- **Standing `C-x menu` head legend (ADR-0023):** row 2 of every scene's head
  band now always carries the `C-x menu` namespace pair (the scene namespace's
  renamed label), so the palette / switcher entry is visible everywhere. The
  session head's row-1 `Ctrl-l palette` keycap is removed.
- **Borderless floating popups (ADR-0023):** the which-key card, tooltip,
  dropdown, and popover render without edge lines on modern (TrueColor /
  256-color) terminals, separated by an elevated background instead — matching
  the toast's visual language. Hybrid (ANSI-16) and monochrome terminals keep
  their frames.

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
