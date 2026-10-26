---
id: ADR-0105
title: "Presentation Linkage Mechanism and the Dependency Firewall"
status: accepted
date: 2026-12-01
scope: presentation/nuo-ui, host/nuo, workspace/topology, architecture/enforcement
superseded_by: null
negative_knowledge: true
---

# 0105. Presentation Linkage Mechanism and the Dependency Firewall

- Status: Accepted
- Date: 2026-12-01
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md)
- Amended by: [ADR-0106](0106-final-presentation-topology-nuo-present-and-nuo-shell.md) — the
  extraction completed: `crates/nuo-present` is the sole presentation library, the shell is
  `nuo::shell`, `crates/nuo-ui` is deleted, and R3 is enforced unconditionally (the
  `--strict-ui` advisory stage below is closed).

---

## Context and Problem Statement

[ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md)
ratified the extraction of a pure `crates/nuo-ui` substrate and the relocation of
the interactive shell and client transport into the `nuo` host binary. Two gaps
left its invariants unenforceable and, in one case, unsatisfiable as written:

1. **`[INV-ARCH-CLI-LEAN-LINK]` is unsatisfiable as a *build-time* claim without a
   feature gate.** The invariant demands that non-interactive subcommands
   (`nuo thread`, `nuo status`, `nuo run --headless`) execute "without … linking
   terminal raw-mode drivers". But a Rust binary links *every* dependency of its
   crate unconditionally: if the interactive shell is a module of the single
   `nuo` binary crate, then `nuo status` still links `crossterm`/`nuotc`, because
   `main` retains an `Interactive` dispatch path that keeps them reachable. The
   only way to make "lean" true is to make the terminal stack an **optional
   feature** of `nuo`.

2. **The invariants have no mechanical enforcement.** `[INV-ARCH-UI-ZERO-IO]` and
   "Server Purity" are stated as red-lines in AGENTS.md but nothing in CI tests
   the dependency graph. A single careless `use crossterm` in a library crate
   would silently re-pollute the headless server.

3. **The extraction is incomplete, and the name currently lies.** `crates/nuo-ui`
   has been renamed but not decomposed: it is still the 133k-line interactive
   monolith, declaring `crossterm`, `arboard`, `libc`, `dirs`, `nuo-host`,
   `tokio-tungstenite`, … A measurement of the nominally-pure modules confirms the
   entanglement is structural, not cosmetic:

   | module | leaks into shell state |
   |---|---|
   | `theme.rs` | `crate::config` |
   | `design.rs` | `crate::event_loop` |
   | `layout/` | `crate::model` (×11), `crate::time`, `crate::disclosure` |
   | `text_layout.rs` | `crate::model` |
   | `declarative/` | `crossterm`, `crate::terminal`, `crate::overlays`, `crate::tools` |

   The presentation tree reaches *into* the shell rather than receiving its
   `Props`. Therefore the split cannot be a mechanical `git mv`; it is a
   decoupling program, and a big-bang would break the tree.

## Decision Drivers

- **Mechanisms over exhortations** (AGENTS.md): an invariant without a gate is a
  comment. The dependency direction must be checked by CI.
- **Domain precision**: `nuo-ui` must mean *pure zero-IO projection*, and the
  binary must be able to build *without* the terminal stack.
- **Honest lineage**: the ADR must describe the shipped graph, not the intended
  one.

## Architectural Decisions

### 1. The terminal stack is an optional, default-on feature of the `nuo` binary

Whether the interactive shell lives as a module (`nuo::shell`, per ADR-0103) or
behind a thin boundary, the terminal/presentation stack (`crossterm`, `nuotc`,
clipboard, raw-mode) MUST be reachable only through an **optional Cargo feature**
of `nuo` (working name `interactive`, `default = ["interactive"]`). A build with
`--no-default-features` MUST produce a `nuo` that links no raw-mode driver, so
`[INV-ARCH-CLI-LEAN-LINK]` becomes a *build-time* fact.

> **Landed.** `nuo` gates its `nuo-ui` dependency behind the default-on
> `interactive` feature; `Mode::Interactive` carries the raw interactive
> arguments parsed at dispatch, and the interactive/dev entry points are
> `cfg`-gated. CI verifies `cargo tree -p nuo --no-default-features` excludes
> `nuotc`/`crossterm`/`nuo-ui`.

### 2. The dependency firewall is a normative, gated invariant

`[INV-ARCH-FIREWALL-01] Presentation Dependency Firewall`: the workspace
dependency graph MUST satisfy, and CI MUST verify via
the architecture test `test_inv_arch_firewall_and_topology` (ADR-0111):

- **R1 — Server purity.** `nuo-server` MUST NOT depend on the terminal/presentation
  substrate (`nuotc`, `crossterm`, `nuo-ui`, `nuo-shell`, `nuo-tui`).
- **R2 — Client-server anti-inversion.** Only the `nuo` host binary may depend on
  the presentation crates; only the presentation layer may link the terminal
  substrate. No library crate may.
- **R3 — Zero-IO UI substrate** (`[INV-ARCH-UI-ZERO-IO]`). `crates/nuo-ui` MUST NOT
  declare network, subprocess-supervision, OS-socket, clipboard, or terminal-raw
  dependencies. Only **shipped** dependencies are judged: `[dev-dependencies]` are
  test-only and excluded, so a test-time composition root does not violate the rule.

R1 and R2 are enforced unconditionally. R3 is the extraction's acceptance gate:
`crates/nuo-present` (the first extracted pure substrate) is hard-gated under R3, while
the remaining `nuo-ui` shell debt stays advisory under `--strict-ui` until the shell's
physical relocation completes.

### 3. The extraction is a decoupling program with acceptance gates

The ADR-0103 milestone roadmap is restated with mechanical gates:

- **M2 — Transport.** Client transport lives in `nuo::transport`; no crate above
  Layer 2 owns sockets. *Gate:* `nuo-ui` declares no `tokio-tungstenite`/`netune`.
- **M3 — Pure substrate.** Presentation modules receive `Props` instead of
  reaching into shell state (`crate::model`/`crate::config`/`crate::event_loop`/
  `crate::terminal`). *Gate:* `--strict-ui` green.
- **M4 — Shell consolidation & retirement.** Interactive runtime lives in the
  `nuo` host binary behind the `interactive` feature; the shell is deleted from
  `nuo-ui`; `nuo-tui`/`nuox` remain retired. *Gate:* lean build
  (`--no-default-features`) links no raw-mode driver; CI runs `--strict-ui`.

### 4. Dependency closure: what may still move, and in what order

A module may cross into `crates/nuo-present` only when every `crate::…` it names is
already there (or moves with it). A closure analysis of the current graph gives the exact
remaining order:

- **Extracted (18)**: `chord` (the pure chord model), `key`, `keyvocab`, `syntax`, `time`,
  `phase`, `fuzzy`, `design`, `footer_stack`, `trust_gate`, `composer_mode`,
  `composer_attachments`, `providers`, `theme`, `primitives`, `elevation`, `empty_state`.
- **Pure but not yet movable — a closed cycle**: `theme` ⇄ `primitives` ⇄ `elevation`.
  They must move as one batch, which additionally requires `components` and `keymap`
  (see below).
- **The gate — `keymap` — is resolved.** The pure chord model (`KeyCode`, `KeyModifiers`,
  `Key`) now lives in `nuo-present::chord` and `keymap` re-exports it; the terminal driver
  survives only in `keymap::key_from_event` (and its test inverse
  `input::keystroke::event_from_chord`). `keymap` itself is now blocked solely on
  `surfaces`, and `empty_state` solely on `theme`.
- **`components/`** is free of shell state and of the driver, and its keycap/footer
  constructors take rendered tokens rather than `Key` values. It still names sibling
  substrate modules (`model`, `text_layout`, `primitives`, `theme`, `disclosure`,
  `elevation`, `footer`/`keycap`/… within its own tree) that have not moved.
- **The remaining substrate is one mutually-recursive cluster**:
  `{theme, primitives, elevation, components, model, text_layout, notice, markdown_table,
  disclosure, tools, layout}` — each member names another, so they can only cross
  `nuo-present` together, and the cluster additionally names three shell modules:
  `keymap` (via `components.which_key`), `render` (via `model`'s tests, `disclosure`,
  `message_body`) and `config` (via `tools`). `primitives` is a live re-export facade over
  `components::{footer, inline_layout, path}` — overlays import `FooterHint` and
  `render_modal_footer` through it — so `components` cannot be separated from the batch.
  Extracting the cluster therefore requires the `keymap::Key` unification plus lifting the
  `render`/`config` references.
- **Shell-only, moves to `nuo` in M4** (never to `nuo-present`): `app`, `event_loop`,
  `render`, `input`, `surfaces`, `views`, `overlays`, `session`, `modal_keys`, `composer*`,
  `chrome`, `transcript`, `step_interaction`, `config`, `terminal`, `client`, `clipboard*`,
  `headless`, `mouse_mode`, `browser`, `cli`, `runner`, `pre_attach`.

## Consequences
- **Positive**: The two presentation invariants become machine-checked; the
  "lean CLI" promise becomes a build-time fact; the migration debt is named and
  measurable rather than hidden behind a renamed crate.
- **Negative & Trade-offs**: R3 is advisory until M4, so `nuo-ui` may still carry
  IO deps in the interim — a deliberate, documented staging rather than a
  toothless declaration. The `interactive` feature adds a build matrix
  (`--no-default-features` must be kept green).

## Rejected Alternatives & Negative Knowledge

### Alternative A: Big-bang extraction in one change (Rejected)

- **Why considered**: fastest path to the target topology.
- **Why rejected**: the presentation tree is entangled with shell state across
  133k lines (see the table above); a bulk move cannot compile without the
  decoupling, and would leave the workspace broken. Incremental, gated milestones
  are the only safe path.

### Alternative B: Keep the monolith named `nuo-ui` (Rejected)

- **Why rejected**: a crate named after a purity contract it violates is a lie in
  the dependency graph and a trap for every future contributor.

### Alternative C: Enforce a generic "no upward dependency" layer rule (Rejected)

- **Why rejected**: the sanctioned composition root (`nuo-server`) links
  `providers/*` and `tools/*` by design (ADR-0092..0099, ADR-0012); a generic
  upward rule would flag legitimate edges. The firewall checks the *specific*
  red-lines (server purity, anti-inversion, UI zero-IO), not a naive layer order.

## Links

- Related ADRs:
  [ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md),
  [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md),
  [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md),
  [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md),
  [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md)
