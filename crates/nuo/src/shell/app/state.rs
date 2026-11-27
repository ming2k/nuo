//! Partitioned `App` state.
//!
//! Each group owns one concern; `App` is a composition of these groups
//! ([ADR-0186](../../../../docs/adr/0186-state-is-partitioned-and-giants-are-prohibited.md)).
use super::*;

/// `session` state (client-wide session coordination; per-thread fields live on
/// [`crate::shell::app::workspaces::ThreadInstance`], ADR-0253 `[INV-INSTANCE-01]`).
pub struct SessionState {
    /// Thread IDs currently subscribed on this multiplexed connection
    /// (ADR-0251 `[INV-MUX-01]`).
    pub subscribed_threads: std::collections::HashSet<String>,
    /// The **live primary session id** (M1).
    pub live_session_id: String,
    /// Raw current working directory captured at startup.
    pub cwd: std::path::PathBuf,
    /// The id of the session the TUI is currently viewing.
    pub current_session_id: String,
    /// Durability-health banner state (D4).
    pub persistence_health: Option<nuo_protocol::monitor::PersistenceHealth>,
    /// Server-attested global lifecycle phase (ADR-0260 `[INV-SRVPHASE-01]`).
    pub server_phase: nuo_protocol::ServerPhase,
    /// Instant when `server_phase` last transitioned (used for 120ms anti-flicker hysteresis).
    pub server_phase_since: std::time::Instant,
    /// Instant when foreground `session_phase` last transitioned (120ms hysteresis).
    pub thread_phase_since: std::time::Instant,
    /// Deferred initial thread subscription queued while connecting or while the
    /// server is still in `ServerPhase::Initializing` (ADR-0260 `[INV-SRVPHASE-02]`,
    /// ADR-0267 `[INV-SRVPHASE-09]`). Cleared only once the server confirms
    /// `Wire::Subscribed` / `Wire::Pick` or issues a terminal refusal.
    pub pending_initial_subscribe: Option<(nuo_protocol::ThreadTarget, Option<std::path::PathBuf>)>,
    /// Whether `pending_initial_subscribe` has already been dispatched on the
    /// current connection and is awaiting server acknowledgement. Reset on
    /// `LinkEstablished` and on retryable `ERR_SERVER_NOT_READY` refusals so
    /// mid-flight server restarts never strand the client in `Preparing`.
    pub initial_subscribe_in_flight: bool,
    /// Launch options preserved for picker fallback (`PickRequired` / `ERR_SESSION_GONE`).
    pub initial_init_options: nuo_protocol::ThreadInitOptions,
    /// Caller project scope remembered across reconnects and picker fallbacks.
    pub project_scope: Option<std::path::PathBuf>,
    /// When switching threads, holds the short id of the target thread being loaded.
    pub switching_session: Option<String>,
    /// Sessions whose last interactive round is currently running.
    pub running_sessions: std::collections::HashSet<String>,
    /// Unseen terminal outcomes (`Succeeded` / `Failed`) recorded when a thread
    /// finishes or fails while it is not the actively viewed tab. Single source
    /// of truth covering both subscribed resident threads and cold/unsubscribed
    /// tabs observed via `host_sessions`; cleared automatically when the user
    /// views the thread or starts a new round on it.
    pub unseen_outcomes:
        std::collections::HashMap<String, crate::shell::app::workspaces::UnseenOutcome>,
    /// Sessions whose outbox pause is on.
    pub queue_blocked_sessions: std::collections::HashSet<String>,
    /// The latest harness workspace-security snapshot (trust-gate state).
    pub workspace_security: nuo_protocol::WorkspaceSecuritySnapshot,
    /// Rows shown in the threads picker (`/threads` or `nuo attach`).
    pub sessions_overview: Vec<nuo_protocol::ThreadOverview>,
    /// Live monitor snapshot for the thread fleet orchestrator dashboard.
    pub host_sessions: Vec<nuo_protocol::MonitoredThread>,
    /// The session whose outbox the Queue view auto-paused on entry.
    pub(crate) queue_exit_session: Option<(String, QueuePauseOrigin)>,
    /// Full detail for the thread under the info sub-view cursor.
    pub session_detail: Option<nuo_protocol::ThreadDetail>,
    /// The effective TUI config.
    pub tui_config: nuo_view::tui_config::TuiConfig,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TranscriptSearchState {
    pub active: bool,
    pub query: String,
    /// Indices into `ThreadInstance::messages` that match `query`.
    pub matches: Vec<usize>,
    /// Index into `matches` for the currently focused match.
    pub current: usize,
}

/// Terminal-scoped viewport state (ADR-0253 `[INV-INSTANCE-01]`).
///
/// Everything that belongs to this terminal process, independent of which thread tab is
/// active. Lives as a sibling field on `App` alongside thread state so render and input
/// paths can split-borrow `&mut app.viewport` and the active thread simultaneously.
pub struct Viewport {
    /// Mounted UI instances and committed semantic geometry.
    pub ui: crate::shell::ui::ComponentTree,
    /// Drag gesture state.
    pub drag: SelectionDrag,
    /// Message index of the step currently resting under the mouse pointer.
    pub hovered_step: Option<usize>,
    /// Last measured transcript viewport height in rows.
    pub view_height: u16,
    /// Body height of the currently-open overlay modal.
    pub modal_body_height: u16,
    /// Epoch the breathing indicator is timed against.
    pub spinner_epoch: std::time::Instant,
    /// Epoch the empty-state help carousel is timed against.
    pub carousel_epoch: std::time::Instant,
    /// Which layout strategy arranges the transcript message stream.
    pub transcript_layout: crate::shell::render::layout::Strategy,
    /// Whether a disclosure toggle auto-scrolls to keep the toggled card well-placed.
    pub expand_auto_scroll: bool,
    /// Effective default-expand state for a reasoning trace.
    pub reasoning_default_expanded: bool,
    /// Per-surface receipt logs (ADR-0116 / ADR-0121).
    pub logs: nuo_view::model::log::SurfaceLogs,
    /// The dashboard console's receipt transcript.
    pub host_console_log: Vec<crate::shell::overlays::ConsoleLine>,
}

/// `composer` state.
pub struct ComposerState {
    pub input: String,
    /// Caret position as a **byte** offset into `input` — the unit every local
    /// consumer uses (editor motions, the composer grid, the `@`-mention
    /// scanner). The completion wire contract is a **Unicode scalar** index;
    /// `Self::wire_cursor` is the one conversion between them.
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
    /// Latched whenever the user finishes a completion: an `Enter` commit (any
    /// kind), an `Esc` dismiss, **or a slash-command accept via Tab/Enter**
    /// (a terminal accept — see `Self::accept_completion`). While `true`,
    /// the completion popup is suppressed even if `completion_kind()` would
    /// otherwise show one — so accepting a command does not immediately flash
    /// a subcommand menu or a collapsed single-exact-match list. Cleared by
    /// the next `InsertChar` / `Backspace` (the user is editing again, so
    /// live completions are once again useful). `@path` accepts via Tab do
    /// **not** latch — Tab is meant to keep cycling path candidates.
    pub completion_dismissed: bool,
    pub suggestion_index: Option<usize>,
    /// Backend-owned slash-command vocabulary published by the server.
    pub command_catalog: nuo_tool::CommandCatalog,
    /// Latest race-checked completion rows returned by the server.
    pub backend_completions: Vec<nuo_tool::InputCompletion>,
    /// Backend completion round-trip awaiting consumption by the loop.
    pub(crate) backend_completion_signal: Option<crate::shell::event_loop::CompletionSignal>,
    pub completion_response_input: Option<String>,
    pub completion_response_cursor: usize,
    pub completion_requested: Option<(String, usize)>,
    pub completion_request_id: u64,
    /// Images pasted (Ctrl+V) and waiting to be sent with the next message.
    /// Each entry is paired 1-to-1 with an `[Image #N]` chip inside
    /// `App::input`; the chip's `#N` is `index + 1` after
    /// `App::reconcile_attachments` has run.
    pub pending_images: Vec<ImagePart>,
    /// Large pasted text blocks staged behind `[Pasted text #N +M lines]`
    /// chips inside `App::input`. Each entry is the full original paste;
    /// the matching chip in the input is just a short label so the input
    /// box stays compact. Order matches the chip numbering.
    pub pending_text_pastes: Vec<String>,
    /// Atomic draft snapshot held while a transient sub-mode borrows the
    /// composer line (ADR-0051 `[INV-COMP-03]`, ADR-0247).
    ///
    /// Same value type as a parked draft ([`nuo_view::composer_draft::ComposerDraft`]),
    /// because "the composer's text plus its caret plus its staged attachments"
    /// is one concept with two lifetimes: a *stash* that lives as long as a
    /// sub-mode, and a *park* that lives as long as the thread.
    pub stashed_draft: nuo_view::composer_draft::ComposerDraft,
    /// In-memory attachment cache for recorded history entries, keyed by
    /// `(text, session_id)` — see [`HistoryAttachments`]. Seeded by
    /// `Self::record_input_history`, consumed by the ↑/↓ and Ctrl+R recall
    /// paths so re-sending an interrupted or completed message restores its
    /// images and large pastes instead of shipping a bare chip label.
    pub history_attachments: HashMap<(String, Option<String>), HistoryAttachments>,
    /// FIFO insertion order of `Self::history_attachments` keys, so the
    /// cache can be pruned oldest-first when it outgrows its cap.
    pub history_attachments_order: VecDeque<(String, Option<String>)>,
    pub input_history: Vec<nuo_protocol::HistoryEntry>,
    /// **Derived** prompt rows for the viewed session, reconstructed from the
    /// transcript (see `Self::backfill_session_history`). Never persisted:
    /// the session file is the durable source of truth for thread
    /// content (ADR-0018), so these rows exist only so the inline ↑/↓ recall
    /// can walk a resumed thread's prompts without this client having
    /// recorded them. Indexed by `input_history.len() + i` in
    /// `Self::current_session_history` — see `Self::history_entry`.
    /// Ordered oldest-first (transcript append order) so growth never shifts
    /// existing indices.
    pub session_history_backfill: Vec<nuo_protocol::HistoryEntry>,
    /// How many transcript messages `Self::backfill_session_history` has
    /// already consumed for the current session, so a long streaming session
    /// rescans only its tail. Reset to `0` on every viewed-session change.
    pub session_history_backfill_cursor: usize,
    /// Whether identical prompt text collapses to one history entry across
    /// sessions (`[input_history] dedup`, default `true`). Read by
    /// `Self::record_input_history` and threaded into the persisted merge.
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
    /// The inline ↑/↓ history **pointer**. Together with the stashed draft
    /// this forms the input-history pointer model:
    ///
    /// - `None` — the composer shows the **draft** (the live, editable, remembered
    ///   input slot). This is the "newest" position: the input that has **not
    ///   been successfully sent** (still being composed, restored by a Phase-1
    ///   unsend, inserted from Ctrl+R, or recalled from the queue).
    /// - `Some(p)` — the composer shows history row `p` of the current session's
    ///   newest-first slice (`Self::current_session_history`), as a **read-only
    ///   snapshot**: edits made on a history row are temporary and are discarded
    ///   when the pointer moves away — coming back to the row reloads the
    ///   original text.
    ///
    /// ↑ moves the pointer toward older rows (and stashes the draft into
    /// its stash slot on the first press); ↓ moves it back toward the
    /// newest row and, past it, back to `None` (restoring the draft). A
    /// successful send clears the draft, because the input has been historicised
    /// and is no longer "unsent".
    /// The in-place history traversal cursor (ADR-0051 [INV-COMP-02]).
    pub history_index: Option<usize>,
    /// Target queue mode for the live composer while a round is running.
    pub composer_send_mode: ComposerSendMode,
    /// Pending steer items the user *deleted* (QueueDelete `D` in the modal)
    /// rather than recalled. `SteerCancelled` restores the text to the
    /// composer for a recall, but a deletion must not resurrect the text; the
    /// cancelled arm consults this set and skips the restore (ADR-0137).
    pub suppress_steer_restore: std::collections::HashSet<(String, String)>,
    /// Session-affine outbox — the **next-round queue projection**. Pending
    /// items are never appended to the transcript; the activity strip shows the
    /// count/preview and the queue modal manages them.
    ///
    /// Ownership is split by state (see `QueuedDispatchState`): `Waiting` rows
    /// are a projection of the driver's `FollowUpQueue` — the server decides
    /// when they ship, and `QueueUpdated` rewrites them — while `Dispatching`
    /// and `Steering` rows exist only here (an in-flight ack, and a steer held
    /// in the running round). The client never dispatches this queue itself.
    pub pending_dispatch: VecDeque<QueuedDispatch>,
    /// Epoch milliseconds of the last composer submission. The ledger records
    /// when the provider was dispatched; together they let the latency timeline
    /// show what happened *before* dispatch (queue, context projection, hooks).
    pub last_submit_ms: Option<u64>,
    /// The caret's vertical-motion memory: the display column ↑/↓ aim for.
    /// Owned by the composer buffer, so it survives keystrokes but is
    /// invalidated by any move the caret makes on its own — see
    /// [`CaretMotion`].
    pub(crate) caret_motion: CaretMotion,
    /// Requests the operator submitted before the thread reached `Ready`.
    /// Flushed in order on the `Ready` transition; the server also buffers
    /// defensively, but the client-side queue keeps the UI honest.
    pub pending_prompts: Vec<nuo_protocol::AgentRequest>,
    /// When the live composer was last mirrored into the parked-draft ledger
    /// (ADR-0247). Drives the autosave cadence so a crash loses at most one
    /// second of typing, without fsyncing on every keystroke.
    pub draft_autosave_at: std::time::Instant,
    /// **Which thread's draft the live buffers currently are** (ADR-0247).
    ///
    /// The composer is one buffer and every thread has one draft, so exactly one
    /// identity must own the buffer at any instant — and a transition is
    /// precisely the moment the two can disagree. Recording the owner makes the
    /// handoff explicit and checkable: arriving at the thread already named here
    /// is a no-op (re-focusing the active tab must not reset a draft), and
    /// arriving anywhere else parks this buffer and installs that thread's.
    ///
    /// `None` before the first thread is known (a pre-attach client).
    pub draft_thread: Option<String>,
}

/// `human` state.
pub struct HumanState {
    /// Human-in-the-loop request queues (M1): the applier owns the
    /// queues; `pending_permission` / `pending_question` / `pending_input`
    /// remain the *mounted front* projections the sheets render.
    pub pending_permissions: std::collections::VecDeque<PermissionRequest>,
    pub pending_questions: std::collections::VecDeque<UserQuestionRequest>,
    pub pending_inputs: std::collections::VecDeque<nuo_protocol::InputRequest>,
    /// Full-duplex (ADR-0029): which subagent (by parent tool-call id)
    /// surfaced a given permission / ask_user request, so the modal's reply
    /// can be tagged for down-routing.
    pub subagent_permission_parent: HashMap<String, String>,
    pub subagent_question_parent: HashMap<String, String>,
    pub pending_permission: Option<PermissionRequest>,
    /// The interaction sheet currently mounted in the composer slot, if any
    /// The slot's two sibling components — the draft editor
    /// and an AI-initiated sheet — are mutually exclusive: `Some(kind)` means
    /// the sheet has replaced the composer. App-level slot state, *not*
    /// router foreground identity.
    pub active_sheet: Option<crate::shell::sheet::SheetKind>,
    /// How many permission requests are queued in the runtime (front
    /// mirrored into `pending_permission`). `> 1` renders the `N queued`
    /// sheet badge.
    pub pending_permission_depth: usize,
    /// How many user questions are queued in the runtime behind the one
    /// mirrored into `App::question`. `> 0` renders the `+N` sheet badge.
    pub pending_question_depth: usize,
    /// The pending interactive-input request (L3.5 β) from an interactive
    /// `bash` command, or `None`. Set when a `RoundEvent::InputRequest` arrives;
    /// the input-injection modal reads it for its prompt/command/secret.
    pub pending_input: Option<nuo_protocol::InputRequest>,
    /// The open question (ask_user) modal's self-contained MVU state, or
    /// `None` when no question modal is open. Replaces the four separate
    /// `question_*` fields that previously scattered the modal's state across
    /// `App`; all interaction now flows through `QuestionModel::update`.
    pub question: Option<crate::shell::question_model::QuestionModel>,
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
    pub permission_confirm_always: bool,
    /// Whether the inline permission sheet is expanded to show the full
    /// description + arguments. Collapsed by default so the prompt stays
    /// brief; "Details" toggles this.
    pub permission_show_details: bool,
    pub permission_scroll: usize,
    pub permission_max_scroll: usize,
}

/// `surfaces` state.
pub struct SurfacesState {
    /// Selection cursor for the **sheet/scene** surfaces (permission and
    /// question sheets, the input-injection prompt, and the Dashboard's host
    /// list). Floating dialogs no longer share it: each dialog entity owns its
    /// own cursor (`SurfaceRouter::dialogs`), so this is no longer a
    /// cross-dialog scratchpad field (ADR-0035, `[INV-SURFACE-01]`).
    pub modal_index: usize,
    /// Body scroll offset of the config category list.
    pub config_scroll: usize,
    /// Which pane of the `/config` Settings View currently owns the keyboard.
    pub config_focus: crate::shell::overlays::ConfigFocus,
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
    /// Cached workspace + user theme files loaded on startup and refreshed
    /// when opening `/settings`. Keeps Settings rendering and Up/Down
    /// navigation 100% free of filesystem I/O (`[INV-ARCH-VIEW-ZERO-IO]`).
    pub theme_files: Vec<nuo_ui_primitives::ThemeFile>,
    /// What the pointer currently rests on inside the Tier 1 TabBar (ADR-0223, ADR-0152).
    ///
    /// Kept on the app rather than derived from the pointer position at draw
    /// time because the *committed* scene is the authority for what is under the
    /// pointer: the pill that painted the close mark is the pill that answers
    /// the click on it.
    pub tab_hover: Option<nuo_view::view_header::TabBarHover>,
    /// When the pointer arrived on [`Self::tab_hover`]'s **anchor** (a pill, the
    /// add affordance, or the rail).
    ///
    /// Deliberately not reset when the pointer steps from a pill onto its own
    /// close mark: that is one continuous visit, and restarting the clock there
    /// would rewind the title reveal under the user's pointer (ADR-0152).
    pub tab_hover_since: Option<std::time::Instant>,
    /// Whether the last drawn frame had a TabBar hover title reveal in flight.
    ///
    /// Written by the draw, read by the event loop's `animating` predicate — the
    /// same one-way reporting the Models dialog uses for focused-row reveal
    /// (ADR-0152), so an idle strip costs no repaints while a hovered overflow
    /// still finishes its single pass.
    pub tab_reveal_active: bool,
    /// Whether the last drawn frame painted a live `Running` text shimmer on
    /// any *visible* TabBar pill. Driven strictly by the renderer so off-screen
    /// or hover-paused running tabs never burn 30Hz wakeups.
    pub tab_shimmer_active: bool,
    /// Latest authoritative `[web]` selection/readiness snapshot from the harness.
    /// Refreshed when the Settings view opens (`QueryWebSearchConfig`) and
    /// on every `WebSearchConfigUpdated` ack.
    pub websearch_config: Option<nuo_protocol::WebSearchConfigView>,
    /// Floating dropdown state and anchor for settings popup selectors (e.g.
    /// Web Search provider and Web Fetch reader selectors).
    pub config_dropdown: Option<(
        nuo_ui_primitives::components::dropdown::DropdownState<String>,
        nuo_ui_primitives::components::dropdown::DropdownAnchor,
    )>,
    /// Authoritative screen rectangle of the active settings detail row,
    /// updated during settings render to anchor popovers precisely.
    pub config_selected_rect: Option<graticule::Rect>,
    /// Live search filter substring for the dashboard thread fleet table.
    pub dashboard_filter: String,
    /// Whether the `/` inline filter search input is open on the dashboard.
    pub dashboard_filtering: bool,
    /// Whether the dashboard is currently displaying the Model Usage & Analytics view
    /// (`true`) or the Threads Fleet Orchestration view (`false`, default) (ADR-0129).
    pub dashboard_view_usage: bool,
    /// Scroll slot + selection-follow for the `/host` panel body.
    pub host_scroll: usize,
    pub host_modal_follow: bool,
    /// Which pane of the `/host` session dashboard owns the keyboard: the
    /// console/input region (default) or the sessions dock (`Tab` toggles).
    pub host_focus: crate::shell::overlays::DashboardFocus,
    /// Scroll offset for the dashboard's console pane.
    pub host_detail_scroll: usize,
    /// The dashboard's session preview modal: the session id
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
    /// Whether the dashboard's kill confirmation is armed: `k` on a dock
    /// selection asks first (`k` again confirms within the window, anything
    /// else cancels). Killing is irreversible, so it stays a two-surface
    /// gesture like the queue's `Shift+D`.
    pub host_kill_confirm: Option<String>,
    /// Id the armed kill confirmation refers to (kept separately so a dock
    /// selection move between presses can be compared against it).
    pub host_kill_confirm_id: Option<String>,
    /// Scroll offset of the log scene's body.
    pub command_log_scroll: usize,
    /// `/host` Enter on a hosted session: the id to switch to, read by the
    /// caller after the TUI exits to re-attach.
    pub switch_to_target: Option<String>,
    /// Pending action requested by clicking a row in the Help dialog.
    pub pending_help_action: Option<crate::shell::overlays::help::HelpActionKind>,
    /// Entries recently run from the command palette (RFC-0005).
    pub(crate) palette_recents: crate::shell::palette::Recents,
    /// Pending "copy the rows this fold hides" request, published by a click on
    /// a folded command body's `copy` verb.
    ///
    /// The pointer handler holds no clipboard channel (the write is an async task
    /// owned by the action loop), so the click states the *intent* and the loop
    /// serves it — the same resolution the keyboard path uses, which is what keeps
    /// one verb from having two different copy implementations.
    pub pending_fold_copy: Option<usize>,
    /// Optional opening prompt to submit after re-attaching to a forked child
    /// (`switch_to_target`). Set when a retired `/btw <prompt>` steer forks,
    /// so the aside's question lands in the new branch (`[INV-DEPR-01]`).
    pub switch_to_prompt: Option<String>,
    /// Which full-screen overlay (if any) the TUI opened straight into at
    /// startup instead of a thread view. In that mode the overlay is not
    /// a transient modal — there is no thread the user asked for behind
    /// it — so closing it must quit the program rather than drop into an empty
    /// chat. Cleared (set to [`crate::shell::StartupOverlay::None`]) once a
    /// session is opened from the picker. Always `None` for the in-session
    /// `/sessions` modal, which just dismisses on Esc/click-out.
    pub startup_overlay: crate::shell::StartupOverlay,
    /// The PreAttach interstitial state, mounted when the attaching
    /// workspace's first-contact trust snapshot is `Quarantined`
    /// `Some` means the chat surface is gated and the
    /// per-frame render paints a full-screen black interstitial
    /// instead of chat; the per-frame sync clears it once a
    /// subsequent `HarnessState` reports `aggregate() == Trusted`.
    /// Force-mounted by `NUO_FORCE_PRE_ATTACH=1` for acceptance.
    pub pre_attach: Option<crate::shell::PreAttachState>,
    /// Lowercase provider name → whether a usable API key is configured.
    pub key_status: HashMap<String, bool>,
    /// Live model-picker snapshot (default id + per-model favorite / key-ready
    /// / last-used). Drives the `/models` and `/connections` pickers' rendering
    /// and sort order. Refreshed from the response listener each frame.
    pub provider_picker: ProviderPickerSnapshot,
    /// Whether the server has delivered at least one authoritative
    /// `ProviderPickerSnapshot`. Distinguishes "startup snapshot not received
    /// yet" (suppress false setup warning) from "synced with zero connections"
    /// (surface `LandingGuidance::NeedsProvider`).
    pub provider_picker_synced: bool,
    /// Theme.
    pub theme: Theme,
    /// Terminal capability profile (ADR-0053).
    pub profile: graticule::TerminalProfile,
    /// User-supplied ASCII logo lines loaded at startup from
    /// `$XDG_CONFIG_HOME/nuo/logo.txt` (clamped to the empty-state bounding
    /// box). `None` when no user logo is present → built-in wordmark is used.
    /// Passed into the empty-state hero via `TranscriptProps::logo`.
    pub logo: Option<Vec<String>>,
    /// User remaps of the full-screen-view surface verbs (`session.*` dotted
    /// keys). The Session/Subagent/Side resolvers and the composer
    /// hint row consult it.
    pub surface_overrides: crate::shell::keymap::SurfaceOverrides,
    /// Canonical active color-scheme id (`zen`, a built-in preset, or
    /// `custom`). The renderer theme is rebuilt from this value immediately
    /// when the Appearance page applies a choice.
    pub color_scheme: String,
    /// Last persisted custom semantic palette. Retained while a preset is
    /// active so switching schemes never discards the user's colors.
    pub custom_color_scheme: nuo_ui_primitives::ColorSchemeConfig,

    /// Whether clicking outside a dismissable modal closes it (mirroring Esc).
    /// From `[tui] click_outside_dismiss` (default `true`): when true, an
    /// outside click dismisses a dismissable modal like Esc (the draft is
    /// parked, so nothing is lost). Modals holding precious in-progress input
    /// are never click-dismissable regardless of this flag, and the `nuo
    /// resume` startup picker's click-outside still quits. Esc / Ctrl+C always
    /// close/quit regardless of this flag.
    pub click_outside_dismiss: bool,
    /// User remaps of the global chords (`[keybindings]` config).
    /// Resolution and the visible keycaps both consult it, so a remapped
    /// binding fires and advertises consistently.
    pub key_overrides: crate::shell::keymap::GlobalOverrides,
    /// Show a brief "copied" toast. Held until this deadline elapses so the
    /// duration is wall-clock consistent regardless of the event-loop cadence.
    pub copy_toast_until: Option<std::time::Instant>,
    pub copy_toast_message: String,
    pub copy_toast_failed: bool,
    /// A transient notice toast (command acknowledgments such as
    /// `/delegate on`, routed to the toast layer by `nuo_view::model::layer`). Unlike the
    /// inline `MessageKind::Notice`, this never enters the transcript: it
    /// renders as a top-right bubble that fades on its own, mirroring the copy
    /// toast. Severity drives the bubble's accent color. Held until
    /// `notice_toast_until` elapses so the duration is wall-clock consistent
    /// regardless of the loop cadence. A newer toast replaces an in-flight one.
    pub notice_toast_until: Option<std::time::Instant>,
    pub notice_toast_message: String,
    /// The subsystem tag painted on the fault bubble (`backend`, `copy`, …) so
    /// the bubble names the same topic the viewer will show.
    pub notice_toast_topic: String,
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
    /// The parked drafts, one per thread (ADR-0247).
    ///
    /// Owned here rather than on each tab: a tab is a view, and the text belongs
    /// to the thread. This is the only place an unsent draft can outlive the
    /// view that held it — and the only place it outlives the process.
    pub drafts: crate::shell::draft_store::DraftLedger,
    /// Authoritative foreground surface: the root Scene plus the LIFO overlay
    /// stack floating over it. The router is the single navigation
    /// truth — there is no parallel `active_modal` / `active_panel` /
    /// `current_view` mirror to read instead.
    pub(crate) surfaces: crate::shell::surfaces::SurfaceRouter,
    /// MRU open order of dialogs. Dialog *state* lives in the
    /// encapsulated entities (`SurfaceRouter::dialogs`); this store only tracks
    /// which dialogs have been opened and in what order (ADR-0035).
    pub(crate) surface_store: crate::shell::surfaces::SurfaceStore,
    /// Full detail and usage for the connection under the detail sub-view. Populated by
    /// an on-demand `QueryConnectionDetail` round-trip when the sub-view opens (Enter).
    /// `None` while the round-trip is in flight.
    pub connection_detail: Option<nuo_protocol::ConnectionDetail>,
}

/// `provider_editor` state.
pub struct ProviderEditorState {
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
    /// (the server's resolution), **not** re-derived client-side: this
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
    /// Toggled with Space when `Self::editor_thinking_available` is true;
    /// orthogonal to effort.
    pub editor_thinking: bool,
    /// Capability-override tri-state for **vision** (layer 1), shown
    /// in the per-model settings editor. Cycled with Space: `None` = inherit
    /// (no override) → `Some(true)` force on → `Some(false)` force off.
    pub editor_vision_override: Option<bool>,
    /// Capability-override tri-state for **tool calling**, same cycling and
    /// semantics as `Self::editor_vision_override`.
    pub editor_tool_override: Option<bool>,
    /// Focused field of the provider editor (`Modal::CustomProvider`) as an
    /// index into `Self::custom_fields` — the per-template visible field set
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
    /// Connection budget policy configured in create/edit modal (ADR-0065).
    pub custom_budget: nuo_protocol::ConnectionBudget,
    /// Models seeded by the active template (create mode). Submitted as the
    /// provider's model list unless the editor exposes a free-text Model field
    /// (then the single typed model is submitted instead). Empty in edit mode.
    pub custom_models: Vec<String>,
    /// Base URL placeholder for the active template (the expected endpoint shape).
    pub custom_url_hint: String,
    /// Template-specific user agent carried into newly-created channels.
    pub custom_user_agent: Option<String>,
    /// How newly-created connections authenticate (from the selected template).
    pub custom_auth: nuo_protocol::ConnectionAuth,
    /// The **model provider id** the active create flow was seeded from, or
    /// `None` in edit mode / when no template is active. Sent as
    /// `AddConnection::provider` (the wire field): it is the
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
}

/// `telemetry` state.
pub struct TelemetryState {
    /// Shared token-source ledger (reported vs. estimated token accounting),
    /// read by the Telemetry modal.
    pub token_ledger: Option<Arc<nuo_protocol::TokenSourceLedger>>,
    /// Latest session-scoped AI context snapshot from the harness.
    pub context_tokens: Option<nuo_protocol::ContextTokenSnapshot>,
    /// Per-session context snapshots (M1): the applier keys them by
    /// session; the render loop projects the *viewed* session's value into
    /// `context_tokens` each frame.
    pub context_tokens_by_session: HashMap<String, nuo_protocol::ContextTokenSnapshot>,
    /// Cross-session usage-statistics report (ADR-0129 [INV-USAGE-01]).
    /// Session-independent: it aggregates the durable store under `data/usage/`,
    /// which survives session cleanup.
    pub usage_stats: Option<nuo_protocol::usage_stats::UsageStatsReport>,
    /// Whether an on-demand usage report fetch is currently in flight.
    pub usage_loading: bool,
    /// Scroll offset for the model usage statistics report view.
    pub usage_scroll: usize,
    /// Provider quota pool snapshot (`QueryProviderQuotas`, ADR-0036).
    pub provider_quotas: Option<nuo_protocol::ProviderQuotaSnapshot>,
    /// Cached connection usage states for ambient indicators.
    pub connection_usages: std::collections::HashMap<String, nuo_protocol::ConnectionUsageState>,
}

/// Client-side transport connection lifecycle (ADR-0260 `[INV-SRVPHASE-05]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientLinkPhase {
    /// Discovering, spawning, dialing, or handshaking with the Nuo server.
    Connecting { label: String },
    /// Connected to a Nuo server (`server_id` identifies the server boot instance).
    Connected { server_id: String },
    /// Backing off before re-dialing after a dropped link or server restart.
    Reconnecting {
        endpoint: String,
        attempt: u32,
        max_attempts: u32,
        delay_ms: Option<u64>,
    },
    /// Exhausted retry budget or fatal connection error.
    Failed { message: String },
}

/// `link` state.
pub struct LinkState {
    pub tx: mpsc::UnboundedSender<crate::shell::client::ClientOutbound>,
    pub should_quit: Arc<AtomicBool>,
    /// Wall-clock instant of the last user key press or input edit. Used by
    /// the event loop to quiesce background animation redraws while active
    /// composition / typing is in progress.
    pub last_key_press: std::time::Instant,
    /// Dead-link latch (D6): set the first time an outbox send
    /// fails (the session driver's receiver is gone) or the reader dies.
    pub link_down: bool,
    /// The link died and reconnection is requested (ADR-0136, ADR-0260).
    pub relink_requested: bool,
    /// Client transport connection phase projected above `ServerPhase` and
    /// `ThreadPhase` (ADR-0260 `[INV-SRVPHASE-05]`).
    pub phase: ClientLinkPhase,
    /// Instant when `phase` last transitioned (used for 120ms anti-flicker hysteresis).
    pub phase_since: std::time::Instant,
    /// Last observed `server_id` from `Wire::ServerHello` to detect server
    /// restarts across reconnects (ADR-0260).
    pub known_server_id: Option<String>,
    /// True when an in-process `ConnectionSupervisor` owns reconnection in-place
    /// so `App` never tears down across link drops (ADR-0260).
    pub supervised: bool,
    /// Cancels an in-flight connect or reconnect backoff when the user aborts.
    pub supervisor_cancel: Option<tokio_util::sync::CancellationToken>,
    /// Wakes the supervisor immediately when an outbox send detects a dead channel.
    pub supervisor_reconnect_notify: Option<Arc<tokio::sync::Notify>>,
    /// One-shot backend navigation signals (M1): the applier latches
    /// them; the loop consumes and clears when it mounts the surface.
    pub open_sessions_signal: bool,
    pub open_tree_signal: bool,
}
