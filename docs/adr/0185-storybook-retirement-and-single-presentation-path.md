---
id: ADR-0185
title: "The Offline Storybook Is Retired; the Production Renderer Is the Sole Presentation Path"
status: accepted
date: 2026-11-14
scope: tui, view, ui-primitives, cli, devtools
superseded_by: null
supersedes: [ADR-0072, ADR-0073]
negative_knowledge: true
---

# 0185. The Offline Storybook Is Retired; the Production Renderer Is the Sole Presentation Path

- Status: Accepted
- Date: 2026-11-14
- Supersedes: [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md), [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md)

## Context and Problem Statement

ADR-0072 introduced a pure declarative component paradigm and ADR-0073 an
offline storybook to verify it. The production TUI did not converge on either:

- `crates/nuo/src/shell/declarative` is a ~180 KB parallel component suite
  (`ToolCard`, `NoticeCard`, `ModelRowCard`, `PaletteCard`, `ReasoningCard`,
  `ToastCard`, `TurnBandCard`, …).
- Production uses only `AskUser`; `ToolCard` is referenced outside the module
  only by a doc comment.
- The real tool renderer is `shell/disclosure/renderers/tools.rs`. Its header
  claims it is "connected with ToolCard to guarantee zero drift", but no code
  or test enforces that.
- There are three `ToolStatus` types: the domain `ToolStepStatus`, the view
  `nuo_view::tools::ToolStatus`, and the declarative component `ToolStatus`.
- ADR-0073 requires the storybook to be compiled under
  `feature = "dev-storybook"` and excised from release. No such feature
  exists; the suite and its `notify` watcher compile into every interactive
  build.

The parallel suite is not a verification mechanism; it is a second
implementation with no drift protection.

## Architectural Decisions

### 1. The storybook and the declarative component registry are deleted

`shell/declarative/` is removed, including `Workbench`, `Story`,
`StorybookComponent`, the story-only cards, and the `nuo dev storybook` /
`nuo dev export` CLI commands. No feature gate preserves them.

### 2. `AskUser` survives as a production overlay

`shell/declarative/ask_user.rs` is moved to `shell/overlays/ask_user.rs` and
kept as a pure render/state pair. It is not part of a component registry and
does not depend on a story abstraction.

### 3. One tool status, one renderer

The declarative `ToolStatus` is deleted. The domain
`ToolStepStatus` remains the model source of truth, and
`nuo_view::tools::ToolStatus` remains its presentation classification. The
production tool-step renderer is the only renderer.

### 4. Snapshot tests remain the visual regression mechanism

The existing transcript/tool-step snapshot tests and the text/color/geometry
firewalls are the verification path. A component that needs isolated rendering
gets a pure function and a snapshot test beside it; it does not get a second
application shell.

### 5. Invariants

- **`[INV-TUI-SINGLE-RENDER-01]` Single Presentation Path**: No type named
  `ToolStatus` may exist outside `nuo-view::tools`; no `shell/declarative`
  module may exist.
- **`[INV-TUI-SINGLE-RENDER-02]` No Storybook in Release**: The `nuo` binary
  has no `dev storybook` / `dev export` subcommand and no `notify` dependency
  used only for a story watcher.

## Consequences & Trade-offs

- **Positive**: One tool-card implementation; no three-way status drift; the
  release binary loses the storybook and its watcher; ADR-0073's promised
  feature gate is no longer a fiction.
- **Negative & Incurred Costs**: The offline component gallery is gone. Visual
  iteration uses the real TUI plus snapshot tests. This is the deliberate
  trade: the gallery had no production consumer.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Add the missing `dev-storybook` feature
- **Why considered**: Preserves ADR-0073 as written.
- **Why rejected**: A feature gate fixes the binary size, not the parallel
  implementation. The components still would not be used in production, so the
  drift and the third `ToolStatus` remain.

### Alternative B — Replace the production renderer with `ToolCard`
- **Why considered**: Makes the declarative suite real.
- **Why rejected**: The production renderer carries behavior the card does not
  (streaming diff cache, sticky anchors, selection, cell drag, subagent
  interleaving). Replacing it would be a rewrite with no architectural gain.

## Links

- [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md)
- [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md)
- [ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md)
