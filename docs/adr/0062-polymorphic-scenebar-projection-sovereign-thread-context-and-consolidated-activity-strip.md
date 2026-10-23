---
id: ADR-0062
title: "Polymorphic SceneBar Projection, Sovereign ThreadContextBar, StickyAnchor Semantics, and Consolidated ActivityStrip Topology"
status: accepted
date: 2026-11-02
scope: tui/nuo-tui, presentation/chrome, interaction/tabs, architecture/spatial
superseded_by: null
negative_knowledge: true
---

# 0062. Polymorphic SceneBar Projection, Sovereign ThreadContextBar, StickyAnchor Semantics, and Consolidated ActivityStrip Topology

- Status: Accepted
- Date: 2026-11-02
- Deciders: Nuo Architecture Working Group
- Consulted: TUI Presentation, Interaction Design, Product Architecture
- Informed: System Architects, Release Engineering
- Amends: [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md)

> **Amendment note (2026-10-23)**: §2a and `[INV-HEAD-07]` were added in place. Tier 3's posture vocabulary (`auto` / `open`, drawn only on deviation) and the workspace path's right anchoring are revised here; the tier's domain, height, and elevation decisions above are unchanged. `[INV-HINT-03]` of [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md) (run-mode flags on the right edge) is superseded for the *form* of those flags by `[INV-HEAD-07]`, while its governing intent — one predictable location that never migrates with the scene — survives unchanged.

---

## Context and Problem Statement

[ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md) decomposed the Nuo TUI header into three sovereign tiers to solve horizontal budget collisions. While this eliminated text collisions on narrow terminals, visual and architectural anomalies emerged under production usage:

1. **Rigid 3-Row Allocation for Non-Thread Scenes**:
   ADR-0060 hardcoded `PAGE_HEADER_ROWS = 3` for every scene. However, runtime role (`[ROLE]`), bound workspace (`~/projects/...`), and safety posture tokens (`auto`, `open`) are strictly **Thread Agent** domain properties. When viewing singleton non-thread scenes (`Dashboard`, `Settings`), Row 2 was either forced to render disconnected thread metadata from an underlying background session or leave artificial empty ghost space, wasting valuable vertical lines on dense data grids.

2. **Cognitive Inversion of Task Identity vs. Environment**:
   Under ADR-0060, Row 2 rendered thread governance (Role and Workspace) while Row 3 rendered the actual task title and scene breadcrumb stack. This forced the human user to skip over secondary environmental parameters before reading the primary semantic question: *"What task is this viewport executing?"*

3. **Ontological Flaw in Header Naming**:
   The header tiers were conflated: Row 1 was Client-owned, but Row 2 was named `Thread Governance` and Row 3 was named `Scene Head`. In reality, every active Tab projects an instance of `Scene` (polymorphic across `Thread`, `Dashboard`, and `Settings`). The primary identifier row belongs universally to the active Scene, not exclusively to threads.

4. **Misleading "Step" Nomenclature for Scrolled Context Anchors**:
   The sticky summary pinned below the header during transcript scroll was termed `StickyStepBar`. In reality, this bar is not a workflow progress bar; it serves as a **contextual anchor and collapse handle** when expanded cards with extensive content (such as `search_text` match listings, long diffs, or execution logs) scroll past the top edge.

5. **Footer Chrome Bloat and Composer-Adjacent Layout Jitter**:
   Above the user input box (`Composer`), the system could stack up to four independent single-row bars simultaneously: `DurabilityBanner` (rare storage degraded warning), `QueueBar` (staged follow-ups), `TasksBar` (background workers), and `ActivityBar` (agent execution phase). This created vertical claustrophobia, upward displacement of the input box, and distracting layout jitter during active agent turns.

---

## Decision Drivers

- **Domain-Aligned Polymorphism**: Align chrome tiers strictly with entity ownership: Client owns `TabBar`; active Scene owns `SceneBar`; Thread-specific governance owns `ThreadContextBar`.
- **Dynamic Vertical Budgeting**: Non-thread scenes (`Dashboard`, `Settings`) consume exactly 2 top rows; thread-backed scenes expand to 3 rows only when governance context exists.
- **Top-Down Cognitive Hierarchy**: Lead with high-level task identity and scene stack, followed secondarily by execution parameters and security constraints.
- **Strict Bounding of Footer Stack**: Eradicate independent low-frequency banners from the layout stack and enforce a hard limit of at most 1 single-row activity strip above the composer.
- **Semantic Precision**: Standardize on `StickyAnchor` for scrolled disclosure context preservation.

---

## Considered Options

- **Option 1 (Chosen)**:
  - **Top Tier 1 (`TabBar`)**: Client-global multi-tab projection with right-anchored `C-x menu`.
  - **Top Tier 2 (`SceneBar`)**: Universal polymorphic scene bar. Left: Scene identity and task title (`THREAD <uuid> <title>` or `DASHBOARD <summary>`). Right: Scene navigation stack (`‹ thread › subagent(2)`).
  - **Top Tier 3 (`ThreadContextBar`)**: Sovereign thread-only environment band. Left: `[ROLE]` + posture tokens (`auto`, `open`). Right: workspace path. Present only on thread viewports (non-thread scenes collapse to 0 rows).
  - **Viewport Anchor (`StickyAnchor`)**: Dedicated context anchor and collapse handle for scrolled disclosure cards.
  - **Bottom Activity (`ActivityStrip`)**: Consolidated single-row strip bounded to at most 1 line above `Composer`, aggregating staged follow-ups, background tasks, and live execution status. `DurabilityBanner` retired as a layout row and redirected to transient alert modals/status indicators.
- **Option 2 (Static 3-Row with Thread Governance on Row 2)**:
  Maintain ADR-0060 as written, forcing non-thread scenes to display dummy fillers.
- **Option 3 (Collapse Everything into Floating HUDs)**:
  Eliminate fixed rows entirely in favor of floating window overlays.

---

## Decision Outcome

Chosen option: **Option 1**.

### 1. Polymorphic Top Chrome Architecture (`[INV-HEAD-05]`, `[INV-SCENE-02]`)

```text
┌────────────────────────────────────────────────────────────────────────┐
│ [Refactor DB]  [Fix Auth]  [dashboard]                        C-x menu │ ◄ Tier 1: TabBar (Client Domain, Always Present)
├────────────────────────────────────────────────────────────────────────┤
│ THREAD b3c4  Fix retry logic in auth loop      ‹ thread › subagent(2) │ ◄ Tier 2: SceneBar (Scene Domain, Polymorphic)
├────────────────────────────────────────────────────────────────────────┤
│ [DEVELOPER]  auto  open                                 ~/projects/nuo │ ◄ Tier 3: ThreadContextBar (Thread Domain Only)
└────────────────────────────────────────────────────────────────────────┘
```

1. **Tier 1: `TabBar` (Client Domain)**:
   - Fixed height: 1 row. Owned by the Client container.
   - Displays clean semantic tab titles (`[INV-TAB-09]`, no ordinal index prefixes).
   - Right-anchored `C-x menu` client affordance.

2. **Tier 2: `SceneBar` (Scene Domain, Polymorphic)**:
   - Fixed height: 1 row. Universal to all scenes.
   - **Left**: Scene Identity + Human Task Title.
     - For `Thread`: `THREAD <id_tail>` (dimmed) + `  ` + authentic chat title (bold heading). If untitled, title slot is clean whitespace without layout shift (`[INV-SCENE-01]`).
     - For `Dashboard`: `DASHBOARD` + cluster overview summary.
     - For `Settings`: `SETTINGS` + category breadcrumb.
   - **Right**: Drill-in navigation stack / scene history trail (`‹ thread › subagent #1 › diff`) with back affordance (`‹`).

3. **Tier 3: `ThreadContextBar` (Thread Domain, Conditional)**:
   - Height: 1 row on Thread viewports; **0 rows** on non-thread viewports (`Dashboard`, `Settings`).
   - Reads **who → how → where** left to right.
   - **Left**: Staffing role badge `[ROLE]` (brand tone), then the safety posture tokens (`auto`, `open`; warning tone) joined by the peer-enumeration gap (`JOIN_ENUMERATE_COLS`) with no separating glyph.
   - **Right**: Tilde-shortened `workspace` path, right-anchored (dimmed) — see §2a.

### 2a. Posture Vocabulary and Right-Anchored Workspace (Amendment, `[INV-HEAD-07]`)

The original `UNATTENDED` / `UNCONFINED` pair was the single largest consumer of Tier 3's horizontal budget (22 columns before any separator), and it collided with the row's own content in two ways:

1. **The workspace path paid for it.** The path's budget was computed as `text_width - badge - flags`, so enabling `/unattended` silently re-truncated a *different* element of the row. Worse, the truncation helper cut from the right, so a narrowed row produced `~/projects…` — ancestors the user already knows retained, leaf (the part that names the project) discarded.
2. **The tokens read as sentences, not state.** `UNCONFINED` is a double negative ("not *not* confined"); both words are long enough to read as prose in a row whose whole job is glanceability, and both were emitted in caps, colliding visually with the `[ROLE]` badge slot and with the footer stack's `[N queued]` count idiom.

The row is therefore reformulated on three rules:

- **Silence is the safe default.** Only deviations are drawn. The default posture — interactive confirmation with workspace-confined file tools — costs zero columns; a normally-behaving session shows nothing but `[ROLE]` and the path. This inverts the polarity of the old flags, which occupied space precisely when nothing was wrong relative to them.
- **Closed vocabulary, lowercase, unbracketed.** `auto` = tool calls are approved without asking (`--unattended` / `/unattended on`). `open` = file tools may leave the workspace (`/confinement off`). Positive naming (what *is* the case) replaces the negated `unconfined`. Lowercase keeps the token from competing with the `[ROLE]` badge, and the bare form keeps it from reading as a count. Long-form explanations belong to the slash-command catalog (`BuiltinCmd::SPECS` in `nuo-server`, the single source of truth that `/unattended on|off` and `/confinement on|off` project from); permanent chrome carries self-describing tokens only (`[INV-TUI-12]`).
- **The shrinkable element is right-anchored.** The workspace path moves to the right edge because it is the only element on the row that may shrink. Anchoring it there makes the governance cluster's screen position invariant: a posture toggle grows the cluster into the inter-cluster gap instead of re-truncating the path, and a narrow terminal degrades by shortening the path rather than by deleting safety information.

Path shortening goes through the shared `PathView` ladder (`Full` → `.../parent/leaf` → fish → basename → middle-truncate) rather than a tail truncation, so the **leaf survives and ancestors are elided**. `CONTEXT_BAR_WORKSPACE_MIN_COLS` (4) is that ladder's own last stop — the bare leaf, which still names the project — so the floor sits where honest information runs out rather than above it. Below that the path is dropped entirely: the bar either names the workspace or stays silent about it, and never draws a fake one.

### 2b. In-Family Micro-Elevation Stratification (`[INV-HEAD-06]`)

To eradicate monolithic flat visual clutter and establish intuitive spatial depth without garish contrast:
- **Tier 1 (`TabBar`)**: Uses `theme.tabbar_bg()` (subtly recessed base, ~2.8% luminance offset in dark mode), acting as a calm window shell that elevates foreground active tab pills.
- **Tier 2 (`SceneBar`)**: Uses `theme.scenebar_bg()` (primary focal plane, ~5.0% luminance offset), creating an intentional optical spotlight on the active task title and drill-down breadcrumbs.
- **Tier 3 (`ThreadContextBar`)**: Uses `theme.contextbar_bg()` (subordinate metadata plane, ~4.0% luminance offset), stepping down gracefully to the main transcript background (`theme.app_bg`).
- **Profile Adaptation**: Under Monochrome and ANSI-16 profiles, all three tiers safely converge onto `theme.element_bg` with structural borders/modifiers.

---

### 3. Viewport Disclosure Anchor: `StickyAnchor` (`[INV-DISCLOSE-01]`)

- When expanded component cards (such as `search_text` results, code diffs, command execution output, or reasoning disclosures) scroll past the top edge of the transcript area, their summary pins directly below the active top chrome as a `StickyAnchor`.
- The `StickyAnchor` conveys the active card identifier, execution status, and item match count, serving as a click handle to fold or navigate back to the component summary.

---

### 4. Consolidated Single-Row Activity Strip: `ActivityStrip` (`[INV-FOOTER-02]`)

The vertical stack above `Composer` is strictly bounded to **at most 1 row**:

```text
┌────────────────────────────────────────────────────────────────────────┐
│ [2 Queued]  ✻ Responding (DeepSeek V3) ...          Esc Esc Interrupt  │ ◄ ActivityStrip (Max 1 row, conditional)
├────────────────────────────────────────────────────────────────────────┤
│ > What is the next step in the refactor?                               │ ◄ Composer (Adaptive)
├────────────────────────────────────────────────────────────────────────┤
│ ↵ Send · ⌥↵ Newline                 deepseek/deepseek-chat 4.2k/64k 🟢 │ ◄ ModelBar (Fixed 1 row)
└────────────────────────────────────────────────────────────────────────┘
```

1. **`ActivityStrip` Consolidation**:
   - Left: Staged follow-up indicators (`[2 Queued]`) and background worker badges (`[1 Task]`).
   - Center/Left: Live agent execution phase (`✻ Responding (DeepSeek V3)...`, `● Thinking...`, `Awaiting confirmation`).
   - Right: Global cancellation affordance (`Esc Esc Interrupt`).
   - Height is strictly 0 when harness is idle and outbox/tasks are empty.
2. **Durability Banner Demotion**:
   - Storage durability health warnings (`DurabilityBanner`) are formally eradicated as a persistent footer layout slot. Degraded storage states trigger transient system notification alerts and set the connection indicator in `ModelBar` to error posture.

---

## Normative Invariants

- **`[INV-HEAD-05]` Polymorphic Top Topology**: The top chrome MUST organize into Tier 1 `TabBar` and Tier 2 `SceneBar`. Tier 3 `ThreadContextBar` MUST strictly render on thread-backed viewports and collapse to 0 rows on non-thread scenes (`Dashboard`, `Settings`).
- **`[INV-SCENE-02]` Universal SceneBar Contract**: Every `SceneBar` MUST project primary entity identity and task title on the left, and subordinate drill-down scene breadcrumbs on the right.
- **`[INV-DISCLOSE-01]` StickyAnchor Semantics**: Pinned summaries for scrolled cards MUST be identified as `StickyAnchor` (context anchor and collapse handle), eliminating the misleading "step" nomenclature.
- **`[INV-FOOTER-02]` Bounded Pre-Composer ActivityStrip**: The auxiliary chrome space immediately above `Composer` MUST NOT exceed 1 row. Ambient queue items, background tasks, and live activity MUST consolidate into `ActivityStrip`.
- **`[INV-HEAD-06]` In-Family Micro-Elevation Ladder**: The top chrome tiers MUST NOT share a flat identical background fill; they MUST exhibit a discrete, in-family micro-elevation luminance ladder (`tabbar_bg` ~2.8% -> `scenebar_bg` ~5.0% -> `contextbar_bg` ~4.0%) on direct-color capable profiles to establish visual hierarchy without garish contrast.
- **`[INV-HEAD-07]` Silent-by-Default Posture, Right-Anchored Workspace**: The `ThreadContextBar` MUST report safety posture as a closed vocabulary of lowercase, unbracketed tokens (`auto`, `open`) and MUST draw only deviations from the safe default (interactive confirmation + workspace-confined file tools), so a default-posture session spends zero columns on posture prose. The workspace path MUST be anchored to the right edge — it is the only element permitted to shrink — so that posture state changes and terminal narrowing never displace the governance cluster. `CONTEXT_BAR_WORKSPACE_MIN_COLS` MUST be pinned to the shortening ladder's own last stop, so the path is present whenever honest information remains and omitted below it rather than rendered as an ambiguous fragment.
- **`[INV-HEAD-08]` Posture Outranks Identity**: Within the `ThreadContextBar`'s left cluster, the posture block MUST be budgeted before the role badge and MUST NEVER be truncated: a clipped token (`open` → `op`) still renders in the warning tone and so asserts a safety state the session may not hold. The role badge yields instead, eliding its inner text inside intact brackets (`[SECURITY-AU…]`) and disappearing entirely below a legible floor rather than degrading to `[D…]`. Priority on the row is therefore posture (safety) > role (identity) > workspace (location). Both invariants are enforced by `view_header::tests::row3_*`; the markers themselves MUST come from `theme.glyphs.ellipsis` so a non-Unicode terminal degrades with the rest of the chrome.

---

## Rejected Alternatives & Negative Knowledge

### 1. Retaining Fixed 3-Row Header for All Scenes (Rejected Alternative)
- *Failure Mode*: Violates Domain Boundary and Space Economy. Dashboard and Settings views were forced to render empty padding rows or unrelated thread context, wasting 12-15% of vertical space on compact terminals.

### 2. Multi-Bar Stack Above Composer (Rejected Alternative)
- *Failure Mode*: Violates Visual Stability. Having `DurabilityBanner`, `QueueBar`, `TasksBar`, and `ActivityBar` each reserve independent rows caused erratic layout jumps of up to 4 lines as agent phases shifted, creating acute visual fatigue and displacing the active input field.

### 3. Keeping the Uppercase `UNATTENDED` / `UNCONFINED` Pair (Rejected Alternative, amends §2a)
- *Why considered*: They were already shipped, and full words read as unambiguous in isolation.
- *Failure Mode*: Two defects. **Layout coupling**: the pair's width was deducted from the workspace path's budget, so a posture toggle reflowed an unrelated element. **Lexical cost**: `UNCONFINED` is a double negative, both words read as prose in a glance-row, and caps collided with the `[ROLE]` badge slot and the footer's `[N queued]` count idiom. Substituting the lowercase tokens `auto` / `open` — and drawing them only when the posture deviates — removes 22 columns from the row's worst case and makes the default case free.

### 4. Truncating the Workspace Path From the Tail (Rejected Alternative, amends §2a)
- *Why considered*: It is a one-line helper (`truncate_to_width`) and needs no path model.
- *Failure Mode*: It preserves exactly the wrong half. `~/projects/nuo/crates/nuo-tui` narrows to `~/projects…`, keeping ancestors the user already knows while discarding the leaf that names the project. The shared `PathView` ladder keeps the leaf and elides ancestors instead, which is the only information-correct direction; its floor then makes the "omit rather than lie" rule expressible.
