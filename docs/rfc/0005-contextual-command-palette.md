# RFC-0005: The Contextual Command Palette

- **Status:** Accepted — implemented; ratified by
  [ADR-0218](../adr/0218-the-command-palette-is-a-pure-projection-of-context.md)
- **Revision:** Final
- **Date:** 2026-11-18
- **Related:** ADR-0035 (scene / dialog / sheet topology), ADR-0077 (categorised
  palette and sigil facets), ADR-0145 (peer focus planes), ADR-0151
  (`[INV-PLANE-01]`, `[INV-PLANE-05]` natural-language palette)
- **Supersedes:** `overlays/command_palette.rs` (`filter_palette_commands`,
  `PaletteEntry`), `keymap::commands_for_palette`, `ComposerState::recent_commands`,
  the `Scope::Dialog(_)` rows of `COMMAND_REGISTRY`, and the `/` palette facet

## 0. Recommendation (TL;DR)

Rebuild the palette as a **pure projection**: `context → sources → entries →
rank → rows`. Every stage is a plain function over plain data, and every stage
can be tested without a terminal or an `App`.

The palette stops being a list of global commands and becomes the answer to
*"what can I do here, right now?"*. It reads the scene under it, the dialog
under it, the focus plane and the run state. It lists the verbs that **exist**
in that context. Verbs that exist but **cannot run right now** are dimmed and
say why.

Picking an entry runs **the same `InputAction` its key produces**, through the
same dispatcher. The palette has no second implementation of anything.

## 1. Problems with the current palette

| # | Defect | Consequence |
|---|---|---|
| P1 | It only offers `Scope::Global` registry rows. | It has nothing to say about the Diff, Command-log, Dashboard or Settings scenes, or about the dialog under it. That is exactly the context where users need guidance. |
| P2 | `AppContext` is assembled by hand in five places (palette render, ↑/↓, pointer, Enter, and the dialog-key overlay), and the copies differ (`is_responding` comes from `running_sessions` in one and `viewed_chrome()` in another). | The highlighted row and the executed row can be different entries. |
| P3 | `Scope::Dialog(_)` registry rows have no effect: `execute_command_by_id` falls into `_ => {}`. | 28 declared commands could never run. They exist only to feed the `?` key overlay, as text duplicated next to the real resolver in `modal_keys.rs`. |
| P4 | Right-hand text is a free-form `hint` string (`"Esc Esc (running)"`, `"/trace"`). | Keycaps can drift from the resolver, and user remaps are not shown. |
| P5 | Recency is keyed on label or slash strings (`Vec<String>`). | Renaming a label silently drops history. |
| P6 | The empty view sorts by category and then alphabetically. | Curated order is lost, and the user's current context has no priority. |
| P7 | The prefix filter repeats the same match block four times, and the `/` facet contradicts `[INV-PLANE-01]`. | It is hard to change and it advertises slash routing. |
| P8 | Matching scores a concatenated `label hint slash description` string. | Description noise outranks title hits, and there are no match positions to highlight. |
| P9 | `SwitcherTarget` / `switcher_rows` and the `hint()` helpers survived from the pre-palette quick switcher, and only their own test used them. | Dead code that looks like a second navigation model. |

## 2. Model

```text
             ┌──────────── palette::context ─────────────┐
   &App ───▶ │ PaletteContext::capture(app)  (one place) │
             └───────────────────┬───────────────────────┘
                                 ▼
             ┌──────────── palette::source ──────────────┐
             │ dialog verbs │ scene verbs │ tabs │ registry │  relevance (structural)
             └───────────────────┬───────────────────────┘
                                 ▼   Vec<Entry>
             ┌──────────── palette::rank ────────────────┐
             │ Query::parse → facet filter → fuzzy score │  availability, recency
             │ → stable order                            │
             └───────────────────┬───────────────────────┘
                                 ▼   Projection { entries, rows }
             ┌──────────── palette::view ────────────────┐
             │ header · query · sections · detail · keys │
             └───────────────────────────────────────────┘
```

### 2.1 Context

`PaletteContext` is captured **once per use** from `&App` by a single function.
It describes the state *under* the palette:

- the scene;
- the dialog beneath the palette (when the palette sits on one);
- the focus plane and the focused entry's kind;
- run state, session, selection, queue depth and notices;
- the open tabs;
- the effective key overrides.

The palette's own overlay is never part of its context, so `has_overlay` means
"something other than the palette is open".

### 2.2 Relevance and availability

There are two separate questions. The old code merged them.

- **Relevance** is structural: *does this verb exist here?* A Diff verb is not
  relevant on the Dashboard, and a Queue-dialog verb is not relevant without
  the Queue dialog. An irrelevant entry is **absent**. Hiding it is context,
  not clutter.
- **Availability** is transient: *can it run right now?* "Interrupt" is
  relevant in a thread but unavailable while idle. An unavailable entry is
  **present, dimmed, sorted after available ones, and carries its reason**.
  The palette teaches the user what blocks the entry. Pressing Enter on it
  shows the reason and does not close the palette.

### 2.3 Entry

```rust
struct Entry {
    key: EntryKey,           // stable identity: recency, selection, tests
    title: Cow<str>,         // what is matched and highlighted
    detail: &'static str,    // one-line guidance shown for the selection
    keywords: &'static str,  // extra match terms (aliases, slash forms)
    section: Section,        // where it is grouped
    keycap: Option<Key>,     // the *effective* chord, never prose
    danger: DangerLevel,
    availability: Availability,
    effect: Effect,          // what Enter does
}

enum Effect {
    Command(CommandId),      // registry executor (execute_command_by_id)
    Action(InputAction),     // the exact action the keycap resolves to
}
```

`EntryKey` is a `&'static str` for declared verbs (`"diff.next-file"`) and a
derived key for tabs (`"tab:<id>"`). Labels can change without losing history.

### 2.4 Sources

Each source is a function `fn(&PaletteContext, &mut Vec<Entry>)`. Adding a new
context means adding a table, not touching the ranker or the view.

| Source | Relevant when | Section |
|---|---|---|
| `dialog` | a dialog sits under the palette | `Dialog(kind)` |
| `scene` | no dialog is open: the current scene's verbs (behind a dialog they would act on the wrong surface) | `Scene(kind)` |
| `tabs` | more than one tab is open | `Tabs` |
| `registry` | `Scope::Global \| Session` rows with palette disclosure | `GoTo`, `Thread`, `Actions`, `App` |

Scene and dialog verbs are declared in **one table**,
`palette::verbs`. Each scene and dialog has an exhaustive `match`, so a new
`SceneKind` or `DialogKind` does not compile until someone decides what its
palette says. The same table feeds the in-dialog `?` reference overlay. The
dead `Scope::Dialog(_)` rows and their `CommandId`s are deleted.

### 2.5 Query and facets

`Query::parse` takes an optional leading sigil, then the needle:

| Sigil | Facet | Keeps |
|---|---|---|
| *(none)* | All | everything |
| `:` | Here | dialog and scene verbs only ("what does *this* screen do?") |
| `@` | Places | tabs and go-to destinations |
| `>` | Actions | every verb that is not a place |

The `/` facet is removed: the palette is not the slash catalogue
(`[INV-PLANE-01]`). Slash forms stay as **keywords**, so typing `/stats` still
finds *Thread Stats*.

### 2.6 Ranking

Ranking is deterministic and pure:

1. Available entries come before unavailable ones: within each group in the
   grouped view, and across the whole list when searching.
2. With no needle, entries are ordered by section (dialog, scene, recent, tabs,
   go-to, thread, actions, app), then by declaration order. A curated table
   order is meaningful, and alphabetising discards it. Recent entries (up to 5)
   are lifted into a `Recent` section.
3. With a needle, the title is matched first and the matched positions are
   highlighted. Keywords and detail are matched only as a fallback, with a
   fixed penalty and no highlight. Results sort by score, then by a recency
   bonus and a small context bonus for dialog and scene verbs, then by section
   and declaration order.

### 2.7 Execution

`InputAction::PaletteRun` is handled at the top of `dispatch_action`:

1. Project again from the current `App` (the same function render used).
2. Take the selected entry. If it is unavailable, show a toast with the
   reason, keep the palette open, and stop.
3. Pop the palette, then record the entry in `Recents`.
4. For `Effect::Command(id)`, call `execute_command_by_id`. For
   `Effect::Action(a)`, re-enter `dispatch_action(a)`. A palette pick and a
   keystroke share one code path.

Because the palette pops **before** dispatch, the dialog or scene under it is
the active surface again. Its verbs act on its highlighted row exactly as the
key would.

### 2.8 View

```text
╭ Commands · Diff viewer ────────────────────────────────╮
│ ❯ ne                                                   │
│────────────────────────────────────────────────────────│
│ Diff viewer                                            │
│   Next file                                         n  │  ← match highlighted
│ Go to                                                  │
│   New Tab                                       Alt-t  │
│   Notices                          no notices retained │  ← dimmed + reason
│────────────────────────────────────────────────────────│
│ Jump to the next changed file                          │  ← detail of selection
│ ↑↓ move  : here  @ places  > actions     Enter run  Esc│
╰────────────────────────────────────────────────────────╯
```

The section headers are not selectable rows. ↑/↓ and the mouse address entries
only.

## 3. Invariants

- **`[INV-PAL-CTX-01]`** The palette context is captured by exactly one
  function. Render, navigation, pointer and execution all project through
  `palette::project`.
- **`[INV-PAL-CTX-02]`** An entry's keycap is the binding that fires in that
  context. Tests resolve every declared scene and dialog keycap through the
  real resolver (`resolve_scene_key_motion` / `resolve_modal_key`) and assert it
  yields the entry's `Effect::Action`.
- **`[INV-PAL-CTX-03]`** A palette pick dispatches the same `InputAction` or
  `CommandId` as its key. The palette implements no verb itself.
- **`[INV-PAL-CTX-04]`** Irrelevant entries are absent. Unavailable entries are
  present, dimmed, after available ones, and carry a reason. Enter on them does
  nothing except explain.
- **`[INV-PAL-CTX-05]`** Scene and dialog verb tables are exhaustive over
  `SceneKind` and `DialogKind`, with no wildcard arm.

## 4. Rejected alternatives

1. **Keep the registry as the only source and add `Scope::Scene(_)` rows.** This
   keeps one table, but the registry holds `CommandId`s that need a bespoke
   executor. Scene verbs already *are* `InputAction`s with real handlers.
   Routing them through `CommandId` would add a second executor for each verb.
   That is the P3 failure again.
2. **Cache the projection on the dialog entity during render.** This would make
   "what you saw is what runs" literal, but render would mutate state and the
   cache could go stale. Re-projecting is deterministic over unchanged state
   and costs microseconds.
3. **Hide unavailable entries.** This is cleaner, but it removes the guidance:
   searching "interrupt" while idle would return nothing, and the user could
   not tell whether the verb does not exist or just cannot run yet.
4. **Merge the server slash catalogue into the palette.** This violates
   `[INV-PLANE-01]` and needs an async catalogue. The composer's completion menu
   already owns that vocabulary.
