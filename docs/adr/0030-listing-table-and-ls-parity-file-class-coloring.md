---
id: ADR-0030
title: "Listing Tables and `ls`-Parity File-Class Coloring"
status: accepted
date: 2026-10-10
scope: tui/nuo-tui, presentation/disclosure, capability/tools, terminal/elevation
superseded_by: null
negative_knowledge: true
---

# 0030. Listing Tables and `ls`-Parity File-Class Coloring

- Status: Accepted
- Date: 2026-10-10
- Deciders: Nuo Architecture Working Group
- Consulted: TUI, Tool, and Presentation maintainers
- Informed: System Architects
- Amends: [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md) §"Tool presentation" (the `list_dir` presenter now renders a table body)
- Related: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md), [ADR-0018](0018-on-demand-session-review-diagnostics-and-multi-process-history-union.md), [ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md)

---

## Context and Problem Statement

An expanded `list_dir` step rendered as a **count tally** over a stack of
glyph-prefixed rows:

```
  33 items
  ▸ .cargo           22 B
  ▸ .git             210 B
  · Cargo.lock       109443 B
```

Three things were wrong with this, all visible in the same screenshot:

1. **The top tier summarised instead of labelling.** The first line was a
   human-readable total (`33 items`) parsed from the tool's own header. A
   listing is a two-column table; its first line should say *what the columns
   are*, not *how many rows there are*. The count is already implied by the rows
   and is decoration the reader skips.
2. **The type glyph carried no type.** `▸` vs `·` distinguished directory from
   file — but a directory in a terminal is conventionally marked by a trailing
   `/`, and the glyph spent a column of horizontal budget to duplicate that fact
   less legibly.
3. **The colors were not the shell's.** Directories rendered in the block's
   generic `info` accent and files in `code_fg`; the shell's `ls` — the mental
   model every user brings to a listing — renders a directory **blue**, an
   executable **green**, and a symbolic link **cyan** (per `LS_COLORS` /
   `dircolors` defaults). Faithfully matching that convention is what makes a
   listing scannable at a glance.

The root constraint that shaped the fix: **`list_dir`'s output carried no
executable bit and no symlink flag.** Its per-row tag was a two-way
`[DIR]` / `[FILE]`, so the renderer *could not* color an executable or a link
correctly even if it wanted to — the information was not on the wire. Any
faithful `ls` parity therefore has to start at the tool.

## Decision Drivers

- **Shell parity.** A file listing must read like the `ls` output the user knows,
  or it forces a translation step on every glance.
- **Colour the observed fact, never a guess.** The green/cyan classes must come
  from metadata the tool actually read, not from a filename heuristic — a
  `.sh` file that is not executable is *not* green in `ls`, and a renderer that
  guesses otherwise is wrong.
- **No invented type on degraded input.** A restored session, or a payload that
  predates the class tags, must degrade to plain paths rather than have the
  renderer fabricate a directory/file/exec distinction it cannot see.
- **Terminal-capability honesty (ADR-0180 via [ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md)).** A 16-color console
  has no RGB ladder, and monochrome must emit no hue at all; the `ls` hues must
  collapse to named ANSI slots and to the reset content tone respectively, never
  leak a truecolor the terminal cannot render.
- **Selectable row ranges stay anchored.** The table head and the trailing `/`
  are decoration; selecting a listing must still copy the underlying raw paths
  in reading order.

## Considered Options

- **Option 1 (chosen):** Widen the tool's per-entry tag to a four-way
  `[DIR]` / `[EXEC]` / `[LINK]` / `[FILE]` **class**, and render `list_dir` as a
  table: a `Name` / `Size` header row, no per-row glyph, a trailing `/` on
  directories, and the name coloured by the observed class using the shell's
  blue / green / cyan convention.
- **Option 2:** Keep the tool output unchanged and infer the class in the
  renderer from the name/extension.
- **Option 3:** Keep the count band as the first line and only change the row
  coloring.
- **Option 4:** Keep the type glyph and add coloring; leave the header as-is.

## Decision Outcome

Chosen option: **Option 1.** It is the only option that delivers true `ls`
parity (the executable and symlink classes are observable facts the tool holds
and the renderer cannot reconstruct), and it does so without a heuristic that
would be wrong precisely where it matters — the extension's green is a
*permission* fact, not a *name* fact.

Concretely:

- **The class travels on the wire.** `ListDirTool` classifies each entry
  (`EntryClass::classify`) in priority order — **symlink** (the link's own type,
  never followed), then **directory**, then the **executable bit** (`mode &
  0o111`, POSIX only), else a plain **file** — and prints it as the row's leading
  tag. The executable bit is consulted only where the host exposes a POSIX mode;
  elsewhere nothing is reported as executable.
- **The renderer draws a table.** `draw_listing_content` detects a tagged body,
  measures one name column (widest entry, directory `/` included, clamped so the
  size column always fits) and one size column (widest byte size), paints a
  `Name` … `Size` header over those columns, and renders each entry as
  `name[/]●size` with the size right-aligned in its column.
- **Color is the class, resolved by the theme.** `Theme::listing_color(ListingClass)`
  is the single source: `code_text()` for a file; the canonical `ls` blue / green
  / cyan otherwise — nudged toward the scheme's content tone on a light surface,
  mapped to `LightBlue` / `LightGreen` / `LightCyan` on ANSI-16, and collapsed to
  `code_text()` under monochrome.
- **`find_files` is untouched.** Its count-band + per-directory title-band layout
  is a different shape (grouped paths, not a table) and stays as it was; only a
  *tagged* body is drawn as a table, so the two listings diverge by their data,
  not by the tool name.

### Invariants & Behavioral Boundaries

- `[INV-LIST-01]` **Colour the observed class; never infer it.** The renderer
  colours the class the tool emitted. It MUST NOT derive a class from a filename,
  extension, or trailing slash.
- `[INV-LIST-02]` **Degrade, never invent.** A line without a recognised class
  tag is a plain path row, not a guessed entry. A body with no tagged rows is a
  `find_files` run (count band), never a table with a fabricated header.
- `[INV-LIST-03]` **The table head is decoration.** The header row registers no
  selectable region; a copy of the block yields the raw paths in reading order.
- `[INV-LIST-04]` **Capability honesty.** The `ls` hues resolve to named ANSI
  slots on a 16-color terminal and to the reset content tone under monochrome —
  no class emits a truecolor the active archetype cannot express.
- `[INV-LIST-05]` **The directory `/` is a presentation fact.** The trailing `/`
  is added by the renderer for a `Dir` class; it is never sent on the wire and
  never duplicated when a name already carries one.

### Positive Consequences

- A `list_dir` reads like the `ls` output the user already knows: blue
  directories, green executables, cyan links, a trailing `/` on a directory, and
  a size column.
- The class is a first-class, single-source fact (tool → tag → `ListingClass` →
  `Theme::listing_color`), so a future surface (an overlay, a different palette)
  reuses the one decision instead of re-deriving it.
- The header row states the table's contract and costs one decorative row that
  replaces the equally-decorative count band — no net vertical cost.

### Negative Consequences & Trade-offs

- **The model-visible tool text changed.** `list_dir`'s output now carries a
  four-way tag and a wider fitting field. This is a tool-output contract change:
  its tests, the TUI fixtures, and the manual-testing scenario all move with it.
  The trade is deliberate — faithful colouring is impossible without the fact on
  the wire — and the tags remain human-readable to the model.
- The line width drops from a fixed 25-column name field to the widest entry,
  which is denser but means the raw text is no longer column-aligned on its own;
  the alignment is now a renderer concern (measured, not padded on the wire).

## Rejected Alternatives & Negative Knowledge

### Option 2 (Rejected): Infer the class from the name/extension in the renderer
- Why considered: No tool-output change; the renderer stays the only surface
  touched.
- Why rejected: `ls`'s green is a **permission** fact, not a **name** fact. A
  `script.sh` with `0644` is not green in `ls`, and a `Makefile` with `0755` is.
  A name heuristic is wrong exactly at the boundary the colour is meant to
  signal, so it would teach the user to distrust the green. A heuristic that is
  wrong where it is most informative is worse than no colour.

### Option 3 (Rejected): Keep the count band; only recolor the rows
- Why considered: Smallest change; the header was not the reported defect.
- Why rejected: It leaves the table's columns unlabelled while introducing two
  aligned columns (name, size) that visibly beg a header. A count tally over a
  two-column table is the wrong first line; the header *is* the table's
  identity, and it subsumes the tally's only useful job.

### Option 4 (Rejected): Keep the type glyph and add colour
- Why considered: The glyph is an existing, tested affordance.
- Why rejected: It duplicates the directory signal (`▸` *and* blue *and* a
  trailing `/`) while spending a column of horizontal budget the name column
  needs — the terminal glyph is the least legible of the three. The trailing `/`
  is the shell's own, more legible, and color carries the rest.

### Colouring an executable and a directory the same green (Rejected)
- Why considered: GNU `ls` renders an executable *directory* in both blue and
  green (a `dircolors` `ow`/`tw` combination), so one might argue a directory
  should show its executable bit.
- Why rejected: The listing is a **type** cue first; a directory is navigated,
  not run, and the block has exactly one cell to colour. Collapsing to a single
  class (a link wins, then a directory, then the executable bit) keeps the cue
  unambiguous; a two-dimensional "type × permission" cell is a `ls -l` concern,
  out of scope for a summary table.

## Compliance

- `ListDirTool` must classify in the fixed priority order (link → dir → exec →
  file) and must consult the executable bit only via a POSIX mode; a
  non-POSIX host reports no executable.
- `draw_listing_content` must draw a table only for a **tagged** body and must
  fall back to the count band + title-band layout for a `find_files` body —
  covered by `list_dir_expanded_renders_listing` and
  `find_files_expanded_groups_entries_under_directory_titles`.
- `Theme::listing_color` must be the only source of the listing hues, must
  resolve to named ANSI slots under ANSI-16 and to `code_text()` under
  monochrome — covered by `listing_color_matches_ls_convention`,
  `listing_color_uses_named_slots_under_ansi16`, and
  `listing_color_is_hueless_under_monochrome`.
- The absence of per-row glyphs and of the count band must be asserted, not
  merely snapshotted (`list_dir_expanded_renders_listing` asserts `▸` / `·` /
  `N items` are gone).

## Links

- Supersedes the `list_dir` half of the 0.0.7 layered-listing change (the
  `find_files` half stands).
- Related ADRs: [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md)
  (tool presentation registry), [ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md)
  (terminal capability honesty), [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md)
  (retained canvas).
- Related code: `tools/nuo-tool-fs/src/lib.rs` (`EntryClass`, `ListDirTool::execute`),
  `nuo-tui/src/theme.rs` (`ListingClass`, `Theme::listing_color`),
  `nuo-tui/src/disclosure/renderers/payloads.rs` (`draw_listing_content`,
  `draw_listing_header`, `parse_dir_entry`).
- Tests: `tools/nuo-tool-fs/src/lib.rs` (`entry_class_prefers_link_then_dir_then_executable`,
  `list_dir_tags_executables_and_directories`), `nuo-tui/src/theme.rs`
  (`listing_color_*`), `nuo-tui/src/snapshot_tests.rs`
  (`list_dir_expanded_renders_listing`), `nuo-tui/src/render/tests/tool_steps.rs`
  (`list_dir_table_colors_names_by_ls_class`).
