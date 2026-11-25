---
id: ADR-0253
title: "One Owner per Field: ThreadInstance for the Thread, Viewport for the Terminal"
status: accepted
date: 2026-11-25
scope: protocol/wire, server/transport, tui/state, tui/event-loop, tui/workspaces, tui/tabs, persistence/drafts, governance
superseded_by: null
negative_knowledge: true
---

# 0253. One Owner per Field: ThreadInstance for the Thread, Viewport for the Terminal

- **Status**: Accepted
- **Date**: 2026-11-25
- **Amends**:
  [ADR-0251](0251-multiplexed-thread-sessions-and-resident-per-thread-workspaces.md) — the foreground stops
  being an exception: it lives in the same `ThreadInstance` map as any resident thread, and wire protocol
  advances to v35 to carry `ThreadKey::generation`;
  [ADR-0247](0247-a-draft-belongs-to-a-thread-and-outlives-every-view-transition.md) — `ComposerState` holds
  which thread owns the live composer buffers, `DraftLedger` holds parked draft content, and `ThreadInstance`
  holds no duplicate draft copy;
  [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md) §2 — a tab records which
  thread it points at; the thread's own pose is not duplicated onto the tab;
  [ADR-0018](0018-on-demand-session-review-diagnostics-and-multi-process-history-union.md) — the emptiness
  predicate has one definition and every layer derives from it;
  [ADR-0186](0186-state-is-partitioned-and-giants-are-prohibited.md) — the partition is by *owner*, and one
  owner may not hold two copies of one fact.

---

## Context and Problem Statement

The client's per-thread state has no single owner. It is spread across eight places, and six of those hold a
*different copy of the same fact*.

### 1. Measured duplication (field-level, verified on the tree)

```text
ThreadWorkspace           36 fields
TranscriptState           29 fields
        ├─ intersection    17 fields   (same name, same type, copied by hand)
        └─ remainder       12 fields   (all terminal/viewport state)
ThreadWorkspace-only 19 → 15 of them also exist flat on SessionState/TelemetryState
SessionChrome              9 fields → 4 of them duplicate the above
TabContextState            2 fields: focus, scroll
```

Same fact, multiple owners:

| Fact | Owners | Where |
| :--- | :--- | :--- |
| `phase` | **3** | `SessionState` · `ThreadWorkspace` · `SessionChrome` |
| `round_count` | **3** | same |
| `current_turn` | **3** | same |
| `round_started_at` | **3** | same |
| `scroll` | **3** | `TranscriptState` · `ThreadWorkspace` · `TabContextState` |
| `focus` | **3** | same |
| `loop_status` | 2 | `SessionState` · `ThreadWorkspace` |
| `provider` / `model` / `workspace` / `role` | 2 | `SessionState` (flat, foreground only) · `ThreadWorkspace` |

### 2. The copies are kept in step by hand

Four functions exist only to reconcile them:

```text
checkpoint_active_tab_context   tab        ← transcript      (focus, scroll)
install_workspace               transcript ← workspace       (36 fields)
park_active_workspace           workspace  ← transcript      (36 fields)
apply_chrome                    SessionState ← SessionChrome (4 fields)
```

`park_active_workspace` and `install_workspace` are two ~60-line field-by-field moves of the same 36 fields,
written twice in opposite directions. **A field added to one and forgotten in the other is a silent data
loss with no failing test** — the compiler is satisfied because the struct field is initialized to `Default`.

### 3. The foreground is an exception, so every mutation has two code paths

`apply_transcript` writes `app.transcript.messages`; `apply_transcript_for` writes
`workspaces[thread_id].messages`. Two implementations of one operation, differing in which copy they touch,
selected by `is_background_thread`. The same split exists for harness snapshots, session phases, and chrome.

### 4. Instances can be created from nothing

`apply_transcript_for`, `apply_harness_snapshot`, and the phase applier use `.or_default()` on `workspaces`
— so a `thread_id` that was never subscribed, never hydrated, and never had a tab materializes a resident
instance.

### 5. The emptiness rule has three definitions

| Layer | Predicate | Counts a command row? |
| :--- | :--- | :--- |
| durable authority | `ThreadData::is_user_facing_empty` | no (explicitly excluded) |
| persistence gate | `ThreadSession::should_skip_persist` | inherits the above |
| client recycle test | `App::is_current_thread_empty` | **yes** — reads `transcript.messages` |

Nothing keeps them in agreement. One non-dialogue row in `transcript.messages` flipped the client's recycle
predicate while the durable store still considered the thread empty.

## Decision Drivers

- **One fact, one owner.** A duplicated field is a silent-loss generator whose reconciliation cost is paid
  on every future field.
- **The compiler must be the gate.** Two hand-written 60-line copies cannot be covered exhaustively by
  tests; a single declaration is compiler-verified.
- **Ownership, not convenience, decides the split.** State that persists per tab across tab switches belongs
  on `ThreadInstance`; terminal-global ephemeral state (hit-test tree, in-flight mouse drag, hover target,
  terminal geometry, animation clocks, client-global logs) belongs on `Viewport`.
- **Rust split-borrow ergonomics.** `Viewport` and `Threads` must be sibling fields on `App` so render and
  input paths can borrow `&mut app.threads` (foreground `ThreadInstance`) and `&mut app.viewport`
  simultaneously.
- **The draft's content already has a durable owner.** ADR-0247 established that unsent text belongs to the
  thread and survives every view transition; `ThreadInstance` must not duplicate it.
- **"Empty" is a durable fact, not a client impression.** Client recycle and store persistence must agree.

## Architectural Decisions

### 1. Two state types, split by owner, as sibling fields on `App`

```rust
/// Everything that belongs to one resident thread and survives tab switches.
/// One shape for foreground and background alike — no second type, no second path.
pub struct ThreadInstance {
    // Identity (also the key into `DraftLedger`, ADR-0247)
    pub key: ThreadKey,
    pub session_phase: protocol::ThreadPhase,

    // Content & thread-local layout caches
    pub messages: Vec<TranscriptMessage>, // future follow-up: adopt ADR-0117 `ThreadDoc`
    pub layout_height_cache: HeightCache,
    pub command_log: Option<CommandLog>,
    pub diff_viewer: Option<DiffViewerState>,

    // View pose of this thread (preserved across tab switches)
    pub scroll: usize,
    pub follow_bottom: bool,
    pub content_lines: usize,
    pub max_scroll: usize,
    pub sticky_step: Option<usize>,
    pub sticky_summary_line: Option<usize>,
    pub pin_summary_line: Option<usize>,
    pub scroll_settle_pending: bool,
    pub transcript_changed_pending: bool,
    pub selection: SelectionState,
    pub focus: Focus,
    pub focus_stack: Vec<ZoomFrame>,
    pub search: TranscriptSearchState,

    // Projections mirrored from the server
    pub chrome: SessionChrome,
    pub attribution: Attribution,       // provider, model, workspace, role
    pub harness: HarnessProjection,     // loop_status, harness_retry_pending, unattended, confined
    pub context: ContextProjection,     // session_tree, session_context, token_report, context_plan
}

/// Everything that belongs to this terminal process, independent of which thread tab
/// is active. Lives as a sibling field on `App` alongside `threads: Threads` so render
/// and event handlers can split-borrow `&mut app.threads` and `&mut app.viewport`.
pub struct Viewport {
    pub ui: ComponentTree,              // mount tree + committed hit-test geometry
    pub drag: SelectionDrag,            // in-flight mouse gesture
    pub hovered_step: Option<usize>,    // pointer hover target
    pub view_height: u16,
    pub modal_body_height: u16,
    pub spinner_epoch: Instant,         // animation clocks
    pub carousel_epoch: Instant,
    pub transcript_layout: Strategy,    // client layout configuration
    pub expand_auto_scroll: bool,
    pub reasoning_default_expanded: bool,
    pub logs: SurfaceLogs,              // client-global receipts (ADR-0121)
    pub host_console_log: Vec<ConsoleLine>, // dashboard console log
}

pub struct Threads {
    instances: HashMap<String, ThreadInstance>,
    foreground: Option<String>,
}
```

The split is derived from lifetime and ownership: the twelve `TranscriptState` fields moved into `Viewport`
are precisely the terminal-global ephemeral and configuration fields that never swap when switching tabs.
The remaining per-thread fields live in `ThreadInstance` (23 fields, well within ADR-0186's 80-field ceiling).

### 2. `ThreadKey` carries a server-assigned generation; instances come only from an authority

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadKey {
    pub id: String,
    /// Monotonic epoch assigned by the server per subscription/re-key
    /// (initial attach = 1; incremented on in-place `/new`, `/resume`, `/fork`,
    /// or re-subscription).
    pub generation: u64,
}
```

- **Wire contract (Protocol v35)**: `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` advance from `34` to `35`.
  `Wire::Welcome` and `Wire::Subscribed` carry `generation: u64`, and `Wire::Response` carries
  `generation: u64` (defaulting to `0` for non-thread-scoped responses). Any thread-scoped frame whose
  `generation` is older than the resident `ThreadInstance.key.generation` is dropped.
- **`SessionGate` (`shell/session_gate.rs`) is deleted.** Under ADR-0251 multiplexed threads, distinct
  threads already route by `thread_id`; same-thread resets/re-keys are discriminated by `generation`, so
  the superseded `HashSet<String>` is obsolete.
- **Explicit construction only.** `ThreadInstance` is created only by explicit registration (`Welcome`,
  `Subscribed`, or initial startup binding). Implicit `.or_default()` / `.or_insert_with` creation on the
  instance map is removed.

### 3. A tab points at a thread; it does not hold the thread's state

```rust
pub struct ClientTab {
    pub kind: TabKind,          // Thread(String) | Dashboard | Settings
    pub title: Option<String>,  // authentic title (ADR-0050)
}
```

`TabContextState` (`focus`, `scroll`) is deleted. ADR-0091 `[INV-TAB-12]` (switching tabs preserves where
the user was without leaking context) is satisfied by `ThreadInstance` owning its own pose.

### 4. The draft: `ComposerState` owns live buffer identity, `DraftLedger` owns content

`ThreadInstance` does **not** carry a duplicate `draft_owner` field (its identity is already `key.id`).
`ComposerState` holds the single `draft_owner: Option<String>` naming which thread currently owns the live
composer buffers, while `DraftLedger` (`shell/draft_store.rs`) remains the sole content store keyed by
`thread_id` (ADR-0247).

### 5. "Empty" has one definition and two directions of conformance

- A thread is empty iff it has **no content-bearing dialogue**, no parked draft in `DraftLedger`, and no
  active round (`round_count == 0`). Command rows, turn anchors, and receipts are **not** dialogue and do
  not make a thread non-empty.
- `ThreadInstance::is_dialogue_empty()` derives directly from `ThreadDoc` (filtering out non-dialogue
  entries), matching `ThreadData::is_user_facing_empty`.
- A conformance test verifies both predicates agree over representative states (empty, dialogue-only,
  command-only, interrupt-only, round-started) in both directions.

### 6. Foreground is not special

`Threads::foreground` is a key, not a copy. Reading or mutating the foreground thread and reading or
mutating a resident background thread are the same operation on `ThreadInstance`. `apply_transcript` and
`apply_transcript_for` collapse into one function parameterized by `thread_id`.

## Normative Invariants Established

- `[INV-INSTANCE-01]` **One owner per field.** A thread-scoped field has exactly one in-memory owner
  (`ThreadInstance`); a terminal-scoped field has exactly one (`Viewport`). Enforced by single declaration
  and by deleting duplicate fields from `SessionState`, `TranscriptState`, and `TabContextState`.
- `[INV-INSTANCE-02]` **No hand-written workspace reconciliation.** `ThreadWorkspace`, `TabContextState`,
  `park_active_workspace`, `install_workspace`, `checkpoint_active_tab_context`, and `apply_chrome` cease
  to exist. Enforced by a governance gate asserting none of those symbols exist in `crates/nuo`.
- `[INV-INSTANCE-03]` **Instances come from an authority.** A `ThreadInstance` is constructed only through
  explicit registration; `.or_default()` / `.or_insert_with` on the instance map is prohibited, and stale
  generations are dropped on apply.
- `[INV-INSTANCE-04]` **A tab points, it does not hold.** `ClientTab` holds no transcript or view pose
  fields (`TabContextState` deleted).
- `[INV-INSTANCE-05]` **The draft has one content owner.** `DraftLedger` is the sole content store;
  `ThreadInstance` holds no copy of draft text or duplicate owner id.

## Consequences & Trade-offs

- **Positive**: Eight owners collapse to two (`ThreadInstance` + `Viewport`). Four manual reconciliation
  functions disappear, eliminating silent field drops on tab switch.
- **Positive**: Foreground and background share one mutation path, preventing foreground/background drift.
- **Positive**: `ThreadDoc` (ADR-0117) is adopted in production, making raw `Vec::push` of non-admitted
  rows a compile error.
- **Positive**: `SessionGate` and `.or_default()` phantom instances are deleted.
- **Negative & incurred**: Mechanical migration touching `app.transcript.*` and `app.session.*` call sites,
  sequenced in three compiling steps.
- **Negative & incurred**: Advancing `PROTOCOL_VERSION` to `35` for `generation` requires coordinated
  client/server update (already standard for this workspace's unified binary, ADR-0005).
- **Negative & accepted**: Accessing the foreground thread goes through `HashMap` lookup by foreground key;
  negligible compared to terminal rendering and Markdown layout.

## Discarded Alternatives & Negative Knowledge

### Alternative A — Keep both `TranscriptState` and `ThreadWorkspace` and share a trait

- **Why considered**: Smaller diff.
- **Why rejected**: Leaves two declarations of the same 17 fields and keeps `park`/`install` alive.

### Alternative B — Generate `park`/`install` with a macro

- **Why considered**: Eliminates hand-copy omissions without restructuring.
- **Why rejected**: Automates the bug generator while keeping two declarations of every field.

### Alternative C — One state struct for both thread and viewport

- **Why considered**: Fewer types.
- **Why rejected**: Mixes two incompatible lifetimes. `ui` (committed hit-test geometry), `drag` (in-flight
  mouse gesture), `hovered_step`, `view_height`, animation clocks, and client-global `logs` belong to the
  terminal viewport and must persist across tab switches rather than resetting or swapping with each thread.

### Alternative D — `Arc<Mutex<ThreadInstance>>` shared across views

- **Why considered**: Multiple readers without map lookup.
- **Why rejected**: Unnecessary lock contention inside a single-threaded event loop; `Threads` map +
  sibling `Viewport` gives zero-lock split borrows.

### Alternative E — Keep `messages.is_empty()` as the client recycle rule

- **Why considered**: Already exists.
- **Why rejected**: `messages` is a render buffer containing command rows and turn anchors; deriving
  emptiness from dialogue facts aligns client and store without read-time text filtering.

## Implementation

Three steps, each compiling, each shippable, tests green after each:

| # | Step | Verification |
| :--- | :--- | :--- |
| 1 | Move the 12 terminal-scoped fields out of `TranscriptState` into `Viewport` as a sibling field on `App`. Pure field move; no semantics change. | `cargo test -p nuo` |
| 2 | Merge remaining `TranscriptState` + `ThreadWorkspace` + per-thread `SessionState`/`SessionChrome` fields into `ThreadInstance` inside `Threads`; delete `park_active_workspace`, `install_workspace`, `checkpoint_active_tab_context`, `apply_chrome`, `TabContextState`; collapse `apply_transcript`/`apply_transcript_for`. | `[INV-INSTANCE-01]`, `[INV-INSTANCE-02]`, `[INV-INSTANCE-04]` |
| 3 | Add `ThreadKey { id, generation }` and bump `PROTOCOL_VERSION` to `35`; delete `SessionGate`; remove `.or_default()` on instance map; align emptiness predicate with durable authority. (Follow-up: adopt `ThreadDoc` over `messages`.) | `[INV-INSTANCE-03]`, `[INV-INSTANCE-05]` |

Verification:

```bash
cargo check --workspace --all-targets
cargo test -p nuo-protocol -p nuo-server -p nuo
cargo test -p nuo-tests --test integration governance
cargo clippy --workspace --all-targets
```

## Links

- Amends: [ADR-0251](0251-multiplexed-thread-sessions-and-resident-per-thread-workspaces.md),
  [ADR-0247](0247-a-draft-belongs-to-a-thread-and-outlives-every-view-transition.md),
  [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md) §2,
  [ADR-0018](0018-on-demand-session-review-diagnostics-and-multi-process-history-union.md),
  [ADR-0186](0186-state-is-partitioned-and-giants-are-prohibited.md)
- Applies: [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md),
  [ADR-0123](0123-attach-failure-typing-stale-tab-retirement-and-identity-addressed-session-rebasing.md),
  [ADR-0166](0166-a-tool-call-has-one-identity-and-the-tool-step-reducer-is-idempotent.md),
  [ADR-0179](0179-one-capability-one-producer-and-no-ghost-builtins.md)
- Related: [ADR-0252](0252-one-action-manifest-declared-primitives-and-compositions.md)
- Code: `crates/protocol/src/lib.rs`, `crates/server/src/transport/`,
  `crates/nuo/src/shell/app/{state,workspaces,surfaces,composer}.rs`,
  `crates/nuo/src/shell/event_loop/{apply,sync}.rs`, `crates/nuo/src/shell/session_gate.rs`
