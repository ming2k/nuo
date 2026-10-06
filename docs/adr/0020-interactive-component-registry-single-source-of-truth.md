---
id: ADR-0020
title: "Interactive Component Registry: One Declared Table as the Single Source of Truth for Tool Presentation, Settings Rows, and Disclosure Defaults"
status: accepted
date: 2026-10-04
scope: tui/nuo-tui, presentation/disclosure, settings/components, interaction/affordance
superseded_by: null
negative_knowledge: true
---

# 0020. Interactive Component Registry: One Declared Table as the Single Source of Truth for Tool Presentation, Settings Rows, and Disclosure Defaults

- Status: Accepted
- Date: 2026-10-04
- Deciders: Nuo Architecture Working Group
- Consulted: TUI Presentation, Settings, and Interaction Teams
- Informed: System Architects
- Complements: [ADR-0008](0008-single-tool-contract.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md)

---

## Context

The Settings → Components pane is the user-facing control surface for two
questions the transcript answers on every frame:

1. **Which transcript entries are interactive** (focusable, clickable,
   collapsible)?
2. **Which of them open by default?**

Both answers were previously maintained by hand in three places that had no
mechanical relationship to each other:

- `tools::presenter_for` — a `match name { … }` arm list mapping tool names to
  presenters, plus each presenter's own `default_expanded()`;
- `views::settings::components` — a hardcoded array of panel rows with
  `item_count() -> 9` and per-row labels, descriptions, and config keys;
- `event_loop::actions` — a nine-arm `match config_detail_index { 0 => …, 8 => … }`
  that toggled those same rows by position and hand-fanned each change out to
  a hand-picked subset of alias names.

Three concrete failures followed from that shape:

1. **Alias coverage drifted silently.** `presenter_for` maps
   `run_command | execute_command | bash` to one presenter, but the settings
   toggle wrote only `execute_command`. A step recorded as `run_command` (the
   name `nuo-harness` emits) ignored the user's choice. `write_todos` and
   `update_todo` were likewise unconfigurable.
2. **A new tool did not appear in Settings.** Adding a presenter file and a
   registry arm left the panel, its `item_count()`, and the dispatcher's index
   arms stale — the component rendered with whatever defaults its presenter
   happened to carry, with no way to configure it. The maintainer had to
   remember three edits in three modules, and forgetting the third produced a
   panel whose item count disagreed with the arms that consumed its indices.
3. **Positional identity made the list fragile.** Because the dispatcher keyed
   on `config_detail_index`, inserting a row above an existing one silently
   re-bound every toggle below it.

Separately, an audit of the marker contract found four components that rendered
as interactive without being wired end to end:
`StepKind` had no variant for the compaction card, so its click fell through to
a text selection; a `PROVIDER_RETRY_BLOCK_IDX` sentinel was defined and
matched but never recorded by any renderer; `INPUT_MSG_IDX` collided with it on
the same numeric value, so any region that did record it would have been
routed to the composer; and the compaction card advertised `[Enter / Space to
inspect]` while Space activation was swallowed by the focused-target guard.

## Decision

**A single declared table, `tools::TOOL_COMPONENTS`, is the authoritative
source of truth for the tool-backed interactive components.**

1. **One table, three consumers.** Each entry declares an `id`, panel copy, the
   presenter for each member name, and the component's disclosure policy
   (`expanded_by_default`). From that table:
   - `presenter_for(name)` resolves the rendering presenter;
   - the Settings pane derives one row per entry, in table order
     (`row_for_index`), so `item_count()` is computed, never written;
   - the activation dispatcher resolves `config_detail_index` through the same
     `row_for_index` and matches on `ComponentRowId`, never on a positional
     literal;
   - `set_component_default_expanded` writes **every** member name, so alias
     coverage is a property of the table rather than of a maintainer's
     diligence.

2. **Declaring a component is the only required act.** Adding a presenter file
   and one table entry makes the panel row appear, makes its toggle work, and
   makes every alias follow. No other module needs editing.

3. **A component may span several presenters.** A row's meaning is the
   user-facing disclosure policy, not the drawing routine: Search groups the
   text, glob, and directory presenters; Diffs groups edit and write. Members
   must agree on the declared default; a test asserts it, so two sources of
   truth for one fact cannot drift.

4. **Undeclared tools stay collapsed and unlisted.** Dynamic (MCP) tools have
   no declared component identity, so they resolve to the fallback presenter,
   default to collapsed, and do not appear in the panel. This is the safe
   direction for a tool the user never selected.

5. **Disclosure defaults are declared, not per-presenter.** `edit_text`,
   `write_file`, and the shell family open by default (the change is the point,
   the output is the point); everything else — including checklists, whose
   collapsed summary already reports progress — collapses.

6. **An interactive marker is a four-channel contract.** A component that
   renders the disclosure marker or a focus affordance must be wired in all
   four: the sentinel it records, the `StepKind` that classifies a pointer hit,
   the `InteractiveTargetKind` that classifies a focus target, and the
   hover/focus color pass-through. `StepKind` and `InteractiveTargetKind` are
   kept in lock-step, and `InteractiveTarget::for_block` is the single sentinel
   → target mapping that both `interactive_targets()` and hit-testing use.

7. **Advertised keys must be live.** A hint may only name a chord the focused
   component actually routes. Fixing the gap in either direction is
   acceptable — wire the chord, or drop it from the hint — but a hint that
   names a dead chord is a defect. `Enter` and `Space` are the activation pair,
   matching the dialog toggle convention.

8. **Fully disclosed entries carry no disclosure marker.** A notice's severity
   glyph *is* its marker vocabulary, and a command entry's invocation/reply
   pair is one component in two lifecycle states. Neither renders `+`/`-`, and
   neither folds. Entries whose body is genuinely foldable (tool steps,
   reasoning, compaction cards, subagent tasks) render the shared marker.

## Consequences

### Positive

- **Adding a tool component is one file plus one table entry.** The panel, its
  count, its ordering, and its toggles follow mechanically.
- **Alias coverage is structural.** A legacy spelling (`bash`, `write_todos`)
  cannot be missed, because no module enumerates names by hand.
- **Panel and dispatcher cannot disagree.** Both resolve rows through
  `row_for_index`, and `ComponentRowId` carries the tool identity, so a row's
  meaning is stable against insertion.
- **Marker honesty is testable.** Lock-stepped kinds plus a single
  sentinel → target mapping turn "this looks interactive but isn't" into a
  compile error or a failing test rather than a user report.
- **A dead sentinel cannot hide.** `INPUT_MSG_IDX` no longer shares a value
  with a block sentinel, so an accidental re-route into the composer is
  structurally impossible.

### Negative / Accepted Costs

- The table is a chokepoint: a contributor adding a tool must add a table
  entry, and the compiler will not tell them if they forget one that no other
  code path references. Regression tests (`component_table_is_authoritative…`,
  `panel_rows_are_derived_from_the_tool_registry`) cover the declared set, and
  an undeclared tool degrades safely (collapsed, unlisted) rather than wrongly.
- Components are grouped by disclosure policy rather than by presenter, so a
  reader looking for `ListDirPresenter` must follow `members` to find it.
- `ToolComponent` implements `PartialEq` by `id` (not by all fields), which is
  a deliberate identity semantics for row matching and must not be relied on
  for structural comparison.

## Rejected Alternatives

### 1. Keep three hand-maintained lists, add a "remember to update Settings" note

*Why rejected*: The drift had already occurred three separate times (alias
coverage, item count, panel/dispatcher index agreement) despite the code
living in one repository with one author. Documentation is not a mechanism;
the reminder would fail exactly as often as it had already failed.

### 2. Share one `const` list between the renderer and the dispatcher only

*Why rejected*: Solves the panel-vs-dispatcher divergence but leaves the
presenter registry, the per-presenter `default_expanded`, and the alias
fan-out as independent copies of the same vocabulary. The `run_command`
coverage bug would survive, because the config keys a toggle writes are not
derivable from a label list. The table must own the *names*, not just the
labels.

### 3. Derive panel rows by reflecting over the presenter registry at runtime

*Why rejected*: Rust has no runtime reflection, and a proc-macro registry would
invert the dependency (the macro must be invoked at each presenter
definition) while producing a panel whose ordering depends on link order. A
declared table is explicit, ordered, auditable, and greppable — the failure
mode of "which row is index 3?" is answered by reading one list.

### 4. Make every tool default to expanded so the panel writes nothing

*Why rejected*: The transcript is a reading surface. Expanding unknown and MCP
tools by default would let an unvetted tool dominate the viewport with a wide
body it never advertised, and it inverts the principle that disclosure is
opt-in per component.

### 5. Give notices and command entries disclosure markers for visual uniformity

*Why rejected*: A `+`/`-` marker is a promise that the body folds. Neither
entry has a folding state to honor (command entries are designed marker-free), so
adding the marker would create the very affordance gap this ADR
exists to close — a control that looks operable and isn't. Uniformity of
*meaning* is preserved by the shared severity glyph and header contract;
uniformity of *glyph* is not a goal.

### 6. Introduce a dedicated `ProviderRetry` interactive target kind

*Why rejected*: A live provider retry renders through the notice renderer with
a countdown — it is a notice in every respect that matters to interaction, and
it already records `NOTICE_BLOCK_IDX`. A peer kind would need its own sentinel,
its own `StepKind` arm, and its own hover/focus pass-through to represent a
distinction the user cannot observe.

## Compliance

- `tools::TOOL_COMPONENTS` is the only place a tool name, panel label, or
  disclosure default may be declared.
- `views::settings::components::item_count()` must remain derived; a literal
  count is a regression.
- `StepKind` and `InteractiveTargetKind` must be extended together, and
  `InteractiveTarget::for_block` must handle every sentinel a renderer records.
- A renderer may only advertise a chord that `InteractiveEntry::handle_focused_key`
  (or the scene key scheme) routes while that component is focused.
