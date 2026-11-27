//! Single-owner resident thread instances (`ThreadInstance` / `Threads`, ADR-0253).
//!
//! Every resident thread — foreground and background alike — has one representation
//! ([`ThreadInstance`]). Switching tabs moves the whole [`ThreadInstance`] atomically
//! between the active slot and the resident map in O(1) with zero field-by-field copying
//! and zero manual reconciliation (`[INV-INSTANCE-01]`, `[INV-INSTANCE-02]`).

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use nuo_protocol::{LoopStatus, ThreadTarget};
use nuo_view::model::document::TranscriptMessage;
use nuo_view::model::selection::{SelectionDrag, SelectionState};

use crate::shell::app::state::TranscriptSearchState;
use crate::shell::app::{App, ProviderRetryState, ZoomFrame};
use crate::shell::focus::Focus;

/// Monotonic thread identity carrying a server-assigned generation
/// (ADR-0253 `[INV-INSTANCE-03]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ThreadKey {
    pub id: String,
    pub generation: u64,
}

impl ThreadKey {
    pub fn new(id: impl Into<String>, generation: u64) -> Self {
        Self {
            id: id.into(),
            generation,
        }
    }
}

/// Unseen terminal outcome of a round that finished while this thread was not
/// the actively viewed tab.
///
/// Projected onto the Tier 1 TabBar as semantic text color (`theme.ok()` for
/// `Succeeded`, `theme.err()` for `Failed`) and cleared automatically the
/// moment the user views the thread or starts a new round on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnseenOutcome {
    Succeeded,
    Failed,
}

/// Everything that belongs to one resident thread and survives tab switches
/// (ADR-0253 `[INV-INSTANCE-01]`).
///
/// Foreground and background threads share this single type; there is no separate
/// foreground copy and no field-by-field reconciliation function.
pub struct ThreadInstance {
    // ---- Identity & readiness ---------------------------------------------
    pub key: ThreadKey,
    pub session_phase: nuo_protocol::ThreadPhase,

    // ---- Content & layout caches ------------------------------------------
    pub messages: Vec<TranscriptMessage>,
    pub layout_height_cache: crate::shell::render::HeightCache,
    pub command_log: Option<nuo_view::model::command_log::CommandLog>,
    pub diff_viewer: Option<crate::shell::overlays::DiffViewerState>,

    // ---- View pose of this thread (preserved across tab switches) ---------
    pub scroll: usize,
    pub follow_bottom: bool,
    pub content_lines: usize,
    pub max_scroll: usize,
    pub sticky_step: Option<usize>,
    pub transcript_changed_pending: bool,
    pub sticky_summary_line: Option<usize>,
    pub pin_summary_line: Option<usize>,
    pub scroll_settle_pending: bool,
    pub selection: SelectionState,
    pub focus: Focus,
    pub focus_stack: Vec<ZoomFrame>,
    pub search: TranscriptSearchState,

    // ---- Per-thread server projections ------------------------------------
    pub current_provider: String,
    pub current_model: String,
    pub current_workspace: String,
    pub current_role: Option<String>,
    pub loop_status: LoopStatus,
    pub harness_retry_pending: bool,
    pub phase: Option<nuo_view::phase::Phase>,
    pub provider_retry: Option<ProviderRetryState>,
    pub unattended: bool,
    pub confined: bool,
    pub round_count: u64,
    pub current_turn: u64,
    pub round_started_at: Option<std::time::Instant>,
    pub last_turn_performance: Option<nuo_protocol::TurnPerformanceSnapshot>,
    pub session_cost_nano_usd: u64,
    pub background_tasks: Vec<nuo_view::chrome::tasks_bar::BackgroundTaskItem>,
    pub session_tree: nuo_protocol::ThreadTree,
    pub session_context: Option<nuo_protocol::ThreadContextSnapshot>,
    pub token_report: Option<nuo_protocol::TokenSourceReport>,
    /// Per-round completion summaries received live during this client session,
    /// consumed by `/trace`'s L1 Round Inspector for exact wall/paused timing
    /// (ADR-0263 `[INV-TRACE-03]`).
    pub round_summaries: HashMap<u64, nuo_protocol::RoundSummary>,
    pub context_plan: Option<nuo_protocol::ContextPlanView>,
    pub context_plan_loaded: bool,
}

impl ThreadInstance {
    /// Construct a fresh thread instance explicitly from an authority
    /// (`Welcome`, `Subscribed`, or startup binding).
    pub fn new(id: impl Into<String>, generation: u64, phase: nuo_protocol::ThreadPhase) -> Self {
        Self {
            key: ThreadKey::new(id, generation),
            session_phase: phase,
            messages: Vec::new(),
            layout_height_cache: crate::shell::render::HeightCache::default(),
            command_log: None,
            diff_viewer: None,
            scroll: 0,
            follow_bottom: true,
            content_lines: 0,
            max_scroll: 0,
            sticky_step: None,
            transcript_changed_pending: false,
            sticky_summary_line: None,
            pin_summary_line: None,
            scroll_settle_pending: false,
            selection: SelectionState::None,
            focus: Focus::composer(),
            focus_stack: Vec::new(),
            search: TranscriptSearchState::default(),
            current_provider: String::new(),
            current_model: String::new(),
            current_workspace: String::new(),
            current_role: None,
            loop_status: LoopStatus::Idle,
            harness_retry_pending: false,
            phase: None,
            provider_retry: None,
            unattended: false,
            confined: true,
            round_count: 0,
            current_turn: 0,
            round_started_at: None,
            last_turn_performance: None,
            session_cost_nano_usd: 0,
            background_tasks: Vec::new(),
            session_tree: nuo_protocol::ThreadTree::default(),
            session_context: None,
            token_report: None,
            round_summaries: HashMap::new(),
            context_plan: None,
            context_plan_loaded: false,
        }
    }

    /// Whether this thread carries no user-facing dialogue content (ADR-0253 §5).
    ///
    /// Command rows and non-dialogue markers do not make a thread non-empty, aligning
    /// client recycle semantics with durable `ThreadData::is_user_facing_empty`.
    pub fn is_dialogue_empty(&self) -> bool {
        self.round_count == 0 && !self.messages.iter().any(|m| !m.is_command_result())
    }

    /// Write this thread's activity phase, retiring its transport-setback
    /// clause under [`nuo_view::phase::ends_transport_setback`].
    pub fn set_phase(&mut self, phase: Option<nuo_view::phase::Phase>) {
        if nuo_view::phase::ends_transport_setback(phase.as_ref()) {
            self.provider_retry = None;
        }
        self.phase = phase;
    }

    /// Whether this thread currently has a live round or optimistic dispatch in flight.
    pub fn is_responding(&self) -> bool {
        !self.loop_status.is_idle() || self.phase.is_some() || self.round_started_at.is_some()
    }

    /// Project this thread's view-scoped chrome directly from its single-owner fields.
    pub fn chrome_snapshot(&self) -> crate::shell::app::SessionChrome {
        crate::shell::app::SessionChrome {
            phase: self.phase.clone(),
            responding: self.is_responding(),
            round_count: self.round_count,
            current_turn: self.current_turn,
            round_started_at: self.round_started_at,
            can_retry: self.loop_status.is_idle() && self.harness_retry_pending,
            last_turn_performance: self.last_turn_performance,
            session_cost_nano_usd: self.session_cost_nano_usd,
            transport_setback: self.provider_retry.clone(),
        }
    }

    /// Reset per-thread transient chrome & telemetry when `/new` or `/resume`
    /// replaces a thread in place under a new id so the fresh thread never
    /// inherits the superseded thread's activity bar, cost, tasks, or context plan.
    pub fn reset_ephemeral_chrome(&mut self) {
        self.loop_status = LoopStatus::Idle;
        self.harness_retry_pending = false;
        self.set_phase(None);
        self.current_turn = 0;
        self.round_started_at = None;
        self.last_turn_performance = None;
        self.session_cost_nano_usd = 0;
        self.background_tasks.clear();
        self.session_tree = nuo_protocol::ThreadTree::default();
        self.token_report = None;
        self.round_summaries.clear();
        self.context_plan = None;
        self.context_plan_loaded = false;
    }

    /// Apply a view-scoped chrome edit directly to this thread's authoritative
    /// fields (ADR-0253 `[INV-INSTANCE-01]` / `[INV-INSTANCE-02]`).
    pub(crate) fn apply_chrome_edit(
        &mut self,
        edit: crate::shell::event_loop::mutations::ChromeEdit,
    ) {
        use crate::shell::event_loop::mutations::ChromeEdit;
        use nuo_view::phase::Phase;

        match edit {
            ChromeEdit::RoundLifecycle {
                round_count,
                running,
                can_retry,
            } => {
                self.round_count = round_count;
                self.harness_retry_pending = can_retry;
                if running {
                    self.loop_status = LoopStatus::Running;
                    self.current_turn = 0;
                    if self.round_started_at.is_none() {
                        self.round_started_at = Some(std::time::Instant::now());
                    }
                    if self.phase.is_none() {
                        self.set_phase(Some(Phase::Admitting));
                    }
                } else {
                    self.loop_status = LoopStatus::Idle;
                    self.set_phase(None);
                    self.current_turn = 0;
                    self.round_started_at = None;
                }
            }
            ChromeEdit::ActivityFolded(phase) => {
                self.set_phase(Some(phase));
            }
            ChromeEdit::PhaseOnly(phase) => {
                self.set_phase(phase);
            }
            ChromeEdit::StreamStarted => {
                if self.round_started_at.is_none() {
                    self.round_started_at = Some(std::time::Instant::now());
                }
                if !matches!(self.phase, Some(Phase::Reasoning | Phase::Answering)) {
                    self.set_phase(Some(Phase::Answering));
                }
            }
            ChromeEdit::TurnStarted { round, turn } => {
                self.round_count = round;
                self.current_turn = turn;
                self.set_phase(Some(Phase::AwaitingModel));
            }
            ChromeEdit::TransportSetback(retry) => {
                self.provider_retry = Some(retry);
                self.phase = Some(Phase::AwaitingModel);
            }
            ChromeEdit::RoundCompleted(summary) => {
                self.loop_status = LoopStatus::Idle;
                self.set_phase(None);
                self.current_turn = 0;
                self.round_started_at = None;
                if summary.round > 0 {
                    self.round_summaries.insert(summary.round, summary);
                }
            }
            ChromeEdit::RoundFailed | ChromeEdit::RoundEnded => {
                self.loop_status = LoopStatus::Idle;
                self.set_phase(None);
                self.current_turn = 0;
                self.round_started_at = None;
            }
            ChromeEdit::TurnPerformance(performance) => {
                if let Some(cost) = performance.cost_nano_usd {
                    self.session_cost_nano_usd = self.session_cost_nano_usd.saturating_add(cost);
                }
                self.last_turn_performance = Some(*performance);
            }
        }
    }
}

/// Single-owner container for all resident [`ThreadInstance`]s
/// (ADR-0253 `[INV-INSTANCE-01]`, `[INV-INSTANCE-02]`, `[INV-INSTANCE-03]`).
///
/// The foreground instance is held in an inline slot (zero-panic, O(1) access without
/// `unwrap`), and inactive resident threads live in `background`. Switching tabs moves
/// whole [`ThreadInstance`] values atomically — never copying fields one by one.
pub struct Threads {
    foreground: ThreadInstance,
    background: HashMap<String, ThreadInstance>,
}

impl Threads {
    /// Construct the container with its initial foreground thread instance.
    pub fn new(initial: ThreadInstance) -> Self {
        Self {
            foreground: initial,
            background: HashMap::new(),
        }
    }

    /// The foreground thread's id.
    pub fn foreground_id(&self) -> &str {
        &self.foreground.key.id
    }

    /// Borrow the foreground [`ThreadInstance`].
    pub fn fg(&self) -> &ThreadInstance {
        &self.foreground
    }

    /// Mutably borrow the foreground [`ThreadInstance`].
    pub fn fg_mut(&mut self) -> &mut ThreadInstance {
        &mut self.foreground
    }

    /// Borrow any resident [`ThreadInstance`] by id (foreground or background).
    pub fn get(&self, id: &str) -> Option<&ThreadInstance> {
        if id == self.foreground.key.id {
            Some(&self.foreground)
        } else {
            self.background.get(id)
        }
    }

    /// Mutably borrow any resident [`ThreadInstance`] by id (foreground or background).
    pub fn get_mut(&mut self, id: &str) -> Option<&mut ThreadInstance> {
        if id == self.foreground.key.id {
            Some(&mut self.foreground)
        } else {
            self.background.get_mut(id)
        }
    }

    /// Resolve a mutation's target thread: `None` or matching foreground id targets
    /// the foreground instance; any other id targets that resident background instance.
    /// Returns `(Some(&mut ThreadInstance), is_foreground)`.
    pub fn resolve_mut(&mut self, thread_id: Option<&str>) -> (Option<&mut ThreadInstance>, bool) {
        match thread_id {
            None | Some("") => (Some(&mut self.foreground), true),
            Some(tid) if tid == self.foreground.key.id || self.foreground.key.id.is_empty() => {
                (Some(&mut self.foreground), true)
            }
            Some(tid) => (self.background.get_mut(tid), false),
        }
    }

    /// Whether `id` is currently resident (either foreground or background).
    pub fn contains(&self, id: &str) -> bool {
        id == self.foreground.key.id || self.background.contains_key(id)
    }

    /// Explicitly register a thread instance from an authority (`Welcome`, `Subscribed`,
    /// or explicit tab subscription). If already resident, updates its generation/phase
    /// when `generation >= existing`.
    pub fn register(
        &mut self,
        id: impl Into<String>,
        generation: u64,
        phase: nuo_protocol::ThreadPhase,
    ) -> &mut ThreadInstance {
        let id = id.into();
        if id == self.foreground.key.id {
            if generation >= self.foreground.key.generation {
                self.foreground.key.generation = generation;
                self.foreground.session_phase = phase;
            }
            return &mut self.foreground;
        }
        use std::collections::hash_map::Entry;
        match self.background.entry(id.clone()) {
            Entry::Occupied(occupied) => {
                let inst = occupied.into_mut();
                if generation >= inst.key.generation {
                    inst.key.generation = generation;
                    inst.session_phase = phase;
                }
                inst
            }
            Entry::Vacant(vacant) => vacant.insert(ThreadInstance::new(id, generation, phase)),
        }
    }

    /// Atomically make `target_id` the foreground thread by moving whole
    /// [`ThreadInstance`] values. If `target_id` is not yet resident, registers a
    /// cold `Binding` instance for it.
    pub fn activate(&mut self, target_id: &str) {
        if target_id == self.foreground.key.id {
            return;
        }
        let next = self.background.remove(target_id).unwrap_or_else(|| {
            ThreadInstance::new(
                target_id,
                0,
                nuo_protocol::ThreadPhase::Preparing {
                    stage: nuo_protocol::PrepareStage::Thread,
                },
            )
        });
        let prev = std::mem::replace(&mut self.foreground, next);
        if !prev.key.id.is_empty() {
            self.background.insert(prev.key.id.clone(), prev);
        }
    }

    /// Remove a resident thread instance (foreground or background) when closing a tab.
    ///
    /// If `id` is currently the foreground instance, clears its identity so the
    /// subsequent `activate(next_id)` drops it instead of parking the closed tab
    /// into `background`.
    pub fn remove(&mut self, id: &str) -> Option<ThreadInstance> {
        if id == self.foreground.key.id {
            let removed = std::mem::replace(
                &mut self.foreground,
                ThreadInstance::new("", 0, nuo_protocol::ThreadPhase::Ready),
            );
            Some(removed)
        } else {
            self.background.remove(id)
        }
    }

    /// Re-key an instance when `/new`, `/resume`, or `/fork` replaces a thread's id
    /// in place, advancing its generation and resetting transient chrome when the
    /// thread identity actually changes.
    pub fn rekey(&mut self, previous_id: &str, next_id: impl Into<String>, generation: u64) {
        let next_id = next_id.into();
        let id_changed = previous_id != next_id;
        if previous_id == self.foreground.key.id {
            self.foreground.key.id = next_id;
            if generation > self.foreground.key.generation {
                self.foreground.key.generation = generation;
            } else {
                self.foreground.key.generation = self.foreground.key.generation.saturating_add(1);
            }
            if id_changed {
                self.foreground.reset_ephemeral_chrome();
            }
            return;
        }
        if !id_changed {
            if let Some(inst) = self.background.get_mut(previous_id) {
                inst.key.generation = inst.key.generation.max(generation);
            }
            return;
        }
        if let Some(mut inst) = self.background.remove(previous_id) {
            inst.key.id = next_id.clone();
            inst.key.generation = if generation > inst.key.generation {
                generation
            } else {
                inst.key.generation.saturating_add(1)
            };
            inst.reset_ephemeral_chrome();
            self.background.insert(next_id, inst);
        }
    }

    /// Sync the foreground key when a caller/test initializes `current_session_id`
    /// directly without going through `activate`.
    pub fn sync_foreground_id(&mut self, id: &str) {
        if self.foreground.key.id != id && !id.is_empty() && !self.background.contains_key(id) {
            self.foreground.key.id = id.to_string();
        }
    }

    /// Whether any resident thread (foreground or background) currently
    /// carries a live transport setback.
    pub fn has_live_transport_setback(&self) -> bool {
        self.foreground.provider_retry.is_some()
            || self
                .background
                .values()
                .any(|inst| inst.provider_retry.is_some())
    }

    /// Whether any resident thread (foreground or background) currently has a
    /// live round or optimistic dispatch in flight.
    pub fn any_responding(&self) -> bool {
        self.foreground.is_responding() || self.background.values().any(|inst| inst.is_responding())
    }

    /// Clear `Phase::AwaitingUser` on any resident thread parked on a permission
    /// gate without disturbing threads in other active phases.
    pub fn clear_awaiting_user_phases(&mut self) -> bool {
        let mut fg_changed = false;
        if self.foreground.phase == Some(nuo_view::phase::Phase::AwaitingUser) {
            self.foreground.set_phase(None);
            fg_changed = true;
        }
        for inst in self.background.values_mut() {
            if inst.phase == Some(nuo_view::phase::Phase::AwaitingUser) {
                inst.set_phase(None);
            }
        }
        fg_changed
    }
}

impl Deref for Threads {
    type Target = ThreadInstance;

    fn deref(&self) -> &Self::Target {
        &self.foreground
    }
}

impl DerefMut for Threads {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.foreground
    }
}

impl App {
    /// Mark a thread's unseen terminal outcome (`Succeeded` / `Failed`) as
    /// viewed so its TabBar title returns to the quiet idle tone.
    pub(crate) fn acknowledge_thread_outcome(&mut self, thread_id: &str) {
        self.session.unseen_outcomes.remove(thread_id);
    }

    /// Switch the foreground thread to `target_id` in O(1) without tearing down
    /// `App`, reconnecting, or copying fields (ADR-0251 / ADR-0253).
    pub(crate) fn activate_thread_workspace(&mut self, target_id: &str) {
        self.acknowledge_thread_outcome(target_id);
        // Ensure foreground key matches `current_session_id` before swapping.
        if !self.session.current_session_id.is_empty()
            && self.threads.foreground_id() != self.session.current_session_id
        {
            self.threads
                .sync_foreground_id(&self.session.current_session_id);
        }

        if target_id == self.session.current_session_id {
            if !target_id.is_empty()
                && self
                    .session
                    .subscribed_threads
                    .insert(target_id.to_string())
            {
                self.subscribe_thread(
                    ThreadTarget::Attach {
                        id: Some(target_id.to_string()),
                    },
                    None,
                );
            }
            return;
        }

        self.park_current_thread_draft();
        self.viewport.drag = SelectionDrag::default();
        self.viewport.hovered_step = None;

        self.threads.activate(target_id);
        self.session.current_session_id = target_id.to_string();
        self.session.live_session_id = target_id.to_string();
        self.hand_composer_to_current_thread();

        if !target_id.is_empty()
            && self
                .session
                .subscribed_threads
                .insert(target_id.to_string())
        {
            self.subscribe_thread(
                ThreadTarget::Attach {
                    id: Some(target_id.to_string()),
                },
                None,
            );
        }
    }

    /// Release a closed thread tab's resident instance and unsubscribe it on
    /// the multiplexed connection (ADR-0251 / ADR-0253).
    pub(crate) fn close_thread_workspace(&mut self, thread_id: &str) {
        self.threads.remove(thread_id);
        self.session.running_sessions.remove(thread_id);
        self.session.unseen_outcomes.remove(thread_id);
        if self.session.current_session_id == thread_id {
            self.session.current_session_id.clear();
        }
        if self.session.live_session_id == thread_id {
            self.session.live_session_id.clear();
        }
        if self.session.subscribed_threads.remove(thread_id) {
            self.unsubscribe_thread(thread_id);
        }
    }

    /// Re-key a resident instance and subscription when `/new` or `/resume`
    /// replaces a thread's id in place.
    pub(crate) fn rekey_thread_workspace(&mut self, previous_id: &str, next_id: &str) {
        self.threads.rekey(previous_id, next_id, 0);
        if previous_id != next_id {
            self.session.running_sessions.remove(previous_id);
            self.session.unseen_outcomes.remove(previous_id);
            if self.session.subscribed_threads.remove(previous_id) {
                self.session.subscribed_threads.insert(next_id.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests;
