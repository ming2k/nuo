---
id: ADR-0024
title: "Two-Row Head Band: Uniform Session-Identity Row and Scene Row"
status: accepted
date: 2026-10-06
scope: tui/nuo-tui, presentation/chrome, interaction/affordance
superseded_by: null
negative_knowledge: true
---

# 0024. Two-Row Head Band: Uniform Session-Identity Row and Scene Row

- Status: Accepted
- Date: 2026-10-06
- Deciders: Nuo Architecture Working Group
- Consulted: TUI, Interaction, and Design-System maintainers
- Informed: System Architects
- Amends: [ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md) §2 (the row-2 legend's *content*)

---

## Context and Problem Statement

Every scene draws a fixed head band pinned to the terminal's top edge. After
[ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md) the band was described as "identity + status on row 1, the `C-x menu`
namespace legend on row 2", but in practice each scene read its rows
differently:

- The **Conversation** head put `SESSION` + id-tail + `[ROLE]` badge + workspace
  on row 1, with the run-mode flags (`UNATTENDED` / `UNCONFINED`) right-aligned
  on the same row, and the `C-x menu` pair on row 2.
- The **`/btw` aside** and **Subagent** heads replaced row 1 with their own
  identities (`/btw Side conversation`, `SUBAGENT [role] label (i/n)`) and used a
  `Main › …` breadcrumb for row 2.
- **Dashboard** and **Settings** each hard-coded a one-word identity
  (`DASHBOARD all projects`, `SETTINGS`) on row 1.

Two frictions followed:

1. **No scene was named.** A user landing in a scene had no single, plain word
   telling them *where* they were. The scene identity was either a breadcrumb
   (`Main › Aside`) or absent entirely (`SETTINGS`) — never a consistent,
   lowercase scene name.
2. **The session's ambient facts were scattered.** The session identity
   (id tail, role, workspace) lived on row 1 for the thread but was
   *replaced* by scene-specific content in every other scene, and the run-mode
   flags rode a different row depending on the scene. Nothing tied a scene back
   to the session the client was attached to.

Separately, the thread scene had no place to show the **chat's title**
(the AI-generated or manual session title) even though it existed durably and
was already surfaced in the sessions picker.

This ADR ratifies a **uniform two-row head band**: row 1 is always the ambient
**session identity**; row 2 is the **scene row** — the scene named plainly, its
context, and the session's persistent run-mode flags beside the standing
namespace pair ([ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md) `[INV-HINT-01]` preserved). Under [INV-AGENT-01] it
records the rejected alternatives.

---

## Decision Drivers

- **One glance, one scene**: the user must see *which scene* they are in, in
  plain language, on every scene.
- **Session continuity**: the ambient session facts (id tail, role, workspace)
  must stay visible regardless of the scene the user navigates into.
- **Run-mode honesty**: the persistent safety flags (`UNATTENDED`,
  `UNCONFINED`) must sit in one predictable place, not migrate with the scene.
- **Title discoverability**: the thread's chat title should be visible in
  the head, not only in the sessions picker.
- **Namespace discoverability**: the `C-x menu` pair stays a standing row-2
  affordance on every scene ([ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md) `[INV-HINT-01]`).

---

## Considered Options

- **Option 1 (Chosen)**: Two rows, uniform jobs — row 1 the session identity on
  every scene; row 2 the scene name + scene context + run-mode flags + the
  `C-x menu` namespace pair.
- **Option 2**: Keep per-scene row 1 (scene identity) and add the scene name to
  row 2 without unifying row 1's content.
- **Option 3**: Single-row band carrying both the session identity and the scene
  name (drop row 2's context, keep only the namespace pair).
- **Option 4**: Keep the status quo; only add the chat title to the thread
  row 1.

---

## Decision Outcome

Chosen option: **"Option 1"**.

### 1. Row 1 is the uniform session identity

Row 1 is the **session identity** on every scene that has an ambient session:
`SESSION` + the persistent-id tail (dimmed) + the `[ROLE]` badge (brand) + the
tilde-shortened workspace. While a session switch is loading, the id-tail slot
shows the target id as `<target> (loading…)`. Settings and Dashboard draw this
same row; the Conversation and `/btw`/Subagent scenes draw it too. A non-session
context (tests/showcase) that supplies no session head simply omits row 1.

The run-mode flags are **no longer on row 1**: they moved to row 2 (see §3) so
they sit in one predictable place across scenes.

### 2. Row 2 is the scene row

Row 2 names the scene the user stands in, in plain lowercase: `thread`
(the default home scene), `dashboard`, `settings`, `subagent`, `aside`. After
the name comes the scene's own **context** (left-aligned, truncated first under
width pressure):

- **thread**: the chat's title, derived from the first real chat prompt
  and cleaned by the shared titler rule (`nuo_wire::clean_title`); `None` before
  the first prompt renders the label alone.
- **dashboard**: the live fleet summary (`3 session(s) 1 running …`); flagged
  to the warning tone when a session needs attention.
- **settings**: the view-stack breadcrumb (`Main › Settings`, `Main › Aside ›
  Settings`, …).
- **subagent**: `[ROLE] <label> (i/n)`.
- **aside**: the coarse primary-session status (`main running`,
  `⚠ main approval needed`); the attention states escalate to the warning tone.

### 3. The run-mode flags and namespace pair ride the right edge

Row 2's right edge carries the session's persistent run-mode flags —
`UNATTENDED` (warning tone) when `--unattended` / `/unattended on`, and
`UNCONFINED` (warning tone) when `/confinement off` — followed by the standing
`C-x menu` namespace pair ([ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md) `[INV-HINT-01]`). The flags are session
facts the user must never lose sight of, so they sit beside the namespace pair
that is present on every scene.

### Invariants & Behavioral Boundaries

- **INV-HINT-02**: Every scene's head band renders **two** rows: row 1 the
  session identity (on scenes that have an ambient session), row 2 the scene
  row. Row 2 always leads with the scene's plain lowercase name and always
  carries the `C-x menu` namespace pair; chrome never advertises a chord the
  dispatcher does not honour ([ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md) `[INV-HINT-01]`).
- **INV-HINT-03**: The session's run-mode flags (`UNATTENDED`, `UNCONFINED`)
  render on **row 2's right edge** on every scene that shows them — never on
  row 1 — so their location never migrates with the scene.
- **INV-HINT-04**: A scene name is drawn from the closed
  [`ViewKind::scene_label`] set (`thread` / `dashboard` / `settings` /
  `subagent` / `aside`); a scene never invents a name inline.

### Positive Consequences

- The user always sees, in one glance and in plain language, which scene they
  stand in and which session they are attached to.
- The run-mode flags have one predictable home; a user cannot be in an
  unattended session without the flag being on row 2.
- The chat title is discoverable in the head, not only in the sessions picker.
- The head band's two rows have stable, scene-independent jobs, so a new scene
  slots in by declaring a `ViewKind` and a context string.

### Negative Consequences & Trade-offs

- The thread head no longer shows the run-mode flags on row 1; a user who
  read them there must look one row lower. Mitigation: they are still always
  present, just on row 2.
- Row 2 now carries more (label + context + flags + namespace) on narrow
  terminals. Mitigation: the scene label is the row's anchor and the context
  truncates first, so the label and the namespace pair are never crowded out.

---

## Rejected Alternatives & Negative Knowledge

### Option 2 (Rejected) — per-scene row 1, add the scene name to row 2 only
- Why considered: smaller diff; preserves each scene's current row-1 identity.
- Why rejected: leaves the session identity *replaced* in every non-thread
  scene, so the user loses the tie back to the session they are attached to the
  moment they navigate. The whole point is a stable identity row.

### Option 3 (Rejected) — single-row band with identity + scene name
- Why considered: keeps the band one row and reclaims a line for the transcript.
- Why rejected: a single row cannot carry the scene context (title / fleet
  summary / breadcrumb) *and* the identity without one crowding out the other,
  and it re-creates the confusion of mixing identity and affordance on
  one row. The two-row band's jobs are the reason it reads cleanly.

### Option 4 (Rejected) — keep the status quo, add the title to row 1
- Why considered: minimal change.
- Why rejected: it leaves the exact defects this ADR removes — no plain scene
  name, scattered session facts, scene-dependent flag placement — and would put
  the title in competition with the run-mode flags for row-1 space.

### Rejected sub-idea — Title-case scene names (`Conversation`, `Dashboard`)
- Why considered: reads as a proper noun; matches the old caps style.
- Why rejected: the scene name is *meta* chrome (where am I), not an identity
  brand; lowercase keeps it visually subordinate to the `SESSION` identity and
  distinct from keycap labels, and matches the switcher's own lowercase hints.

---

## Links

- Related ADRs: [ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md) (scene namespace + standing legend),
  [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md) (TUI presentation)
- Related modules: `nuo-tui::view_header` (`ViewHints`, `ViewKind::scene_label`,
  `SessionHead`, `parent_status_context`), `nuo-tui::render::draw_transcript`,
  `nuo-tui::event_loop::render`, `nuo-tui::overlays::dashboard`,
  `nuo-tui::views::settings`
