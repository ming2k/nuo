---
id: ADR-0050
title: "Authentic Thread Title Lifecycle, Clean Scene Head, and Semantic Tab Projection"
status: accepted
date: 2026-10-26
scope: tui/nuo-tui, wire/nuo-wire, server/nuo-server, presentation/chrome, architecture/surfaces
superseded_by: null
negative_knowledge: true
---

# 0050. Authentic Thread Title Lifecycle, Clean Scene Head, and Semantic Tab Projection

- Status: Accepted
- Date: 2026-10-26
- Deciders: Nuo Architecture Working Group
- Consulted: TUI Presentation, Wire Protocol, Server Runtime, UX Working Group
- Informed: System Architects, Release Engineering
- Amends: [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md)

> **Superseded in part (2026-10-23)**: safety posture flags were replaced by the `auto` / `open` posture tokens, and the workspace path moved from following the title slot to the `ThreadContextBar`'s right edge ([ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md) `[INV-HEAD-07]`, `[INV-HEAD-08]`). The anchor-stable title slot and the `thread` breadcrumb root established here are **unaffected and remain in force**.

---

## Context and Problem Statement

Following the establishment of the persistent Thread entity model ([ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md)), the unified Client Tab Workspace ([ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md)), and the Three-Tier Header Topology ([ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md)), several legacy compromises and ergonomic anomalies persisted in the visual presentation layer:

1. **Synthetic Title Spoofing (Category Error)**:
   Historically under [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md) §2, the client extracted the user's first raw chat prompt from the active transcript and applied `nuo_wire::clean_title` to synthesize an ad-hoc "title". A user's first input is frequently an operational imperative (e.g. `check why this function panics: fn foo() { ... }`), a greeting, or multi-line instructions. Spoofing this raw prompt as the thread title misrepresents the thread's semantic identity, causes visual clutter with broken sentences, and violates the principle of authentic entity properties. Either a thread possesses an authentic title (explicitly assigned via manual rename or distilled by an asynchronous background AI titler), or it does not.

2. **Redundant Scene Naming on Default Canvas (`thread` Label Clutter)**:
   Under [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md), Row 2 led with the literal scene label (`thread`, `dashboard`, `settings`). While peripheral singleton tool scenes (`dashboard`, `settings`) benefit from identifying themselves, `thread` is the application's primary, default conversation canvas where operators spend 95% of their working time. Rendering a static, prominent `thread` label in the root scene head consumes valuable terminal horizontal space and introduces cognitive noise. Breadcrumbs are only semantically relevant during deep inspection drill-in ([ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md)).

3. **Opaque and Inscrutable Tab Titles (`thread-xxxx` Drift)**:
   Although [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md) and [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md) formalized the client TabBar, the implementation hardcoded tab labels as `format!("{}:thread-{}", i + 1, id_tail(id))`. When operators maintain multiple parallel conversation tabs (`1:thread-a1b2`, `2:thread-8f3c`, `3:thread-4e10`), the tab headers convey zero semantic distinction. Operators are forced to repeatedly switch tabs to inspect transcripts, defeating the core value proposition of multi-tab orchestration.

We require a modern, forward-looking architectural realignment that establishes authentic title lifecycle contracts, purges redundant scene labels from the root conversational head, and dynamically projects semantic thread titles into the client TabBar.

---

## Decision Drivers

- **Domain Integrity & Honesty**: A title is an authentic entity attribute. If a thread has no established title, the system must not spoof one from arbitrary user prompt text.
- **Cognitive Clarity & High Signal-to-Noise**: In the primary dialogue canvas, remove redundant labels (`thread`) so that operators focus exclusively on actual context (workspace path, authentic title, and safety flags).
- **Multi-Tasking Ergonomics**: Tabs must project human-understandable, semantic topics (e.g. `1: Refactor DB`, `2: Fix Auth`) instead of opaque hexadecimal UUID fragments.
- **Reactive Synchronization**: Tab titles and scene headers must dynamically reflect title establishment (whether by background AI summarization or explicit manual rename) in real-time.

---

## Considered Options

- **Option 1 (Chosen)**:
  - Establish an explicit `title: Option<String>` property across wire and monitor records, distinguishing authentic titles from transcript previews.
  - Eliminate raw prompt spoofing in the TUI; if no title is established, the title slot remains empty.
  - Remove the static `thread` scene label from Row 2 when at the root conversation level (`history.cursor == 0`). If an authentic title exists, it leads the row; otherwise, the bound workspace path leads directly. The `thread` label is retained strictly as the root breadcrumb anchor during multi-level drill-in (`breadcrumbs.len() > 1`).
  - Upgrade the TabBar projection to display authentic thread titles (bounded by sensible truncation), retaining `thread-<id_tail>` strictly as an untitled fallback.
- **Option 2**: Retain prompt spoofing as a fallback title when no explicit title exists, but update the tab bar to use the spoofed prompt.
- **Option 3**: Keep `thread` in Row 2 but only change TabBar labels to display the short UUID plus the first 5 characters of prompt text.

---

## Decision Outcome

Chosen option: **Option 1**.

### 1. Authentic Title Contract (`[INV-TITLE-01]`)

A thread's title is strictly an authentic entity property:
1. **Explicit Manual Rename**: Assigned by the operator via `/title <name>` or `AgentRequest::RenameSession`.
2. **Cognitive Distillation**: Generated asynchronously by the background AI titler subagent ([ADR-0016](0016-periodic-background-session-review-and-multi-turn-agent-topology.md), `nuo_wire::cognitive`).
3. **Untitled / Empty**: When neither has occurred, the title is `None`. The client MUST NOT synthesize or spoof a title by truncating `TranscriptMessage` prompts. First-turn prompt text is solely a message transcript preview, never a thread title.

Wire and persistence models (`SessionSummary`, `SessionOverview`, `MonitoredSession`) expose `pub title: Option<String>`. When a title is established or renamed, the server broadcasts updates, and the client updates its tab and scene models reactively.

### 2. Anchor-Stable Scene Head Band (`[INV-HEAD-03]`)

Row 2 (Scene Context Head, [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)) is streamlined with slot-invariant anchor stability:
- **Root Conversation View (`cursor == 0`, `breadcrumbs.len() <= 1`)**:
  - The leading title slot is anchor-stable: when an authentic title exists, the title leads the row (styled in heading tone); when untitled (fresh session awaiting AI distillation or explicit rename), `thread` stands in as a muted placeholder in the title slot.
  - Once an authentic title is established, the title replaces `thread` in-place. This preserves slot consistency and eliminates visual layout shift (workspace path jumping horizontally).
  - The bound workspace path follows the title slot, and right-hand persistent safety posture tokens (`auto`, `open`) remain anchored to the right edge.
- **Hierarchical Drill-in View (`breadcrumbs.len() > 1`)**:
  - The `thread` identifier is preserved strictly as the root breadcrumb anchor (e.g. `< thread > subagent: explore > diff`), maintaining clear spatial orientation during deep task inspection ([ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md)).
- **Peripheral Tool Scenes (`dashboard`, `settings`)**:
  - Continue to display their explicit scene labels (`dashboard`, `settings`) as singleton tool viewports.

### 3. Semantic TabBar Projection (`[INV-TAB-08]`)

Row 1 (Client TabBar) dynamically projects semantic titles with typed placeholder fallback:
- When a thread tab possesses an authentic title, it renders `[<index>:<title>]` (e.g. `1:Refactor DB`, `2:Fix auth`), truncated cleanly with an ellipsis (`…`) if exceeding the maximum width budget (16 display columns).
- When a thread tab has no authentic title (fresh session or awaiting AI distillation), it renders `[<index>:thread#<id_tail>]` (e.g. `1:thread#b3c4`), combining explicit entity-kind identification (`thread`) with compact disambiguation (`#<id_tail>`) within the terminal tab budget.
- Upon receiving a title establishment event from the server, `SurfaceRouter::update_tab_title` updates the mounted tab title in real-time, replacing `thread#<id_tail>` with the authentic title in-place without restarting or redrawing transcripts.

---

## Invariants & Behavioral Boundaries

- **`[INV-TITLE-01]` Authentic Title Invariant**: A thread title MUST strictly reflect an authentic entity property (operator manual rename or detached AI titler distillation). The client MUST NOT synthesize or spoof a thread title by truncating raw user chat prompts or first-turn inputs.
- **`[INV-HEAD-03]` Anchor-Stable Scene Head**: In the root thread view (`history.cursor == 0`), the scene context head MUST maintain an anchor-stable title slot. When untitled, `thread` stands in as a muted placeholder; once an authentic title is established, the title replaces `thread` in-place. The `thread` noun is also preserved as the root breadcrumb anchor during drill-in navigation (`breadcrumbs.len() > 1`).
- **`[INV-TAB-08]` Semantic TabBar Projection**: Mounted thread tabs in the client TabBar MUST dynamically project the authentic thread title when present, bounded by reasonable width truncation. In the untitled initial state, tabs project `<index>:thread#<id_tail>` as a typed placeholder; once an authentic title is established, the title replaces `thread#<id_tail>` in-place. Full raw UUIDs MUST NOT be embedded unshortened into TabBar items to preserve horizontal terminal budget.

---

## Rejected Alternatives

### 1. Spoofing Raw First-Turn Chat Prompts as Fallback Titles
- *Why rejected*: Violates Authentic Title Invariant (`[INV-TITLE-01]`). Raw prompts are instructions or inquiries, not titles. Truncating strings like `Please examine the following diff and tell me why...` generates ugly, broken headlines that confuse operators and erode interface polish.

### 2. Retaining Static `thread` Label on Row 2 Alongside Authentic Title
- *Why rejected*: Violates Clean Scene Head. In the core conversational surface, writing `thread > My Title` once a title exists is redundant visual clutter that wastes terminal columns. The in-place replacement model replaces `thread` with the title rather than prepending it.

### 3. Permanently Static Hexadecimal Tab Labels (`thread-xxxx`)
- *Why rejected*: Violates Semantic TabBar Projection (`[INV-TAB-08]`). Forces severe cognitive friction in multi-tab workflows. Operators cannot correlate `thread-b3c4` with specific tasks without switching through each tab.

### 4. Embedding Unshortened 36-Character UUIDs in Tabs (`thread#0194eb82-...`)
- *Why rejected*: Violates Terminal Width Economy (`[INV-TAB-08]`). A full UUID consumes 40+ columns per tab, causing two tabs to exhaust the TabBar budget and push off the `C-x menu` affordance on standard 80-120 column terminals. The compact `#<id_tail>` gives sufficient disambiguation while fitting cleanly inside the 16-column tab budget.

---

## Positive Consequences

- **High-Fidelity Multi-Tasking**: Operators instantly recognize open conversation contexts from the TabBar.
- **Pristine Minimalist Header**: The root dialogue scene feels clean, modern, and uncluttered, reclaiming space for workspace paths and status.
- **Honest Domain Semantics**: Completely eliminates misleading synthetic pseudo-titles.

---

## Negative Consequences & Mitigation

- Freshly created threads initially display `thread#<id_tail>` until an explicit title is given or the AI titler completes its asynchronous round.
- *Mitigation*: The AI titler runs detached and typically completes within the first round, automatically upgrading the tab and scene head dynamically.
