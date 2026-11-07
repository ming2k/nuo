# TUI Modals Reference

Factual reference for the overlay/modal surfaces: the framework, the sizing
presets, the provider model list's identity-column rule, and the universal
multi-column budget (ADR-0152).

This page is cited directly by `crates/nuo/src/shell/overlays/provider/models.rs` (the
"name-first, id-fallback" rule and the column budget).

---

## 1. Surfaces and Framework

The overlay vocabulary is declared in `crates/nuo-tui/src/surfaces/mod.rs`:

- **`DialogKind`** — the full-page dialog set (tools, MCP, skills, permissions, usage stats, quotas, thread stats, session trace, models, connections, history search, queue, threads, session tree, switcher, port share).
- **`SheetKind`** — lightweight interaction sheets (permission, question, input injection, model editor, provider preset, OAuth pending, custom provider, provider delete confirm).
- **`OverlaySurface`** / **`Surface`** — the dispatch enum the render loop matches on.

All modals are drawn through one frame: `modal_frame` in `crates/nuo-tui/src/elevation.rs` splits a single header row, a flexible body, and a single footer row, honoring an elevation archetype (structured vs hybrid). Geometry comes from `crates/nuo-tui/src/primitives.rs`.

---

## 2. Modal Sizing Presets

Fixed-size modals (`FixedModalSpec(width_percent, height_percent)`):

| Preset | width % | height % |
| :--- | :--- | :--- |
| `PROVIDER` | 76 | 80 |
| `MODELS` | 84 | 80 |
| `SESSIONS` | 82 | 78 |

Content-sized modals (`ContentModalSpec(width_percent, min_rows, max_viewport_percent)`):

| Preset | width % | min rows | max viewport % |
| :--- | :--- | :--- | :--- |
| `TOOLS` | 64 | 11 | 84 |
| `MCP` | 64 | 9 | 84 |
| `QUEUE` | 66 | 9 | 84 |
| `THREAD_STATS` | 76 | 11 | 80 |
| `SESSION_TRACE` | 80 | 11 | 92 |
| `USAGE_STATS` | 76 | 12 | 86 |
| `MODEL_EDITOR` | 66 | 6 | 60 |
| `OAUTH_PENDING` | 76 | 7 | 80 |
| `CUSTOM_PROVIDER` | 72 | 8 | 80 |
| `PERMISSIONS` | 72 | 7 | 80 |
| `SKILLS` | 72 | 7 | 80 |

---

## 3. Provider Model List: Name-First, Id-Fallback

The provider model modal (`draw_models_modal`, `crates/nuo-tui/src/overlays/provider/models.rs`) renders each model as a two-column row: a **primary identity column** and a **secondary wire-id column**.

**Rule.** The primary column reads the provider's own display name for the model when it publishes one, and falls back to the raw wire id when the catalog publishes no label — so the column is never empty.

- The sizing keys on the *rendered label*: `label = name.unwrap_or(model)`.
- The wire id sits dimmed in the column beside the label, and that column
  **drops for the whole list** when the budget cannot fund a readable width
  (`id_floor = 12`), because a fragment of the only typeable handle reads as a
  *different* model.
- The label column is clamped to at least 16 columns and never owns more than
  34% of the body.

**Identity is unaffected by what is drawn.** Activation, favorites, hiding, and
every configuration surface still key on the wire id (`rm.model`). The display
name is presentation only, never the wire value.

---

## 4. Multi-Column Rows: One Budget, One Direction

Per **ADR-0152** (`[INV-ROW-GRID-01]`, `[INV-TEXT-OVERFLOW-01]`,
`[INV-TUI-MARQUEE-01]`), every multi-column modal row resolves its columns
through `nuo_ui_primitives::components::columns` **before** a single cell is
fitted.

### 4.1 The column spec

| Field | Meaning |
| :--- | :--- |
| `demand` | width the column's data actually needs (measured, never guessed) |
| `min_width` | floor; the width below which the field stops being readable |
| `max_share` | the column's **relative maximum width**, as a % of the body |
| `priority` | shrink order (`0` = gives way last) |
| `droppable` | may vanish rather than render a fragment |

### 4.2 The six phases

1. **Optional-first** — a `droppable` column may never be funded out of an
   essential column's *floor*. A wire id that only annotates a few rows does not
   get to squeeze the model name every row needs. The test is "can every field
   that must stay readable still be read?", not "does the row overflow".
2. **Clamp** — `min(demand, max_share % of body)`, never below the floor.
3. **Fit** — shrink by priority to the floors, then drop columns that cannot
   reach their floor, then (last resort) drop columns in ascending priority —
   rightmost first among equals, since a row reads left to right — until one
   survivor remains, and shrink that. A one-column `…` is not a column.
4. **Restore** — space freed by a drop flows back to the truncated fields, up to
   their **own demand, never their cap**: padding a 16-character name to 55
   columns adds no readability, it just moves the next column away.
5. **Justify** — the remainder goes into the gaps, bounded by `max_gap`.
6. **Place** — every `start` comes from one left-to-right accumulation.

**The trailing reserve is a preference, not a right.** When honouring a badge
would leave a kept column below its floor, the badge yields and the columns take
its space — a nameless row is unreadable everywhere, while the badge's fact is
still reachable in the detail view. The verdict travels with the geometry
(`Grid::layout` returns `Solved { columns, reserve }`), because a solver that
reclaims the space while the renderer still paints the badge overflows the row.

Phase 6 is the law: **positions are a function of widths, never of
neighbours.** No column can be starved by another column's content.

### 4.3 The Models list, concretely

| Field | Overflow | Floor | Max share |
| :--- | :--- | ---: | ---: |
| model name | `End` | 16 | 34% |
| wire id | `Middle` | 12 | 18% (droppable, priority 1) |
| capability tags | `End` | whole block | 10% (droppable, priority 3) |
| connection | `End` | 10 | 24% |
| status badge | `End` | — | reserved at the trailing edge |

The connection column answers *whose quota am I spending?*; it has a floor
precisely so a verbose model name can never take that answer away.

### 4.4 Overflow vocabulary

`nuo_ui_primitives::components::overflow::fit` is the one implementation of
"this string does not fit". The strategy is chosen by the field's **semantics**:

| Field | Strategy | Why |
| :--- | :--- | :--- |
| prose, names, titles | `End` | the meaning lives at the head |
| wire ids, hashes, semver | `Middle` | both ends discriminate (`claude…0219`) |
| paths, breadcrumbs | `Start` | the leaf is the identity |
| the focused row's overflowing cell | `Window` | reveal, don't destroy |

`Fitted::rebase` maps fuzzy-match character indices onto the painted text, so a
truncated cell still highlights the glyphs it actually draws. `fit` flattens
control graphemes to a space (a cell can never break a row); `fit_line` is the
one exception and stops at the first break, appending the marker so the cut is
never silent.

### 4.5 Reveal is focus's privilege

Only the row that owns focus may slide an overflowing cell
(`marquee_offset`, ping-pong, `MarqueeCadence::{hold_ms: 900, step_ms: 180,
tail_hold_ms: 700}`). Every other row stays static: a table in which every row
drifts is unreadable and costs CPU proportional to row count. The dialog reports
`reveal_active` back to the runtime, so the animation heartbeat lives exactly
while something moves and an idle list costs no repaints.

A window is still a truncation and still says so: a leading marker when the head
has scrolled away, a trailing marker when the window stops short of the end
(which is why `window_max_offset` holds a column back). A cell that fits never
moves.

### 4.6 Mechanical governance

`cargo test -p nuo --test architecture_firewall` fails if a truncation helper
builds its own ellipsis, or if a renderer sizes a column by percentage
arithmetic. Both are bug classes, not style preferences: a private truncation is
how two surfaces disagree about which end of a string matters, and a private
percentage is how one column comes to be starved by another's content.

---

## 5. Canonical Disabled / Inert Item Pattern

Per **ADR-0076** (`[INV-TUI-INERT-01]`, `[INV-TUI-INERT-02]`), all selectable list dialogs and option pickers adhere to a strict, universal disabled item contract:

- **Skip Navigation**: Arrow key navigation (<kbd>Up</kbd>, <kbd>Down</kbd>, wrap-around) routes through `rotate_selectable` and skips unusable/locked items automatically. Selection cursors never land on an unusable row.
- **Strict Inertia**: Pointer clicks on disabled rows are consumed as no-ops (they do not focus or activate). Direct activation inputs on disabled rows are inert with zero side effects.
- **Subdued Visual Presentation**: Unusable rows render entirely in `theme.dim()` or `theme.muted()`, with normal font weight (`Modifier::BOLD` is stripped), and on standard `theme.panel()` background (no brand selection band).
- **Gestalt Spacing Firewall**: `ListRow` guarantees an unconditional minimum separation (`GROUP_GAP`) between label/column atoms and trailing badges, preventing text smashing on narrow viewports.
- **Visual Language Grammar**: Lock and status reasons strictly follow the canonical em-dash grammar: `locked — {reason}`.

---

## 6. Footer Hints

Modal footers render keycap hints through `render_modal_footer` (`crates/nuo-tui/src/components/footer.rs`). Each hint carries a priority; when the width cannot fit every hint, lower-priority hints drop before higher-priority ones, so the essential dismiss/confirm affordances always survive truncation.
