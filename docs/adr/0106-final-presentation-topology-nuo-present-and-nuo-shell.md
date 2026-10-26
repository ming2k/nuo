---
id: ADR-0106
title: "Final Presentation Topology: nuo-present as the Sole Zero-IO Substrate and nuo::shell as the Interactive Runtime"
status: accepted
date: 2026-12-02
scope: presentation/nuo-present, presentation/nuo-ui, host/nuo, workspace/topology, architecture/enforcement
superseded_by: null
negative_knowledge: true
---

# 0106. Final Presentation Topology: `nuo-present` as the Sole Zero-IO Substrate and `nuo::shell` as the Interactive Runtime

- Status: Accepted
- Date: 2026-12-02
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md), [ADR-0105](0105-presentation-linkage-mechanism-and-dependency-firewall.md)

---

## Context and Problem Statement

[ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md)
named `crates/nuo-ui` as the pure presentation substrate, but that crate still carried
the (pre-split) interactive shell at the time of ratification, so the name described an
intent rather than the shipped graph. [ADR-0105](0105-presentation-linkage-mechanism-and-dependency-firewall.md)
then staged the fix: it created `crates/nuo-present` as the *actually* pure zero-IO
substrate, hard-gated it under R3, and kept `nuo-ui`'s remaining IO debt **advisory**
under `--strict-ui` "until the shell's physical relocation completes."

That relocation is an obligation left open by both ADRs, and leaving it open has a
concrete cost: while `nuo-ui` exists as a library crate that declares `crossterm`,
`arboard`, `libc`, `dirs`, `nuo-host`, and `tokio-tungstenite`, every reader of the
workspace must remember that the crate named after the purity contract is the one
violating it. A staging flag (`--strict-ui` advisory) is a correct *interim* mechanism,
but it is not a resting state: it makes R3 a convention for the crate the invariant is
named after.

The extraction program's end state was also underspecified. ADR-0103 §2 internalized
client transport into `nuo::transport`; ADR-0103 §3 described the interactive shell as
`nuo::shell`. Neither said what happens to `nuo-ui` once the shell leaves it, and
ADR-0105 §4 predicted that the presentation cluster
(`{model, text_layout, markdown_table, notice, message_body, tools, components, layout,
disclosure, …}`) could only cross into `nuo-present` as one mutually-recursive batch.

## Decision Drivers

- **The name must match the graph.** A crate is a claim about its dependency set;
  `nuo-ui` was a claim the graph contradicted. Mechanisms over exhortations
  (AGENTS.md) means the invariant is the manifest, not the docstring.
- **Staging must terminate.** A permanent advisory gate is a toothless gate. R3 must
  end up enforced unconditionally on the crate that actually holds the substrate.
- **The lean-link invariant is a build fact, not a convention.** `[INV-ARCH-CLI-LEAN-LINK]`
  must hold because the terminal stack is behind a feature, not because a reviewer
  noticed a stray `use`.
- **One presentation library, not two.** A workspace with two presentation crates
  (one pure, one impure, sharing an import surface) is exactly the "junk drawer"
  topology ADR-0089 retired for clients.

## Architectural Decisions

### 1. `crates/nuo-present` is the sole presentation library

All pure presentation code lives in `crates/nuo-present`:

- **Semantic data model** — the parsed document, the rendered-cell layout map, and
  selection state (`model`).
- **Text engine** — `text_layout` (wrapping, markup elision, selection spans, the
  content-addressed `BlockWrapCache`) and `markdown_table`.
- **Presentation registry** — the per-tool presenter table (`tools`), the disclosure
  policy port (`disclosure_config`), and the disclosure state/colour projection
  (`disclosure`).
- **Component tree** — `components` (`footer`, `keycap`, `inline_layout`, `path`,
  `modal`, `list`, `options`, `row`, `scroll`, `selectable_body`, `notice`, `toast`,
  `tooltip`, `dropdown`, `detail`, `meta_strip`, `popover`, `which_key`).
- **Arrangement** — `layout` (`Strategy`, `Stream`, `HeightCache`, `VirtualLayoutIndex`,
  the turn-band strategy) and `message_body`.

Each renders `f(Props, State, Theme) -> Frame` in pure memory and declares **no**
terminal driver, network, subprocess, socket, clipboard or filesystem-layout dependency
(`[INV-ARCH-UI-ZERO-IO]`).

### 2. The presentation bundle crosses as one batch, via injected projection hooks

The cluster is mutually recursive (`model` ⇄ `tools` ⇄ `components` ⇄ `layout` ⇄
`text_layout` ⇄ `notice` ⇄ `message_body`), so it crossed as a single unit. Crossing it
required lifting the last three references the cluster held *outward* into the shell:

- **`components::which_key` no longer names `keymap::scene_namespace`.** The card takes
  `&[WhichKeyItem]` (`chord`, `label`, `active`); the shell's keymap formats the verb
  table into items. A verb can still never be dispatchable without being advertised —
  the formatter is adjacent to the table — but the component no longer reaches into the
  key namespace.
- **`tools` no longer names the shell's config type.** `disclosure_config` is a pure port
  (`DisclosureSource` + `tool_expanded`); it crossed with the cluster, so the tool
  registry's default-expand resolution is internal to the substrate.
- **`layout` no longer names `disclosure::renderers::RenderCtx`.** The per-message step
  dispatcher is a function pointer (`StepDrawFn`) carried on `Stream`; the renderer
  (shell side) installs it. `StickyAnchor` and `HeightCache` moved into `layout` so the
  scroll/height accounting lives with the arrangement code that consumes it, and
  `BlockWrapCache` moved into `text_layout` so the wrap cache lives with the wrapper.

None of these is a wrapper or an indirection for its own sake: each removes a
*downward* edge (substrate → shell) that would have made the batch unmovable.

### 3. The interactive runtime is `nuo::shell`, gated behind `interactive`

The interactive application runtime (terminal lifecycle, crossterm event loop,
`App` state, input router, view tree, overlays, chrome, clipboard, and the client
RPC bridge) is an **internal module of the `nuo` host binary**: `nuo::shell`. It owns
every IO dependency the presentation layer must not have — `crossterm`, `nuotc`,
`arboard`, `libc`, `dirs`, `nuo-host`, `tokio-tungstenite` — and it is compiled only
under the default-on `interactive` feature.

`nuo::transport` (ADR-0103 §2) is the data source; `nuo-present` is the pure
presentation sink; `nuo::shell` is the coordinator that links them. Non-interactive
subcommands (`nuo thread`, `nuo status`, `nuo server …`, `nuo run --headless`) never
link the shell, so `cargo build -p nuo --no-default-features` produces a binary with
no raw-mode driver — `[INV-ARCH-CLI-LEAN-LINK]` is a build-time fact.

### 4. `nuo-ui` and the intermediate `nuo-shell` crate are deleted

- `crates/nuo-ui` is **removed** from the workspace. It was a transitional name whose
  dependency set contradicted its contract; the substrate it was supposed to be lives
  in `nuo-present`, and the shell it actually was lives in `nuo::shell`.
- The intermediate `crates/nuo-shell` crate minted during the relocation is **not**
  retained. ADR-0103 §3 ratifies the shell as `nuo::shell`, an internal module; a
  one-consumer library crate for it would re-introduce the "second presentation
  crate" the single-binary posture rejects.
- The root `Cargo.toml` lists neither. No compatibility alias is provided: per ADR-0089
  there is no multi-crate public SDK to preserve.

### 5. R3 is enforced unconditionally; retirement is itself a gate

`scripts/check_presentation_firewall.py` now:

- **R1** — `nuo-server` must not depend on the terminal substrate or any presentation
  substrate.
- **R2** — only the presentation layer may link the terminal substrate; only the `nuo`
  host binary may link the presentation library. **Any** crate depending on a retired
  presentation crate (`nuo-ui`, `nuo-shell`, `nuo-tui`) fails, whatever its layer.
- **R3** — `crates/nuo-present` must declare no forbidden IO dependency. Enforced
  **unconditionally**; the `--strict-ui` advisory stage is over. A resurrected
  `crates/nuo-ui` fails outright.

CI runs the checker with `--strict-ui` and verifies the lean-link exclusion
(`cargo tree -p nuo --no-default-features`).

## Concentric Topology (shipped)

```text
┌──────────────────────────────────────────────────────────────┐
│  Layer 4: Host Binary (`nuo`)                                │
│  - Unified single executable                                 │
│  - Internal Transport & Supervisor (`nuo::transport`)        │
│  - Interactive Shell (`nuo::shell`, `interactive` feature)   │
├──────────────────────────────────────────────────────────────┤
│  Layer 3: Presentation & Capabilities                        │
│  - Pure Zero-IO Substrate: `crates/nuo-present`               │
│  - Terminal Canvas Engine: `nuotc` (ADR-0003)                │
│  - Specialized Tools: `tools/*`                              │
│  - Model Providers: `providers/*`                            │
├──────────────────────────────────────────────────────────────┤
│  Layer 2: Core Runtime Substrate                             │
├──────────────────────────────────────────────────────────────┤
│  Layer 1: Pure Contracts & Codecs                            │
└──────────────────────────────────────────────────────────────┘
```

## Consequences

- **Positive**: the crate named after the purity invariant *is* the pure crate; R3 is a
  hard gate with no advisory carve-out; the lean-link promise is a build fact; the
  workspace has exactly one presentation library.
- **Positive**: the shell's IO dependencies are ordinary module dependencies of the one
  binary that owns them, rather than a library's public manifest.
- **Negative & Trade-offs**: the presentation substrate and the shell can no longer be
  versioned or released independently — they are not independently consumable, which is
  the intended single-binary posture. `nuo`'s own manifest is larger because it now
  declares the terminal stack directly, so the `interactive` feature list is the single
  place to audit what the shell links.
- **Neutral**: `nuo::shell` is a public module of the `nuo` library because the binary
  target consumes it; it is not a supported external API and carries no stability
  promise.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Keep `nuo-ui` as a re-export shim over `nuo-present` (Rejected)

- **Why considered**: zero churn for any out-of-tree consumer of `nuo_ui::*`.
- **Why rejected**: there are no consumers to preserve — ADR-0089 retired the client SDK
  posture and no external crate depends on `nuo-ui`. A shim would keep a second
  presentation crate in the graph purely to spell a retired name, which is the
  "honest lineage" failure ADR-0105 itself diagnosed.

### Alternative B: Retain `crates/nuo-shell` as the interactive runtime crate (Rejected)

- **Why considered**: it isolates the shell from the binary's lib target and keeps the
  optional-feature list in `nuo` smaller.
- **Why rejected**: ADR-0103 §3 ratifies the shell as `nuo::shell`. A one-consumer
  library crate is an extra workspace member and a second presentation-crate name for
  no dependency benefit; the `interactive` feature on `nuo` already gives the build-time
  exclusion the lean-link invariant needs. Keeping it would also leave `nuo-ui` and
  `nuo-shell` as two retired names to police instead of one invariant on one substrate.

### Alternative C: Leave the `--strict-ui` advisory gate in place (Rejected)

- **Why considered**: it "works today" and avoids a CI change.
- **Why rejected**: an advisory gate on the crate the invariant is named after is a
  toothless declaration. The staging existed to make the migration mergeable, not to be
  the destination. Once the substrate is in place, the gate must be unconditional or R3
  is a comment.

## Links

- Related ADRs:
  [ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md),
  [ADR-0105](0105-presentation-linkage-mechanism-and-dependency-firewall.md),
  [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md),
  [ADR-0089](0089-nuo-client-retirement-and-four-way-responsibility-realignment.md),
  [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md),
  [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md)
- Gate: `scripts/check_presentation_firewall.py --strict-ui`
