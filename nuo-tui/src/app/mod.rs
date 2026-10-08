//! The TUI's central application state ([`App`]) plus the `Modal` kind and
//! the `impl App` blocks that hold pure state-management methods.
//!
//! Input-box completion lives in [`crate::completion`]; the event/render
//! loop and shared runtime live in `crate::event_loop`. Everything
//! else that mutates `App` either lives here (state navigation, focus,
//! sticky/pinned step bookkeeping) or in `completion.rs` (the only other
//! `impl App` block).
//!
//! [`crate::completion`]: crate::completion

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use nuo_wire::{
    AgentRequest, ConnectionAuth, ImagePart, LoopStatus, ParentStatus, PermissionRequest,
    ProviderPickerSnapshot, SessionOverview, UserQuestionRequest,
};

use crate::chrome::tasks_bar::BackgroundTaskItem;
use crate::completion::CompletionItemKind;
use crate::composer_attachments;
use crate::event_loop::resolve_focused_mut;
use crate::fuzzy;
use crate::model::document::{NoticeSeverity, TranscriptMessage};
use crate::model::layout::InteractiveTarget;
use crate::model::selection::{SelectionDrag, SelectionState};
use crate::providers::{
    ConnectionTemplate, CustomField, RankedModel, RankedProvider, edit_fields,
    models_flat_filtered_from, providers_filtered_from,
};
use crate::render::Theme;

use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedDispatchState {
    /// Staged in the outbox, waiting for its turn (auto-drain or a recall /
    /// delete / reorder from the Queue modal).
    Waiting,
    /// A fresh round is being started for this item (`FollowUp` sent,
    /// `FollowUpStarted` not yet received).
    Dispatching,
}

/// Target queue mode for the live composer while a round is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComposerSendMode {
    /// Send as steering input at the next safe turn boundary.
    #[default]
    Steer,
    /// Send as follow-up input when the agent finishes active work.
    FollowUp,
}

/// A user message owned by the compact outbox (the **next-round** queue).
///
/// Follow-up content and a busy-Enter steer whose round ended before admission
/// (handed back by `UserInputUnavailable`) wait here to become the **next**
/// round's prompt. The item is intentionally absent from the transcript until
/// the harness dispatches it, so pending state never scrolls away or
/// masquerades as conversation history. (A *live* insert is different: it is
/// a transcript entry from the moment it is sent — see
/// `DeliveryStatus::Queued` — and never passes
/// through the outbox.)
#[derive(Debug, Clone)]
pub struct QueuedDispatch {
    pub id: String,
    pub session_id: String,
    pub state: QueuedDispatchState,
    /// The user's literal prompt text, sent verbatim to the agent on dispatch.
    pub text: String,
    /// When the item was staged (epoch ms). Surfaced by the persistent queue
    /// bar and the Queue modal as the item's send time, distinct from the
    /// `sent_at_ms` stamped on the dispatch request — this is *queued-at*.
    pub queued_at_ms: u64,
    /// Pasted images staged for this message (Ctrl+V). Empty for plain text.
    pub images: Vec<ImagePart>,
    /// Large pasted text blocks staged behind `[Pasted text #N +M lines]`
    /// chips inside `text`. Empty for plain-text drafts. Order matches the
    /// chip numbering, so the Nth chip expands to `pending_text_pastes[N-1]`.
    pub text_pastes: Vec<String>,
}

/// The attachments staged behind a recorded history entry, retained **in
/// memory** so ↑/↓ and Ctrl+R recall can restore a just-sent / interrupted
/// message's images and large pastes. Keyed by the same `(text, session_id)`
/// identity [`nuo_wire::merge_history`] uses, so a recall finds the
/// payloads that shipped with the exact prompt text.
///
/// Deliberately **not** persisted: input history in SQLite is rebuildable cosmetic
/// telemetry, and base64 image blobs would balloon the database and
/// duplicate conversation data. The cache lives for the process lifetime
/// (capped, newest-first) and is re-seeded on every send, which is exactly
/// the window the interrupt → ↑/↓ → resend flow needs.
#[derive(Debug, Clone, Default)]
pub struct HistoryAttachments {
    pub images: Vec<ImagePart>,
    pub text_pastes: Vec<String>,
}

/// Outcome of [`App::recall_queued`]. Every queued dispatch is a next-round
/// item, so recall always restores the newest staged message into the
/// composer immediately (no agent roundtrip to cancel).
pub enum RecallQueued {
    Restored(QueuedDispatch),
}

/// Which surface owns the terminal cursor right now — the single source of
/// truth that the event loop's hide/show state machine, the immediate
/// pre-draw cursor re-sync, and the composer's `show_caret` flag all derive
/// from.
///
/// The terminal cursor is what the host terminal's IME anchors its
/// composition window to, so the owner must be exactly the one text-input
/// surface the user is typing into — or [`Self::None`] when no such surface
/// exists (a transcript step has keyboard focus, the view is zoomed into an
/// subagent task, or a read-only / decision modal is open). In the `None` case
/// the cursor is hidden so the IME has no stale anchor to bind to, which is
/// the bug that previously let the IME "drift" when a disclosure was
/// clicked mid-composition: the caret left the composer but the cursor
/// stayed visible at its old coordinate.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum CaretOwner {
    /// The live chat composer (no overlay, no subagent zoom, no transcript-step focus).
    Composer,
    /// A text field belonging to a non-conversation root scene — today the
    /// Dashboard's inline new-session / prompt task line, whose text is the
    /// composer buffer but whose field is the scene's own footer. Distinct
    /// from [`Self::Composer`] because the composer's readline family and
    /// selection handling must not apply to a scene prompt.
    Scene,
    /// An active overlay surface that renders its own caret (dialog search, sheet form, etc.).
    Overlay,
    /// No text-input surface is active — the cursor must be hidden.
    None,
}

/// Which end of an active input selection the caret should adopt when the
/// selection is broken. `Head` is the edge nearest the hidden caret — the
/// point where the mouse button was released for a drag selection — while
/// `Tail` is the opposite end (where the drag began).
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum SelectionEdge {
    Tail,
    Head,
}

/// Capturable snapshot of the main transcript's scroll position, saved when
/// zooming into a nested view and restored on return.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct ScrollSnapshot {
    pub offset: u16,
    pub follow_bottom: bool,
}

/// One frame on the focus stack: the subagent task call-id plus the parent
/// view's scroll snapshot, restored verbatim when the frame is popped.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoomFrame {
    pub call_id: String,
    pub saved_scroll: ScrollSnapshot,
}

/// Which button is focused in the provider-delete confirm overlay
/// ([`App::pending_provider_delete`]). `Cancel` is the safe default — Enter
/// dismisses without deleting; the user must move focus to `Delete` to destroy
/// the provider. The derive places `Default` on the first variant (`Cancel`),
/// matching the "safe-default" contract documented above.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderDeleteChoice {
    #[default]
    Cancel,
    Delete,
}

/// Transient state representing an ongoing provider retry countdown or execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRetryState {
    pub attempt: usize,
    pub max_attempts: usize,
    pub retry_at: std::time::Instant,
    pub failure: String,
}

impl ProviderRetryState {
    pub fn summary(&self, now: std::time::Instant) -> String {
        let retry = self.attempt.saturating_sub(1);
        let max_retries = self.max_attempts.saturating_sub(1).max(retry);
        let timing = if now < self.retry_at {
            format!(
                "next in {}",
                format_retry_duration(self.retry_at.saturating_duration_since(now))
            )
        } else {
            format!(
                "running for {}",
                format_retry_duration(now.saturating_duration_since(self.retry_at))
            )
        };
        format!("retry {retry}/{max_retries} ({timing})")
    }
}

pub fn format_retry_duration(duration: std::time::Duration) -> String {
    let millis = duration.as_millis() as u64;
    if millis >= 10_000 {
        format!("{}s", millis.div_ceil(1_000))
    } else {
        format!("{:.1}s", millis as f64 / 1_000.0)
    }
}

/// The view-scoped chrome of one session: the typed activity phase, the
/// responding flag, and the structural round/turn counters. Each session —
/// the primary and every live `/btw` aside — owns an entry in
/// [`App::session_chrome`], and a view renders exclusively from the entry of
/// the session it displays ([`App::viewed_chrome`]). This is what keeps an
/// aside view from inheriting the primary's activity bar (and vice versa):
/// before this type existed these were single global fields, so whichever
/// view was focused showed the *primary's* state no matter which session was
/// actually streaming.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionChrome {
    /// Typed activity phase (None when idle). Per session: a background
    /// aside's live phase lives in its own entry, invisible to the main view.
    pub phase: Option<crate::phase::Phase>,
    /// Whether this session currently has a live round (drives the
    /// breathing/spinner animation and Esc-to-interrupt arming).
    pub responding: bool,
    /// Round counter for this session (Activity modal's `round N`).
    pub round_count: u64,
    /// Current turn within this session's round (1-indexed for display).
    pub current_turn: u64,
    /// When this session's current round started (elapsed-timer segment).
    pub round_started_at: Option<std::time::Instant>,
    /// Whether this session has a stopped round parked for `/retry`
    /// (ADR-0128). Mirrored from the session-scoped harness snapshot — the
    /// authoritative durable resume point, not a transcript scan — so the
    /// hint bar only offers `/retry` for a round that actually stopped
    /// before completing.
    pub can_retry: bool,
    /// Latest completed principal ReAct turn's performance sample. Kept
    /// session-scoped so primary and `/btw` views never borrow each other's
    /// hint-bar measurement.
    pub last_turn_performance: Option<nuo_wire::TurnPerformanceSnapshot>,
    /// The transport setback this session's in-flight model request reported
    /// (`RetryScheduled`): a retry countdown rendered as a clause beside the
    /// activity label, never as the label itself (see `crate::phase` rule 2).
    ///
    /// Session-scoped for the same reason as `phase`: a background aside
    /// backing off against a rate-limited upstream must not paint a retry
    /// countdown onto the primary view's bar ([`App::provider_retry`] is the
    /// primary's own slot).
    ///
    /// **Lifetime:** no producer ever clears this. It is retired — together
    /// with the phase it annotates — by the first phase write that is not
    /// [`crate::phase::Phase::AwaitingModel`], which
    /// [`SessionChrome::set_phase`] enforces as the single writer of both
    /// (ADR-0235).
    pub transport_setback: Option<ProviderRetryState>,
}

impl SessionChrome {
    /// Write this session's activity phase, retiring its transport-setback
    /// clause under [`crate::phase::ends_transport_setback`] (ADR-0235).
    ///
    /// This is the **only** way anything in the process ends a setback clause,
    /// and the rule it applies is derived from the phase rather than from the
    /// event that produced it: the response translator publishes a setback and
    /// then forgets about it, so a wire event it does not know about cannot
    /// leave a dead countdown on the bar.
    pub fn set_phase(&mut self, phase: Option<crate::phase::Phase>) {
        if crate::phase::ends_transport_setback(phase.as_ref()) {
            self.transport_setback = None;
        }
        self.phase = phase;
    }
}

/// Whether [`App::adopt_as_draft`] may clobber a composer that currently
/// holds unsent, unsaved work. Explicit user gestures (queue recall,
/// Ctrl+R insert) may — the user asked for it. Asynchronous events
/// (a Phase-1 unsend restore) may not — the in-progress draft they would
/// destroy was never sent anywhere and has no other copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftAdoption {
    /// Replace whatever the composer holds. For user-initiated paths where
    /// the current content is either sent or superseded.
    Replace,
    /// Adopt only if the composer is idle (empty text, no staged
    /// attachments). Otherwise leave the in-progress draft untouched. For
    /// the asynchronous Phase-1 unsend restore.
    OnlyIfIdle,
}

/// The OAuth add-flow handoff payload (ADR-0197 M1): a mutation from the
/// translator, applied by the loop against the add-flow state above.
pub enum OauthAddSignal {
    Pending {
        url: String,
        user_code: String,
        message: String,
    },
    Done,
    Failed {
        message: String,
    },
}

pub struct App {
    pub input: String,
    /// Structured transcript messages (semantic document model).
    pub messages: Vec<TranscriptMessage>,
    /// Version of the shared runtime buffer that `messages` was last synced
    /// from. The loop re-clones the buffer only when the runtime version moves
    /// past this, so an unchanged transcript costs no per-frame deep clone.
    /// Starts at 0 (the `Versioned` sentinel) so the first frame always syncs.

    /// O(1) streaming-delta target for `messages` (id, index); reset on
    /// wholesale replacement (ADR-0187).

    /// Side-conversation transcript (ADR-0017). Populated only while a `/btw`
    /// side session is live; per-turn events tagged with the side `session_id`
    /// route here instead of into `messages`.
    pub side_messages: Vec<TranscriptMessage>,
    /// Companion to `messages_version` for the side buffer.

    /// O(1) streaming-delta target for `side_messages`; reset on wholesale
    /// replacement (ADR-0187).

    /// Per-message laid-out height cache (Stage 2). Lets the transcript renderer
    /// skip re-wrapping off-screen messages, making per-frame layout O(visible)
    /// instead of O(transcript). Cleared whenever the transcript changes (a
    /// `messages_version` / `side_messages_version` bump) so a cached height is
    /// only ever read while the message's content is unchanged.
    pub layout_height_cache: crate::render::HeightCache,
    /// True while the user is composing into the `/btw` aside view
    /// (ADR-0017/0103). Drives [`App::focused_messages`] to swap the viewed
    /// transcript to [`App::side_messages`] and reserves the aside header.
    pub in_side_view: bool,
    /// Active side `session_id`, learned from `AgentResponse::SideViewOpened`.
    /// The response listener routes a `Turn { session_id, .. }` event into the
    /// side buffer when this matches, and into the primary buffer otherwise.
    pub side_session_id: Option<String>,
    /// Coarse primary-session status, mirrored from
    /// `AgentResponse::ParentStatus` for the side banner.
    pub parent_status: ParentStatus,
    /// Live `/btw` asides list (ADR-0103), mirrored from
    /// `AgentResponse::BtwList`. Drives the asides modal and the main view's
    /// header aside count. Kept even while inside an aside view so jumping
    /// back never needs a round trip.
    pub btw_list: Vec<nuo_wire::BtwAsideSummary>,
    /// Per-session chrome (activity text, responding flag, round/turn
    /// counters) for every session this client has observed, keyed by
    /// `session_id` — the primary **and** every live aside. A view renders
    /// from its own session's entry (see [`App::viewed_chrome`]), so an
    /// aside view never inherits the primary's activity bar and the primary
    /// never shows an aside's: chrome is view-scoped, not global (the
    /// pre-scoped fields below remain the *primary's* entry and the source
    /// of truth for the main view).
    pub session_chrome: std::collections::HashMap<String, SessionChrome>,
    /// The primary's chrome, saved when entering an aside view and restored
    /// on exit. Entering swaps the *displayed* chrome to the aside's own
    /// [`SessionChrome`] entry; exiting restores the primary's exactly as it
    /// was (a running primary round keeps its activity bar, elapsed timer,
    /// and counters across the aside detour).
    pub saved_primary_chrome: Option<SessionChrome>,
    pub scroll: u16,
    /// Whether the view follows the newest content (auto-scroll to bottom).
    pub follow_bottom: bool,
    /// Last measured stream height in lines and viewport height, used to pin
    /// the view to the bottom while following.
    pub content_lines: usize,
    pub view_height: u16,
    pub max_scroll: u16,
    /// Expanded step pinned under the HUD bar (its message index + screen rect),
    /// when its body is scrolled into view. Clicks inside the rect collapse it.
    pub sticky_step: Option<usize>,
    /// Shared token-source ledger (reported vs. estimated token accounting),
    /// read by the Telemetry modal. `Some` in the standalone path (the
    /// in-process harness shares this ledger); `None` in attach mode, where
    /// the accounting lives server-side and the modal renders the on-demand
    /// [`Self::token_report`] snapshot instead.
    pub token_ledger: Option<Arc<nuo_wire::TokenSourceLedger>>,
    /// Token-source report fetched on demand from the harness for the viewed
    /// session. Populated by a `QueryTokenUsage` round-trip when the
    /// Telemetry modal opens in attach mode (`token_ledger` is `None`);
    /// `None` while the round-trip is in flight (the modal renders a loading
    /// placeholder). Cleared when the viewed session switches.
    pub token_report: Option<nuo_wire::TokenSourceReport>,
    /// Latest session-scoped AI context snapshot from the harness. This is a
    /// provider usage/projection value, never a persisted transcript estimate.
    pub context_tokens: Option<nuo_wire::ContextTokenSnapshot>,
    /// Per-session context snapshots (ADR-0197 M1): the applier keys them by
    /// session; the render loop projects the *viewed* session's value into
    /// `context_tokens` each frame.
    pub context_tokens_by_session: HashMap<String, nuo_wire::ContextTokenSnapshot>,
    /// The **live primary session id** (ADR-0197 M1): the harness repoints
    /// its shared store on `/new`, `/session open`, `/resume`, and `/fork`;
    /// the translator reports each repoint here and session-scoped client
    /// state (the ↑/↓ prompt history's origin above all) follows.
    pub live_session_id: String,
    /// Human-in-the-loop request queues (ADR-0197 M1): the applier owns the
    /// queues; `pending_permission` / `pending_question` / `pending_input`
    /// remain the *mounted front* projections the sheets render.
    pub pending_permissions: std::collections::VecDeque<PermissionRequest>,
    pub pending_questions: std::collections::VecDeque<UserQuestionRequest>,
    pub pending_inputs: std::collections::VecDeque<nuo_wire::InputRequest>,
    /// Full-duplex (ADR-0029): which subagent (by parent tool-call id)
    /// surfaced a given permission / ask_user request, so the modal's reply
    /// can be tagged for down-routing.
    pub subagent_permission_parent: HashMap<String, String>,
    pub subagent_question_parent: HashMap<String, String>,
    /// The latest harness workspace-security snapshot (trust-gate state).
    pub workspace_security: nuo_wire::WorkspaceSecuritySnapshot,
    /// One-shot backend navigation signals (ADR-0197 M1): the applier latches
    /// them; the loop consumes and clears when it mounts the surface.
    pub open_sessions_signal: bool,
    pub open_tree_signal: bool,
    pub open_host_signal: bool,
    /// Set by the applier when a view transition (aside enter/exit) landed;
    /// the loop consumes it to re-anchor scroll exactly once.
    pub view_transitioned: bool,
    /// Set by the applier when a transcript document changed; the loop
    /// consumes it for bottom-follow scroll staging.
    pub transcript_changed_pending: bool,
    pub side_transcript_changed_pending: bool,
    /// Effective default-expand state for a reasoning trace
    /// (`[tui.default_expanded] thinking`, ADR-0197 M1: applier-owned).
    pub reasoning_default_expanded: bool,
    /// Backend completion round-trip awaiting consumption by the loop.
    pub(crate) backend_completion_signal: Option<crate::event_loop::CompletionSignal>,
    /// The effective TUI config (the applier's disclosure defaults read it;
    /// the translator no longer carries config clones).
    pub tui_config: crate::config::TuiConfig,
    /// Cross-session usage-statistics report fetched on demand from the
    /// harness (`QueryUsageStats`, ADR-0122). Session-independent: it
    /// aggregates the durable store under `data/usage/`, which survives
    /// session cleanup. `None` while the round-trip is in flight (the
    /// overlay renders a loading placeholder).
    pub usage_stats: Option<nuo_wire::usage_stats::UsageStatsReport>,
    /// Provider quota pool snapshot (`QueryProviderQuotas`, ADR-0036).
    pub provider_quotas: Option<nuo_wire::ProviderQuotaSnapshot>,
    /// Cached connection usage states for ambient indicators.
    pub connection_usages: std::collections::HashMap<String, nuo_wire::ConnectionUsageState>,
    /// The body (scrollable content) height of the currently-open overlay
    /// modal, captured each render from the rect the modal renderer paints
    /// its body into. This is the per-modal equivalent of `view_height` (which
    /// measures the transcript viewport) and is what `ScrollPageUp` /
    /// `ScrollPageDown` use as the page step so a page advance always matches
    /// the actual modal body rather than the transcript behind it. `0` when
    /// no modal is open (or before the first render after one opens), in
    /// which case page handlers fall back to `view_height`.
    pub modal_body_height: u16,
    /// Content-line index of the sticky step's real summary. Used to re-anchor
    /// the scroll offset when the user collapses the pinned step so the summary
    /// lands at the top of the viewport instead of jumping to unrelated content.
    pub sticky_summary_line: Option<usize>,
    /// Content-line the user asked to keep pinned at the top of the viewport by
    /// collapsing a sticky summary. While set, the per-frame scroll clamp is
    /// allowed to scroll past the natural `max_scroll` so a short tail of
    /// content below the collapsed step does not yank the header back down.
    /// Cleared on any manual scroll, view reset, or when auto-follow resumes.
    pub pin_summary_line: Option<usize>,
    /// Latched when a disclosure toggle (expand/collapse of a tool step,
    /// command result, thinking, provider-retry, or notice card) changed the
    /// transcript's height, so the event loop's next frame must be *staged*
    /// — laid out to measure the new `content_lines` — before the toggle's
    /// target scroll offset is applied. The staged pass emits no terminal
    /// bytes, so the terminal only ever sees the final viewport, never an
    /// intermediate one that gets re-clamped a frame later (the source of the
    /// expand/collapse flicker).
    ///
    /// Cleared by the loop once the settled offset has been painted, by any
    /// manual scroll, and by view resets — the same lifecycle as
    /// [`Self::pin_summary_line`].
    pub scroll_settle_pending: bool,
    /// Stack of nested zoom frames (subagent tasks). Empty means the root
    /// conversation is shown; the top frame is the currently focused view.
    /// Each frame carries the parent's scroll snapshot, restored on exit.
    pub focus_stack: Vec<ZoomFrame>,
    pub tx: mpsc::UnboundedSender<AgentRequest>,
    pub should_quit: Arc<AtomicBool>,
    pub suggestion_index: Option<usize>,
    /// Latched whenever the user finishes a completion: an `Enter` commit (any
    /// kind), an `Esc` dismiss, **or a slash-command accept via Tab/Enter**
    /// (a terminal accept — see [`Self::accept_completion`]). While `true`,
    /// the completion popup is suppressed even if `completion_kind()` would
    /// otherwise show one — so accepting a command does not immediately flash
    /// a subcommand menu or a collapsed single-exact-match list. Cleared by
    /// the next `InsertChar` / `Backspace` (the user is editing again, so
    /// live completions are once again useful). `@path` accepts via Tab do
    /// **not** latch — Tab is meant to keep cycling path candidates.
    pub completion_dismissed: bool,
    /// Backend-owned slash-command vocabulary published by the server.
    pub command_catalog: nuo_wire::CommandCatalog,
    /// Latest race-checked completion rows returned by the server.
    pub backend_completions: Vec<nuo_wire::InputCompletion>,
    pub completion_response_input: Option<String>,
    pub completion_response_cursor: usize,
    pub completion_requested: Option<(String, usize)>,
    pub completion_request_id: u64,
    pub cursor_position: usize,
    pub input_scroll: usize,
    /// Whether the next composer render should move the input viewport to
    /// keep the logical caret visible. Editing and caret movement re-arm
    /// following; wheel browsing and drag-selection edge autoscroll suspend
    /// it so a render cannot immediately undo the user's scroll gesture.
    pub input_scroll_follow_cursor: bool,
    /// Edge-autoscroll direction armed while a mouse selection drag that
    /// started inside the composer leaves the input's text rows: `Some(true)`
    /// scrolls up (pointer above), `Some(false)` down. Stepped by the event
    /// loop's heartbeat tick so holding the pointer still at the edge keeps
    /// scrolling, and cleared when the pointer re-enters or the drag ends.
    pub input_drag_scroll: Option<bool>,
    /// Authoritative foreground surface: the root Scene plus the LIFO overlay
    /// stack floating over it (ADR-0205). The router is the single navigation
    /// truth — there is no parallel `active_modal` / `active_panel` /
    /// `current_view` mirror to read instead.
    pub(crate) surfaces: crate::surfaces::SurfaceRouter,
    /// Selection cursor for the **sheet/scene** surfaces (permission and
    /// question sheets, the input-injection prompt, and the Dashboard's host
    /// list). Floating dialogs no longer share it: each dialog entity owns its
    /// own cursor (`SurfaceRouter::dialogs`), so this is no longer a
    /// cross-dialog scratchpad field (ADR-0035, `[INV-SURFACE-01]`).
    pub modal_index: usize,
    /// MRU open order for the quick switcher. Dialog *state* lives in the
    /// encapsulated entities (`SurfaceRouter::dialogs`); this store only tracks
    /// which dialogs have been opened and in what order (ADR-0035).
    pub(crate) surface_store: crate::surfaces::SurfaceStore,
    /// Recently executed commands for MRU display in Command Palette.
    pub(crate) recent_commands: Vec<String>,
    /// The session whose outbox the Queue view auto-blocked on entry
    /// (ADR-0139). `dismiss_active_dialog` is an `&mut App` method that
    /// cannot see the loop's `viewed_session_id`, so the block site records
    /// the target here and the exit hook consumes it.
    pub(crate) queue_exit_session: Option<String>,
    /// Wall-clock instant of the last user key press or input edit. Used by
    /// the event loop to quiesce background animation redraws while active
    /// composition / typing is in progress.
    pub last_key_press: std::time::Instant,
    /// Session DAG tree representation for `/tree` visualization.
    pub session_tree: nuo_wire::SessionTree,
    /// Full detail for the session under the info sub-view cursor. Populated by
    /// an on-demand `QuerySessionDetail` round-trip when the sub-view opens
    /// (`i`) and refreshed whenever the selection moves while in the sub-view.
    /// `None` while the round-trip is in flight.
    pub session_detail: Option<nuo_wire::SessionDetail>,
    /// Full detail and usage for the connection under the detail sub-view. Populated by
    /// an on-demand `QueryConnectionDetail` round-trip when the sub-view opens (Enter).
    /// `None` while the round-trip is in flight.
    pub connection_detail: Option<nuo_wire::ConnectionDetail>,
    /// Body scroll offset of the config category list.
    pub config_scroll: usize,
    /// Which pane of the `/config` Settings View currently owns the keyboard.
    pub config_focus: crate::overlays::ConfigFocus,
    /// Selected category in the `/config` Settings View (0..5).
    pub config_category: usize,
    /// Selected item/field index in the active category's detail pane.
    pub config_detail_index: usize,
    /// Detail row currently under the mouse pointer in the Settings detail
    /// pane, if any. Drives the row's hover band so the pointer and the keyboard
    /// cursor share one affordance; recomputed on pointer motion and cleared
    /// whenever the pointer leaves the pane or the category changes.
    pub config_hover_index: Option<usize>,
    /// Scroll offset for the `/config` detail pane body.
    pub config_detail_scroll: usize,
    /// Latest authoritative `[web]` selection/readiness snapshot from the harness.
    /// Refreshed when the Settings view opens (`QueryWebSearchConfig`) and
    /// on every `WebSearchConfigUpdated` ack.
    pub websearch_config: Option<nuo_wire::WebSearchConfigView>,
    /// Floating dropdown state and anchor for settings popup selectors (e.g.
    /// Web Search provider and Web Fetch reader selectors).
    pub config_dropdown: Option<(
        crate::components::dropdown::DropdownState<String>,
        crate::components::dropdown::DropdownAnchor,
    )>,
    /// Authoritative screen rectangle of the active settings detail row,
    /// updated during settings render to anchor popovers precisely.
    pub config_selected_rect: Option<nuotc::Rect>,
    pub current_provider: String,
    pub current_model: String,
    /// Raw current working directory captured at startup. Used to resolve
    /// `@path` mention completions against the real filesystem.
    pub cwd: std::path::PathBuf,
    /// The id of the session the TUI is currently viewing (`primary_session_id`
    /// outside a side view, the side id inside one). Learned each frame from
    /// the session source and stamped onto every recorded input-history entry
    /// so the inline ↑/↓ recall can walk this session's prompts only, while
    /// Ctrl+R searches the whole cross-session history.
    pub current_session_id: String,
    /// The workspace label for the current session — the project root's
    /// display path (already tilde-shortened), or empty when no workspace is bound.
    pub current_workspace: String,
    /// The active staffing role for the current session (e.g. "developer", "philosophist") (ADR-0244).
    pub current_role: Option<String>,
    /// Latest session-context snapshot for the Tools / Mcp / Skills /
    /// Permissions managers, or `None` before the first `QuerySessionContext`
    /// round-trip completes. Refreshed each frame from the response listener.
    pub session_context: Option<nuo_wire::SessionContextSnapshot>,
    pub loop_status: LoopStatus,
    /// Whether the primary session has a stopped round parked for `/retry`
    /// (ADR-0128). Mirrored from the session-scoped harness snapshot — the
    /// durable resume point — and consumed by [`Self::viewed_chrome`] to
    /// build the primary's retry affordance. Asides read their own
    /// `SessionChrome::can_retry`.
    pub harness_retry_pending: bool,
    /// Typed activity-bar phase for the primary session (`None` = idle /
    /// bar hidden). Never holds transport setbacks — see `crate::phase`.
    pub phase: Option<crate::phase::Phase>,
    /// The primary session's transport setback (retry countdown), rendered as a
    /// clause beside [`App::phase`]. The primary's slot, mirroring
    /// [`SessionChrome::transport_setback`] for the primary view exactly as
    /// `phase` mirrors [`SessionChrome::phase`]; asides keep theirs in their
    /// own chrome entry. Write it only through [`App::set_phase`], which owns
    /// the clause's lifetime (ADR-0235).
    pub provider_retry: Option<ProviderRetryState>,
    /// Durability-health banner state (ADR-0196 D4): the server's
    /// persistence-writer degradation, folded from the monitor stream.
    /// `None` / `Healthy` renders no banner.
    pub persistence_health: Option<nuo_wire::monitor::PersistenceHealth>,
    /// Whether all tool permissions are auto-approved this session
    /// (`--unattended` / `/unattended on`). Mirrored from the harness snapshot.
    pub unattended: bool,
    /// Whether workspace filesystem confinement is enforced this session
    /// (`/confinement on|off`). Mirrored from the harness snapshot.
    pub confined: bool,
    /// Harness round counter, mirrored each frame.
    pub round_count: u64,
    /// Current turn within the active round (1-indexed for display:
    /// `0` means the round has started but no model request has fired yet —
    /// e.g. the "queued" / "preparing context" phase).
    pub current_turn: u64,
    /// Wall-clock instant the current round started, or `None` between rounds.
    /// Drives the muted `<elapsed>` segment in the activity bar.
    pub round_started_at: Option<std::time::Instant>,
    pub pending_permission: Option<PermissionRequest>,
    /// The interaction sheet currently mounted in the composer slot, if any
    /// (ADR-0173 §3). The slot's two sibling components — the draft editor
    /// and an AI-initiated sheet — are mutually exclusive: `Some(kind)` means
    /// the sheet has replaced the composer. App-level slot state, *not*
    /// router foreground identity.
    pub active_sheet: Option<crate::sheet::SheetKind>,
    /// How many permission requests are queued in the runtime (front
    /// mirrored into `pending_permission`). `> 1` renders the `N queued`
    /// sheet badge (ADR-0173 §3).
    pub pending_permission_depth: usize,
    /// How many user questions are queued in the runtime behind the one
    /// mirrored into `App::question`. `> 0` renders the `+N` sheet badge.
    pub pending_question_depth: usize,
    /// The pending interactive-input request (L3.5 β) from an interactive
    /// `bash` command, or `None`. Set when a `RoundEvent::InputRequest` arrives;
    /// the input-injection modal reads it for its prompt/command/secret.
    pub pending_input: Option<nuo_wire::InputRequest>,
    /// The open question (ask_user) modal's self-contained MVU state, or
    /// `None` when no question modal is open. Replaces the four separate
    /// `question_*` fields that previously scattered the modal's state across
    /// `App`; all interaction now flows through `QuestionModel::update`.
    pub question: Option<crate::question_model::QuestionModel>,
    /// Scroll offset inside `Modal::Question`. Reset to 0 each time a question
    /// modal opens; clamped each frame by the modal's body renderer and, when
    /// `question_modal_follow` is set, nudged so the highlighted option stays on
    /// screen.
    pub question_scroll: usize,
    /// When true, the question modal's body scroll follows the ↑/↓ option
    /// highlight (the default after open / navigation). Cleared the moment the
    /// user scrolls manually (wheel / page keys) so they can browse a long
    /// option list freely, and re-set the moment they navigate again. Mirrors
    /// `session_modal_follow` / `history_modal_follow`.
    pub question_modal_follow: bool,
    /// Rows shown in the sessions picker (`/sessions` or `nuo attach`).
    pub sessions_overview: Vec<SessionOverview>,
    /// When switching sessions, holds the short id of the target session being loaded.
    pub switching_session: Option<String>,
    /// Live monitor snapshot for the `/host` server control panel
    /// (ADR-0096), mirrored from `UiRuntime::host_sessions` each frame.
    pub host_sessions: Vec<nuo_wire::MonitoredSession>,
    /// Scroll slot + selection-follow for the `/host` panel body.
    pub host_scroll: usize,
    pub host_modal_follow: bool,
    /// Which pane of the `/host` session dashboard owns the keyboard: the
    /// console/input region (default) or the sessions dock (`Tab` toggles).
    pub host_focus: crate::overlays::DashboardFocus,
    /// Scroll offset for the dashboard's console pane.
    pub host_detail_scroll: usize,
    /// The dashboard's session preview modal (ADR-0097 §3): the session id
    /// opened by Enter on a dock selection. Selection alone never opens it;
    /// Esc closes. Read-only.
    pub host_preview: Option<String>,
    /// Scroll offset for the preview modal body.
    pub host_preview_scroll: usize,
    /// Whether the dashboard's inline new-session prompt is open. While true,
    /// the composer input buffer is the task description and Enter creates a
    /// session instead of attaching.
    pub host_prompting: bool,
    /// What the open dashboard prompt does on submit: `true` = create a new
    /// session (from `n`), `false` = prompt the selected session (from `p`).
    pub host_prompt_new: bool,
    /// The dashboard console's receipt transcript (ADR-0097 §3): one entry
    /// per dispatched directive plus the server's answer. Lives for the
    /// dashboard's open lifetime (cleared on open) — it is a cockpit log,
    /// not history.
    pub host_console_log: Vec<crate::overlays::ConsoleLine>,
    /// Whether the dashboard's kill confirmation is armed: `k` on a dock
    /// selection asks first (`k` again confirms within the window, anything
    /// else cancels). Killing is irreversible, so it stays a two-surface
    /// gesture like the queue's `Shift+D`.
    pub host_kill_confirm: Option<String>,
    /// Id the armed kill confirmation refers to (kept separately so a dock
    /// selection move between presses can be compared against it).
    pub host_kill_confirm_id: Option<String>,
    /// `/host` Enter on a hosted session: the id to switch to, read by the
    /// caller after the TUI exits to re-attach (ADR-0096).
    pub switch_to_target: Option<String>,
    /// Which full-screen overlay (if any) the TUI opened straight into at
    /// startup instead of a conversation view. In that mode the overlay is not
    /// a transient modal — there is no conversation the user asked for behind
    /// it — so closing it must quit the program rather than drop into an empty
    /// chat. Cleared (set to [`crate::StartupOverlay::None`]) once a
    /// session is opened from the picker. Always `None` for the in-session
    /// `/sessions` modal, which just dismisses on Esc/click-out.
    pub startup_overlay: crate::StartupOverlay,
    /// The PreAttach interstitial state, mounted when the attaching
    /// workspace's first-contact trust snapshot is `Quarantined`
    /// (ADR-0175). `Some` means the chat surface is gated and the
    /// per-frame render paints a full-screen black interstitial
    /// instead of chat; the per-frame sync clears it once a
    /// subsequent `HarnessState` reports `aggregate() == Trusted`.
    /// Force-mounted by `NUO_FORCE_PRE_ATTACH=1` for acceptance.
    pub pre_attach: Option<crate::PreAttachState>,
    pub permission_confirm_always: bool,
    /// Whether the inline permission sheet is expanded to show the full
    /// description + arguments. Collapsed by default so the prompt stays
    /// brief; "Details" toggles this.
    pub permission_show_details: bool,
    pub permission_scroll: usize,
    pub permission_max_scroll: usize,
    pub input_history: Vec<nuo_wire::HistoryEntry>,
    /// **Derived** prompt rows for the viewed session, reconstructed from the
    /// transcript (see [`Self::backfill_session_history`]). Never persisted:
    /// the session file is the durable source of truth for conversation
    /// content (ADR-0018), so these rows exist only so the inline ↑/↓ recall
    /// can walk a resumed conversation's prompts without this client having
    /// recorded them. Indexed by `input_history.len() + i` in
    /// [`Self::current_session_history`] — see [`Self::history_entry`].
    /// Ordered oldest-first (transcript append order) so growth never shifts
    /// existing indices.
    pub session_history_backfill: Vec<nuo_wire::HistoryEntry>,
    /// How many transcript messages [`Self::backfill_session_history`] has
    /// already consumed for the current session, so a long streaming session
    /// rescans only its tail. Reset to `0` on every viewed-session change.
    pub session_history_backfill_cursor: usize,
    /// Whether identical prompt text collapses to one history entry across
    /// sessions (`[input_history] dedup`, default `true`). Read by
    /// [`Self::record_input_history`] and threaded into the persisted merge.
    pub input_history_dedup: bool,
    /// Whether `/slash` command invocations are recorded into the input
    /// history (`[input_history] record_commands`, default `false`).
    pub input_history_record_commands: bool,
    /// Whether `record_input_history` actually touches
    /// SQLite storage. Production keeps this `true` (set from
    /// `main`'s TUI entry point); tests construct `App` directly and default
    /// it to `false`, so a unit test can never write (or truncate!) the
    /// user's real database — a bug that once
    /// polluted it with synthetic `prompt N` rows stamped `session-a`.
    /// In-memory history still behaves identically; only the database write is
    /// suppressed.
    pub input_history_persist: bool,
    /// The inline ↑/↓ history **pointer**. Together with [`Self::history_draft`]
    /// this forms the input-history pointer model:
    ///
    /// - `None` — the composer shows the **draft** (the live, editable, remembered
    ///   input slot). This is the "newest" position: the input that has **not
    ///   been successfully sent** (still being composed, restored by a Phase-1
    ///   unsend, inserted from Ctrl+R, or recalled from the queue).
    /// - `Some(p)` — the composer shows history row `p` of the current session's
    ///   newest-first slice ([`Self::current_session_history`]), as a **read-only
    ///   snapshot**: edits made on a history row are temporary and are discarded
    ///   when the pointer moves away — coming back to the row reloads the
    ///   original text.
    ///
    /// ↑ moves the pointer toward older rows (and stashes the draft into
    /// [`Self::history_draft`] on the first press); ↓ moves it back toward the
    /// newest row and, past it, back to `None` (restoring the draft). A
    /// successful send clears the draft, because the input has been historicised
    /// and is no longer "unsent".
    pub history_index: Option<usize>,
    /// The live, editable, remembered input slot — the content of the **draft**
    /// mode (when [`Self::history_index`] is `None`). It is stashed here when ↑
    /// leaves the draft for a history row, and restored when ↓ walks back past
    /// the newest row, so an accidental ↑/↓ never loses what the user was
    /// composing. It is **cleared on send** (the input has been historicised)
    /// and replaced whenever a new input is adopted as the draft (Phase-1
    /// unsend restore, Ctrl+R insert, queue recall). Distinct from
    /// `stashed_input`, which is borrowed by modal flows.
    pub history_draft: String,
    /// Attachments staged behind [`Self::history_draft`] (the images and
    /// large pastes that were in the composer when the first ↑ stashed it),
    /// so ↓ past the newest entry restores them together with the text.
    pub history_draft_images: Vec<ImagePart>,
    pub history_draft_text_pastes: Vec<String>,
    /// In-memory attachment cache for recorded history entries, keyed by
    /// `(text, session_id)` — see [`HistoryAttachments`]. Seeded by
    /// [`Self::record_input_history`], consumed by the ↑/↓ and Ctrl+R recall
    /// paths so re-sending an interrupted or completed message restores its
    /// images and large pastes instead of shipping a bare chip label.
    pub history_attachments: HashMap<(String, Option<String>), HistoryAttachments>,
    /// FIFO insertion order of [`Self::history_attachments`] keys, so the
    /// cache can be pruned oldest-first when it outgrows its cap.
    pub history_attachments_order: VecDeque<(String, Option<String>)>,
    /// Images pasted (Ctrl+V) and waiting to be sent with the next message.
    /// Each entry is paired 1-to-1 with an `[Image #N]` chip inside
    /// [`App::input`]; the chip's `#N` is `index + 1` after
    /// [`App::reconcile_attachments`] has run.
    pub pending_images: Vec<ImagePart>,
    /// Large pasted text blocks staged behind `[Pasted text #N +M lines]`
    /// chips inside [`App::input`]. Each entry is the full original paste;
    /// the matching chip in the input is just a short label so the input
    /// box stays compact. Order matches the chip numbering.
    pub pending_text_pastes: Vec<String>,
    /// Session-affine compact outbox — the **next-round queue**. Pending items
    /// are never appended to the transcript; the queue bar shows counts and
    /// the follow-ups modal manages the items. Every staged message waits for the running
    /// round to finish naturally before starting a new one (next-round only).
    pub pending_dispatch: VecDeque<QueuedDispatch>,
    /// Target queue mode for the live composer while a round is running.
    pub composer_send_mode: ComposerSendMode,
    /// Whether the `Ctrl+X` scene namespace is armed, awaiting its second
    /// stroke (ADR-0298). A plain flag: the namespace has exactly one opening
    /// stroke, so an enum with a single non-`None` variant was a two-state
    /// type wearing three.
    pub scene_namespace_armed: bool,
    /// Sessions whose outbox is hard-blocked by the user. While a session is
    /// blocked, no queued message auto-drains — not even after its round
    /// reaches natural completion and the harness goes idle. The queue modal
    /// blocks a session on open (so items can be managed safely) and resumes
    /// on close; `Ctrl+P` toggles the block from inside the modal.
    /// Independent of the transient "paused" coloring: a session can be idle
    /// (visibly paused) without being blocked, and vice versa.
    pub queue_blocked_sessions: std::collections::HashSet<String>,
    /// Sessions whose last interactive round reached its natural completion
    /// event and whose harness has subsequently reported idle. Both facts are
    /// tracked separately so errors/interrupts never auto-run follow-ups.
    pub running_sessions: std::collections::HashSet<String>,
    /// Semantic selection state.
    pub selection: SelectionState,
    /// Drag gesture state.
    pub drag: SelectionDrag,
    /// Mounted UI instances and committed semantic geometry (ADR-0195).
    pub ui: crate::ui::ComponentTree,
    /// Message index of the step (tool step or reasoning trace) whose header
    /// currently rests under the mouse pointer (inline or sticky pinned), so
    /// the next draw lights it up to the intermediate hover tone as a click
    /// affordance. `None` whenever the pointer is elsewhere or an overlay
    /// modal is open.
    pub hovered_step: Option<usize>,
    /// Whether the pointer last acted on the transcript area (ADR-0174): a
    /// click anywhere in the transcript content — a step summary, message
    /// text, or the blank space between and around messages — parks the
    /// keyboard's attention on the transcript ("browse focus") and dims the
    /// composer panel, while clicking the composer itself or typing any
    /// printable character (the bounce-to-composer grammar) hands attention
    /// back. Pure pointer-derived transient state, recomputed from real
    /// interactions: it is cleared by every keyboard path that touches the
    /// composer and by view switches, so a mouse-driven user always sees
    /// which surface the next keypress lands on.
    pub transcript_focused: bool,
    /// Which layout strategy arranges the transcript message stream. Selected
    /// via `[tui] transcript_layout`; defaults to the turn-banded layout (each
    /// tool-bearing ReAct turn grouped under a labelled header). See
    /// `crate::render::layout::Strategy`.
    pub transcript_layout: crate::render::layout::Strategy,
    /// Canonical active color-scheme id (`zen`, a built-in preset, or
    /// `custom`). The renderer theme is rebuilt from this value immediately
    /// when the Appearance page applies a choice.
    pub color_scheme: String,
    /// Last persisted custom semantic palette. Retained while a preset is
    /// active so switching schemes never discards the user's colors.
    pub custom_color_scheme: nuo_wire::ColorSchemeConfig,

    /// Whether clicking outside a dismissable modal closes it (mirroring Esc).
    /// From `[tui] click_outside_dismiss` (default `true`): when true, an
    /// outside click dismisses a dismissable modal like Esc (the draft is
    /// parked, so nothing is lost). Modals holding precious in-progress input
    /// are never click-dismissable regardless of this flag, and the `nuo
    /// resume` startup picker's click-outside still quits. Esc / Ctrl+C always
    /// close/quit regardless of this flag.
    pub click_outside_dismiss: bool,
    /// Whether a disclosure toggle (expand/collapse) auto-scrolls to keep the
    /// toggled card well-placed. From `[tui] expand_auto_scroll` (default
    /// `false`): when false, the toggle changes only the card's height and the
    /// scroll offset is left exactly where the user put it; when true, the
    /// expand path shifts the summary toward the viewport top and the collapse
    /// path keeps a scrolled-past summary visible. Enabled toggles settle
    /// through the staged measure-then-paint path (`scroll_settle_pending`),
    /// so the auto-scroll itself never flickers.
    pub expand_auto_scroll: bool,
    /// User remaps of the global chords (`[keybindings]` config, ADR-0172).
    /// Resolution and the visible keycaps both consult it, so a remapped
    /// binding fires and advertises consistently.
    pub key_overrides: crate::keymap::GlobalOverrides,
    /// User remaps of the full-screen-view surface verbs (`session.*` dotted
    /// keys, ADR-0172). The Session/Subagent/Side resolvers and the composer
    /// hint row consult it.
    pub surface_overrides: crate::keymap::SurfaceOverrides,
    /// Keyboard-focused activatable target in the current frame, and the TUI's
    /// only navigation state — there is no separate "browse mode". `None` means
    /// every key has its ordinary input-box meaning (typing flows into the
    /// prompt). `Some` means a transcript step is highlighted: `Ctrl+↑`/`Ctrl+↓`
    /// (or bare `↑`/`↓`) cycle it, `Enter` activates it, and `Esc` clears it.
    /// Mouse hover/click is an acceleration path onto the same state.
    pub focused_target: Option<InteractiveTarget>,
    /// Show a brief "copied" toast. Held until this deadline elapses so the
    /// duration is wall-clock consistent regardless of the event-loop cadence.
    pub copy_toast_until: Option<std::time::Instant>,
    pub copy_toast_message: String,
    pub copy_toast_failed: bool,
    /// A transient notice toast (command acknowledgments such as
    /// `/delegate on`, surfaced via `NoticeSurface::Toast`). Unlike the
    /// inline `MessageKind::Notice`, this never enters the transcript: it
    /// renders as a top-right bubble that fades on its own, mirroring the copy
    /// toast. Severity drives the bubble's accent color. Held until
    /// `notice_toast_until` elapses so the duration is wall-clock consistent
    /// regardless of the loop cadence. A newer toast replaces an in-flight one.
    pub notice_toast_until: Option<std::time::Instant>,
    pub notice_toast_message: String,
    pub notice_toast_severity: NoticeSeverity,
    /// When set (e.g. via `NUO_DEV_TOAST`), keeps the toast pinned during keypresses for visual dev inspection.
    pub dev_toast_pinned: bool,
    /// Deadline until which a second Ctrl+C quits. Wall-clock based (like
    /// the copy/notice toasts) so the quit window is a real duration —
    /// previously this was a per-tick counter, which stretched the intended
    /// ~2s window to ~20s whenever the loop idled at its 1s heartbeat.
    pub ctrl_c_armed_until: Option<std::time::Instant>,
    /// Deadline until which a second Esc interrupts the running task.
    /// Wall-clock based for the same reason as `ctrl_c_armed_until`: the
    /// loop wakes far more often than its 100ms animation heartbeat (every
    /// keystroke, mouse move, stream delta, and dirty-notify), so the old
    /// 20-tick counter burned the intended ~2s window in a few hundred
    /// milliseconds — the "Esc again interrupts" toast flashed and vanished
    /// before a second press could land.
    pub esc_armed_until: Option<std::time::Instant>,
    /// Epoch the breathing indicator is timed against. The spinner phase is
    /// derived from wall-clock elapsed time since this instant rather than a
    /// per-frame counter, so the breathing cadence stays constant regardless of
    /// how often the loop redraws (mouse movement, streaming, paste, etc. all
    /// wake the loop at irregular intervals and would otherwise jitter it).
    pub spinner_epoch: std::time::Instant,
    /// Epoch the empty-state help carousel is timed against (ADR-0104). The
    /// slide index is derived from wall-clock elapsed time since this instant
    /// (same pattern as [`Self::spinner_epoch`]) so the rotation cadence stays
    /// constant regardless of draw frequency.
    pub carousel_epoch: std::time::Instant,
    /// Epoch milliseconds of the last composer submission. The ledger records
    /// when the provider was dispatched; together they let the latency timeline
    /// show what happened *before* dispatch (queue, context projection, hooks).
    pub last_submit_ms: Option<u64>,
    /// The composer draft parked while the input-injection sheet
    /// (L3.5 β) borrows the input line. Under ADR-0139 the
    /// picker flows (Models / Connections / History) park their drafts in
    /// per-view slots on the `PanelRegistry`; this remaining global slot
    /// serves the one request-driven borrowed-line surface, whose
    /// lifecycle (queue-front arrival → reply) never coexists with a
    /// picker's.
    pub injection_stashed_input: String,
    /// Provider id targeted by the unified key editor (`Modal::ModelEditor`).
    pub editor_target: Option<String>,
    /// Which editor field is focused. `0` = API key (text entry); `1` = effort
    /// (←/→ cycling); `2` = thinking (Space toggle, when available). The
    /// effort/thinking rows are only shown for models that expose those controls,
    /// so `editor_field` is clamped to `0` otherwise.
    pub editor_field: u8,
    /// API-key buffer for the editor (the input line is borrowed for the
    /// focused field).
    pub editor_key: String,
    /// Wire model id the key editor will activate once a key is entered (carried
    /// from the Models-picker selection or the provider's current model; not
    /// user-editable).
    pub editor_model: String,
    /// When true, `Modal::ModelEditor` edits the selected provider model's
    /// channel settings only (for example OpenAI effort or Anthropic
    /// effort/thinking), not the provider API key or active provider.
    pub editor_model_settings_only: bool,
    /// When `editor_model_settings_only` is true, whether the edited model is
    /// **built-in** (served by a built-in provider like `anthropic`). A built-in
    /// model's per-model reasoning knobs persist to the `[model_reasoning]`
    /// table via `EditModelReasoning`; a user-defined model's knobs persist to
    /// its channel via `EditConnectionModel` (ADR-0045).
    pub editor_target_is_builtin: bool,
    /// Current reasoning-effort selection in the key editor, as a lowercase wire
    /// string. Defaults to `"high"`; cycled with ←/→ over the selected model's
    /// supported levels.
    pub editor_effort: String,
    /// The effort ladder the edited route actually supports, as wire strings in
    /// ascending depth. Captured from the picker snapshot when the editor opens
    /// (the server's ADR-0149 resolution), **not** re-derived client-side: this
    /// binary does not link `nuo-providers`, so `resolve_model` sees no
    /// baseline table and would render an empty ladder (the node slider would
    /// collapse to a bare value row). Empty means the route exposes no effort
    /// knob, and the editor shows the value-only fallback.
    pub editor_effort_levels: Vec<String>,
    /// Whether the selected model exposes a separate thinking on/off switch.
    /// OpenAI GPT effort has no separate thinking field, so this is false
    /// there; Anthropic adaptive channels set it true.
    pub editor_thinking_available: bool,
    /// Current extended-thinking on/off selection in the key editor. Defaults
    /// to `true` (adaptive thinking on — the recommended mode for Claude).
    /// Toggled with Space when [`Self::editor_thinking_available`] is true;
    /// orthogonal to effort.
    pub editor_thinking: bool,
    /// Capability-override tri-state for **vision** (ADR-0149 layer 1), shown
    /// in the per-model settings editor. Cycled with Space: `None` = inherit
    /// (no override) → `Some(true)` force on → `Some(false)` force off.
    pub editor_vision_override: Option<bool>,
    /// Capability-override tri-state for **tool calling**, same cycling and
    /// semantics as [`Self::editor_vision_override`].
    pub editor_tool_override: Option<bool>,
    /// Focused field of the provider editor (`Modal::CustomProvider`) as an
    /// index into [`Self::custom_fields`] — the per-template visible field set
    /// (Name / Base URL / Token / Model / Protocol / Client Identity). Text
    /// fields borrow the composer line; selectors are cycled inline.
    pub custom_field: u8,
    /// The ordered visible fields of the provider editor, chosen by the active
    /// template (create) or the edited connection's provider (edit). Empty when
    /// no editor is open.
    pub custom_fields: Vec<CustomField>,
    /// Wire protocol of the connection being created/edited. Curated providers
    /// carry their fixed wire; the `custom` provider exposes this as an inline
    /// selector.
    pub custom_protocol_wire: String,
    /// Client identity (User-Agent and impersonation headers) selected for a
    /// connection. Curated providers keep their provider-defined identity.
    pub custom_client_identity: nuo_wire::ClientIdentity,
    /// Models seeded by the active template (create mode). Submitted as the
    /// provider's model list unless the editor exposes a free-text Model field
    /// (then the single typed model is submitted instead). Empty in edit mode.
    pub custom_models: Vec<String>,
    /// Base URL placeholder for the active template (the expected endpoint shape).
    pub custom_url_hint: String,
    /// Template-specific user agent carried into newly-created channels.
    pub custom_user_agent: Option<String>,
    /// How newly-created connections authenticate (from the selected template).
    pub custom_auth: nuo_wire::ConnectionAuth,
    /// The **model provider id** the active create flow was seeded from, or
    /// `None` in edit mode / when no template is active. Sent as
    /// `AddConnection::provider` (the wire field, ADR-0201): it is the
    /// connection's durable provider binding, and the catalog resolves the
    /// connection's models from it on later startups.
    pub custom_provider_id: Option<String>,
    /// True while an "Add curated connection → OAuth" flow is in flight.
    pub awaiting_oauth_add: bool,
    pub oauth_pending_message: String,
    pub oauth_pending_url: String,
    pub oauth_pending_user_code: String,
    pub oauth_pending_error: Option<String>,
    /// Selected copyable card in OAuth Pending modal (0 = URL, 1 = Code).
    pub oauth_selected_item: usize,
    /// Scroll offset for the OAuth pending modal body. Reset when the modal
    /// opens or its content changes.
    pub oauth_scroll: usize,
    /// Scroll offset for the custom-provider editor body. Rendered body sets
    /// the upper bound automatically.
    pub custom_scroll: usize,
    /// When `Some(id)`, the provider editor is **editing** the existing user
    /// provider `id` (meta only: Name/Base URL/Token; models stay managed in the
    /// Models picker). `None` is create mode.
    pub custom_edit_id: Option<String>,
    /// Provider-editor buffers holding the unfocused text fields (the focused one
    /// lives in the borrowed composer line). Name / Base URL / Token / Model.
    pub custom_name: String,
    pub custom_base_url: String,
    pub custom_token: String,
    pub custom_model: String,
    /// Selected row of the provider-template chooser (`Modal::ProviderPreset`),
    /// indexing `crate::PROVIDER_PRESETS`. Cycled with `↑/↓`.
    pub preset_choice: usize,
    /// Scroll offset for the template-chooser body. The rendered body
    /// sets the upper bound automatically (via `render_body`), and `↑/↓` move
    /// the selection so the chosen template stays on-screen.
    pub preset_scroll: usize,
    /// Pending provider-delete confirmation overlay. `Some(id)` means the
    /// confirm dialog is open over the Connections list: the provider
    /// `id` is staged for deletion and waits on the user's choice. Set when
    /// `Shift+D` lands on a deletable custom provider; cleared on Cancel, Esc,
    /// outside-click, and after a confirmed Delete dispatches the request.
    pub pending_provider_delete: Option<String>,
    /// Focused button in the provider-delete confirm overlay. Defaults to
    /// [`ProviderDeleteChoice::Cancel`] (the safe choice) each time the overlay
    /// opens; ←/→/Tab move between the two buttons.
    pub provider_delete_focus: ProviderDeleteChoice,
    /// Lowercase provider name → whether a usable API key is configured.
    pub key_status: HashMap<String, bool>,
    /// Live model-picker snapshot (default id + per-model favorite / key-ready
    /// / last-used). Drives the `/models` and `/connections` pickers' rendering
    /// and sort order. Refreshed from the response listener each frame.
    pub provider_picker: ProviderPickerSnapshot,
    /// Theme.
    pub theme: Theme,
    /// Terminal capability profile (ADR-0180).
    pub profile: nuotc::TerminalProfile,
    /// User-supplied ASCII logo lines loaded at startup from
    /// `$XDG_CONFIG_HOME/nuo/logo.txt` (clamped to the empty-state bounding
    /// box). `None` when no user logo is present → built-in wordmark is used.
    /// Passed into the empty-state hero via `TranscriptProps::logo`.
    pub logo: Option<Vec<String>>,
    /// Dead-link latch (ADR-0197 D6): set the first time an outbox send
    /// fails (the session driver's receiver is gone). A dead link is a
    /// visible chrome state — the activity bar reports it — never a
    /// swallowed send. It does not clear: with the driver gone nothing can
    /// acknowledge recovery, and pretending otherwise would be a lie.
    pub link_down: bool,
    /// Background tasks tracking for the TasksBar (ADR-0212).
    pub background_tasks: Vec<BackgroundTaskItem>,
}

mod composer;
mod history;
mod link;
mod providers;
mod queue;
mod subagents;
mod surfaces;
mod tasks;
