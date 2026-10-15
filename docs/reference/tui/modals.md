# TUI Modals Reference

Factual reference for `nuo-tui`'s overlay/modal surfaces: the framework, the sizing presets, and the provider model list's identity-column rule.

This page is cited directly by `crates/nuo-tui/src/overlays/provider/models.rs` (the "name-first, id-fallback" rule).

---

## 1. Surfaces and Framework

The overlay vocabulary is declared in `crates/nuo-tui/src/surfaces/mod.rs`:

- **`DialogKind`** — the full-page dialog set (tools, MCP, skills, permissions, usage stats, quotas, session stats, session trace, models, connections, history search, queue, threads, session tree, switcher, port share).
- **`SheetKind`** — lightweight interaction sheets (permission, question, input injection, model editor, provider preset, OAuth pending, custom provider, provider delete confirm).
- **`OverlaySurface`** / **`Surface`** — the dispatch enum the render loop matches on.

All modals are drawn through one frame: `modal_frame` in `crates/nuo-tui/src/elevation.rs` splits a single header row, a flexible body, and a single footer row, honoring an elevation archetype (structured vs hybrid). Geometry comes from `crates/nuo-tui/src/primitives.rs`.

---

## 2. Modal Sizing Presets

Fixed-size modals (`FixedModalSpec`, in columns × rows):

| Preset | Size |
| :--- | :--- |
| `PROVIDER` | 76 × 80 |
| `SESSIONS` | 82 × 78 |

Content-sized modals (`ContentModalSpec`, min-cols / min-rows / max-cols):

| Preset | Size |
| :--- | :--- |
| `TOOLS` | 64 / 11 / 84 |
| `MCP` | 64 / 9 / 84 |
| `QUEUE` | 66 / 9 / 84 |
| `SESSION_STATS` | 72 / 11 / 80 |
| `SESSION_TRACE` | 80 / 11 / 92 |
| `USAGE_STATS` | 76 / 12 / 86 |
| `MODEL_EDITOR` | 66 / 6 / 60 |
| `OAUTH_PENDING` | 76 / 7 / 80 |
| `CUSTOM_PROVIDER` | 72 / 8 / 80 |
| `PERMISSIONS` | 72 / 7 / 80 |
| `SKILLS` | 72 / 7 / 80 |

---

## 3. Provider Model List: Name-First, Id-Fallback

The provider model modal (`draw_models_modal`, `crates/nuo-tui/src/overlays/provider/models.rs`) renders each model as a two-column row: a **primary identity column** and a **secondary wire-id column**.

**Rule.** The primary column reads the provider's own display name for the model when it publishes one, and falls back to the raw wire id when the catalog publishes no label — so the column is never empty.

- The sizing keys on the *rendered label*: `label = name.unwrap_or(model)`.
- The wire id sits dimmed in the column beside the label, and that column **drops for the whole list** when the body is too narrow to fund a readable id (`id_budget < MIN_ID_COLUMN_WIDTH`, with `MIN_ID_COLUMN_WIDTH = 10`).
- The label column is clamped to at least 18 columns and never wider than the region.

**Identity is unaffected by what is drawn.** Activation, favorites, hiding, and every configuration surface still key on the wire id (`rm.model`). The display name is presentation only, never the wire value.

---

## 4. Footer Hints

Modal footers render keycap hints through `render_modal_footer` (`crates/nuo-tui/src/components/footer.rs`). Each hint carries a priority; when the width cannot fit every hint, lower-priority hints drop before higher-priority ones, so the essential dismiss/confirm affordances always survive truncation.
