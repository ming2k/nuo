---
id: ADR-0110
title: "Running-State Interaction Topology, Action Permissibility Discipline, and Visual Demarcation Purity"
status: accepted
date: 2026-12-05
scope: presentation/nuo-ui-primitives, presentation/nuo-view, ui/declarative, interaction/affordance, tui/lifecycle
superseded_by: null
negative_knowledge: true
---

# 0110. Running-State Interaction Topology, Action Permissibility Discipline, and Visual Demarcation Purity

- Status: Accepted
- Date: 2026-12-05
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation, Interaction Design, Harness Runtime, Tooling Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0008](0008-single-tool-contract.md), [ADR-0026](0026-streamed-tool-input-progress.md), [ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md), [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md), [ADR-0076](0076-canonical-unselectable-list-item-topology-and-inert-skip-navigation.md)

---

## Context and Problem Statement

In conventional Web and naive GUI development, in-flight or busy components (such as buttons, cards, or input panels) are frequently subjected to a blanket "dim-and-disable" treatment: when an asynchronous operation begins, the component's opacity is lowered (grayed out) and interaction is completely severed to prevent duplicate submissions.

Importing this naive pattern into Nuo's semantic terminal user interface (TUI) introduces catastrophic architectural and ergonomic failures:

1. **Semantic Inversion and Cognitive Conflict with Terminal Vocabulary**:
   In Nuo's design system ([`docs/reference/tui/visual-language.md`](../reference/tui/visual-language.md), ADR-0076), the dimmed palette token (`theme.dim()`) is strictly reserved for **dead, cancelled, locked, or inert entities**:
   - `ToolStatus::Cancelled => theme.dim()`: An aborted tool call.
   - Locked catalog models in `/models`: Ineligible items bypassed by skip-navigation (`rotate_selectable`).
   - Static non-interactive decorative glyphs.
   Applying `theme.dim()` to an active, executing component inverts this vocabulary: users subconsciously deduce that the task has been aborted, cancelled, or frozen, causing panic during critical operations (e.g. multi-file diff applications or live shell commands).
2. **Terminal Contrast Degradation and Epistemic Blindness**:
   Unlike web browsers with 24-bit alpha blending on high-DPI displays, terminal emulators operate on diverse background luminance profiles (pure dark, light, solarized, transparent). Lowering luminance via `theme.dim()` or `modal_dim_factor` renders monospace code, paths, and diff hunks borderline unreadable. Dimming an active execution obscures the very operational evidence the user needs to inspect.
3. **Deprivation of Live Inspection Rights (Read-vs-Write Conflation)**:
   In-flight work is not an inert dead-end. While a command runs or a file diff streams, the user must retain full operational inspection capabilities:
   - Expanding and collapsing cards to observe real-time output streams (`push_tool_stream`).
   - Scrolling viewport buffers and inspecting live logs.
   - Selecting text ranges and copying stdout diagnostics.
   Treating a running component as generically "non-operable / disabled" conflates *write mutation* (which must be guarded) with *read inspection* (which must remain unconditionally open).
4. **Destruction of Composer Steering & Queue Pipelines**:
   During an ongoing turn (`is_responding == true`), the live composer is intentionally kept actionable: users can draft follow-up questions (`Enter queue follow-up`) or issue urgent steering prompts (`Alt+S steer now`). Disabling or dimming the input surface when the agent is running would destroy Nuo's foundational concurrent turn-staging architecture.
5. **Loss of Abort Prominence**:
   When an operation hangs or runs wild, the user's primary requirement is *interruption*. Dimming the component downplays its presence precisely when the cancellation affordance (<kbd>Esc</kbd> / Abort) must be most prominent.

A decisive architectural standard is required to formalize the running-state interaction topology, action permissibility, and visual presentation across all layers of the Nuo UI presentation substrate.

---

## Decision Drivers

- **Purity of Visual Channels**: Preserve the orthogonal separation established in `nuo-view`: Hue denotes lifecycle status, Luminance denotes disclosure weight, and Tint denotes transient pointer/keyboard affordance.
- **Strict Tri-Partition of Action Permissibility**: Disentangle Mutating actions, Inspecting actions, and Abort actions.
- **Terminal Readability Floor**: In-flight parameters, live diffs, and stream logs must retain 100% foreground contrast.
- **Single Breathing Anchor**: Adhere strictly to ADR-0008 and ADR-0026: the activity bar remains the single animated breathing anchor. Components do not introduce visual pollution with heavy pulsing animations.
- **Clean Break from Legacy Ad-hoc Flags**: Provide a domain-agnostic `ActionStatus` primitive in `nuo-ui-primitives` that replaces boolean soup (`busy`, `disabled`, `running`) with an auditable state machine.

---

## Architectural Decisions

### 1. The Action Permissibility Tri-Partition (`[INV-TUI-RUNNING-02]`)

Every interactive component, card, and action control must partition incoming user events according to a strict three-dimensional permissibility matrix when in the `Running` state:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Component In Running State                      │
├───────────────────┬────────────────────────────────┬───────────────────┤
│  Mutating Action  │      Observational Action      │   Control Action  │
│  [Guarded Inert]  │      [Unconditionally Open]    │   [High Salience] │
├───────────────────┼────────────────────────────────┼───────────────────┤
│ • Re-submitting   │ • Toggle expand / collapse     │ • Keyboard <Esc>  │
│ • Re-triggering   │ • Scroll log / diff viewport   │ • Cancel / Abort  │
│ • Editing inputs  │ • Text selection & clipboard   │ • Steer Now       │
│ • Param mutation  │ • Header / target inspection   │ • Priority Pause  │
└───────────────────┴────────────────────────────────┴───────────────────┘
```

1. **Mutating Actions (Guarded)**:
   Any user message that would re-trigger execution or mutate parameters while a task is in flight must be intercepted and safely consumed as a no-op at the `update(msg)` layer.
2. **Observational Actions (Unconditionally Open)**:
   Inspection capabilities must remain fully active. Component state transitions (`ToggleExpand`, `ScrollUp`, `ScrollDown`, `SelectRange`) process normally.
3. **Control Actions (High Salience)**:
   Cancellation, interruption, and steering affordances must be elevated in salience rather than dimmed.

### 2. Strict Lifecycle-Tone Demarcation (`[INV-TUI-RUNNING-01]`)

The tone tokens of `Theme` must adhere to strict semantic allocation:

| Lifecycle State | Tone Channel | Style Token | Bold Modifier | Permitted Usage |
| :--- | :--- | :--- | :--- | :--- |
| **Idle / Actionable** | Default Foreground | `theme.fg()` / `theme.text` | Yes (when focused) | Ready for input |
| **Running / In-Flight** | **Active Accent Hue** | `theme.info()` / `theme.warning` | **Yes (always bold)** | **Work actively in-flight** |
| **Completed / Ok** | Semantic Success | `theme.system_fg` / weight | Weight-only | Terminal success |
| **Failed / Error** | Semantic Error | `theme.err()` / `theme.error_fg` | Yes | Non-zero exit / failure |
| **Cancelled / Inert** | **Subdued Dim Tone** | `theme.dim()` | **No (stripped bold)** | **Aborted, dead, or locked** |

**Universal Ban on `theme.dim()` for Active States**:
Under no circumstances may `theme.dim()` or darkened surface masks be applied to an executing, ingesting, or running component. Running components must be painted in an active, energetic hue (`theme.info()` or `theme.warning`), asserting active vitality.

### 3. Canonical Progress and Status Grammar (`[INV-TUI-RUNNING-03]`)

Component headers in the running state must follow the canonical typography specified in [`docs/reference/tui/visual-language.md`](../reference/tui/visual-language.md):
- Active header grammar: `{Lead} {Tool/Action} {Target} · {status_token} ({elapsed})`
- Example:
  - `▼ ⠋ edit_text src/auth.rs · running (1.4s)`
  - `▶ ⠋ Run cargo test · running (3.2s)`
  - `⠋ Connecting api.openai.com (2s)`
- Upon completion, the transient status token transitions cleanly into a static, frozen fact badge:
  - `▼ ✓ edit_text src/auth.rs (+12 -3) (340ms)`
  - `▶ ✓ Run cargo test (2.8s)`

### 4. Single Breathing Anchor Invariance (`[INV-TUI-RUNNING-04]`)

Per ADR-0008, the transient `ActivityBar` remains the single global breathing anchor of the TUI. Leaf components in the running state must not introduce animated full-border sweeps, pulsating backgrounds, or competing multi-glyph animations. A running component is limited to:
- A single-character discrete spinner frame (`⠋`, `⠙`, `⠹`...) in the header or button slot.
- Monotonically increasing structured text duration indicators (`(1.2s)`).

### 5. Domain-Agnostic Action Primitive in `nuo-ui-primitives`

The pure UI substrate (`crates/nuo-ui-primitives`) provides the canonical `ActionStatus` primitive to govern interactive action controls across all dialogs, cards, and toolbars:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ActionStatus {
    #[default]
    Idle,
    Running {
        spinner_frame: usize,
        elapsed_secs: Option<u64>,
    },
    Disabled {
        reason: Option<String>,
    },
}
```

`ActionStatus` provides mathematical guarantees:
- `is_actionable()` returns `true` **strictly** for `ActionStatus::Idle`.
- `style(theme, is_focused, border)` enforces `theme.info()` + `Modifier::BOLD` for `Running`, and `theme.dim()` + stripped bold for `Disabled`.
- `format_label(base, running)` composes liveness glyphs deterministically.

---

## Normative Invariants

- **`[INV-TUI-RUNNING-01] Strict Lifecycle-Tone Separation`**:
  `theme.dim()` MUST NEVER be rendered on any component, card, button, or step that is actively running, executing, or in flight. Running states MUST be rendered in an active accent hue (`theme.info()` or `theme.warning`) with bold weight. `theme.dim()` is reserved exclusively for cancelled, aborted, dead, or inert locked items.
- **`[INV-TUI-RUNNING-02] Action Permissibility Tri-Partition`**:
  Components in the running state MUST guard against re-entrant mutating submissions (activation messages are consumed as no-ops), while unconditionally preserving read/inspect capabilities (expand, collapse, scroll, selection, copy) and promoting cancellation/interrupt affordances.
- **`[INV-TUI-RUNNING-03] Progress Grammar & Frozen Fact Transition`**:
  Running status labels and elapsed timers MUST adhere to the canonical text join grammar (`running (1.2s)`), and MUST freeze into immutable duration facts upon reaching a terminal state.
- **`[INV-TUI-RUNNING-04] Single Breathing Anchor Invariance`**:
  Local components in running state MUST NOT introduce pulsating, full-container flashing, or multi-element breathing animations. In-flight indication is limited to structured status tokens and at most a single-character discrete spinner frame.

---

## Consequences

- **Positive**:
  - **Zero Cognitive Misleading**: Users never mistake an in-flight operation for an aborted or dead step.
  - **Uncompromised Operational Observability**: Developers can monitor, expand, and scroll long-running command streams and diffs without artificial visual or input barriers.
  - **Bulletproof Re-entrancy Protection**: Double-submits and race conditions are mathematically eliminated at the TEA `update` boundary via `ActionStatus::is_actionable()`.
  - **Terminal Ergonomic Parity**: High-contrast, clean visual presentation across all terminal color profiles.
- **Negative & Trade-offs**:
  - Components with action buttons must declare an `ActionStatus` rather than relying on loose string vectors.

---

## Rejected Alternatives & Negative Knowledge

### 1. Dimmed Busy Mask Overlays
- *Failure Mode*: Rendering a semi-transparent or darkened mask over an executing card (borrowed from Web CSS `opacity: 0.5`) obscures stdout logs and diff lines, rendering monitoring impossible on terminals with low dynamic range.

### 2. Disabling the Composer During Execution
- *Failure Mode*: Treating the entire application as "locked" while an agent turn executes prevents the user from typing ahead, queuing follow-up prompts, or steering agent actions via `Alt+S`.

### 3. Toast-on-Busy Re-entry Interception
- *Failure Mode*: Emitting a toast (`"Please wait, action is running"`) on every accidental keypress during execution adds noise to the transcript and distracts the user. The action must be silently and safely guarded as an inert no-op.

---

## Links

- Related ADRs: [ADR-0008](0008-single-tool-contract.md), [ADR-0026](0026-streamed-tool-input-progress.md), [ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md), [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md), [ADR-0076](0076-canonical-unselectable-list-item-topology-and-inert-skip-navigation.md)
- Reference Docs: [`docs/reference/tui/visual-language.md`](../reference/tui/visual-language.md), [`docs/reference/tui/interaction.md`](../reference/tui/interaction.md)
