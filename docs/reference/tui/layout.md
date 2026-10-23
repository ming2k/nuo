# TUI Layout Reference

Factual reference for how `nuo-tui` divides the terminal viewport vertically, the identity of each chrome band, and the design tokens (`crates/nuo-tui/src/design.rs`) that size them.

This page is the review anchor cited by `crates/nuo-tui/src/footer_stack.rs` (the "flush-ness" of footer rows) — when a token changes, this page must be re-reviewed.

---

## 1. Vertical Band Composition

The full-frame entry point is `render::draw_transcript` (`crates/nuo-tui/src/render/mod.rs`). The viewport is composed top-to-bottom from three sovereign zones: the **head band**, the **content body**, and the **footer stack**.

```text
┌────────────────────────────────────────────────────────────────────┐
│[Refactor DB]  [Fix Auth]  [dashboard]                      C-x menu│   ◄ 1  TabBar
├────────────────────────────────────────────────────────────────────┤
│THREAD b3c4  Fix retry logic in auth loop     ‹ thread › subagent(2)│   ◄ 2  SceneBar
├────────────────────────────────────────────────────────────────────┤
│[DEVELOPER]  auto  open                               ~/projects/nuo│   ◄ 3  ThreadContextBar
├────────────────────────────────────────────────────────────────────┤
│                                                                    │
│              ▾ tool:search_text "query" (12 matches)               │   ◄ 4  StickyAnchor
│                                                                    │
│                    Polymorphic Content Viewport                    │   ◄ 5  Content Body
│             (thread transcript · dashboard · settings)             │
│                                                                    │
├────────────────────────────────────────────────────────────────────┤
│[2 queued]  ● responding  [12s]                    Esc Esc interrupt│   ◄ 6  ActivityStrip
├────────────────────────────────────────────────────────────────────┤
│› What is the next step in the refactor?                            │   ◄ 7  Composer
├────────────────────────────────────────────────────────────────────┤
│20.2k (8%)                               deepseek-chat max @deepseek│   ◄ 8  ModelBar
└────────────────────────────────────────────────────────────────────┘
```

| # | Band | Rows | Module | Identifier |
| :--- | :--- | :--- | :--- | :--- |
| 1 | TabBar | 1 | `view_header.rs` | `draw_tab_bar` |
| 2 | SceneBar | 1 | `view_header.rs` | `draw_scene_bar` |
| 3 | ThreadContextBar | 1 (thread) / 0 (non-thread) | `view_header.rs` | `draw_thread_context_bar` |
| 4 | StickyAnchor | 1 (overlay, only when scrolled) | `disclosure/renderers/sticky.rs` | `draw_sticky_anchor_if_needed` |
| 5 | Content Body | `Constraint::Min(0)` (adaptive) | `render/mod.rs` | transcript / hero |
| 6 | ActivityStrip | ≤ 1 | `chrome/activity_strip.rs` | `draw_activity_strip` |
| 7 | Composer | adaptive (min 4) | `composer.rs` | `draw_composer` |
| 8 | ModelBar | 1 | `chrome/model_bar.rs` | `draw_model_bar` |

The head band is a **single shared chrome slot** (`PAGE_HEADER_ROWS = 3`) whose tier rows are occupied adaptively: the TabBar and SceneBar rows appear when content exists, and the ThreadContextBar row occupies its slot only on thread scenes. Both header and footer rows are placed by layout split; a hidden row is height 0 and its space collapses, moving the rows below it up.

---

## 2. Head Band Tiers

The head band carries three tiers (ADR-0060, ADR-0062). It is a sibling of the content body, not nested inside it, so it replaces the viewport's top margin rather than occupying content rows. Under `[INV-HEAD-06]`, the tiers exhibit discrete in-family micro-elevation backgrounds rather than sharing a flat identical fill:

| Tier | Rows | Domain | Background Token | Left content | Right content |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **TabBar** | 1 | Client | `theme.tabbar_bg()` (~2.8% offset) | Project tabs with clean semantic titles (`Refactor DB`, `dashboard`), no ordinal prefix | `C-x menu` global client affordance |
| **SceneBar** | 1 | Scene | `theme.scenebar_bg()` (~5.0% offset) | Scene identity + authentic task title (`THREAD <id_tail> <title>`) | Drill-in breadcrumb stack (`‹ thread › subagent #1 › diff`) |
| **ThreadContextBar** | 1 on thread / 0 elsewhere | Thread | `theme.contextbar_bg()` (~4.0% offset) | `[ROLE]` badge, then safety posture tokens (`auto`, `open`) — omitted entirely when the session sits in the safe default | Tilde-shortened workspace path, right-anchored and shrinkable |

The TabBar is always present when a head exists; the SceneBar is universal across scene kinds; the ThreadContextBar renders **0 rows** on `Dashboard` and `Settings`, so non-thread viewports show a 2-row head. On Monochrome and ANSI-16 profiles, background fills converge safely onto `theme.element_bg`.

The ThreadContextBar reads **who → how → where** left to right, and the workspace is right-anchored for a correctness reason rather than a compositional one: it is the only element that may shrink, so pinning it to the right edge keeps the governance cluster (role + posture) at a fixed screen position. Toggling `/unattended` therefore grows the left cluster into the gap instead of re-truncating the path, and a narrow terminal degrades by shortening the path rather than by stealing columns from safety.

Posture is a **closed vocabulary of lowercase tokens**, not sentences, and only deviations from the safe default are drawn (interactive confirmation + workspace-confined file tools). `auto` means tool calls are approved without asking (`--unattended` / `/unattended on`); `open` means file tools may leave the workspace (`/confinement off`). Lowercase keeps the token from competing with the `[ROLE]` badge, and the bare (unbracketed) form keeps it from reading as a `[2 queued]`-style count. Each token is deliberately self-describing (`auto`, `open` name the state they assert), so the bar needs no tooltip layer; the authoritative long-form wording lives in the slash-command catalog (`BuiltinCmd::SPECS` in `nuo-server`), projected by `/unattended on|off` and `/confinement on|off`. Permanent chrome carries tokens only (`[INV-TUI-12]`: no decorative emoji, terse professional tokens).

Path shortening uses the shared `PathView` ladder (`Full` → `.../parent/leaf` → fish → basename → middle-truncate), so the **leaf** survives and ancestors are elided — the opposite of tail truncation. `CONTEXT_BAR_WORKSPACE_MIN_COLS` (4) is the ladder's own last stop — the bare leaf, which still names the project — so the floor is set where honest information actually runs out rather than above it. Below that the path is dropped: the bar either names the workspace or stays silent about it.

Priority on the row is **posture > role > workspace** (`[INV-HEAD-08]`). The posture block is budgeted first and is never truncated — a clipped `open` reading `op` would still be painted in the warning tone, asserting a safety state the session may not hold. The role badge takes the pressure instead: it elides inside intact brackets (`[SECURITY-AU…]`) and disappears below a legible floor rather than degrading to `[D…]`. The path absorbs whatever is left.

Every truncation marker comes from `theme.glyphs.ellipsis` (`…` under Unicode, `...` under ASCII), so a non-Unicode terminal degrades with the rest of the chrome.

---

## 3. Footer Stack

The footer is a **declarative** stack: the caller lists rows `(id, height)` in draw order and a single `place` pass computes each rect and the total height (`crates/nuo-tui/src/footer_stack.rs`). The row identity enum is exhaustive:

| `FooterRowId` | Height | Purpose |
| :--- | :--- | :--- |
| `TopGap` | `FOOTER_TOP_GAP_ROWS = 0` | Persistent separator between transcript and chrome |
| `ActivityStrip` | `ACTIVITY_STRIP_ROWS = 1` (or 0) | Consolidated status: queued follow-ups, background tasks, active round |
| `Composer` | adaptive, `COMPOSER_MIN_HEIGHT = 4` | The input box |
| `ModelBar` | `MODEL_BAR_ROWS = 1` (or 0) | Context usage meter + model identity (the route's catalog-resolved label, name-first / id-fallback per ADR-0081 — never the raw `vendor/model` wire id) |

The rows place flush by construction: the zero-gap tokens (`FOOTER_TOP_GAP_ROWS`, `ACTIVITY_COMPOSER_GAP_ROWS`, `COMPOSER_HINT_GAP_ROWS`, all `0`) mean no gap row is ever inserted between adjacent chrome rows. This flush-ness is the contract asserted by `crates/nuo-tui/src/footer_stack.rs`.

---

## 4. Design Tokens

Sizes come from `crates/nuo-tui/src/design.rs` (non-color tokens). Values below are the source of truth.

### Row counts and heights

| Token | Value | Meaning |
| :--- | :--- | :--- |
| `PAGE_HEADER_ROWS` | 3 | Maximum head-band rows (TabBar + SceneBar + ThreadContextBar) |
| `ACTIVITY_STRIP_ROWS` | 1 | Consolidated activity strip height |
| `MODEL_BAR_ROWS` | 1 | Model/hint bar height |
| `QUEUE_BAR_ROWS` | 1 | Queue bar height (retained token) |
| `COMPOSER_VERTICAL_CHROME_ROWS` | 3 | Composer chrome around the text (pad + gap + meta row) |
| `COMPOSER_MIN_HEIGHT` | 4 | Minimum composer height for a one-line draft |
| `COMPOSER_MAX_HEIGHT_DIVISOR` | 2 | Composer grows to at most `terminal_height / 2` |
| `CONTEXT_BAR_WORKSPACE_MIN_COLS` | 4 | Honesty floor for the ThreadContextBar's workspace path — the ladder's last stop (bare leaf). Below it the path is dropped rather than drawn as a fragment |

### Gap rows (flush-ness contract)

| Token | Value | Meaning |
| :--- | :--- | :--- |
| `FOOTER_TOP_GAP_ROWS` | 0 | Transcript ↔ footer breathing (content owns its own scroll padding) |
| `ACTIVITY_COMPOSER_GAP_ROWS` | 0 | ActivityStrip ↔ Composer |
| `COMPOSER_HINT_GAP_ROWS` | 0 | Composer ↔ hint bar |
| `STREAM_TOP_GAP_ROWS` | 1 | Scroll padding at the top of the message stream |
| `STREAM_BOTTOM_GAP_ROWS` | 1 | Scroll padding at the bottom of the message stream |
| `MESSAGE_GAP_ROWS` | 1 | Between transcript messages |

### Insets and gutters

| Token | Value | Meaning |
| :--- | :--- | :--- |
| `TRANSCRIPT_H_INSET` | 2 | Uniform horizontal inset applied once at the transcript band entry point |
| `TRANSCRIPT_BODY_LEADING_INDENT` | 2 | Only indent prose adds atop the already-inset band |
| `FOOTER_H_INSET` | `= TRANSCRIPT_H_INSET` | Same inset inside the footer band |
| `MODEL_BAR_INNER_PADDING` | 1 | Edge indent of the model-bar cluster |
| `MODEL_BAR_GAP_MIN` | 2 | Minimum gap between the model bar's two halves |
| `BAR_LEGEND_GAP_MIN` | 6 | Bar content ↔ right-pinned keycap legend |

### Minimum terminal geometry

| Token | Value | Meaning |
| :--- | :--- | :--- |
| `MIN_TERMINAL_COLS` | 44 | Narrowest usable width (fits the 42-column wordmark with margin) |
| `MIN_TERMINAL_ROWS` | 12 | Shortest usable height (head + footer chrome + a transcript row) |

Below either minimum, `design::below_minimum(width, height)` is the single authority: the renderer swaps the whole UI for a centered "terminal too small" notice (`draw_too_small_notice`), and the event loop freezes input and animation so nothing changes behind the notice.

---

## 5. Viewport Margins

The content body's margins are defined in `crates/nuo-tui/src/primitives.rs`, not `design.rs`:

| Token | Value | Meaning |
| :--- | :--- | :--- |
| `VIEWPORT_TOP_MARGIN` | 1 | Content-body top margin (replaced by the head band when a head exists) |
| `VIEWPORT_BOTTOM_MARGIN` | 0 | Content-body bottom margin (the footer pins flush) |
| `VIEWPORT_H_MARGIN` | 0 | Content-body horizontal margin |

`render::transcript_band_rect` (`crates/nuo-tui/src/render/mod.rs`) is the single point where `TRANSCRIPT_H_INSET` is applied, so no downstream component re-clips or hand-pads a leading gutter.
