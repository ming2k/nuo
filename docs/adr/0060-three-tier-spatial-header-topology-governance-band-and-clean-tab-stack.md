---
id: ADR-0060
title: "Three-Tier Spatial Header Topology: Dedicated Thread Governance Band, Clean Tab Projection, and Right-Aligned Scene Stack"
status: accepted
date: 2026-10-31
scope: tui/nuo-tui, presentation/chrome, interaction/tabs, architecture/spatial
superseded_by: null
negative_knowledge: true
---

# 0060. Three-Tier Spatial Header Topology: Dedicated Thread Governance Band, Clean Tab Projection, and Right-Aligned Scene Stack

- Status: Accepted
- Date: 2026-10-31
- Deciders: Nuo Architecture Working Group
- Consulted: TUI Presentation, Interaction Design, Product Architecture
- Informed: System Architects, Release Engineering
- Amends: [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)

> **Superseded in part (2026-10-23)**: this ADR's Row 2 *layout* — thread governance (`[ROLE]`, workspace, posture) sharing the scene row — was superseded by [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md) `[INV-HEAD-05]`, which made the header two universal tiers plus a thread-only `ThreadContextBar`. The posture pair shown in the diagram below was further replaced by the `auto` / `open` posture tokens under `[INV-HEAD-07]` and `[INV-HEAD-08]`. The diagrams below are preserved as the record of what was decided here; **do not implement from them** — see ADR-0062 for the live topology. `[INV-TAB-09]` (no ordinal prefixes in tab titles) is unaffected and remains in force.

---

## Context and Problem Statement

The Nuo TUI header has evolved through multiple iterations: [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md) established a two-row band (Session Row and Scene Row); [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md) envisioned a client-centric three-tier header; and [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) established authentic title contracts and in-place placeholder replacement.

Despite these advancements, visual collision and semantic conflation persisted in the presentation layer:

1. **Severe Horizontal Budget Collision in Row 2**:
   In standard 80–120 column terminals, Row 2 was forced to multiplex:
   - Authentic thread title or scene breadcrumb `< thread > subagent > ...`
   - Bound workspace path `~/projects/nuo/core/...`
   - Safety execution posture tokens `auto` and `open`
   When titles were descriptive or workspace paths nested, text was aggressively truncated, resulting in an unreadable `...` smear across both context and path.

2. **Semantic Conflation of Governance and Viewport Scene**:
   `[ROLE]`, `workspace`, `auto`, and `open` represent **Session Execution Governance & Boundaries**. Conversely, the chat title and drill-in breadcrumbs represent the **Viewport Scene & Navigation State**. Forcing governance flags into the scene row blurred the boundary between operational safety and view navigation.

3. **Cluttered Tab Ordinal Prefixes (`1:`, `2:`)**:
   Under [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), tabs prepended numeric indices (`1:Refactor DB`, `2:Fix Auth`) to advertise `Alt+1..9` direct switching. In modern workspaces with multiple open tabs, this consumed 2–3 display columns per tab, stealing scarce horizontal budget from human-readable task titles and producing visual clutter akin to raw engineering drafts.

4. **Vestigial `thread` Placeholder**:
   [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) retained `thread` as a muted placeholder on untitled sessions to prevent layout shift when the workspace path shared the same line. Once workspace and governance are disentangled from the scene title, the layout shift failure mode disappears, rendering the dummy `thread` placeholder redundant.

---

## Decision Drivers

- **Zero Horizontal Budget Collision**: Eliminate text truncation caused by packing disparate concerns onto a single line.
- **Strict Domain Stratification**: Clearly segregate Client Workspace (Row 1), Session Governance (Row 2), and Scene Viewport (Row 3).
- **Clean Aesthetic & High Information Density**: Remove artificial numeric prefixes (`1:`) from tabs and purge vestigial placeholders (`thread`) from the scene title slot.
- **Asymmetric Spatial Balance**: Place primary task context on the left and subordinate navigation stacks / drill-in breadcrumbs on the right.

---

## Considered Options

- **Option 1 (Chosen)**:
  - **Tier 1 (Client TabBar)**: Clean tab labels projecting authentic titles or `thread#<tail>` without `1:` ordinal prefixes. Right-anchored `C-x menu` client affordance.
  - **Tier 2 (Thread Governance Band)**: Left-anchored `THREAD <id_tail> [ROLE]` coupled with right-anchored safety posture tokens `auto` and `open`, and right-anchored workspace.
  - **Tier 3 (Scene Head & Navigation Stack)**: Left-anchored authentic task title / scene context (no dummy `thread` placeholder when untitled). Right-anchored drill-in stack / breadcrumbs (`< thread > subagent #1 > diff`).
- **Option 2**: Maintain a two-row header by moving governance flags to the footer status bar.
- **Option 3**: Keep the 2-row layout and truncate titles to a hard limit of 15 characters to preserve room for workspace and flags.

---

## Decision Outcome

Chosen option: **Option 1**.

### 1. Three-Tier Header Topology (`[INV-HEAD-04]`, `[INV-THREAD-02]`)

The TUI header band is formally ratified as a dedicated three-tier chrome structure (`PAGE_HEADER_ROWS = 3`):

```text
┌────────────────────────────────────────────────────────────────────────┐
│ [Refactor DB]  [Fix Auth]  [dashboard]                        C-x menu │ ◄ Row 1: Client Global TabBar (Client Tier)
├────────────────────────────────────────────────────────────────────────┤
│ THREAD b3c4 [DEVELOPER]  auto  open                     ~/projects/nuo │ ◄ Row 2: Thread Identity & Governance (Governance Tier)
├────────────────────────────────────────────────────────────────────────┤
│ Fix retry logic in auth loop                   ‹ thread › subagent(2) │ ◄ Row 3: Scene Head & Navigation Stack (Scene Tier)
└────────────────────────────────────────────────────────────────────────┘
```

1. **Row 1: Client Global TabBar**:
   - Fixed height: 1 row. Owned by the Client container.
   - Projects open tabs with clean semantic titles (e.g. `Refactor DB`, `dashboard`, `settings`) or compact untitled fallbacks (`thread#b3c4`).
   - Carries the standing `C-x menu` global client affordance on the right edge.
2. **Row 2: Thread Identity & Governance Band**:
   - Fixed height: 1 row. Owned by the active thread entity model (`ThreadContext`).
   - Left side: `THREAD ` + `id_tail` (dimmed) + `[ROLE]` badge (brand tone) + safety posture tokens (`auto`, `open`).
   - Right side: Persistent workspace path, right-anchored.
3. **Row 3: Scene Head & Navigation Stack**:
   - Fixed height: 1 row. Owned by the active scene view (`ViewHints`).
   - Left side: Primary authentic task title or domain scene context (e.g. fleet summary for dashboard, category for settings).
   - Right side: Drill-in navigation stack and breadcrumb trail (`< thread > subagent #1 > diff`) with back affordance (`< `).

### 2. Clean Tab Projection (`[INV-TAB-09]`)

- Tab items in the client TabBar MUST NOT include ordinal index numbers or colon prefixes (e.g. `1:`).
- Direct tab navigation chords (`Alt+1` through `Alt+9`) remain active, documented via footer keycaps and help modals without cluttering individual tab surfaces.

### 3. Clean Scene Title and Right-Aligned Stack (`[INV-SCENE-01]`)

- In the root thread view (`breadcrumbs.len() <= 1`), the dummy `thread` placeholder is eliminated. When an authentic title exists, it leads Row 3; when untitled, the title slot remains empty without layout shift.
- Scene navigation stacks and drill-in breadcrumbs (`breadcrumbs.len() > 1`) are right-aligned on Row 3, visually balancing the primary left-aligned task title.

---

## Normative Invariants

- **`[INV-HEAD-04]` Three-Tier Header Stratification**: The top chrome of thread-backed viewports MUST strictly render across three dedicated single-row tiers: Tier 1 Client TabBar, Tier 2 Thread Governance Band, and Tier 3 Scene Head & Navigation Stack.
- **`[INV-TAB-09]` Clean Tab Projection**: Tab items MUST project clean semantic titles or typed identifiers (`thread#<tail>`) without ordinal index prefixes (`<n>:`).
- **`[INV-SCENE-01]` Zero-Placeholder Clean Scene Head**: Row 3 MUST NOT render a synthetic or placeholder `thread` string on untitled threads. Navigation drill-in breadcrumbs MUST be right-aligned on Row 3 when stack depth exceeds 1.

---

## Rejected Alternatives & Negative Knowledge

### 1. Cramming Governance and Title onto Row 2 (Rejected Alternative)
- *Failure Mode*: Violates Horizontal Budget Economy. Pushing workspace paths, safety flags, and titles into a single row causes frequent truncation ellipses (`...`) on standard terminal sizes, making both the current directory and the current task unreadable simultaneously.

### 2. Moving Safety Posture Flags to Footer (Rejected Alternative)
- *Failure Mode*: Violates High-Visibility Safety Requirements. Safety posture tokens represent critical operational modes where the agent executes unconfirmed actions (`auto`) or writes outside workspace boundaries (`open`). Placing safety tokens in the footer causes banner blindness and risks inadvertent command execution.

### 3. Preserving Dummy `thread` Placeholder on Row 3 (Rejected Alternative)
- *Failure Mode*: Violates Zero-Placeholder Invariant (`[INV-SCENE-01]`). The placeholder existed solely to prevent horizontal shifting of the workspace path under [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md). With workspace relocated to Row 2, the shift failure mode no longer exists, and writing `thread` on an application whose primary purpose is conversational threading represents redundant visual noise.
