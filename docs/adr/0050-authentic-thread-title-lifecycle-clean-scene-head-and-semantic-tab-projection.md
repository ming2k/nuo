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

### 2. Clean Scene Head Band (`[INV-HEAD-03]`)

Row 2 (Scene Context Head, [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)) is streamlined:
- **Root Conversation View (`cursor == 0`, `breadcrumbs.len() <= 1`)**:
  - The redundant `thread` label is completely eliminated.
  - If an authentic title exists, the title leads the row (styled in bold heading tone).
  - If no title exists, the bound workspace path leads the row directly.
  - The right-hand persistent safety flags (`UNATTENDED`, `UNCONFINED`) remain anchored to the right edge.
- **Hierarchical Drill-in View (`breadcrumbs.len() > 1`)**:
  - The `thread` identifier is preserved strictly as the root breadcrumb anchor (e.g. `< thread > subagent: explore > diff`), maintaining clear spatial orientation during deep task inspection ([ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md)).
- **Peripheral Tool Scenes (`dashboard`, `settings`)**:
  - Continue to display their explicit scene labels (`dashboard`, `settings`) as singleton tool viewports.

### 3. Semantic TabBar Projection (`[INV-TAB-08]`)

Row 1 (Client TabBar) dynamically projects semantic titles:
- When a thread tab possesses an authentic title, it renders `[<index>:<title>]` (e.g. `1: Refactor DB`, `2: Fix auth`), truncated cleanly with an ellipsis (`…`) if exceeding the maximum width budget (16 display columns).
- When a thread tab has no authentic title (fresh session or awaiting AI distillation), it falls back to `[<index>:#<id_tail>]` (e.g. `1:#b3c4`), eradicating the verbose and cryptic `thread-` prefix entirely.
- Upon receiving a title establishment event from the server, `SurfaceRouter::update_tab_title` updates the mounted tab title in real-time without restarting or redrawing transcripts.

---

## Invariants & Behavioral Boundaries

- **`[INV-TITLE-01]` Authentic Title Invariant**: A thread title MUST strictly reflect an authentic entity property (operator manual rename or detached AI titler distillation). The client MUST NOT synthesize or spoof a thread title by truncating raw user chat prompts or first-turn inputs.
- **`[INV-HEAD-03]` Clean Root Scene Head**: In the root thread view (`history.cursor == 0`), the scene context head MUST NOT render a redundant `thread` label. If an authentic title exists, the title leads the row; if no title exists, the head displays ambient workspace metadata directly without pseudo-title fillers. The `thread` noun is reserved strictly as the root breadcrumb anchor during drill-in navigation (`breadcrumbs.len() > 1`).
- **`[INV-TAB-08]` Semantic TabBar Projection**: Mounted thread tabs in the client TabBar MUST dynamically project the authentic thread title when present, bounded by reasonable width truncation. The compact `#<id_tail>` identifier is permitted strictly as an untitled fallback; cryptic `thread-xxxx` prefixes are purged from TabBar presentation.

---

## Rejected Alternatives

### 1. Spoofing Raw First-Turn Chat Prompts as Fallback Titles
- *Why rejected*: Violates Authentic Title Invariant (`[INV-TITLE-01]`). Raw prompts are instructions or inquiries, not titles. Truncating strings like `Please examine the following diff and tell me why...` generates ugly, broken headlines that confuse operators and erode interface polish.

### 2. Retaining Static `thread` Label on Row 2 at All Times
- *Why rejected*: Violates Clean Root Scene Head (`[INV-HEAD-03]`). In the core conversational surface, writing `thread` next to the actual conversation is redundant visual clutter that wastes terminal columns. The operator already knows they are in an agent thread; chrome must prioritize content over self-labeling.

### 3. Permanently Static Hexadecimal Tab Labels (`thread-xxxx`)
- *Why rejected*: Violates Semantic TabBar Projection (`[INV-TAB-08]`). Forces severe cognitive friction in multi-tab workflows. Operators cannot correlate `thread-b3c4` with specific tasks without switching through each tab.

### 4. Retaining `thread-` Prefix in Untitled Tab Fallbacks
- *Why rejected*: Violates Semantic TabBar Projection (`[INV-TAB-08]`). In Nuo's conversation viewport, repeating `thread-` wastes 7 columns per tab and produces visual clutter. Compact `#<id_tail>` provides clean disambiguation without legacy baggage.

---

## Positive Consequences

- **High-Fidelity Multi-Tasking**: Operators instantly recognize open conversation contexts from the TabBar.
- **Pristine Minimalist Header**: The root dialogue scene feels clean, modern, and uncluttered, reclaiming space for workspace paths and status.
- **Honest Domain Semantics**: Completely eliminates misleading synthetic pseudo-titles.

---

## Negative Consequences & Mitigation

- Freshly created threads initially display `thread-<id_tail>` until an explicit title is given or the AI titler completes its asynchronous round.
- *Mitigation*: The AI titler runs detached and typically completes within the first round, automatically upgrading the tab and scene head dynamically.
