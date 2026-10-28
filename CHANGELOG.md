# Changelog

All notable changes to this project are documented in this file.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/), and
the project adheres to the federated SemVer model described in
[ADR-0004](docs/adr/0004-federated-cluster-semver-and-release-topology.md).

## Unreleased

### Fixed

- **Composer caret motion is a projection onto the painted row grid (Ctrl-J empty lines are caret-reachable).**
  - **One authority for the composer's rows**: `shell::composer_grid::TextGrid` owns buffer+width → rows,
    caret byte → (row, column), (row, column) → caret byte, and the ↑/↓ motion those compose. The painter,
    the height reservation, the completion anchor, click/keyboard selection and the drag-edge autoscroll all
    read that one grid, so no two consumers can disagree about which row a byte occupies.
  - **Empty rows are caret-addressable**: a row's byte range is now closed (`start..=end`), with a soft-wrap
    seam (`end == next.start`) belonging to the continuation row. A newline's empty line has a zero-width
    range, and the old half-open `start..end` resolution could not map *any* caret byte onto it — the row was
    painted but the arrows skipped straight over it, so a line opened with `Ctrl-J` above or below the caret
    was unreachable.
  - **Layout takes no caret**: rows are a pure function of (text, width). The search for a synthetic
    "caret row" (which made the box grow/shrink with the caret and made an empty trailing line vanish the
    moment the caret left it) is gone; `calculate_composer_height` lost its `byte_cursor` argument and
    `TranscriptProps` lost the field, so layout cannot depend on caret history again.
  - **↑/↓ walk visual rows, not `\n` lines**, and keep a sticky display column (`CaretMotion`, anchored to
    the caret byte it produced so any other edit invalidates it) — a caret passing through a short or empty
    row and back out returns to its original column, and a soft-wrapped line is climbed one painted row at a
    time. Shift+↑/↓ extend the selection through the same motion.
  - **The legacy logical-line walkers are deleted** (`cursor_line_up`/`cursor_line_down`), so nothing can
    reintroduce a second, `\n`-based caret model.
- **Closed command-output fold geometry, tail-heavy window, and a derived disclosure ladder (ADR-0115).**
  - **Folding can no longer grow a step**: the fold control compared `HEAD + TAIL + 1` *lines* against a
    body that paints `HEAD + TAIL + 3` *rows* (the blank/label/blank gap), so an 8-row body "folded" into
    9 rows. `BASH_FOLD_MIN_VERBATIM_ROWS` is now derived from the geometry constants and swept by
    `bash_fold_only_applies_when_it_saves_rows` (`[INV-FOLD-02]`).
  - **The bias is enforced, not tuned**: the retained window is 2 head / 6 tail rows (25/75, mirroring
    `[INV-FOLD-01]`), with `const` assertions that `TAIL > HEAD` and `TAIL >= 2 × HEAD`, so a layout tweak
    cannot start hiding the outcome the fold exists to preserve.
  - **The folded middle is reachable**: activating an expanded command step reveals its hidden rows
    (`collapsed → expanded+folded → expanded+revealed → collapsed`), and the reveal rung exists iff the
    painted body folds — `tool_step_folded_rows`/`bash_disclosure_ladder` ask the same derivation the
    renderer paints with (`[INV-FOLD-04]`). Previously the middle was painted by nothing and selectable
    nowhere, so `N lines omitted` was a dead end.
  - **The fold label carries the diagnosis**: the omitted-row label appends the first salient failure
    signature from the hidden middle (the same scanner the model-facing fold uses), so the human and the
    model are pointed at the same line.
  - **The footer names the short copy**: the single `output truncated · partial kept` claimed a loss over
    a body nothing had cut. Coverage is now tri-partitioned (`Full` / `ModelTruncated` / `CutBeforeUi`),
    so a complete-but-folded log reads `output truncated for model context` and only a payload that
    arrived already cut claims `partial kept` (`[INV-FOLD-03]`).
  - **One derivation, three audiences**: row materialisation, carriage-return resolution, exit-status
    parsing, termination recovery, and coverage moved out of the painter into
    `nuo-view::tools::shell_output`; the renderer, the activation ladder, and the copy path now agree on
    what rows exist by construction, and the three duplicated helpers
    (`parse_unstructured_shell_content`, `command_structured_lines`, `emit_command_lines_folded`) are
    deleted.

## [0.0.53] - 2026-10-27

### Fixed

- **Provider-owned response statefulness (ADR-0114).**
  - **Stateless relay continuation**: `commandcode-plan` accepts `store: true` and returns a response id, but keeps no state. A tool-call turn continued with `previous_response_id` plus only its `function_call_output` therefore reached DeepSeek as a lone `tool` message and failed with `OpenAI Responses HTTP 400 Bad Request: Messages with role 'tool' must be a response to a preceding message with 'tool_calls'`.
  - **Declared surface property**: response statefulness is now `ResponsesState { Stored, Stateless }` on the provider spec (`Stateless` by default; `openai` is the only stored surface), carried by the route and intersected with the dialect's `store` support in the composition root. Anything less than both replays the full local context and never anchors a turn on a remote response id.
  - **User-declared surfaces** (BYOK) default to `Stateless`: an operator declares an endpoint, not its storage guarantees.

## [0.0.52] - 2026-10-27

### Fixed

- **Terminal substrate draw/caret sequencing and nuotc 0.0.5 integration.**
  - **Caret Diff-Draw Precedence**: Updated `nuotc` to `0.0.5`, ensuring diff cell draw commands are emitted before installing the final hardware caret coordinate, eliminating cursor dragging across terminal writes.
  - **Unified Native Byte-Offset Caret Model**: Eradicated legacy char-index conversions (`char_index_at_byte`, `normalize_cursor_char_index`) across `nuo::shell`. Cursor positions consistently operate on UTF-8 byte offsets floored to extended grapheme cluster boundaries.
  - **Multilingual & CJK Visual Caret Navigation**: Fixed multiline `cursor_line_up` and `cursor_line_down` to preserve visual display columns across lines with wide and CJK characters.
  - **Mouse Click & Selection Snapping**: Replaced legacy `chars().count()` in mouse click, drag release, and selection navigation with native grapheme boundary byte offsets.
  - **Overlay Viewports & Caret Placement**: Corrected picker search row cursor clamping and connected command palette and dashboard prompts to horizontal field viewport tracking.

## [0.0.51] - 2026-10-26

### Added

- **Unified dual-tier presentation topology, canonical presenter bridges, and elimination of the Storybook schism (ADR-0112).**
  - **Two-Tier Presentation Topology (`[INV-TUI-TIER-01]`)**: Codified the strict separation of Tier A Stateless Presenters (zero-state functional projections) from Tier B Sovereign TEA Containers (`Component` trait). Eradicated boilerplate taxes on atomic primitives.
  - **Canonical Presenter Bridges (`[INV-TUI-TIER-02]`)**: Eliminated dual-track code drift between live session transcripts and offline Storybook workbenches. Added domain extraction (`from_transcript_message`) and connected layout map rendering (`render_with_layout`) to `ToolCard` and `NoticeCard`.
  - **Dual-Mode Rendering Adaptation (`[INV-TUI-TIER-03]`)**: Guaranteed 1:1 visual parity between sub-millisecond offline Storybook snapshots and production runtime frames with spatial hit testing.
  - **Zero Backward-Compatibility Cruft**: Purged legacy string-vector action wrappers (`with_labels`) in favor of canonical `NoticeAction` and expanded `ToolPayload` to full 6-variant fidelity (`Diff`, `Command`, `Code`, `Listing`, `Matches`, `Generic`).

- **Elimination of `scripts/` directory and consolidation of workspace governance into native Rust (ADR-0111).**
  - **Complete `scripts/` Retirement**: Permanently deleted legacy Python scripts (`check_presentation_firewall.py`,
    `check_doc_links.py`, `check_adr_lineage.py`). Eradicated Python 3 dependency and polyglot build overhead from the repository.
  - **Native Rust Governance Engine (`nuo::dev`)**: Reimplemented presentation firewall (R1–R5), lean CLI link gate
    (`[INV-ARCH-CLI-LEAN-LINK]`), Markdown hyperlink integrity, and ADR lineage in `crates/nuo/src/dev/`.
  - **Audience Firewall Restored (`taxonomy.md` §2)**: The public→internal documentation firewall — public surfaces
    (`docs/{tutorials,how-to,reference,explanation}/`) must never link into `docs/dev/**` — is re-established as a
    native gate in `nuo::dev::doc_links`, closing the parity gap left when `scripts/check_doc_links.py` was retired.
  - **Integration Test Gateways**: Added `crates/nuo/tests/architecture_firewall.rs` and `crates/nuo/tests/doc_governance.rs`,
    unifying repository invariants into standard `cargo test` execution.
  - **Developer CLI Experience**: Added `nuo dev check [all|firewall|docs|adr]` for rapid local verification.

- **Running-state interaction topology, action permissibility discipline, and visual demarcation purity (ADR-0110).**
  - **Action Permissibility Tri-Partition (`[INV-TUI-RUNNING-01..02]`)**: Disentangles Mutate (guarded),
    Inspect (open), and Abort (promoted). Replaces ad-hoc boolean flags (`busy`, `disabled`, `running`)
    with domain-agnostic `ActionStatus` (`Idle`, `Running`, `Disabled`) in `nuo-ui-primitives`.
  - **Dimmed Token Ban (`[INV-TUI-RUNNING-03]`)**: Strictly prohibits applying `theme.dim()` or
    `modal_dim_factor` to in-flight or executing components, reserving dimmed palette tokens exclusively
    for dead, cancelled, or inert entities.
  - **Demarcation Purity (`[INV-TUI-RUNNING-04]`)**: Preserves orthogonal visual channels (Hue for
    lifecycle status, Luminance for disclosure weight, Tint for pointer/keyboard affordances) and
    guarantees readability floor for live stream diffs and logs.

- **Host binary relocation to `crates/nuo` and complete workspace homogenization (ADR-0109).**
  The single application host binary is relocated from the repository root `nuo/` to `crates/nuo/`:
  - **Zero Root Crates (`[INV-ARCH-TIER-01]`)**: The workspace root contains strictly the three
    plural tier containers (`crates/`, `providers/`, `tools/`). Category mismatch at root is
    eliminated.
  - **Pure Wildcard Members**: Root `Cargo.toml` declares `members = ["crates/*", "tools/*", "providers/*"]`
    and `default-members = ["crates/nuo"]`. Special-case host paths are eradicated.
  - **Firewall Gate R5**: `scripts/check_presentation_firewall.py` mechanically enforces the pure
    three-tier layout and single binary census workspace-wide.

- **Provider substrate consolidation & satellite crate retirement (ADR-0108).**
  The artificial separation of outbound transport (`nuo-egress`) and dynamic credential
  negotiation (`nuo-oauth`) is permanently resolved:
  - **`nuo-provider::transport`**: Directly builds on `netune` (owned network stack).
    Unifies singleton socket pooling (`shared_client()`), request construction,
    HTTP 2xx response validation, SSE byte-level stream reassembly, and physical
    network telemetry (`TransportTimings`, `TransportObservation`). The redundant
    dual-client bifurcation (`client::Client` vs `http::Http`) and duplicated
    URL/form encoding logic are eliminated.
  - **`nuo-provider::auth`**: Consolidates all dynamic/negotiated credential lifecycle
    mechanics (OAuth2/PKCE RFC 7636, Device Flow RFC 8628, Loopback server RFC 8252,
    token lease refresh loops, and vendor extensions) directly with `nuo-provider`'s
    `TokenSet`, `CredentialStore`, and `CredentialSource` ([INV-PROV-13]).
  - **Satellite crates retired**: `crates/nuo-egress` and `crates/nuo-oauth` are
    permanently deleted. All 16 provider crates in `providers/*` and `nuo-server`
    now depend exclusively on the single Layer 1 provider authority: `nuo-provider`.

- **Two-tier presentation purification: `nuo-ui-primitives` + `nuo-view`; `nuo-present` retired (ADR-0107).**
  The monolithic presentation substrate `crates/nuo-present` is permanently bifurcated into
  two zero-IO, strictly layered presentation libraries:
  - **`crates/nuo-ui-primitives` (Layer 3a)**: Pure, domain-agnostic UI primitives, design tokens
    (`theme`, `design`, `elevation`), driver-free key chords (`chord`, `key`, `keyvocab`),
    generic terminal components (`modal`, `dropdown`, `footer`, `keycap`, `popover`, `toast`,
    `options`, `list`, `detail`, etc.), layout helpers (`primitives`), and text wrapping (`text_layout`).
    Carries zero AI agent domain logic (`[INV-ARCH-UI-PRIMITIVES-PURITY]`).
  - **`crates/nuo-view` (Layer 3b)**: Domain presentation and AI agent interaction views
    (`model/*`, `tools/*`, `layout/*`, `message_body`, `notice`, `selectable_body`, `trust_gate`,
    `phase`, `composer_mode`, `providers`, `disclosure*`). Single-directionally consumes
    `nuo-ui-primitives` with strictly zero I/O (`[INV-ARCH-VIEW-ZERO-IO]`).
  - **`nuo-present` permanently retired**: Retired from workspace, added to firewall forbidden list.
  - **Firewall Rule R4 added**: Enforces strict purity of `nuo-ui-primitives` against domain leakage.

- **Final presentation topology: `nuo-present` + `nuo::shell`; `nuo-ui` retired (ADR-0106).**
  The ADR-0103/ADR-0105 extraction program is complete. The mutually-recursive
  presentation cluster (`model`, `text_layout`, `markdown_table`, `notice`,
  `message_body`, `tools`, `components`, `layout`, `disclosure*`) now lives wholly in
  the hard-gated zero-IO substrate `crates/nuo-present`, and the interactive runtime is
  an internal module of the host binary (`nuo::shell`, behind the default-on
  `interactive` feature). The transitional `nuo-ui` crate — a name that lied about its
  purity contract — is deleted, along with the intermediate `nuo-shell` crate. The
  firewall now enforces R3 (`[INV-ARCH-UI-ZERO-IO]`) on `nuo-present` unconditionally and
  fails if a retired presentation crate reappears.

### Changed

- **Workspace topology is now a gated invariant, not a description (ADR-0055 `[INV-TOPO-01..05]`).**
  `scripts/check_presentation_firewall.py` gained **R5**, which checks the physical layout
  itself: exactly one application crate at the root (`nuo/`) holding the workspace's only
  `[[bin]]`; every other crate in exactly one flat tier with that tier's namespace prefix
  (`crates/nuo-*`, `providers/nuo-provider-*`, `tools/nuo-tool-*`); no library crate at the
  root; no nesting past `<tier>/<crate>/`; and a literal check that the root manifest's
  `members` is the sanctioned wildcard set. Without it any crate dropped outside a tier was
  invisible to R1–R4, because those rules enumerate the workspace by glob.
  The root `nuo/` placement is therefore **kept deliberately** — it is `[INV-TOPO-01]`, the
  mechanism that makes "`nuo` is the single public binary" ([INV-CLI-01]) legible in the tree.
- **`[INV-ARCH-FLAT-01]` retired; its two stale copies deleted.** ADR-0055 superseded the
  flat-topology rule in 2026-10, but the rule survived in `docs/dev/subsystems.md` and
  `docs/dev/release-and-versioning.md` §5.2 — where it was still cited as a *rejection
  rationale* while the tree it forbade (`crates/`, `providers/`, `tools/`) was in production.
  Both copies are replaced by the ADR-0055 topology, with the retirement recorded inline. Its
  original rationale (avoiding `../../` path indirection) no longer has a failure mode: every
  inter-crate path is declared once in the root `[workspace.dependencies]`, so no member
  manifest contains a relative path at all.
- **Living docs re-aligned with the shipped presentation split (ADR-0107).**
  `docs/dev/subsystems.md` and `docs/dev/standards/architecture.md` still described the retired
  `nuo-present` as "the pure presentation substrate"; they now describe `nuo-ui-primitives`
  (domain-agnostic UI) and `nuo-view` (agent views) separately, and the Layer-3/Layer-4 diagrams
  carry real tier paths.

- **`nuotc` v0.0.4 (clean-break) adopted from its pinned GitHub tag.** The terminal canvas
  engine's compatibility surface is gone upstream (`docs/adr/0008-no-compatibility-shims`), so
  every presentation call site was re-pointed at the real paths rather than shimmed:
  - `Grid` is two types — `CellGrid` (content) and `Damage` (per-row dirty state) — so every
    render entry point constructs `Frame::new(&mut grid, &mut damage)` and reads cells through
    `get`/`get_or_empty`/`cells()` instead of the removed public `content` field.
  - `Widget` is object safe and takes `&mut self` through a clip-carrying `Paint`; every
    `render_widget` call passes `&mut`, `Clear` is a styled struct (`Clear::default()`,
    `Clear::styled`), and `StyledClear` is gone.
  - `Cell::width` is the four-state `CellWidth` enum, so wide-glyph assertions compare
    `CellWidth::Wide`/`Continuation` instead of `2`/`0`.
  - Style and symbol live only on the cell (`cell.style.fg`/`cell.style.bg`/`cell.symbol`);
    the mirrored `fg`/`bg` fields, the `Cell::symbol()`/`style()` accessors and
    `Modifier::UNDERLINED` (now `UNDERLINE`) are gone.
  - The module aliases died with the shims: `nuotc::text` → `nuotc` root re-exports
    (`grapheme_width`, `str_len`, `floor_grapheme_boundary`, `prohibited_line_*`),
    `nuotc::anchor`/`nuotc::flex` → the `nuotc` root, `nuotc::Backend::with_bce` →
    `Backend::new(..).with_bce(..)`.
  - **`ElevationArchetype` is now owned by the presentation substrate**
    (`nuo_ui_primitives::elevation`), because upstream deleted the type along with
    `SpatialCost`: the archetype is a visual-design decision, not a canvas primitive. It keeps
    `for_profile` plus the new `inner_bounds`/`is_structured`/`is_chromatic`/`is_hybrid`
    predicates, and the `Structured` border glyphs are derived from the theme's `GlyphSet`
    (`border_type_for_glyphs`) instead of a canvas-side border-type hook.
  - **`nuo::shell` paint honours the scene clip explicitly.** `nuotc::Paint` exposes cell
    writes but not the `Widget` contract, while the shell's renderers are widget-based. The
    facade therefore renders through a scratch grid the size of the frame and blits only the
    node's clip region, skipping the copy entirely when the clip covers the whole frame. The
    one read-modify-write painter (the modal backdrop recess, which dims cells already on the
    grid) paints straight into the frame by construction.

- **`nuo` gates the whole interactive stack behind `interactive`.** `nuo::shell` is
  `cfg`-gated, and `crossterm`/`nuotc`/`arboard`/`bitflags`/`similar`/`unicode-*` are
  optional dependencies pulled only by that feature, so `cargo build -p nuo
  --no-default-features` links no raw-mode driver (`[INV-ARCH-CLI-LEAN-LINK]`). The
  storybook example declares `required-features = ["interactive"]`.

### Fixed

- **Storybook snapshot legacy naming (ADR-0103).** The 29 insta snapshots kept the
  pre-rename `nuo_tui__` filename prefix after the presentation crate was renamed to
  `nuo-ui`, so insta could not match them and 28 tests failed. The snapshots are renamed
  to the `nuo_ui__` prefix; the `nuo-ui` suite is green again (1506 passed).

- **Causal session forking completeness & `'aside'` legacy purge (ADR-0058, ADR-0059).**
  - **Turn-granular forks are exact.** `/fork <round>.<turn>` previously clamped an
    out-of-range turn to the round boundary *silently*; it now fails with a precise
    error naming the round's real turn count, and intra-round slicing gained a unit
    test asserting both the slice and `[INV-FORK-04]` (parent untouched).
  - **Retired `/btw` steers into a fork instead of dead-ending (`[INV-DEPR-01]`).**
    A submitted `/btw <prompt>` (with its `/aside` / `/side` synonyms), which used to
    return `Unknown command`, is intercepted: the server forks at head and carries
    the message into the new branch through a new `AgentResponse::SessionForked.prompt`
    field, dispatched by the client once it re-attaches. Completion now steers
    `btw`/`aside`/`side` onto `/fork`.
  - **Fork mounts its tab through the checkpointing path.** The applier uses
    `App::open_tab`, so the departing parent tab's context is preserved exactly as
    every other session switch preserves it.
  - **Legacy `'aside'` residue removed.** The dead `"aside"` fork-kind match arms
    (persistence), the `TimelineKind::Aside` variant, the aside-era
    `routes_to_side` translation scaffold with its dead `ChromeEdit::TransportSetback`
    mutation, the vestigial single-variant `Buffer` enum, and the dead
    `App::view_transitioned` / `App::side_transcript_changed_pending` fields are all
    deleted, and the stale `aside`/`btw` prose across server and presentation is
    corrected (a broken rustdoc intra-doc link included).
  - **The in-place `SessionStore::fork()` is retired.** Its repoint-the-store
    behavior is precisely the "in-place timeline switch" ADR-0058 rejected; the
    sovereign `fork_at(...)` is now the sole fork primitive (ADR-0104).

- **Governance & documentation coherence (ADR-0111, ADR-0084, ADR-0108).** The
  `scripts/` → `nuo::dev` port had silently dropped the public→internal *audience
  firewall*; `nuo::dev::doc_links` now re-enforces it (public surfaces must never link
  into `docs/dev/**`), matching ADR-0111 and `taxonomy.md` §2. Dead `scripts/*.py`
  references and stale crate names were purged from living docs and comments: the
  retired `nuo-model-codec` (now `nuo-inference-wire`; its `effort`/`client_identity`/
  `model` modules live in `nuo-provider`), `nuo-oauth` (now `nuo-provider::auth`), and
  the phantom `nuo-llm-client`.

### Changed

- **Presentation purity, first module (ADR-0103 §1 / ADR-0105 M3).** `theme.rs` is now a
  pure projection: its workspace-aware IO family (`available_color_schemes_with_workspace`,
  `normalize_color_scheme_with_workspace`, `color_scheme_index_with_workspace`,
  `color_scheme_label_with_workspace`, `from_color_scheme_with_workspace`, and
  `resolve_with_profile(.., workspace, ..)`) is replaced by file-aware pure functions
  (`*_from(files: &[ThemeFile])`) over caller-supplied `nuo_wire::ThemeFile`s, and the
  filesystem loaders move to `config.rs` (`available_color_schemes`,
  `normalize_color_scheme`, `color_scheme_index`, `color_scheme_label`,
  `theme_from_color_scheme`, `resolve_theme_with_profile`). The theme module no longer
  links filesystem IO — the reference pattern the remaining presentation modules follow
  so `crates/nuo-ui` can shed its IO dependencies ([INV-ARCH-UI-ZERO-IO]).

- **Presentation purity, disclosure seam (ADR-0103 §1 / ADR-0105 M3).** A new pure
  `disclosure_config` port (`DisclosureSource` + `tool_expanded` / `reasoning_expanded`)
  replaces the `transcript.rs` / `step_interaction.rs` dependency on the shell's
  `TuiConfig`: those modules now resolve default-expand state through the port, and the
  shell implements it for `TuiConfig` (dependency inversion). No `TuiConfig` reference
  remains in their production paths.

- **Presentation purity, pure key for declarative components (ADR-0103 §1 / ADR-0105 M3).**
  The declarative component tree (`ask_user`, `notice_card`, `tool_card`, and the
  `StorybookComponent` protocol) no longer depends on the terminal driver: it consumes a
  pure `declarative::key::Key`, and the storybook runner translates
  `crossterm::event::KeyEvent` into `Key` at the boundary. The only remaining terminal
  dependency in `declarative/` is the interactive workbench runner itself.

- **Presentation purity, components off the render re-export (ADR-0103 §1 / ADR-0105 M3).**
  The `components/` modules (`detail`, `dropdown`, `meta_strip`, `popover`, `row`,
  `tooltip`) import `Theme` from the pure `crate::theme` module rather than the shell
  `crate::render` re-export, removing the last `render` coupling from the component
  library. Only `components/composer_hints.rs` (composer-panel shell chrome) still
  reaches into shell state; it is slated to move with the shell in M4.

- **Pure substrate extraction — `crates/nuo-present` (ADR-0103 §1 / ADR-0105 M3).**
  The leak-free pure modules `syntax`, `time`, `phase`, and `fuzzy` move out of the
  shell crate into a new zero-IO crate, `nuo-present`, which `nuo-ui` consumes. The
  presentation firewall hard-gates `nuo-present` under R3
  (`[INV-ARCH-UI-ZERO-IO]`): it may never declare a network, process, terminal,
  clipboard, or filesystem-layout dependency. This is the first physical step of the
  ADR-0103 split: a real pure substrate that the shell depends on, rather than a
  monolith that pretends to be pure.

- **Pure substrate extraction, tranche 2 (ADR-0103 §1 / ADR-0105 M3).** Six more
  leak-free modules move into `crates/nuo-present`: `design` (spacing/geometry tokens),
  `footer_stack`, `trust_gate`, `composer_mode`, `composer_attachments`, and `providers`
  (model-search primitives). `nuo-ui` re-exports each with unchanged visibility, so no
  consumer changed. The firewall now judges **shipped** dependencies only
  (`[dev-dependencies]` excluded — a test-time composition root is not part of the linked
  surface), which dropped `nuo-ui`'s gated debt from 7 to 6 IO deps.

- **Presentation purity, theme/elevation (ADR-0103 §1 / ADR-0105 M3).** `elevation` imports
  `Theme` from the pure `crate::theme` module rather than the shell `crate::render`
  re-export, and the external-theme test in `theme` is now hermetic: it parses a
  `ThemeFile` from TOML instead of writing and reading files through the shell's config
  loader (ADR-0073). The further extraction of `theme`/`primitives` into `nuo-present` is
  deferred: `primitives` is a re-export facade over `components` and `keymap`, and `theme`
  depends on `primitives`, so neither can move until `components` (itself blocked on
  `model`) moves first.

- **Model layer is terminal-driver-free (ADR-0103 §1 / ADR-0105 M3).** Key dispatch for
  focused transcript entries moves out of the pure model into the shell
  (`input::interactive_key::handle_focused_entry_key`); `InteractiveEntry` keeps only its
  content contract (`is_focusable`, `target_kind`, `extract_copy_text`, expansion state).
  `crates/nuo-ui/src/model/` therefore has **no `crossterm` dependency** any more — the
  last terminal-driver coupling in the model layer is gone.

- **`components/` is now a pure, driver-free component library (ADR-0072 / ADR-0103 §1).**
  The two remaining coupled files were resolved: `dropdown` consumes a pure
  `KeyStroke` (semantic key + Shift) instead of a `crossterm::KeyEvent`, and
  `composer_hints` — composer-panel chrome that reads shell state — moved out of
  `components/` to `crate::composer_hints`. The pure `Key`/`KeyStroke` types live in
  `nuo-present`, and the driver is folded into them at a single shell boundary
  (`input::keystroke`). `components/` now has **zero** `crossterm` and **zero** shell-state
  references.

- **Component keycaps take display strings, not key types (ADR-0072).** `KeyAffordance`
  and `FooterHint` no longer accept a `keymap::Key` and call `display()` internally; they
  take the already-rendered `&'static str` token, and the ~68 call sites pass
  `Key::X.display()` explicitly. `components/` therefore no longer depends on the
  crossterm-backed key type: its only remaining `keymap` reference is `which_key`'s
  `scene_namespace::SceneVerb`, which must stay coupled until `keymap::Key` is unified
  (deriving its chord display elsewhere would duplicate the advertised⇔dispatchable
  chord table).

- **The key model is unified on the pure substrate (ADR-0103 §1 / ADR-0105 M3).** The
  semantic key layer now lives in `nuo-present::chord`: pure `KeyCode`, `KeyModifiers`
  (consts, `contains`/`intersects`/`union`/`remove`/`insert`, `BitOr`/`BitOrAssign`) and
  the `Key { code, modifiers }` binding type with its 49 canonical constants, `chord()`,
  `display()` and the token tables. `keymap` re-exports it, so every `Key::CTRL_X` and
  `KeyCode::Esc` call site is unchanged, and the terminal driver survives in exactly one
  place — `keymap::key_from_event` — which folds a `crossterm` event into the pure shape.
  Tests that must drive the real event pipeline invert it through
  `input::keystroke::event_from_chord`.

- **`render`/`config` edges lifted out of the substrate cluster (ADR-0103 §1).** Two
  shell references were removed so the remaining substrate no longer reaches up:
  `tools`' disclosure tests now assert through the pure `disclosure_config` port instead
  of the shell's `TuiConfig`, and the `BlockWrapCache` transparency test moved from
  `model` to `render/tests` (where the cache lives). `model` now names no `render` path
  and `tools` names no `config` path.

- **The `theme` ⇄ `primitives` ⇄ `elevation` cycle is extracted (ADR-0103 §1 / ADR-0105 M3).**
  All three move into `crates/nuo-present`. `primitives` was a re-export facade over
  `components::{footer, inline_layout, path}`; those three re-exports now live in a thin
  shell-side `crate::primitives` shim (which also glob-re-exports the pure module), so the
  substrate no longer names `components` and every `crate::primitives::…` path still
  resolves. The `modal_footer_degrades_by_width_and_priority` test moved with its subject
  to `components/footer.rs`. `nuo-present` now owns the theme system, the geometry/modal
  primitives, and the elevation scale.

- **`empty_state` extracted (ADR-0103 §1 / ADR-0105 M3).** The empty-state carousel is
  now in `nuo-present`; its one shell reference — `keymap::Key::CTRL_R` — became the pure
  `chord::Key::CTRL_R`, since the chord model is already substrate.

- **`keyvocab` moves to the pure substrate (ADR-0103 §1 / ADR-0105 M3).** The chord glyph
  tokens (`"↑↓"`, `"Space"`, `"⇧Tab"`, …) now live in `nuo-present::keyvocab`; `keymap`
  re-exports them and `primitives` imports from the substrate directly, so `primitives` no
  longer names `keymap`.

- **Lean CLI linkage (`[INV-ARCH-CLI-LEAN-LINK]`, ADR-0105).** The interactive shell and
  pure UI substrate are now an optional, default-on `interactive` feature of the `nuo`
  binary: `Mode::Interactive` carries the raw interactive arguments (parsed at dispatch),
  and the interactive/dev entry points are `cfg`-gated. A
  `nuo --no-default-features` build links no terminal presentation stack
  (`nuotc`/`crossterm`/`nuo-ui`), verified in CI. The physical relocation of the shell
  into `nuo::shell` remains the rest of M4.

### Removed

- **Cognitive runtime consolidation — the orphaned `nuo-agent` twin is retired (ADR-0100).**
  `nuo-agent` had zero production callers: the shipped daemon runs
  `nuo-harness::Agent`, and its only inbound edge was a dead bridge
  (`agent/cognitive_bridge.rs` / `Agent::to_cognitive_agent`) whose entry point
  nothing called. Its ACP collaboration capability already lives in `nuo-acp` and
  is wired in production via `nuo/src/registry.rs::assemble_hosted` (ADR-0009 §3),
  so no capability is lost. The crate, the bridge, the now-unused
  `HarnessApprovalHandler`, and the `nuo-harness → nuo-agent` dependency edge are
  all deleted. `nuo-harness` is reaffirmed as the single cognitive runtime
  ([INV-RUNTIME-01]).

### Changed

- **Model routing aperture — `nuo-catalog` renamed to `nuo-model-routing` (ADR-0102).**
  The Layer-2 pipeline crate `nuo-catalog` is renamed to `nuo-model-routing`, cleansing the
  misleading "static dictionary" framing and eliminating namespace collision with the leaf
  ingress discovery crate `providers/nuo-provider-catalog`. The crate's role as the dynamic
  route materialization engine and the cognitive firewall protecting `nuo-harness` from
  connection and credential mechanics is explicitly codified in `[INV-ROUTING-01]` (superseding
  `[INV-CATALOG-07]`). Public interfaces are cleansed of legacy catalog naming: `build_routes()`
  supersedes `build_catalog()`, `RoutingSyncOutcome` supersedes `CatalogSyncOutcome`, and
  `sync_remote_routes` / `sync_connection_routes` supersede their catalog counterparts.
  Consumer crates import directly with zero compatibility shims.

- **`nuo-harness` internal structure reorganized into subsystem groups, with enforced inner layers (ADR-0101).**
  The crate's ~30 flat sibling modules are grouped into five navigational subsystems —
  `cognition/` (the agent's mind), `governance/` (its restraints), `orchestration/`
  (per-round policy), `dispatch/` (tool pipeline), and `runtime/` (substrate) — plus
  `tools/`. This is a pure, behavior-preserving move (`orchestration.rs` becomes
  `orchestration/mod.rs` so its path is preserved). The former `cognition ↔ orchestration`
  and `runtime ↔ cognition` cycles were broken by sinking the shared leaf helpers
  (`runtime::time`, `runtime::retry`, `runtime::message`) and moving the `pipeline`/`aspects`
  engines into `cognition`. Three boundaries are now enforced by the source-scan test
  `tests/layering.rs`: `governance` and `runtime` are strict inner layers, and `cognition`
  is wrapped by `orchestration`/`dispatch` ([INV-HARNESS-03, INV-HARNESS-04]).

- **The governance layer is now a strict inner module of `nuo-harness` (ADR-0100).**
  `ScopedToolDisable` is relocated out of the agent module into
  `nuo-harness::governance`, and retiring the bridge removed the last
  cognition→governance reference, so `governance` now depends on nothing in
  cognition. The boundary is enforced mechanically by the
  `governance_module_has_no_cognition_dependency` regression test
  ([INV-HARNESS-03]). A physical `nuo-cognition`/`nuo-governance` crate split was
  evaluated and rejected: the module graph is cyclic in both directions, so a
  crate cut would require a synthetic port layer (see ADR-0100 §Rejected
  Alternatives).

- **CodeBuddy Bifurcation into Per-Realm Channel Crates (ADR-0097 hardening).**
  The single `nuo-provider-codebuddy` crate — one crate hosting both the
  domestic and international `ModelProviderSpec`s, one shared client-identity
  constant — is split along the physical ingress boundary:
  - **`nuo-provider-codebuddy-auth`** (new): the realm-agnostic auth kernel —
    the vendor web-handoff OAuth engine (`CodeBuddyRealmSurface`, origin
    supplied by the channel), the wrapped credential envelope codecs, the CLI
    `Tencent-Cloud.coding-copilot.info` parser, and the typed
    `CodeBuddyCredentialKind` → `WirePlacement` projection. Registers no spec,
    no endpoint, no registry entry.
  - **`nuo-provider-codebuddy-cn`** (new): the domestic channel — provider id
    renamed `codebuddy` → **`codebuddy-cn`** per the CN-suffix naming rule
    (`glm-cn` precedent), origin `copilot.tencent.com`, the internal-line model
    catalog (Hunyuan 4/3, DeepSeek V4, Kimi K3.1, GLM 5.3 Flash, MiniMax M2.5),
    and its own `X-Domain: copilot.tencent.com`.
  - **`nuo-provider-codebuddy-intl`** (new): the international channel — id
    `codebuddy-intl`, origin `www.codebuddy.ai`, its own
    `X-Domain: www.codebuddy.ai`, and a disjoint model baseline.
  - **Cross-realm leaks closed** ([INV-PROV-REALM-01]): the `X-Domain` header
    left the shared `CODEBUDDY_CLIENT_HEADERS` constant (the international spec
    previously attached the *domestic* host header to every request the
    moment it shared the profile preset); each channel now composes its own.
    The BYOK credential lane enforces a per-channel realm gate: a CLI-stored
    token minted by the other account line is refused outright, keyed on the
    vendor-declared `auth.domain`.
  - **Disjoint model baselines** ([INV-PROV-CHANNEL-01]): the two specs no
    longer share a `MODELS` const; the international baseline no longer
    advertises a domestic-only `hy4-preview`.
  - **`ProviderFactory::build_credential_source` gains the connection's
    provider id**: both realms share the `CodeBuddy` dialect, so realm
    selection required the provider identity, not the dialect alone.
  - Zero backward compatibility: the legacy `codebuddy` provider id is retired
    outright (0.0.49 discipline, `zai-code` precedent) — stale
    `connections.toml` rows fail loudly rather than silently canonicalizing.

- **Documentation citations are a hard CI gate, and the drift it would have
  caught is fixed.** The workspace now denies `rustdoc::broken_intra_doc_links`,
  `rustdoc::private_intra_doc_links`, and `rustdoc::invalid_html_tags`
  (`Cargo.toml` `[workspace.lints.rustdoc]`), enforced by a new `docs` CI step
  (`cargo doc --workspace --no-deps`). Enabling the gate surfaced ~40 dangling
  citations left behind by the recent relocations (ADR-0027, ADR-0085, ADR-0102,
  ADR-0103) — links to names that had moved, been renamed, or never existed
  (`nuo_provider::auth::DeviceFlowMode`, `crate::capability::Tool`,
  `super::super::wire::codec`, `crate::RemoteCatalogCache`,
  `crate::MainAgentProfile`, `CosyIdentity`'s retired `nuo-llm-client`, …) plus
  public docs reaching into private items. Each is now either pointed at its
  current home or de-linked to a plain code span.
- **The orphaned `nuo_provider::oauth` re-export module is removed.** It was a
  legacy alias (`pub use crate::credentials::*`) with zero production
  consumers — reachable only from a test — that misrepresented OAuth as living
  in `nuo-provider`. OAuth lives solely in `nuo-oauth` (`[INV-PROV-07]` spirit:
  a contract-shaped surface with no production consumer is deleted, not kept).
- **ADR-0027's mangled `Related:` citation lists are cleaned.** Both copies
  carried a run of empty backtick citations (``, ``, ``) left by the repo-wide
  legacy-citation retirement; the surviving `ADR-0065` reference is retained.
- **The documentation-integrity gate is codified in the standards, not only in
  the config.** `docs/dev/standards/workflow.md` gains it as PR review gate 5
  and pre-release check 7 (the registry that the CI steps mirror), and
  `docs/dev/standards/adr.md` §4 stops pinning a hardcoded canonical ADR range —
  the lineage checker derives the set from `docs/adr/`, so the standard can no
  longer drift behind the registry it describes.
- **Markdown link integrity is now mechanized, not merely declared.**
  `docs/dev/standards/style.md` §1.3 has always said "dead links or broken
  anchors block publication" and `taxonomy.md` §2 has always forbidden public
  docs from reaching into `docs/dev/` — neither had a mechanism. The new
  `scripts/check_doc_links.py` (stdlib only, CI `doc links` step) checks all 798
  relative targets across `docs/**` and the root `*.md` for existence, anchor
  match, absolute/`file://` locators, and the audience firewall; fenced blocks
  and inline code are excluded so example link syntax is never mistaken for a
  citation. It found one real defect, now fixed: ADR-0014 cited
  `crates/nuo-model-codec/src/connection_auth.rs`, a path retired with the crate.

### Removed

- **The finished one-shot migration helper `scripts/retire_legacy_adr_refs.py` is
  deleted.** It existed to retire citations drawn from the pre-ingestion ADR log
  (the `0085`..`0304` range, never imported into this repository); that pass is
  complete — a dry run today reports only
  the fourteen frozen `migrations.rs` SQL comments, which are exempt by design
  and permanent. Keeping it bought nothing git history does not already record,
  and its own docstring had already gone stale (it still asserted the canonical
  series was `0001`..`0083`). `scripts/check_adr_lineage.py` remains the live
  gate, and `docs/dev/standards/adr.md` §4 no longer advertises the helper.
- The monolithic `nuo-provider-codebuddy` crate is deleted; its workspace
  entry, all imports (server registry, connection auth, TUI presets), and the
  shared `CODEBUDDY_BUILTIN_MODELS` seed list are superseded by the three-crate
  topology above.

## [0.0.50] - 2026-10-24

### Fixed

- **`connections.toml` schema round-trip.** `Connections::load` parsed a separate
  legacy struct while `save` wrote the current `Connection` shape; the two drifted
  on `entitlement` (ADR-0095), so a freshly written file failed to parse and every
  connection was silently dropped — the server then reported "No provider
  configured" despite a valid connection on disk. `load` and `save` now share one
  schema.
- **User-declared provider registry split-brain.** The server kept a second
  registry that was never seeded at boot, so after a restart `AddConnection` /
  `EditConnection` rejected providers the catalog still resolved. There is now one
  registry, replaced on each sync and seeded at boot.
- **`ProviderDialect` parsing.** `FromStr` had drifted from the serde
  representation (`codebuddy` deserialized but did not parse); it now delegates to
  serde.
- **Malformed `model_providers.toml` no longer wipes connections.** It is read
  leniently at load.

### Removed

- The unreachable `QuotaPort::SiliconFlow` path and the `nuo-provider-siliconflow`
  crate.
- The unreachable `codebuddy-intl` OAuth surface and the never-wired CodeBuddy
  origin enricher.
- Unused ADR-0095 vocabulary (`RoutingPolicy`, `SpendGuardrail`,
  `CredentialCapability`, `AuthProtocol`, `CredentialBinding`, `HeaderPlacement`,
  `AuthKind`, `AuthCapability*`) and `Config.routing`.
- The unused `QuotaTracker` trait and `ModelProviderSpec::credential_capabilities`.
- The legacy connection-decode migration layer; no backward compatibility is
  retained for pre-`0.0.49` connection files.

## [0.0.49] - 2026-10-23

### Added

- **SceneBar Breadcrumb Topology, Bidirectional Navigation, and Symmetric Keybindings (ADR-0092).**
  - Canonical Single-Guillemet Typography: Tier 2 SceneBar renders hierarchy using single guillemets (`›` / `\u{203A}`) and back cues (`‹` / `\u{2039}`), abolishing `< thread >` pseudo-tag conflation.
  - Bidirectional Navigation: Added symmetric `Alt+[` (back) and `Alt+]` (forward) navigation chords across drill-in scene depths, retiring modal-sensitive bare `[` / `]` chords and ad-hoc single-view `q` escapes.
  - Interactive Breadcrumb Hit-Testing: Added mouse click hit-testing for both the leading back affordance and individual ancestor breadcrumb segments in `ComponentTree`.
- **Tencent CodeBuddy Subscription Lane with Browser Web-Handoff OAuth (ADR-0094).**
  - First-Class Subscription Flow: Implemented browser web-handoff OAuth (`auth/state` → browser open → `auth/token` polling) with automatic token refresh (`auth/token/refresh`) for Tencent AI CodeBuddy.
  - Dual-Line Origin Pinning: Scoped credentials and endpoints cleanly across domestic (`copilot.tencent.com`) and international (`www.codebuddy.ai`) account lines.
  - Credential-Kind Header Split: Typed discrimination between Bearer subscription JWTs (`Authorization: Bearer <token>`) and BYOK keys (`X-API-Key: <token>`).
- **Canonical Dual-Lane Model Routing and Orthogonal Entitlement Governance (ADR-0095).**
  - Symmetrical Credential Lifecycle: Replaced asymmetric `Auth { ApiKey, OAuth }` with `CredentialCapability` (Static vs Negotiated).
  - Orthogonal Entitlement: Decoupled authentication protocols from commercial entitlements (`FlatRate` subscription vs `Metered` pay-per-token).
  - Priority Failover & Financial Guardrails: Added `PriorityFailover` routing policy with configurable `SpendGuardrail` (interactive confirm vs session dollar caps) to survive flat-rate rate limits seamlessly.

### Changed

- **Abolition of Pseudo-VFS and Adoption of Ephemeral Session Spool Architecture (ADR-0093).**
  - Eradicated in-band prompt-injection guidance traps (`[Guidance: ...]`) and pseudo-VFS virtual path projections.
  - Replaced with RAM-backed ephemeral session execution spools (`~/.local/state/nuo/spool/`) and salience-aware tail-heavy semantic folding with automatic error signature preservation.

## [0.0.48] - 2026-10-23

### Added

- **Tencent AI CodeBuddy Provider Channel (`nuo-provider-codebuddy`).**
  New provider channel for Tencent AI CodeBuddy (`copilot.tencent.com/v2`),
  authenticated via Bearer token or CodeBuddy CLI credentials. Supports Hunyuan 4
  Thinking (`hy4-preview`), Hunyuan 3 (`hy3`), DeepSeek V4 Flash / Pro, Kimi K3.1,
  GLM 5.3 Flash, and MiniMax M2.5 models with streaming thinking deltas
  (`reasoning_content`) and multimodal vision. Authoritative OpenAPI specification
  added to `docs/reference/openapi/` (realm-split as `codebuddy-{cn,intl,common}.yml` under ADR-0097; originally `codebuddy.yml`).

### Changed

- **The CN Plan Channel Presents Itself Under Its Official Product Name:
  `GLM Coding Plan CN`, and Legacy ZAI Artifacts Are Retired.** The `glm-cn`
  provider's label was `ZAI Code (CN)`, which was wrong in three independent ways:
  it named the Zhipu **international** brand (`Z.AI`) after an endpoint that lives on
  `open.bigmodel.cn`; it invented `Code` when the product Zhipu publishes — on
  *both* `bigmodel.cn/glm-coding` and `z.ai/subscribe` — is the **GLM Coding
  Plan**; and it spent parentheses on a region, while
  `docs/reference/tui/visual-language.md` reserves `(...)` for a measure
  qualifying a value (e.g. `4.2k (7%)`) and `ConnectionTemplate::display_title`
  states that any suffix is part of the label.
  - **Label & Description**: The label is now `GLM Coding Plan CN`: the official product name,
    with `CN` kept as a plain site-disambiguating token. The chooser description and module
    documentation reflect the domestic BigModel endpoint serving the flagship GLM-5 family.
  - **Channel Crate & Constant Purified**: Renamed `providers/nuo-provider-zai` to
    `providers/nuo-provider-glm-cn` (`nuo-provider-glm-cn`). Renamed `ZAI_CODE_MODELS` to
    `GLM_CN_MODELS` across `nuo-provider`, `nuo-tui`, and `nuo-server`.
  - **Zero Backward Compatibility / Zero Historical Baggage**: The legacy `zai-code` alias in
    `canonical_provider_id` was retired outright; unrecognized provider IDs fail loudly rather
    than silently canonicalizing. Leftover `zai-code` strings in `nuo-server` provider registry
    tests and `nuo-provider` dialects documentation were updated to `glm-cn`.
  - `PROVIDER_PRESETS` stays sorted by title, so the entry moved ahead of `GitHub Copilot`
    (byte-wise `GLM` < `GitH`); at 18 columns the title clears the narrowest chooser budget
    (53 columns at an 80-column viewport).
- **A Stale Key-Variable Claim Left the ZAI Spec.** The channel's module
  documentation asserted that the plan key resolves through a `ZHIPU_API_KEY`
  legacy name with `ZAI_API_KEY` as the preferred alias. No such built-in
  variable exists: `api_key_env` is a free-form per-connection field
  (`ConnectionSpec`), and grep finds no declared Zhipu/GLM environment name
  anywhere in the tree. The doc now says what the code does.
- **Sovereign Tab Context Isolation and TabBar Boundary Purity ([ADR-0091](docs/adr/0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md), `[INV-TAB-11]`, `[INV-TAB-12]`).**
  - **TabBar Container Purity (`[INV-TAB-11]`)**: Tier 1 TabBar chrome (`UiKey::TabBar` and `UiKey::TabBarItem`) operates strictly as a container affordance. Background clicks and already-active tab clicks are inert, idempotent container gestures: they cancel drags and release pointer capture, but never leak cross-layer side effects (such as blurring the composer, mutating transcript focus, or clearing selection) into the active scene.
  - **Sovereign Tab Context Preservation (`[INV-TAB-12]`)**: `ClientTab` now maintains autonomous `TabContextState` tracking focus mode (`transcript_focused`), focused interactive step targets, reading scroll offset, uncommitted composer input drafts, caret positions, and input viewport scroll. Lateral workspace transitions checkpoint departing tab context and restore arriving tab context without operating habit loss.
  - Replaced legacy "click tabbar background to blur composer" defect with strict container boundary isolation. Single-click switching on inactive tabs remains instantaneous, and clicking the active tab or empty TabBar area preserves the active context.

- **The ThreadContextBar Speaks Tokens, and the Workspace Moved Right**
  ([ADR-0062](docs/adr/0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)
  §2a, `[INV-HEAD-07]`). Tier 3 of the head band no longer spends 22 columns on
  the sentence-flags `UNATTENDED` / `UNCONFINED`. Posture is now a closed
  vocabulary of lowercase, unbracketed tokens — `auto` (tool calls are approved
  without asking) and `open` (file tools may leave the workspace) — joined by
  the peer-enumeration gap and drawn **only on deviation from the safe
  default**, so a normally-behaving session shows nothing but `[ROLE]` and the
  path. The workspace path moves to the row's right edge: it is the only
  element permitted to shrink, so anchoring it there keeps the governance
  cluster (`[ROLE]` + posture) at a fixed screen position — toggling
  `/unattended` now grows the cluster into the inter-cluster gap instead of
  re-truncating an unrelated element.
- **The Workspace Path Shortens From the Ancestor End, or Not at All.** It now
  goes through the shared `PathView` ladder (`Full` → `.../parent/leaf` → fish
  → basename → middle-truncate) instead of a tail truncation, so a narrow row
  keeps the leaf that names the project (`.../crates/nuo-tui`) rather than the
  ancestors the user already knows (`~/projects…`). `CONTEXT_BAR_WORKSPACE_MIN_COLS`
  is pinned to that ladder's own last stop — the bare leaf, four columns, which
  still names the project — so the path is present whenever honest information
  remains. Below it the path is dropped outright: the bar either names the
  workspace or stays silent about it, never drawing a fragment that still
  claims to be the path. (A first cut set the floor at 8, which silently
  discarded widths 4..=7 the ladder serves correctly; the constant is now
  asserted against the ladder itself rather than against a guessed number.)
- **`[INV-HEAD-07]` Ratified**: `[INV-HINT-03]` of ADR-0024 is superseded *in
  form* (its "one predictable location that never migrates with the scene"
  intent survives), and `docs/reference/tui/visual-language.md` gained the
  state-token grammar the change follows.
- **`[INV-HEAD-08]` Ratified — Posture Outranks Identity.** The posture block is
  budgeted before the role badge and is never truncated: a clipped `open`
  reading `op` would still paint in the warning tone, asserting a safety state
  the session may not hold. A long custom role therefore elides inside intact
  brackets (`[SECURITY-AU…]`) and disappears below a legible floor instead of
  degrading to `[D…]`. Truncation markers come from `theme.glyphs.ellipsis`, so
  a non-Unicode terminal gets `...` rather than `…`.
  Regression coverage: `view_header::tests::row3_*` (including
  `row3_posture_outranks_a_long_role_badge`,
  `row3_never_emits_a_truncated_posture_token`,
  `row3_workspace_floor_matches_the_ladder_capability`, and the two
  `row3_degrades_*_under_ascii_glyphs` cases, each verified to fail without
  its fix), plus five golden baselines under
  `snapshot_tests::context_bar` that lock the painted row at the widths where
  each degradation rung engages.
- **Zero Backward Compatibility / Zero Historical Baggage: Retired `ThreadHead` Outright.**
  The monolithic `ThreadHead` struct — a legacy coupling from ADR-0060 that bundled Tier 1
  client tabs (`tabs`, `active_tab`) with Tier 3 thread execution governance (`role`,
  `workspace`, `unattended`, `confined`) — has been deleted.
  - Tier 1 Client TabBar is now purely projected via `TabBarProps` (Client domain only).
  - Tier 3 ContextBar is now purely governed via `ThreadContext` (Thread domain only).
  - Purged fake `ThreadHead` constructions from `DashboardProps` and `SettingsProps`; non-thread
    scenes now cleanly take `Option<TabBarProps>` with zero thread governance leakage.
  - Purged all legacy `UNATTENDED` / `UNCONFINED` strings from ADR-0024, ADR-0050, and ADR-0060,
    aligning historical documentation directly with current invariant specifications.

## [0.0.47] - 2026-10-23

### Added

- **`nuo-client` Is Retired and Its Responsibilities Split Four Ways**
  ([ADR-0089](docs/adr/0089-nuo-client-retirement-and-four-way-responsibility-realignment.md)).
  `crates/nuo-client` is deleted outright rather than kept as a compatibility shell. Its protocol DTOs and
  discovery records move to `nuo-protocol` (`discovery`, `version`, `connection_detail`), client transport and the
  server connection lifecycle move to `nuo-tui::client`, and the interactive command catalog stays with the
  presentation crate that renders it. `nuo-server` therefore has no path to a client crate at all — the
  anti-inversion red-line is now a property of the dependency graph rather than a review convention.
- **Autonomous Persistence Resilience and Transparent Lease Handoff**
  ([ADR-0090](docs/adr/0090-autonomous-persistence-resilience-transparent-lease-handoff-and-zero-plaintext-credentials.md)).
  A `BackupManager` keeps rolling hot backups with `VACUUM INTO` (never `cp` on a live WAL database), prunes to a
  retention bound, verifies `PRAGMA quick_check` plus `user_version` before any write, and preserves the previous
  database as `<target>.pre_restore.bak` so a restore is reversible. A second process that cannot take the
  single-writer lease now degrades into IPC attach instead of failing to start.
- **Version and Protocol Negotiation Moved Into `nuo-protocol`**. `Discovery`, `ServerInfo`, the local endpoint
  defaults, and the version comparator with its stable `protocol_mismatch` / `version_mismatch` error codes are
  protocol facts, so a client can diagnose a skew without linking a transport.

### Changed

- **Routing Syntax No Longer Reaches the User as a Model Name**
  ([ADR-0081](docs/adr/0081-unidirectional-catalog-pipeline-structural-normalization-and-relay-noise-hygiene.md)
  `[INV-CATALOG-05]`). A relay id carries two separators that are not part of a name — a `vendor/` namespace and a
  `:variant` service tier — and the structural lifter tokenized on `-` alone, so `deepseek/deepseek-v4.1-flash`
  and `nex-agi/nex-n2.5-pro:free` rendered as single slash/colon-bearing tokens. The lifter now absorbs both: a
  tier is dropped when it is a member of the closed `SERVICE_TIERS` set, and a namespace is printed only when the
  tail does not already lead with a family brand from `BRAND_LEXICON` — `deepseek/deepseek-v4.1-flash` →
  `DeepSeek V4.1 Flash`, `openai/gpt-5.6-sol` → `GPT 5.6 Sol`, `meta/muse-spark-1.1` → `Meta Muse Spark 1.1`.
- **A Published Label Wins on Both Policies and Is Never Restructured.** `commandcode-plan` and `openrouter`
  declared `DisplayPolicy::Native` while publishing ids-only catalogs, which invents no label and left the raw wire
  id on every surface; both now declare `Structural`, matching what they are. A surface that *does* publish a name
  keeps it verbatim (execution-mode annotations stripped, hyphenated brands folded), because an authored label is
  prose: OpenRouter's `OpenAI: GPT-5.6 Sol` no longer collapses to `OpenAI`, and `Mixtral 8x7B / Instruct` keeps its
  separator. The closed tier set is what makes this safe — a colon that is part of a name is never eaten.
- **Colon-Spelled Promotional Tiers Are Pruned.** `is_relay_promotional_noise` read only hyphenated markers, so
  `nex-agi/nex-n2.5-pro:free` slipped past the `Curated` valve that prunes `beam-free`; the tier segment is now read
  in either spelling, because they are the same routing fact.
- **The Model Bar Draws the Route's Resolved Label.** `ModelBarProps` carries the resolved label beside the wire id,
  so the bottom chrome reads the label the catalog resolved once instead of re-deriving — or echoing — the wire id
  the picker had already relabelled.

### Fixed

- **A Pooled HTTP Connection Can No Longer Cross a Runtime Boundary.** `nuo-egress` shares one
  process-global `shared_client()`, so its keep-alive pool outlives any single caller — but a socket is a tokio
  I/O resource registered with the runtime that dialled it, and once that runtime is dropped the registration is
  dead. The pool handed the socket to the next caller anyway, whose first read failed with
  `RUNTIME_SHUTTING_DOWN_ERROR` ("A Tokio 1.x context was found, but it is being shutdown."), which reached the
  user as a provider transport failure for a request that had never been sent. `netune` v0.0.3 records the owning
  runtime per pooled connection and discards a foreign candidate, so the cost is one handshake instead of an
  unattributable error. The dependency pin moves from `v0.0.1` to `v0.0.3`; `Cargo.lock` records commit `4af9be9`.
- **The Display-Policy Declaration Is Enforced Registry-Wide.** A per-surface test cannot catch a surface that
  declared the wrong policy, because the defect is a disagreement between two declarations.
  `every_registered_surface_renders_its_seeds_without_routing_syntax`
  (`crates/nuo-server/src/provider_registry/tests.rs`) sweeps every registered spec and fails if any seed renders a
  label still carrying `/` or `:` — the check that caught the `openrouter` instance.
- **`ADR-0087` Had No Body.** The registry row and ADR-0088 both cited
  `ADR-0087` while `docs/adr/0087-*.md` did not exist, so the lineage gate would fail on the next commit. The
  decision (model-wire aperture purification, provider/connection orthogonality) is now written down as referenced.
- **The Workspace Is Green Under the Gates It Publishes.** `cargo fmt --all --check` reported 130 diffs and
  `cargo clippy --workspace --all-targets -- -D warnings` failed on 20 sites (14 `needless_borrows_for_generic_args`
  in the telemetry overlay, `unnecessary_sort_by` and `unnecessary_cast` in persistence). Both are silent again, so
  the `verify` job can gate a release tag rather than failing before it.

### Documentation

- **Crate Names in the Contributor Docs Match the Crates.** `docs/dev/subsystems.md`,
  `docs/dev/release-and-versioning.md`, and `docs/reference/state-model.md` still described `nuo-wire` and `nuox` as
  the current names; they now name `nuo-protocol` (with its `nuo-wire` package alias) and drop the retired `nuox`
  binary from the SemVer and release playbooks.
- **`nuo-harness` and `nuo-host` Keep Their READMEs.** Both were deleted wholesale to remove a stale `nuo-client`
  mention; the reference was the defect, not the document, so the READMEs are restored with the reference pointed at
  `nuo-tui::client`.
- **`ADR-0090` Records What Shipped.** Its keyring tier is not implemented: no keyring backend is linked, so Tier 2
  is marked as a target and `[INV-PERSIST-SECRET-01]` is explicitly not claimed as enforced. Backups, lease handoff,
  and ledger-governed reclamation are implemented and verified.

## [0.0.46] - 2026-10-22

### Added

- **Dual-Lane Inference Wire Topology (`generative` and `discriminative`)**
  ([ADR-0086](docs/adr/0086-dual-lane-inference-wire-topology-and-nuo-decision-retirement.md)).
  `crates/nuo-inference-wire` models both generative (chat completions,
  streaming, messages, thinking) and discriminative (probabilistic classification,
  ordinal rubric scoring) inference modalities within a single crate via
  non-intersecting module lanes (`generative` and `discriminative`). Both modalities
  share the pooled transport substrate (`WireClient`, credential sources, endpoint
  telemetry, connection caching) without crate fragmentation (`[INV-WIRE-01]`).
- **Structured Tool Argument Extraction in `nuo-tool` and `nuo-tool-derive`**.
  Extracted tool argument parsing helper (`args.rs`) and derive macros for robust,
  type-safe argument handling across tool capability crates.
- **Google Antigravity Interactions Protocol**. Added interaction channel protocol
  support in `providers/nuo-provider-google-antigravity`.

### Changed

- **Discriminative Domain Nomenclature Migration**.
  Migrated decision types to unambiguous domain nomenclature: `DiscriminativeRequest`,
  `DiscriminativeResponse`, `Question`, `Answer`, `DiscriminativeEndpoint`, and
  `DiscriminativeProtocol`, resolving nomenclature collisions with user permission
  decisions and agent cognitive loops.
- **TUI Overlays and Diagnostics Polish**. Refined modal arbitration, keymap routing,
  connection and model overlays, and multi-segment detail row components in `nuo-tui`.

### Removed

- **`crates/nuo-decision` Retired**
  ([ADR-0086](docs/adr/0086-dual-lane-inference-wire-topology-and-nuo-decision-retirement.md)).
  Excised the standalone `nuo-decision` crate, folding discriminative inference directly
  into `nuo-inference-wire`.
- **Decision IPC Passthrough in `nuo-protocol` Purged** (`[INV-WIRE-02]`).
  Removed intermediate decision re-export module and North-South HTTP client dependency
  from the East-West IPC contract (`crates/nuo-protocol`).

## [0.0.45] - 2026-10-22

### Added

- **`nuo-egress`: One Outbound Egress Substrate for the Whole Process**
  ([ADR-0085](docs/adr/0085-universal-outbound-egress-substrate-and-provider-decoupling.md)).
  The crate that carried model providers' network traffic used to know about
  prompts, images, cache routing keys and vendor error dialects — so anything
  wanting a socket had to link the AI cognitive stack. It is now a Layer-0 leaf
  holding network I/O only: pooled `Client`, the `Egress` transport seam,
  bounded control-plane `Http`/`Request`/`Reply`, byte-exact SSE reassembly, and
  domain-free `EgressError` / `EgressTelemetry`. `nuo-inference-wire::WireClient`,
  `nuo-oauth`, `nuo-server`, `nuo-provider` and every provider crate converge on
  `nuo_egress::shared_client()`, so one platform TLS configuration and one
  connection cache serve the process instead of a cache per instance — an OAuth
  refresh and an inference stream now share warm sockets.
- **`nuo-decision`: System One Decision Engine**. TypeSafe Jev questions,
  answers, calibrated probability responses, endpoints and driver clients move
  into their own crate. It shares the generative lane's transport vocabulary
  (`WireClient`, `WireError`, credential sources) and re-exports it rather than
  redefining it, so the decision face and the generative face cannot drift into
  two wire dialects.

### Changed

- **`nuo-model-codec` Is Now `nuo-inference-wire`**
  ([ADR-0084](docs/adr/0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md)).
  The name says what the crate does: encode an inference request for a vendor
  dialect and demux the answer. The coarse `protocol::{anthropic, openai,
  google}` grouping is split into explicit per-dialect modules
  (`anthropic_messages`, `openai_chat_completions`, `openai_responses`,
  `google_generate_content`, `google_interactions`), because "the OpenAI
  protocol" was never one thing and the Responses and Chat Completions
  dialects disagree on precisely the fields a shared module made easy to
  conflate.
- **Provider Cognition Returns to `nuo-provider`**. Endpoint bindings, envelope
  reshaping, prompt-cache resolution and vision projection are evicted from the
  substrate to the crate that owns provider semantics, and a new `response.rs`
  holds HTTP error classification and context-overflow recognition. Provider
  authors find `Endpoint`, `PromptCacheConfig` and `project_images_for_route`
  where the rest of the provider contract already lives.

### Removed

- **`crates/nuo-code` Retired**. Its AST query and syntax guard surfaces were a
  deterministic infrastructure engine with no consumers outside the Agent-facing
  AST tool, and they now live in `tools/nuo-tool-ast` (`src/syntax/`) — the
  crate that is already the tool projection. ADR-0013 and ADR-0055 are amended
  where they named the crate.

## [0.0.44] - 2026-10-20

### Added

- **Every Documented Gate Now Has a Runner**: `.github/workflows/ci.yml`
  executes the pre-release checklist on every push and pull request instead of
  leaving it to whoever remembered to run it locally — which is how the
  workspace reached 1,011 clippy diagnostics and 52 unformatted files. The
  `verify` job runs `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` and `python3 scripts/check_adr_lineage.py`; the
  `release` job (tags `v*` only, gated on `verify`) builds the prebuilt `nuo`
  binaries for linux-x86_64, macOS-arm64 and windows-x86_64, which Playbook 2 of
  `docs/dev/release-and-versioning.md` already promised.
  `docs/dev/standards/workflow.md` §3 now lists all six items and names the
  runner.
- **`scripts/check_adr_lineage.py`**: walks `.rs` comments and `.md` prose and
  fails on any `ADR-NNNN` citation that does not resolve inside `docs/adr/`,
  with one explicit allowlist — the SQL bodies of shipped migrations, immutable
  by construction.

### Changed

- **ADR Citations Are Resolvable or Absent**: 2,117 occurrences of 170
  pre-ingestion ADR numbers (0085..0304) that no reader could verify and no
  author could amend were retired across 341 files.
  `scripts/retire_legacy_adr_refs.py` stripped the citation token — with its
  `§` anchors, `'s` and `/` shorthand — and kept the sentence, because the
  rationale is load-bearing while the pointer was dead. Scope is comment- and
  prose-only: `.rs` string literals were never touched, since shipped migration
  SQL is pinned by `MIGRATION_CATALOG_FINGERPRINT` and prompt/assertion strings
  are behaviour. Where the citation was the grammatical subject or object, 16
  sentences were reworded by hand, and 18 doc blocks split by a removal were
  rejoined so no doc comment is left detached from its item. 22 message
  literals (log lines, the MCP truncation notice, one-door invariant messages
  and their matching `.contains(..)` assertions) were cleaned deliberately;
  migration SQL comments stay frozen, so 15 legacy numbers remain there by
  design — removing them would strand databases stamped by the old text.

### Fixed

- **Nested Changelog Bullets**: three sub-items under `[0.0.35]` and `[0.0.28]`
  had their indentation stripped by the lineage migration and nest under their
  parent again.

## [0.0.43] - 2026-10-20

### Changed

- **Panic Policy Is Enforced, Not Requested**: `[workspace.lints.clippy]` now
  *denies* `unwrap_used`, `expect_used`, `panic`, `todo` and `dbg_macro`. Test
  code opts out only at the test boundary — every crate root carries
  `#![cfg_attr(test, allow(..))]` and every integration-test crate root a plain
  `#![allow(..)]` — so the shipped graph can never inherit an exemption. This
  generalises the pattern thirteen crate roots already used for
  `unwrap_used`/`expect_used`.
- **Qoder COSY Signing Is Fallible**: `encrypt_info`, the pinned-key RSA wrap and
  `prepare_request{,_for_path}` return `Option`, and both signing call sites
  surface `ProviderErrorKind::Authentication` instead of aborting the transport
  task. The base64↔Qoder substitution is a total `const` table rather than a
  search that "must" find a match, and the outer-thirds swap permutes characters
  so it can no longer split a UTF-8 sequence.
- **Broken Built-In Bash Policy Rule Is Inert**: an unparsable compiled-in regex
  is logged and its rule becomes `CompiledMatcher::Inert` instead of taking the
  session down; a unit test still compiles every built-in pattern.
- **TUI Render Degradation**: a rejected component tree (`commit`) or a rejected
  component (`mount`) is logged and skipped, keeping the previous frame, rather
  than aborting the process and destroying the session state on screen.
- **Session Mesh Registration Is Fail-Open**: an unaddressable session id now
  skips ACP mailbox registration and simply wires no collaboration tools, as the
  surrounding fail-open policy already documented; previously it aborted the
  task. `register_session_mailbox` returns `Option<MailboxHandle>` to make that
  visible in the type.
- **Config Fail-Fast Consolidated**: the three fail-fast paths in `Config` route
  through one documented `fatal_config` choke point (`[INV-CONF-04]`, ADR-0031)
  instead of scattered `panic!`s. Behaviour is unchanged; the abort is now a
  single auditable site.
- **Parameter Structs Over Arity**: `register_provider` and `handle_connection`
  take `RegisterProviderParams` / `ConnectionParams`, and `compacted_card_data`
  returns a named `CompactedCardData` instead of a six-tuple.

### Fixed

- **Checkpoint Integrity Manifest Can No Longer Hash Nothing**:
  `source_manifest` is fallible (`CheckpointError::UnserializableFact`); a fact
  that failed to serialize contributed an empty byte string and silently
  misreported which interval the checkpoint covered.
- **Conformance Report Is an Assertion**: `assert_all_passed` formats its report
  only when the sink is non-conformant, keeping the same message without a
  panicking code path.
- **System Prompt Duplicate Id Names the Id**: registration still fails loudly on
  a duplicate, and the assertion now reports the offending section id.
- **`cargo fmt` and `cargo clippy` Are Green Repo-Wide**: `cargo fmt --all
  --check` passes (ten files had never been formatted since the 2024 style
  edition, and the repo has no `rustfmt.toml`, so the default `max_width` /
  `chain_width` / `single_line_if_else_max_width` thresholds and
  `reorder_imports` applied), and `cargo clippy --workspace --all-targets` went
  from 1011 diagnostics to zero — 954 of which were test-target noise that had
  been hiding the twenty real production sites above.
- **Slash-Command Catalog Type Alias**: the completion spec row uses a named
  `RawCommandSpec` alias.

## [0.0.42] - 2026-10-20

### Added

- **Time-Varying Rate Cards and Pure Cost Rating ([ADR-0080](docs/adr/0080-time-varying-rate-cards-and-off-peak-rating.md))**:
  `ModelPricing` now supports scheduled rate cards (`RateSchedule`, `OffPeakPricing`)
  with pure `(TokenUsage, RatingInstant)` evaluation (`[INV-PRICING-04]`). DeepSeek
  V4.1 Flash and Pro rate cards reflect official 50% off-peak pricing across Beijing
  off-peak hours and Chinese statutory holidays.
- **On-Demand Modal Pricing Disclosure**: Both the flat model details modal
  (`/models` -> `i`) and connection details modal (`/connections` -> `Enter` or
  `@connection`) now feature structured rate cards and off-peak schedules
  co-located with account balance, rejecting bottom-bar clutter.
- **DeepSeek Canonical Model Identity ([ADR-0083](docs/adr/0083-deepseek-model-identity-reasoning-toggle-and-responses-dialect-purity.md))**:
  `nuo-provider-deepseek` now seeds exactly the ids `api.deepseek.com` serves —
  `deepseek-flash` (DeepSeek-V4.1-Flash-1017, multimodal, 1M context) and
  `deepseek-v4-pro`. The retired names `deepseek-v4-flash` and
  `deepseek-v4-flash-vision-exp` stay registered (never seeded) with the serving
  model's capabilities; `deepseek-v4.1-flash` was a Novita/OpenRouter *relay* id,
  never a platform id, and is no longer registered. Plan-included `-free` models
  resolve to zero-cost cards.
- **DeepSeek Reasoning Toggle and Alias Tiers**: `effort_ladders::DEEPSEEK_NATIVE`
  declares the platform's full accepted vocabulary, so `none` (thinking off) is
  reachable and is no longer clamped up to `low`, and `medium`/`xhigh` no longer
  clamp down to `low`. A live `/models` `effort.supported_levels` advertisement is
  expanded to the same set, so a catalog sync cannot change the menu.
- **DeepSeek Responses Dialect Purity**: the Responses builder emits
  `reasoning.summary` / `include: ["reasoning.encrypted_content"]` only where the
  dialect declares them (never for DeepSeek); `response.incomplete` and
  `response.failed` are handled as terminal events (truncated turns keep their
  output, replay artifact and usage); Chat Completions replays an assistant
  turn's `reasoning_content` on the DeepSeek dialect, where the vendor requires it.
- **Single Layer-2 Catalog Crate (`nuo-model-routing`) ([ADR-0082](docs/adr/0082-single-catalog-pipeline-definition-in-a-dedicated-layer2-crate.md))**:
  the catalog pipeline (`derive` / `picker` / `sync` plus its test suite) left the
  duplicated `nuo-harness` and `nuo-server` module trees and now lives in exactly
  one Layer-2 crate that both consumers import directly — no re-export shim and no
  local `catalog` module (`[INV-CATALOG-07]`). The composition-root edge
  (`nuo-model-routing` -> `nuo-server`) is a declared test-only dev-dependency, so the
  shipped build graph stays acyclic and `nuo-model-routing` links no `providers/*` crate
  and no presentation crate.
- **Unidirectional Catalog Pipeline ([ADR-0081](docs/adr/0081-unidirectional-catalog-pipeline-structural-normalization-and-relay-noise-hygiene.md))**:
  ingestion is now three strictly decoupled stages — Source -> Normalization ->
  Overwrite (`[INV-CATALOG-04..06]`). Display names are lifted structurally
  (`deepseek-v4.1-flash` -> `DeepSeek V4.1 Flash`) by tokenization, hyphen-to-space
  folding and a brand lexicon instead of a per-model dictionary, and relay
  ephemera (`*-free`, `*-contributor`, `*-preview`) is filtered from primary
  selection surfaces while explicit user overrides still win.
- **Ignore-Aware Workspace Walker (`nuo-tool-fs`)**: `search` now builds a
  ripgrep-style walker that honors `.gitignore` / `.ignore` / global gitignore and
  prunes VCS metadata, dependency trees and build output (`.git`, `node_modules`,
  `target`, caches), so a workspace sweep no longer wades through vendored and
  generated trees.

### Changed

- **DeepSeek Effort Ladder Correction**: the OpenCode Go/Zen DeepSeek rows now
  carry the platform's accepted reasoning vocabulary (shared ids must agree
  across provider tables), and the retired Flash generation is declared
  multimodal there too, matching the model that serves it upstream.
- **Baseline Fidelity Proof Amendments**: capability rows deliberately changed
  after the registry migration are listed explicitly in
  `POST_MIGRATION_AMENDMENTS` instead of being folded into the historical
  pre-migration snapshot.
- **Dispatch-Anchored Request Rating (`[INV-PRICING-05]`)**: `BeginRequestParams`
  and `RequestUsageRecord` carry the provider dispatch epoch timestamp (`started_at_ms`)
  as the definitive temporal anchor for both durable ledger persistence and cost rating.
- **Session Pre-flight Spend Accumulation**: The local spend circuit breaker reads
  monotonically accumulated turn spend rather than re-evaluating historical tokens
  against current time-varying rates.
- **Reference OpenAPI Fixtures Relocated**: the antigravity and commandcode wire
  captures moved under `docs/reference/openapi/`
  (`commandcode-api.yml` -> `commandcode-plan.yml`), the DeepSeek transcript was
  added as `openapi/deepseek.yml`, and every in-tree reference was retargeted.

## [0.0.41] - 2026-10-19

### Added

- **RAII Test Path Sandbox (`paths::TestDirsGuard`)**: a single guard that holds
  the process-wide override lock and restores the default roots on drop,
  including while unwinding. Every test sandbox now uses it.

### Changed

- **Netune Sourced by Git Tag**: `netune` / `netune-trace` now resolve from
  `https://github.com/ming2k/netune.git` tag `v0.0.1` instead of crates.io, whose
  `0.1.x` line is yanked and unmaintained. All crates declare them once through
  `[workspace.dependencies]`. No API change.
- **Fallible Provider Construction**: `build_provider_for_channel` and
  `build_credential_source` return `Option` instead of panicking when the
  composition root has not registered a factory; callers surface the error at
  their own boundary instead of aborting the task.
- **Isolated Test Roots**: test builds resolve an isolated per-process temporary
  root instead of the developer's real XDG directories. An explicit `NUO_HOME`
  (and a production `set_default`) still wins, so spawned servers and end-to-end
  harnesses keep observing their sandbox.

### Fixed

- **Connection Store No Longer Clobbered by Tests**: the spend-circuit-breaker
  fixture rewrote the developer's real `$XDG_STATE_HOME/nuo/connections.toml`;
  the isolated test root makes that whole class of mistake inert.
- **Per-Crate Builds Without Netune**: `WireClient::new` now exists for every
  feature combination, so `cargo test -p nuo-persistence` and similar builds no
  longer fail to compile.

## [0.0.40] - 2026-10-19

### Changed

- **Process-Shared Netune Connection Pool & Request Prewarm (ADR-0005)**:
  - Converged every netune adapter — the provider egress, the `nuo-model-codec` wire client, and the control-plane `Http` handle — onto one process-wide connection pool via `nuo-provider-transport::network::shared_client`. A warm socket is now warm for the whole process instead of being fragmented across per-instance `Pool::default()` caches.
  - Added `TransportClient::prewarm` and raced it against auth resolution in the Anthropic, Google Antigravity, and OpenAI providers, collapsing DNS/TCP/TLS setup into a pool hit by dispatch time; prewarm failures are advisory and logged, never propagated.
  - `NuoNetEgress` and `WireClient` expose `from_shared_client` for callers that own the client configuration; `Http` handles are now cheap clones over the single pool.

### Fixed

- **Byte-Exact SSE Reassembly Across Chunk Boundaries**: `SseDecoder` now buffers raw bytes and decodes only at `\n` line boundaries, reassembling multi-byte UTF-8 sequences (CJK, emoji) split across network chunks. Eliminates the per-chunk lossy-decode `U+FFFD` corruption in streamed non-ASCII deltas, with regression coverage for split UTF-8 and split CRLF frames.
- **Panic-Free Egress Degradation**: `owned_egress` now degrades a failed shared-client initialization to a logged plaintext connector instead of panicking, so the send path reports the genuine error at dispatch.

## [0.0.39] - 2026-10-18

### Added

- **Unified Remote Transport Topology, Secure Endpoint Resolution & Interactive Remote Attach (ADR-0078)**:
  - Introduced the explicit `ClientTarget` abstraction in `nuo-client`, routing every attach flow through one stream-generic handshake with explicit project scoping — whether the target is a locally discovered/spawned server or an explicitly addressed remote server.
  - Hardened remote endpoint resolution in `nuo-provider-transport` and enabled `rustls-tls-native-roots` certificate verification for websocket transport.
  - `nuo server start` is now detached by default, with `--fg` as the sole explicit foreground declaration.

- **OpenCode Provider Channel Bifurcation & Legacy Container Crate Retirement (ADR-0079)**:
  - Bifurcated `nuo-provider-opencode` into dedicated leaf provider channel crates: `nuo-provider-opencode-plan` (OpenCode Plan subscription surface) and `nuo-provider-opencode-zen` (OpenCode Zen relay surface).
  - Enforced `[INV-PROV-06]` Canonical Provider Spec Export and `[INV-PROV-07]` Channel Boundary Segregation, restoring symmetrical `MODEL_PROVIDER_SPEC` registration at the composition root (`nuo-server`).
  - Completely retired and removed the legacy `nuo-provider-opencode` container crate from the workspace.

### Changed

- **Workspace Namespace Pure Surface Topology & Categorized Palette (ADR-0077)**:
  - Pruned legacy strokes from the `C-x …` workspace namespace: the `Quit` scene verb and its `C-x C-c` / `C-x k` spellings are excised, leaving `Leave` bound exclusively to `C-x w`.
  - Categorized command palette entries (`CommandCategory`) with `@` scoping to Navigation / Settings and `>` scoping to Actions / Commands.
- **`nuox` → `nuo` Surface Consolidation**: renamed the terminal application surface to `nuo` across CLI help, shell completions, and client documentation.

## [0.0.38] - 2026-10-18

### Added

- **Dynamic Composer Height Scroll Reflow & Sticky Viewport Underflow Protection**:
  - Unified composer height calculation across the event loop and transcript renderer via `calculate_composer_height`, eliminating geometry drift during multiline editing.
  - Interactive transcript view now adjusts dynamically in real time when multiline input expands or shrinks the composer box, maintaining proper viewport scroll anchors and preventing content jumping.
  - Added sticky collapse underflow guard: collapses transcript scroll to top when total content fits within the viewport.
  - Sanitized connection quota headers in `/connections`: suppressed redundant provider/quota plan names (e.g. `Antigravity Quota`, `google-antigravity`) to keep badge rows clean and uncluttered.
  - Cleaned Google Antigravity quota parser to emit `None` for plan names when upstream responses echo generic service titles rather than actual tier names.

## [0.0.37] - 2026-11-17

### Added

- **Two-Tier Model Detail Inspector and Actionable Tuning View**:
  Integrated model capability inspection and live behavioral tuning directly into the `/models` dialog surface (`ModelsDialog.info_detail`).
  - Added dedicated Model Detail sub-view accessed via <kbd>i</kbd> (and <kbd>Enter</kbd> on isolated unavailable search results), presenting full un-truncated provider refusal reasons, capabilities, context token limits, and multimodal support.
  - Interactive live tuning knobs directly on the inspector card: <kbd>←</kbd> / <kbd>→</kbd> steps reasoning effort ladders, <kbd>Space</kbd> toggles extended thinking, <kbd>Enter</kbd> saves and activates immediately, and <kbd>Esc</kbd> saves and returns to list view.
  - Sanitized `/models` list row status tokens: replaced verbose multi-clause upstream error prose with clean, compact status badges (`locked`), preventing column collisions and ugly ellipsis truncation.
  - Retired the legacy 50-row plaintext models accordion in `/connections` (`ConnectionDetail`) in favor of a 2-line gateway summary and direct <kbd>m</kbd> shortcut to browse models in `/models` pre-filtered.

## [0.0.36] - 2026-11-17

### Added

- **Canonical Unselectable List Item Topology, Inert Skip Navigation, and Gestalt Spacing Firewall (ADR-0076)**:
  Unified disabled and locked list item interactions across dialog surfaces and option pickers.
  - Ratified `[INV-TUI-INERT-01]` Inert Disabled Item Navigation: selection cursors automatically skip unusable items during arrow navigation and wrap-around, and unusable items are strictly inert to mouse clicks and activation inputs.
  - Ratified `[INV-TUI-INERT-02]` Gestalt Spacing Firewall & Text-Join Hygiene: `ListRow::finish` enforces an unconditional minimum separation (`GROUP_GAP`) before trailing badges, eliminating string collisions on narrow viewports. Status reasons strictly follow canonical em-dash grammar (`locked — {reason}`).
  - Subdued visual presentation: unusable rows render uniformly in `theme.dim()` without bold modifiers.

## [0.0.35] - 2026-10-18

### Added

- **Orthogonal Workspace Container Topology and Intra-Tab Scene Discipline (ADR-0075)**:
  Decoupled horizontal workspace containers (Row 1 TabBar: Thread, Dashboard, Settings) from
  vertical intra-tab drill-in depth (Row 3 SceneBar: Thread, Subagent).
  - Enforced `[INV-TAB-10]` Workspace Container Purity: opening Dashboard or Settings strictly
    allocates or focuses its dedicated sovereign Tab container rather than polluting active thread history.
  - Enforced `[INV-SCENE-02]` Container-Scoped Scene Discipline: `ClientTab::push_scene` mechanically
    rejects cross-container scene injections.
  - Ratified `[INV-COMP-01]` Component Nesting Boundary: code diffs and tool execution outputs remain
    encapsulated as components within the active scene canvas, eliminating scene inflation.
  - Regression tests: `surfaces::tests::test_inv_tab_10_and_inv_scene_02_discipline`.

### Fixed

- **Trust gate confirm loop (workspace trust could never persist)**: a
  wedged SQLite WAL state in the daemon — the on-disk `nuo.db-wal`/`-shm`
  pair removed while the daemon was live — made every fresh read-only open
  fail with `disk I/O error`, so workspace-security reads failed closed and
  the `[file asset trust]` pre-attach gate re-presented itself on every
  Enter with no visible cause. Two-sided hardening:
  - *Persistence (reader-wedge self-heal)*: the supervisor gained a
    typed `RebuildEngine` verb with a `RetireEngine` handshake — the live
    engine is drained with an explicit `wal_checkpoint(FULL)` *while still
    fully functional* (an unlinked WAL's frames are recovered
    deterministically, since the fd-backed read and the main-database write
    are both valid inodes), the engine is then closed cleanly, and only
    after that does the fresh generation open and recreate the sidecar
    pair. `PersistenceHandle::reader()` counts consecutive construction
    failures and requests exactly one rebuild per wedge episode at the
    3-failure threshold (CAS latch, cleared by the next successful open);
    non-owner handles never request a rebuild.
  - *TUI (attestation outcome)*: a failed trust-grant round-trip is
    now rendered on the gate itself (the server's `trust_changed` notice
    text), clearing the submitting spinner so the user can retry with full
    knowledge of the cause instead of looping a silent dialog; trust
    notices with no gate mounted keep their normal transcript/toast flow.
  - Regression tests: `supervision::reader_wedge_heals_through_engine_rebuild`
    (wedge → threshold → rebuild → recovery → durability) and
    `tests::pre_attach_gate::*`. Blameless record:
    `docs/dev/postmortems/2026-10-18-trust-gate-loop-persistence-reader-wedge.md`.

## [0.0.34] - 2026-10-18

### Fixed

- **Release-build regression in `QuestionModel::update_mut`**: the in-place
  update helper was gated behind `#[cfg(debug_assertions)]` while its sole
  production caller (`declarative/ask_user.rs`) invokes it unconditionally —
  release builds (e.g. `wright upgrade`) failed with E0599. The erroneous gate
  is removed; the method is now unconditionally available.

## [0.0.33] - 2026-10-17

### Fixed

- **Tab-Aware `/new` Workspace Projection (ADR-0039, ADR-0058)**: Aligned `/new`
  harness command and its canonical `/session new` invocation with the sovereign
  `ClientTab` workspace topology. When invoked in an active thread with dialogue
  history, `/new` preserves the existing tab and transcript intact, opening and
  focusing a fresh sovereign `ClientTab::Thread` (with optional initial prompt
  execution if arguments are provided), eliminating in-place transcript wipes.
  Invoking `/new` in an already-empty thread resets the session in place,
  preventing tab pollution.
- **Antigravity `claude-{opus,sonnet}-5-5` 404** (`Requested entity was not
  found`): the bare 5-5 ids are catalog-side fold targets, not registered
  PredictionService entities. Reverse-engineering the shipped `agy` 1.4.0
  binary and the paid-pro `fetchAvailableModels` snapshot confirmed the wire
  registers only the per-level entities
  `claude-{opus,sonnet}-5-5-{low,medium,high,max}` — the same
  entity-suffix-is-effort pattern as `gemini-4-argon`. Channel effort and bare
  base ids now route onto them through the typed evidence-based
  `WireEntityResolution` mechanism ([ADR-0074](docs/adr/0074-observed-wire-entity-resolution-and-evidence-driven-per-level-model-routing.md)), with the
  parser→persistence→request-builder evidence belt; both 5-5 flagships were
  added to the provider's baseline surface and offline model seed.

## [0.0.32] - 2026-10-17

### Added

- **OpenCode 2026-10 Promotional Free Models**:
  - Ingested `deepseek-v4.1-flash-free` and `glm-5.3-flash-free` into `nuo-provider-opencode` baselines (`plan.rs`, `zen.rs`, `console.rs`) and model seeds (`OPENCODE_GO_MODELS`, `OPENCODE_ZEN_MODELS`, `OPENCODE_CONSOLE_MODELS`).
  - Marked models with `EPHEMERAL: OpenCode Promotional Free Tier` lifecycle tags, restricting context window to 200K (vs 1M paid) and max output to 32K.
  - Expanded OpenCode Go and Zen baselines with latest flagship models (`glm-5.3`, `glm-5.3-flash`, `qwen3.8-max`, `qwen3.8-flash`, `claude-haiku-5-5`, `gemini-4-argon-contributor`, `kimi-k3`, `mimo-v2.6-flash`, `mimo-v2.6-pro`, `grok-4.7`).
- **OpenCode Multi-Protocol Routing**:
  - Configured protocol roots for OpenCode Go and Zen surfaces: routed `AnthropicMessages` to `inference/go/anthropic/v1` and `GoogleGemini` to `inference/go/google/v1beta`.

### Changed

- **Abolished OpenCode OAuth2 Device Flow & Console Subscription**:
  - Removed obsolete `device.rs` and `oauth.rs` modules, along with `nuo-oauth` and `netune` dependencies from `nuo-provider-opencode`.
  - Re-aligned the general `opencode` provider to standard API key and public keyless access over `https://opencode.ai/zen/v1` catalog.
  - Upgraded OpenCode client profile emulation to `opencode/2.0.32`.
- **Cognitive Memory Role Scoping**:
  - Dialogue memory recording is now strictly guarded by presence of `recall_memory` tool (e.g. `philosophist`), permanently excluding `developer` and `ops` roles from recording conversational transcripts.
- **TUI Surface Navigation**:
  - Refined scene and tab transitions for Dashboard, Settings, and Thread views, including mouse click and tab switching.

## [0.0.31] - 2026-11-09

### Added

- **Unified CAS & VFS In-Process Projection Architecture (ADR-0070)**:
  - Established rootless sovereign VFS addressing under `/tmp/nuo/vfs/{sha256}.{ext}`, operating with zero root requirements across workspace-bound (`developer`) and workspace-free (`ops`, `philosophist`) roles.
  - Implemented in-process virtual path resolution in `read_text` and `read_image`, resolving offstream context directly from memory/CAS with 0 disk I/O and zero workspace git pollution.
  - Abolished the legacy `recall_context` meta-tool, unifying all context inspection under standard POSIX tools (`read_text`, `execute_command (grep)`, `read_image`).
  - Added deterministic physical metrics (`lines` + `KB` for multi-line streams; `characters` + `KB` with `cut/jq` guidance for giant single-line blobs), completely replacing volatile local token estimations.

### Changed

- **TUI `execute_command` Presentation Modernization**:
  - Implemented three-tier flush stacking layout (Part 1: Terminal canvas, Part 2: Process exit status, Part 3: Harness governance) with zero gap intervals, differentiated purely by layered surface background colors (`code_surface`, `match_title_surface`, `harness_bg`).
  - Transformed output folding into a 3-row compact negative space gap (`blank` + `  N lines omitted (KB)` + `blank`), eliminating all intrusive glyph symbols and dashed lines.
  - Eliminated decorative middle dots (`·`) from step headers, standardizing on clean typography: `Run cargo (running)`, `Run cargo (120ms)`, `Run cargo (failed 0ms)`.
  - Replaced aggressive yellow-bold markers with an unaggressive, distinct metadata status footer explaining canonical POSIX exit codes (`exit 1 (general error)`, `exit 127 (command not found)`, `exit 137 (killed / SIGKILL)`).

## [0.0.30] - 2026-10-17

### Fixed

- **Thread Picker Tab Overwrite & Ghost Thread Elimination (`/threads`)**:
  - Empty placeholder threads are now overwritten in-place when selecting an existing thread from `/threads` (or clicking via mouse in `ThreadsDialog`), preventing phantom tab proliferation.
  - Automatically emits `AgentRequest::DeleteSession` for abandoned empty threads so they are discarded without persisting to SQLite.
  - Aligned `CreateNewSession` ('N' in `/threads` modal): resets in-place when current thread is empty; arms `switch_to_new` to create a fresh thread in a new tab when current thread has active conversation, preventing in-place transcript wipes.
- **TUI Lifecycle Elevation & Zero-Flicker Tab Switching**:
  - Elevated `nuotc::terminal::Terminal` instance and `EnterAlternateScreen` lifecycle to outer loop in `run_attached`.
  - Switching thread tabs now preserves raw mode and Alternate Screen continuously without dropping back to the host shell, eliminating physical screen flash.
  - Upgraded terminal substrate `nuotc` to `0.0.3`: decoupled DEC Mode 2026 synchronized updates from color depth, introduced uninitialized sentinels for atomic full-screen diffing, and added `StyledClear` to eliminate background color inversion flicker.

### Fixed

- **Google Antigravity 404 Entity Resolution & PredictionService Wire Alignment**:
  - Mapped `gemini-3.8-flash` directly to upstream CloudCode's verified PredictionService entity `gemini-3.8-flash-tiered`, eliminating HTTP 404 `Requested entity was not found` errors.
  - Decoupled reasoning effort from model naming; effort is conveyed orthogonally via Google-native `thinkingConfig` (`thinkingLevel` / `thinkingBudget`).
  - Aligned Claude and GPT-OSS wire entities to `claude-opus-4-6-thinking` and `gpt-oss-120b-medium`.
  - Ingested upstream `tieredModelIds` (`flash`, `pro`, `flashLite`) in `parse_antigravity_catalog` to ensure primary agent flagship models are prioritized in dynamic catalog discovery without omission.
  - Modernized baseline catalog definitions and purged legacy hallucinated models.

## [0.0.28] - 2026-11-06

### Fixed

- **Google Antigravity End-to-End Inference Wire Mapping & Project Routing**:
  - Dynamically mapped canonical `gemini-3.8-flash` with orthogonal reasoning effort depth to concrete wire variants (`gemini-3.8-flash-minimal/low/medium/high`) preventing upstream 404/400 errors.
  - Standardized companion project fallback to `aicode-consumers` across inference execution (`prepare_request_for_auth`) and quota queries (`fetch_usage_with_project`, `retrieve_antigravity_quota_summary`), preventing HTTP 429 `RESOURCE_EXHAUSTED` faults.
  - Preserved curated priority ordering (`[INV-CATALOG-01]`) for `CatalogShape::GoogleCloudCode` in `fetch_remote_catalog` to guarantee top-tier frontier models (`gemini-4-argon`, `gemini-3.8-flash`) lead model selection.
  - Lifted subscription-disabled variants (e.g. `gemini-4-argon-disabled`) into canonical representations with `Availability::locked`.

## [0.0.27] - 2026-11-06

### Added

- **Canonical Model Catalog Ingestion, Semantic Lifting, and Orthogonal Capability Normalization (ADR-0068)**:
  - Curated Agent Surface Primacy (`[INV-CATALOG-01]`): Prioritized upstream curated agent model sets (`agentModelSorts`) while systematically isolating dedicated sub-task lane models (`commandModelIds`, `commitMessageModelIds`, etc.).
  - Orthogonal Capability Lifting (`[INV-CATALOG-02]`): Folded parameter-suffixed wire variants (`-high`, `-medium`, `-low`, `-minimal`, `-max`, `-tiered`) into canonical model identities and lifted implied parameters into first-class `effort_levels`.
  - Abolition of Legacy Blacklists (`[INV-CATALOG-03]`): Eliminated brittle, hardcoded version string heuristics (`starts_with("gemini-3.6-flash")`) in favor of generic, structural filtering.
  - Provider Dialect Encapsulation (`[INV-CATALOG-04]`): Encapsulated Antigravity live catalog parsing in `nuo-provider-google-antigravity::catalog`, purifying `nuo-provider-catalog` as a shape-keyed dispatch engine.
  - Antigravity 1.3.6 Modernization: Added native support for flagship `claude-sonnet-5-5` and `claude-opus-5-5` on Google Antigravity with full `[low, medium, high, max]` effort depth and transparent wire envelope mapping.

## [0.0.26] - 2026-11-06

### Added

- **Multi-Factor Pricing Specification, Dual-Mode Connection Budget, and Spend Circuit Breaker (ADR-0065)**:
  - Multi-factor model pricing specification capturing prompt token tiers (≤128K, >128K), completion tokens, and cache read/creation costs.
  - Dual-mode connection budget enforcement across token counts and currency spend limits with spend circuit breaker halting execution upon budget exhaustion.
  - Granular token classification and cost accounting recorded across connection usage events.
- **Canonical Protocol Nomenclature and Codec Purity (ADR-0066)**:
  - Renamed `crates/nuo-wire` to `crates/nuo-protocol` to reflect canonical IPC nomenclature across client-server boundaries.
  - Decoupled internal East-West IPC structures from external North-South LLM streaming codecs.

### Changed

- **DocGov Decoupling, Invariant Realism, and Documentation Topology Modernization (ADR-0067)**:
  - Decoupled external docgov tooling, retired `.docgov.yml`, and flattened governance structures to `docs/dev/standards/` and `docs/dev/testing.md`.
  - Degazetted pseudo-invariant tagging across presentation components in favor of typed models, Rust compiler invariants, and deterministic tests.
  - Swept legacy citations and resolved references to canonical ADRs.
- **Google Antigravity Provider Alignment**:
  - Synchronized Antigravity User-Agent profile to `agy/cli/1.3.6` (`cl=999748552`).
  - Purged obsolete legacy models and retired synthetic `-tiered` duplicate; cleanly standardized canonical model set to `gemini-4-argon` and `gemini-3.8-flash` with orthogonal reasoning effort depth.

## [0.0.25] - 2026-11-04

### Fixed

- **Tool Execution Slot State Machine and Viewport Argument Isolation**:
  - Eliminated legacy `("raw", ...)` fallback in `parse_arguments_kv` that leaked unclosed streaming JSON arguments into expanded tool steps.
  - Hardened in-flight component state machine preventing incomplete arguments from triggering execution-phase diff projections during the `Ingesting` phase.
  - Enforced atomic JSON validation and Canonical formatting on lowered tool calls, converting truncated/aborted streams into explicit `[Aborted tool ...]` dialogue facts and preventing unparsed syntax debris from leaking into session history.

## [0.0.24] - 2026-11-04

### Added

- **Orthogonal Agent Tool Surface, Meta-Tool Pruning, and Mutation Topology (ADR-0064)**:
  - Pruned meta-tools and stepwise exploration tools (`ask_user`, `todo`, `list_dir`) from developer role capability sets to eliminate turn inflation and stepwise browsing loops.
  - Hardened `write_file` tool parameters and descriptions requiring complete file content and strictly forbidding placeholder truncation comments.
  - Reinforced `edit_text` as the exclusive primitive for surgical source code modifications.
  - Shifted directory inspection guidance exclusively to pattern-based `find_files` and content-based `search_text`.
- **Consolidated Activity Strip, Scene History Navigation, and Diátaxis Documentation Suite**:
  - Integrated header activity strip providing unified real-time feedback for active tool runs, subagent delegations, and model states.
  - Scene history navigation support in interactive terminal sessions.
  - Complete Diátaxis documentation suite restructuring across tutorials, how-to guides, reference specifications, and architectural explanations.
- **Polymorphic Scenebar Projection and Bifurcated Shell Tool Presentation (ADR-0062, ADR-0063)**:
  - Polymorphic scenebar projection dynamically adapting to session context, multi-thread execution, and telemetry feeds.
  - Bifurcated shell tool execution model cleanly separating interactive foreground actions from background streaming processes.

## [0.0.23] - 2026-10-30

### Added

- **Gemini 4 Argon and Gemini 3.1 Flash Lite Model Support**:
  - Registered `gemini-4-argon` and `gemini-3.1-flash-lite` across model codec, provider specs, and catalog discovery.
  - Wire protocol alignment for Google Gemini API handling `thought: true` boolean flags in response parts.
  - Active running thread visual indicator (`[RUNNING]`) rendered in the session picker overlay.
  - Sovereign thread projection and client tab attach when selecting sessions from the session modal.
- **Pure Thread Topology, Headless Unified Entry, and Persistence Modularization (ADR-0061)**:
  - Headless one-shot execution via top-level prompt and `--headless` flags, retiring legacy `run`.
  - Consolidated thread management under pure `nuo thread` CLI topology and retired vestigial subcommands.
  - Modularized `DatabaseEngine` in `nuo-persistence` into clean domain-specific modules.
- **Canonical Thread Persistence Schema and Spatial Header Topology (ADR-0059, ADR-0060)**:
  - Canonical thread persistence schema and zero-legacy clean break (`[INV-PERSIST-01]`, `[INV-PERSIST-02]`).
  - Three-tier spatial header topology, governance band, and clean tab stack (`[INV-HEADER-01]`).

### Added

- **Granular Causal Session Forking and Sovereign Tab Projection (ADR-0058).**
  - **Granular Temporal Addressing (`[INV-FORK-03]`)**: Extended `/fork` command with granular temporal targeting (`/fork <round>` or `/fork <round>.<turn>`) to branch at arbitrary historical decision points.
  - **Sovereign ClientTab Projection (`[INV-FORK-04]`)**: Mounts forked child threads as sovereign `ClientTab::Thread` viewports on Row 1, allowing concurrent multi-branch exploration with `Alt+1..9` tab switching while preserving the parent session intact.

### Changed

- **Retirement of the `/btw` Aside Subsystem (`[INV-DEPR-01]`, ADR-0058).**
  - Retired the legacy `/btw` command, aside modal overlays, and dedicated aside wire routing, subsuming isolated exploration under first-class causal session forks.
  - Non-destructively redirect legacy `/btw` invocations to `/fork` guidance.

## [0.0.21] - 2026-10-28

### Added

- **Tool Output Claim-Check Mechanism and Context Stub Topology (ADR-0057).**
  - **Zero Context Blowup Guarantee (`[INV-CLAIM-01]`)**: Universal Claim-Check gate offloading oversized tool executions to CAS BlobStore while projecting bounded Context Stubs with head/tail anchors into model context.
  - **In-Flight StreamGuard Defense**: Real-time online stream bounds (200 lines / 16 KB) halting runaway or continuous streaming processes early to prevent context and memory flooding.
  - **Non-UTF-8 Binary-Safe Pipe Ingestion**: Lossless byte-level stream ingestion preventing premature aborts on raw profiling or debugging dumps.
  - **Clean Process Harvesting**: Guaranteed process reaping (`wait` following `kill`) preventing zombie process leaks.


### Changed

- **Terminal configuration canonicalization and disclosure pruning (ADR-0031, ADR-0054).**
  - Canonicalize presentation preferences to `$XDG_CONFIG_HOME/nuo/terminal.toml` and eliminate legacy `tui.toml` shims.
  - Prune redundant tool expansion overrides when matching component defaults, and skip empty `[default_expanded]` tables in TOML serialization.
  - Hermetically sandbox presentation persistence tests with `PathsOverride` to prevent host environment pollution.

## [0.0.19] - 2026-10-27

### Changed

- Maintenance release and workspace dependency synchronization.

## [0.0.18] - 2026-10-27

### Added

- **Capability-first orthogonal terminal composition and autonomous archetype projection (ADR-0053).**
  - **Orthogonal Capability Honesty (`[INV-TERM-01]`)**: Replaces brittle emulator brand heuristics with standards-based orthogonal capability dimensions (color depth, sync updates, mouse protocols, charset encoding).
  - **Two-Phase Archetype Projection (`[INV-TERM-02]`)**: Maps detected capabilities into autonomous elevation archetypes (`RichTrueColor`, `ANSI256`, `ANSI16`, `Monochrome`) and unifies surface elevation containers across presentation views.
  - **Autonomous Zero-Config Adaptation (`[INV-TERM-03]`)**: Automatically tunes visual presentation and theme contrast according to the terminal capability ceiling without manual user configuration.
  - **Spatial Cost Accounting (`[INV-TERM-04]`)**: Accounts for explicit spatial margins and border constraints in responsive layout breakpoints for structured framing on degraded terminal profiles.

## [0.0.17] - 2026-10-24

### Added

- **Unified instruction pipeline, pure role topology, and end-to-end cache fidelity (ADR-0052).**
  - **Deterministic Pipeline Structure (`[INV-PROMPT-01]`)**: Implements strict 5-tier instruction bundling (Global Rules -> Runtime Baseline -> Profile Persona -> Project Directives -> Dynamic Directives) with semantic dependency-directed topological ordering and memoization.
  - **Zero Frame Mutation (`[INV-PROMPT-02]`)**: Directs all harness instruction layers into top-level system frames with zero conversational history mutation or synthetic turn injection.
  - **Role Topology Purity (`[INV-PROMPT-03]`)**: Enforces pure `developer`/`system`/`user`/`assistant`/`tool` role fidelity across all provider adapters without mimicked role multiplexing.
  - **Prompt Cache Frontier Preservation (`[INV-PROMPT-04]`)**: Pinpoints and aligns prompt cache breakpoints across Anthropic, Google Antigravity, and OpenAI wire adapters for optimal prefix cache hits.

- **Composer canonical quadrant modality, atomic draft capsule, and in-place recall topology (ADR-0051).**
  - **Canonical Modality Quad-Partition (`[INV-COMP-01]`)**: Establishes four orthogonal, mutually exclusive composer states: `Normal`, `Recall`, `FollowUp`, and `Steer`.
  - **Atomic Draft Capsule (`[INV-COMP-02]`)**: Unifies text buffer, caret position, and staged attachment handles into an atomic snapshot stashed and restored deterministically across modality shifts.
  - **In-Place Recall Topology (`[INV-COMP-03]`)**: Binds history search and ghost preview entirely to the native composer text surface without separate popup input fields.
  - **Zero Mode Ambiguity (`[INV-COMP-04]`)**: Unifies `Enter` / `Esc` semantic dispatch with visual modal indicator badges and distinct tone highlights.

- **Authentic thread title lifecycle, clean scene head, and semantic tab projection (ADR-0050).**
  - **Single Title Source of Truth (`[INV-TITLE-01]`)**: Binds thread titles durably to session identity with automatic cognitive generation on first turn completion and manual lock preservation.
  - **Clean Scene Head (`[INV-HEAD-01]`)**: De-clutters top-level scene headers into distinct identity and scene-status bands, eliminating redundant title and status duplication.
  - **Semantic Tab Projection (`[INV-TAB-01]`)**: Automatically projects authentic session titles and role badges into the top TabBar tabs with graceful middle-truncation.

- **CLI modernization, resource symmetry, and universal state snapshot protocol (ADR-0048, ADR-0049).**
  - **Domain Alignment & Thread Terminology (`[INV-CLI-01]`, `[INV-RES-01]`)**: Modernizes CLI verbs (`nuo thread`, `nuo start`, `nuo stop`) with symmetric subcommands and deprecates legacy commands with actionable hints.
  - **Universal State Snapshot Protocol (`[INV-SNAP-01]`)**: Standardizes JSON/tabular diagnostic outputs across client and server with real-time daemon status monitoring.
  - **Client-Driven Lifecycle & Zero-TCP IPC (`[INV-IPC-01]`)**: Robust local socket communication with instant client-driven daemon lifecycle handoff.

## [0.0.16] - 2026-10-23

### Added

- **Autonomous tool execution slot, fault-tolerant streaming projection, and bifurcated lifecycle architecture (ADR-0046).**
  - **In-Flight Phase Bifurcation (`[INV-TOOL-LIFE-01]`)**: Deconstructs monolithic tool running states into explicit first-class `Ingesting` (model generating parameters) and `Executing` (local host/sandbox execution) stages with dedicated visual channels and theme accent coloring.
  - **Fault-Tolerant Streaming Projection (`[INV-SLOT-02]`)**: Introduces a resilient streaming JSON parser extracting closed parameter fields (`path`, `command`, `query`) while upstream JSON syntax streams, eliminating generic `Edit text` placeholders in favor of instant semantic file targeting (`Edit src/auth.rs`).
  - **Pre-Commit In-Flight Diff Preview (`[INV-SLOT-05]`)**: Automatically projects syntax-highlighted diff previews into expanded tool steps upon argument availability before disk write operations commit.
  - **Streaming Arguments Wire Delivery**: Upgrades wire protocol and harness event channels so `ToolInputProgress` delivers partial argument payloads for real-time frontend projection.

## [0.0.15] - 2026-10-22

### Added

- **Dynamic port sharing palette, orthogonal control plane, and server lifecycle handoff (ADR-0045).**
  - **Intent-Driven Server Handoff (`[INV-LIFECYCLE-03]`)**: Interactive sessions launched with explicit network port declarations (`nuo --port <port>`) decouple server lifecycles from client detachment, printing actionable handoff guidance upon TUI exit while keeping server tasks running.
  - **Port & Remote Sharing Palette (`C-x p`, `[INV-PALETTE-01]`)**: Introduces a dedicated client-level palette modal to hot-enable, hot-disable, or copy connection commands without interrupting ongoing prompts.
  - **Dynamic Endpoint Control Plane**: Extends wire protocol with `OpenTcpListener`, `CloseTcpListener`, and `GetServerEndpoints` verbs with automatic `server.json` synchronization.
  - **Canonical Baseline Port Realignment (`[INV-PORT-01]`)**: Unifies system-wide default TCP port to 9800 across all diagnostic and discovery fallbacks, eliminating historical 9527 drift.
  - **Command Space Purity (`[INV-CMD-02]`)**: Purged `/debug` from conversational slash commands, preserving Thread Command Purity.

## [0.0.14] - 2026-10-21

### Added

- **Composer-native history search, ghost preview, and draft stash topology (ADR-0044).**
  - **Single Input Source of Truth (`[INV-HIST-01]`)**: Establishes Composer as the sole authoritative text input and physical caret owner (`CaretOwner::Composer`) throughout history search, eliminating floating dual-input popups and eye-jumping caret displacement.
  - **Live Ghost Multiline Preview (`[INV-HIST-02]`)**: Dynamically projects selected historical entries into the Composer viewport with full multi-line ghost preview styling, eliminating blind selection of complex prompts.
  - **Atomic Draft Stash & Rollback (`[INV-HIST-03]`)**: Automatically stashes active uncommitted drafts and attachments into `history_draft` upon entering history search, guaranteeing 100% loss-free restoration on `Esc` dismissal.
  - **Zero Dual-Input Chrome (`[INV-HIST-04]`)**: Strips embedded query inputs, fake carets, and prompt noise from the floating history panel, keeping it as a pure candidate drawer.

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
