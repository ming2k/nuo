//! The TUI's central application state ([`App`]) plus the `Modal` kind and
//! the `impl App` blocks that hold pure state-management methods.
//!
//! Input-box completion lives in [`crate::shell::completion`]; the event/render
//! loop and shared runtime live in `crate::shell::event_loop`. Everything
//! else that mutates `App` either lives here (state navigation, focus,
//! sticky/pinned step bookkeeping) or in `completion.rs` (the only other
//! `impl App` block).
//!
//! [`crate::shell::completion`]: crate::shell::completion
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use nuo_protocol::{AgentRequest, ConnectionAuth, LoopStatus, ProviderPickerSnapshot};

use nuo_tool::{ImagePart, PermissionRequest, UserQuestionRequest};

use crate::shell::chrome::tasks_bar::BackgroundTaskItem;
use crate::shell::completion::CompletionItemKind;
use crate::shell::composer_grid::{CaretMotion, TextGrid};
use crate::shell::event_loop::resolve_focused_mut;
use crate::shell::focus::Focus;
use crate::shell::render::Theme;
use nuo_ui_primitives::fuzzy;
use nuo_view::composer_attachments;
use nuo_view::model::document::{NoticeSeverity, TranscriptMessage};
use nuo_view::model::layout::InteractiveTarget;
use nuo_view::model::selection::{SelectionDrag, SelectionState};
use nuo_view::providers::{
    ConnectionTemplate, CustomField, RankedModel, RankedProvider, edit_fields,
    models_flat_filtered_from_with_quotas, providers_filtered_from,
};

use std::collections::{HashMap, HashSet, VecDeque};

pub mod state;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedDispatchState {
    /// Staged in the outbox, waiting for its turn (auto-drain or a recall /
    /// delete / reorder from the Queue modal).
    Waiting,
    /// A fresh round is being started for this item (`FollowUp` sent,
    /// `FollowUpStarted` not yet received).
    Dispatching,
    /// A busy-Enter steer admitted into a running round at its next safe
    /// turn boundary. Unlike `Waiting`/`Dispatching` (follow-up queue items),
    /// a `Steering` item is never dispatched as a fresh round by the
    /// client: admission is observed from the wire (`SteerAdmitted`), which
    /// materializes the message in the transcript at the seam head and
    /// removes this outbox row (ADR-0137).
    Steering,
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
/// One row type, two owners, split by [`QueuedDispatchState`]:
///
/// - `Waiting` — a projection of the driver's `FollowUpQueue`. The server owns
///   the item's life (it ships at a round boundary), so every local mutation of
///   one must be mirrored with the matching verb (`QueueRemove`,
///   `QueueReorder`, `QueueClear`).
/// - `Dispatching` / `Steering` — client-only until the wire answers. A busy-Enter
///   steer waits in the running round's admission window and the server never
///   sees it in this queue at all, which is why a `QueueUpdated` snapshot must
///   preserve these rows rather than replace them.
///
/// The item is intentionally absent from the transcript until the harness
/// admits it, so pending state never scrolls away or masquerades as thread
/// history ([INV-STEER-01], ADR-0137).
#[derive(Debug, Clone)]
pub struct QueuedDispatch {
    pub id: String,
    pub session_id: String,
    pub state: QueuedDispatchState,
    /// The user's literal prompt text, sent verbatim to the agent on dispatch.
    pub text: String,
    /// When the item was staged (epoch ms), or `None` when the peer that
    /// authored the row did not report one.
    ///
    /// Optional, not defaulted: the snapshot path used to write
    /// `sent_at_ms.unwrap_or(0)`, so a peer that omitted the stamp produced a
    /// row the modal rendered as `08:00` (the Unix epoch in local time) — a
    /// fabricated wall-clock on the user's own message.
    pub queued_at_ms: Option<u64>,
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
/// identity [`nuo_protocol::merge_history`] uses, so a recall finds the
/// payloads that shipped with the exact prompt text.
///
/// Deliberately **not** persisted: input history in SQLite is rebuildable cosmetic
/// telemetry, and base64 image blobs would balloon the database and
/// duplicate thread data. The cache lives for the process lifetime
/// (capped, newest-first) and is re-seeded on every send, which is exactly
/// the window the interrupt → ↑/↓ → resend flow needs.
#[derive(Debug, Clone, Default)]
pub struct HistoryAttachments {
    pub images: Vec<ImagePart>,
    pub text_pastes: Vec<String>,
}

/// Who owns the pause currently in effect on a session's outbox.
///
/// The queue modal auto-pauses on open so items cannot ship while the user is
/// editing them, and releases that pause on close. But the user can also pause
/// deliberately (`Ctrl+P`, `/queue block`), and a close must not release a pause
/// it did not set — nor resurrect one the user dismissed. The distinction has to
/// be *stored*, not inferred: `queue_blocked_sessions` says "paused", not "paused
/// by whom" ([INV-OUTBOX-04]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuePauseOrigin {
    /// The queue modal is open and still owns its auto-pause: the close hook
    /// releases it.
    ModalAutoPause,
    /// The pause in effect is the user's (they toggled it from inside the
    /// modal, or it was already set): the close hook leaves it alone.
    UserOwned,
}

/// Which surface owns the terminal cursor right now — the single source of
/// truth that the event loop's hide/show state machine, the immediate
/// pre-draw cursor re-sync, and the composer's `show_caret` flag all derive
/// from.
///
/// The terminal cursor is what the host terminal's IME anchors its
/// composition window to, so the owner must be exactly the one text-input
/// surface the user is typing into — or `Self::None` when no such surface
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
    /// A text field belonging to a non-thread root scene — today the
    /// Dashboard's inline new-session / prompt task line, whose text is the
    /// composer buffer but whose field is the scene's own footer. Distinct
    /// from `Self::Composer` because the composer's readline family and
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
/// (`App::pending_provider_delete`). `Cancel` is the safe default — Enter
/// dismisses without deleting; the user must move focus to `Delete` to destroy
/// the provider. The derive places `Default` on the first variant (`Cancel`),
/// matching the "safe-default" contract documented above.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderDeleteChoice {
    #[default]
    Cancel,
    Delete,
}

/// The typed provider-retry state of one session: the countdown, the attempt
/// pair and the fault history.
///
/// This is a re-export of [`nuo_view::retry::RetryProgress`] under the name the
/// shell has always used, kept so the chrome code reads naturally. The type
/// itself lives in the presentation crate because it owns the *label* as well as
/// the state: the bar, the compact form and the receipt line are three
/// renderings of one vocabulary, and splitting state from phrasing is exactly
/// how the bar ended up counting `5/29` beside a transcript row counting `6/30`.
pub use nuo_view::retry::RetryProgress as ProviderRetryState;

/// The view-scoped chrome of one session: the typed activity phase, the
/// responding flag, and the structural round/turn counters. Each hosted
/// session owns an entry in `App::session_chrome`, and a view renders
/// exclusively from the entry of the session it displays
/// (`App::viewed_chrome`). This is what keeps one view from inheriting
/// another's activity bar: before this type existed these were single global
/// fields, so whichever view was focused showed the *primary's* state no
/// matter which session was actually streaming.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionChrome {
    /// Typed activity phase (None when idle). Per session: a background
    /// session's live phase lives in its own entry, invisible to the main view.
    pub phase: Option<nuo_view::phase::Phase>,
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
    /// Mirrored from the session-scoped harness snapshot — the
    /// authoritative durable resume point, not a transcript scan — so the
    /// hint bar only offers `/retry` for a round that actually stopped
    /// before completing.
    pub can_retry: bool,
    /// Latest completed principal ReAct turn's performance sample. Kept
    /// session-scoped so two views never borrow each other's hint-bar
    /// measurement.
    pub last_turn_performance: Option<nuo_protocol::TurnPerformanceSnapshot>,
    /// The transport setback this session's in-flight model request reported
    /// (`RetryScheduled`): a retry countdown rendered as a clause beside the
    /// activity label, never as the label itself (see `nuo_view::phase` rule 2).
    ///
    /// Session-scoped for the same reason as `phase`: a background session
    /// backing off against a rate-limited upstream must not paint a retry
    /// countdown onto the primary view's bar (`App::provider_retry` is the
    /// primary's own slot).
    ///
    /// **Lifetime:** no producer ever clears this. It is retired — together
    /// with the phase it annotates — by the first phase write that is not
    /// [`nuo_view::phase::Phase::AwaitingModel`], which
    /// [`SessionChrome::set_phase`] enforces as the single writer of both
    pub transport_setback: Option<ProviderRetryState>,
}

impl SessionChrome {
    /// Write this session's activity phase, retiring its transport-setback
    /// clause under [`nuo_view::phase::ends_transport_setback`].
    ///
    /// This is the **only** way anything in the process ends a setback clause,
    /// and the rule it applies is derived from the phase rather than from the
    /// event that produced it: the response translator publishes a setback and
    /// then forgets about it, so a wire event it does not know about cannot
    /// leave a dead countdown on the bar.
    pub fn set_phase(&mut self, phase: Option<nuo_view::phase::Phase>) {
        if nuo_view::phase::ends_transport_setback(phase.as_ref()) {
            self.transport_setback = None;
        }
        self.phase = phase;
    }
}

/// Whether `App::adopt_as_draft` may clobber a composer that currently
/// holds unsent, unsaved work. Explicit user gestures (queue recall,
/// Ctrl+R insert) may — the user asked for it. Asynchronous events
/// (a Phase-1 unsend restore) may not — the in-progress draft they would
/// destroy was never sent anywhere and has no other copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftAdoption {
    /// Replace whatever the composer holds. For user-initiated paths where
    /// the current content is either sent or superseded.
    Replace,
    /// Adopt the new content as the live buffer while **preserving** what the
    /// composer held as the stashed draft capsule, so ↓ past the newest history
    /// row brings the in-progress draft back.
    ///
    /// This replaced `OnlyIfIdle`, which simply *did nothing* when the composer
    /// was busy — the caller then believed it had restored the user's text while
    /// the text was in fact dropped on the floor. Two ways to be wrong existed
    /// (clobber the draft, or lose the recall) and the enum pretended there was
    /// a middle one. Stashing is the middle: neither side loses, and the capsule
    /// is a first-class, visible slot (`has_stashed_draft` drives its hint).
    StashCurrent,
}

/// The OAuth add-flow handoff payload (M1): a mutation from the
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
    pub session: state::SessionState,
    pub transcript: state::TranscriptState,
    pub composer: state::ComposerState,
    pub human: state::HumanState,
    pub surfaces: state::SurfacesState,
    pub provider_editor: state::ProviderEditorState,
    pub telemetry: state::TelemetryState,
    pub link: state::LinkState,
}

mod composer;
mod history;
mod link;
pub mod notice_catalog;
pub mod notices;
pub mod providers;
mod queue;
pub mod receipts;
mod subagents;
mod surfaces;
mod tasks;
