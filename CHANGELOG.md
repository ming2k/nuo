# Changelog

All notable changes to this project are documented in this file.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/), and
the project adheres to the federated SemVer model described in
[ADR-0004](docs/adr/0004-federated-cluster-semver-and-release-topology.md).

## [Unreleased]

## [0.0.13] - 2026-10-09

### Added

- **Thread domain nomenclature unification and compact TabBar affordance (ADR-0043).**
  Standardizes thread entity domain terminology across CLI (`nuo thread`),
  slash commands (`/threads`), and interactive picker surfaces (`ThreadsDialog`)
  while maintaining backward-compatible aliases (`/sessions`, `/resume`).
  Reorganizes the TabBar client menu affordance into a compact "label-first, dim-chord"
  `menu C-x` presentation, and completes the `C-x` Client Global Workspace namespace
  wiring Settings (`C-x ,`), Threads (`C-x s`), and Close Tab (`C-x w`).
- **Client viewport routing ownership, tab-autonomous scene stacks, and domain scene self-projection (ADR-0042).**
  Formalizes Row 1 / Row 2 client shell vs Row 3 / body domain scene ownership.
  Isolates per-tab navigation history with the root `Esc` invariant, and clarifies
  subagent drill-in vs thread fork orthogonality across TUI surfaces.

## [0.0.12] - 2026-10-09

### Added

- **Thread entity taxonomy and server SSOT fan-out (ADR-0038).** Eliminates
  session abstraction ambiguity by strictly separating transport connections, UI
  viewports, and persistent thread business entities. The server is the canonical
  single source of truth for thread state, fanning out updates across attached
  viewports.
- **Client-centric tab workspace and reactive surface sync (ADR-0039).**
  Unified scene and session tab management in the client workspace, with full
  reactive synchronization of surface and dialog states.
- **Cross-domain action matrix and polymorphic TUI architecture (ADR-0040).**
  Introduces domain-scoped action routing, spatial topology, and a redesigned
  three-tier view header cleanly decoupling display layout from input processing.
- **Thread command purity and explicit tab management topology (ADR-0041).**
  Pure thread command boundaries, strict viewport/workspace separation, and
  tab navigation shortcuts (`Alt+W`, `Alt+1..9`).

## [0.0.11] - 2026-10-08

### Added

- **Provider quota aggregation dashboard (ADR-0036, [INV-QUOTA-01..04]).** Nuo
  now separates forward-looking provider *capacity* (`Quota`) from historical
  token *consumption* (`Usage`). New typed wire aggregates
  (`ProviderQuotaSnapshot`, `ConnectionQuotaEntry`) and the
  `QueryProviderQuotas` request/reply pair back three new surfaces: a `/quota`
  TUI dashboard with ambient selectable account rows, and a headless
  `nuo quota [provider] [--refresh]` CLI verb. Quota fetch is type-scoped and
  concurrent across an account pool, reusing pooled connections with a
  memory TTL cache and explicit project-ID propagation.
- **`nuo quota` CLI command.** Fast, headless inspection of provider
  allowances, sliding-window buckets, credit balances, and account pools.
- **Public reference `docs/reference/lifecycle-and-action-semantics.md`.**
- **`edit_text` tool refinements** for terminal-side text editing.

### Changed

- **Session telemetry bifurcated into two surfaces (ADR-0037,
  [INV-STATS-01, INV-TRACE-01, INV-AFFORDANCE-01]).** The former tabbed
  telemetry modal is split into `SessionStats` (`/stats`, `Ctrl+O`) for
  context/token accounting and `SessionTrace` (`/trace`) for the
  round/turn/attempt execution waterfall. Lateral `Left`/`Right` tab chording
  is eradicated; arrow keys now exclusively scroll or select, and `Enter`
  semantics are consistent per surface, with a frictionless cross-surface
  affordance from Stats to Trace.

### Fixed

- **Deterministic pre-bootstrap takeover and lock ordering (ADR-0034
  [INV-LIFECYCLE-07]).** The server runtime now executes conflicting-process
  takeover and acquires the global instance lock (`server.lock`) *before*
  initializing the session registry or any persistence handles. Database
  handles are acquired with bounded-backoff polling and unrecoverable startup
  errors are no longer cached globally, preventing startup race conditions and
  handle poisoning.

## [0.0.10] - 2026-10-07

### Changed

- **Domain-scoped surface architecture completed (ADR-0035).** Every dialog is
  now an encapsulated entity owning its own cursor, scroll, embedded
  `TextInput`, and sub-layer state; the overlay stack owns each open dialog's
  `Box<dyn DialogView>`; dialogs declare a single `DialogScope`, are gated by
  `is_available`, and are evicted only through reverse-LIFO unwinding that runs
  `on_dismiss`. Removed the shared `App` scratchpad fields, the composer-draft
  hijacking, and the blunt `overlay_stack.clear()`.
- **Configuration matrix is the sole runtime source (ADR-0031).** `Config::load`
  reads only `server.toml` / `client.toml` / `agent.toml`; a one-way promotion
  retires a legacy `config.toml` (as does `nuo config migrate`). The schema is
  strict — `deny_unknown_fields`, fail-fast on parse errors — and the legacy
  `[providers]` / `[websearch]` / `daemon` aliases are gone.
- **"Server" replaces "daemon" in the domain vocabulary (ADR-0033).** Canonical
  types (`ServerConfig`, `ServerAction`, `ServerInfo`, …), runtime artifacts
  (`server.json` / `server.sock` / `server.lock`), user-facing strings, and the
  active docs now say "Server".

### Added

- **`nuo server reload` soft reload (ADR-0034 Level 1).** Re-reads the
  configuration matrix and re-syncs MCP servers + skills with zero connection
  drop, completing the three-tier restart architecture.

## [0.0.9] - 2026-10-07

### Added

- **Domain-separated configuration matrix (ADR-0031).** Split monolithic `config.toml` into domain-isolated server, terminal, client, and agent configuration files; added `nuo config migrate`.
- **Credential relocation to state store (ADR-0032).** Relocated authentication credentials and provider keys into the state store beside `auth.toml`, protecting public dotfiles from secret leaks.
- **Canonical Server entity and dual hosting postures (ADR-0033).** Ratified Server as the canonical container entity; separated client-bound posture (pure UDS, zero TCP listeners) from standalone hosted posture (deterministic TCP port 9800 + UDS).
- **Deterministic lifecycle governance (ADR-0034).** Introduced single-flight startup, phased graceful draining, and a tiered restart hierarchy.
- **Encapsulated dialog lifecycle and surface architecture (ADR-0035).** Dialogs refactored into encapsulated entities with context-bound stacks and deterministic LIFO unwinding.
- **MCP presenter and namespaced tool dispatch.** Integrated `McpPresenter` for Model Context Protocol tools (`⚡ server · tool`) and added support for namespaced tool execution resolution.

### Changed

- Renamed crate `providers/nuo-provider-transport` to `nuo-provider-transport`.
- Renamed crate `providers/nuo-provider-google` to `providers/nuo-provider-google-antigravity`.
- Introduced `nuo server <start|stop|restart|status|token>` CLI verbs.

### Changed

- **Client-lifecycle-bound daemon, explicit foreground headless host, and aggressive interface takeover (ADR-0029).**
  Interactive TUI sessions now couple daemon lifecycle directly to connected clients, automatically terminating when the last client disconnects. Foreground execution is enforced by default for headless and service commands (`nuo start`), and endpoint collisions (Unix Domain Socket or TCP port) trigger automatic termination and takeover of conflicting stale instances.
- **`list_dir` steps render as an `ls`-style table (ADR-0030).** The expanded listing no
  longer leads with a count tally, no longer prefixes each row with a type glyph
  (`▸` / `·`), and no longer draws directories in the block's generic `info`
  tone. It now opens with a **`Name` / `Size` column header** and renders each
  entry as **name + aligned size**, colouring the name the way the shell's `ls`
  does — **blue** for a directory, **green** for an executable, **cyan** for a
  symbolic link, and the scheme's normal content tone for a plain file — with a
  directory carrying a trailing `/`. To make the executable/symlink classes
  observable, `list_dir`'s per-row tag became a four-way `[DIR]` / `[EXEC]` /
  `[LINK]` / `[FILE]` class (it previously emitted only `[DIR]` / `[FILE]` and
  so could not distinguish an executable or a link); the renderer colours the
  tool's observed class rather than guessing a type from the name. The `ls`
  hues collapse to the plain content tone under the monochrome (DEC VT100)
  archetype — no colour cue there, the trailing `/` still carries the type — and
  use the terminal's own named slots under ANSI-16. `find_files` keeps its
  count-band + per-directory title-band layout.

## [0.0.7] - 2026-10-06

### Changed

- **Listing blocks (`find_files` / `list_dir`) are now layered, matching the
  search block.** An expanded listing previously dumped every result line — the
  tool's header included — through the path formatter onto one flat surface:
  `Found N matching files:` rendered as a bogus path row, `list_dir`'s `[DIR]` /
  `[FILE]` rows were unknown to the renderer (so *directories*, which carry no
  trailing `/`, were colored as files), and the byte sizes / omission trailer
  were shown raw. A listing now reuses the three-tier contract the search block
  already had (extracted into shared `draw_band_row` / `draw_title_band`
  primitives, so the two blocks stay identical by construction): a brand-tinted
  **count band** parsed from the tool's own header (`Found N files` for a glob
  search, `path · N items` for a directory listing), a per-directory **title
  band** so `find_files` siblings no longer repeat their shared prefix on every
  row, typed `[DIR]` / `[FILE]` rows as a **glyph + name + aligned dim size
  column**, and a dim **omission band** for `... (N additional entries
  omitted)`. Every selectable row's byte range stays anchored in the raw tool
  output.

- **Reasoning milestone summaries no longer inject a leading `the`.** The
  thinking-trace header (`Thinking through …` / `Thought through …`) used to
  prepend an article to every milestone topic unless it already started with a
  determiner, which produced ungrammatical lines for code-like headings
  (`Thinking through the derive(Clone, copy, debug, PartialEq, eq)`). The
  article is now dropped entirely (`Thinking through derive(Clone, copy, debug,
  PartialEq, eq)`); the summary verb supplies the grammatical frame.

### Fixed

- **The transcript reading position is preserved across a terminal resize
  (scroll anchoring).** Scroll was stored as a raw content-line offset, and a
  content line's meaning depends entirely on the wrap width — so after a column
  change the same offset addressed unrelated text and a manually-scrolled
  viewport jumped, drifting further the larger the width delta. The viewport now
  captures the semantic identity of the content at its top (anchored message id
  + row offset) and, on the pass that follows a width change, resolves it back to
  a content-line offset against the new layout before the frame is committed, so
  the same text stays under the viewport top. A steady width never re-anchors
  (so ordinary scrolling is untouched), `follow_bottom` still re-pins to the
  bottom, and the resolve walks the full transcript so an anchor displaced far by
  a large reflow is still found. See
  [ADR-0028](docs/adr/0028-scroll-anchoring-across-terminal-reflow.md).

- **Single-file `search_text` results no longer lose their filename.** Searching
  a *file* (e.g. `path: "nuo-tui/src/event_loop/actions.rs"`) rooted the walker at
  that file, so stripping the search root from each hit produced an empty path
  and the tool emitted a pathless `:2452: pub(super) fn enter_scene(` line —
  wrong for the model's own tool result and, in the TUI, silently degrading the
  expanded step: the count band tallied `0 files` and the per-file title row
  vanished. `search_text`/`find_files` now fall back to the workspace-relative
  path (then the bare filename) when the search root *is* the file, so the real
  path is preserved. The redundant `· N files` segment is also dropped from the
  match count band when a result spans a single file — `Found N matches` alone
  reads cleaner and no longer risks a bogus `0 files`.

## [0.0.6] - 2026-10-06

### Changed

- **Canonical, strongly-typed built-in tool identity (`BuiltinTool`).** The
  native tools are now enumerated once in `nuo_tool::BuiltinTool` (re-exported
  through `nuo-wire`) and tool identity — `Tool::name`, role and subagent
  whitelists, capability admission, and the TUI's verb/expansion tables — keys
  off the enum instead of raw strings. This removes silent misspellings,
  phantom tools, and registration drift across crates. New surface:
  `Tool::builtin`, `Capability::admits`, `ToolScope::admits_builtin` /
  `admits_tool`, `ToolSelection::only_builtin`,
  `SessionRoleManifest::builtin_tools`, and typed `ToolPolicy::allowed_tools`.
- **Tool admission now honours aliases.** A capability is admitted when either
  its canonical name or any registered variant alias matches the scope, so a
  scope that names a compatibility alias still resolves the canonical tool.

### Removed

- **The legacy `run_command`, `bash`, and `read` tool spellings are retired.**
  `execute_command` is the single canonical name; the shell family's old aliases
  no longer resolve, and the trajectory guard, TUI verb/expansion policy,
  session export, and token-pressure heuristics no longer special-case them.

### Fixed

- **Restored source accidentally truncated by the previous commit.** The tails
  of `nuo-tui/src/model/document.rs` (the compaction card, command settlement,
  and all ADR-0026 tool-step methods) and `nuo-tui/src/lib.rs` (the user-logo
  loader and test module) were lost mid-write; the missing code and the
  ADR-0026 announce/collapse methods are restored so the workspace builds and
  the suite runs.

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
