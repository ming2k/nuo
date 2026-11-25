//! Single-owner resident thread instances (`ThreadInstance` / `Threads`, ADR-0253).
//!
//! Every resident thread — foreground and background alike — has one representation
//! ([`ThreadInstance`]). Switching tabs moves the whole [`ThreadInstance`] atomically
//! between the active slot and the resident map in O(1) with zero field-by-field copying
//! and zero manual reconciliation (`[INV-INSTANCE-01]`, `[INV-INSTANCE-02]`).

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use protocol::{AttachAction, LoopStatus};
use view::model::document::TranscriptMessage;
use view::model::selection::{SelectionDrag, SelectionState};

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

/// Everything that belongs to one resident thread and survives tab switches
/// (ADR-0253 `[INV-INSTANCE-01]`).
///
/// Foreground and background threads share this single type; there is no separate
/// foreground copy and no field-by-field reconciliation function.
pub struct ThreadInstance {
    // ---- Identity & readiness ---------------------------------------------
    pub key: ThreadKey,
    pub session_phase: protocol::ThreadPhase,

    // ---- Content & layout caches ------------------------------------------
    pub messages: Vec<TranscriptMessage>,
    pub layout_height_cache: crate::shell::render::HeightCache,
    pub command_log: Option<view::model::command_log::CommandLog>,
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
    pub phase: Option<view::phase::Phase>,
    pub provider_retry: Option<ProviderRetryState>,
    pub unattended: bool,
    pub confined: bool,
    pub round_count: u64,
    pub current_turn: u64,
    pub round_started_at: Option<std::time::Instant>,
    pub session_tree: protocol::ThreadTree,
    pub session_context: Option<protocol::ThreadContextSnapshot>,
    pub token_report: Option<protocol::TokenSourceReport>,
    pub context_plan: Option<protocol::ContextPlanView>,
    pub context_plan_loaded: bool,
}

impl ThreadInstance {
    /// Construct a fresh thread instance explicitly from an authority
    /// (`Welcome`, `Subscribed`, or startup binding).
    pub fn new(id: impl Into<String>, generation: u64, phase: protocol::ThreadPhase) -> Self {
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
            session_tree: protocol::ThreadTree::default(),
            session_context: None,
            token_report: None,
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
        phase: protocol::ThreadPhase,
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
        let next = self
            .background
            .remove(target_id)
            .unwrap_or_else(|| ThreadInstance::new(target_id, 0, protocol::ThreadPhase::Binding));
        let prev = std::mem::replace(&mut self.foreground, next);
        if !prev.key.id.is_empty() {
            self.background.insert(prev.key.id.clone(), prev);
        }
    }

    /// Remove a background thread instance (called when closing a tab).
    pub fn remove_background(&mut self, id: &str) -> Option<ThreadInstance> {
        self.background.remove(id)
    }

    /// Re-key an instance when `/new`, `/resume`, or `/fork` replaces a thread's id
    /// in place, advancing its generation.
    pub fn rekey(&mut self, previous_id: &str, next_id: impl Into<String>, generation: u64) {
        let next_id = next_id.into();
        if previous_id == self.foreground.key.id {
            self.foreground.key.id = next_id;
            if generation > self.foreground.key.generation {
                self.foreground.key.generation = generation;
            } else {
                self.foreground.key.generation = self.foreground.key.generation.saturating_add(1);
            }
            return;
        }
        if previous_id == next_id {
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
    /// Switch the foreground thread to `target_id` in O(1) without tearing down
    /// `App`, reconnecting, or copying fields (ADR-0251 / ADR-0253).
    pub(crate) fn activate_thread_workspace(&mut self, target_id: &str) {
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
                self.subscribe_thread(AttachAction::Attach(Some(target_id.to_string())), None);
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
            self.subscribe_thread(AttachAction::Attach(Some(target_id.to_string())), None);
        }
    }

    /// Release a closed thread tab's resident instance and unsubscribe it on
    /// the multiplexed connection (ADR-0251 / ADR-0253).
    pub(crate) fn close_thread_workspace(&mut self, thread_id: &str) {
        self.threads.remove_background(thread_id);
        if self.session.subscribed_threads.remove(thread_id) {
            self.unsubscribe_thread(thread_id);
        }
    }

    /// Re-key a resident instance and subscription when `/new` or `/resume`
    /// replaces a thread's id in place.
    pub(crate) fn rekey_thread_workspace(&mut self, previous_id: &str, next_id: &str) {
        self.threads.rekey(previous_id, next_id, 0);
        if previous_id != next_id && self.session.subscribed_threads.remove(previous_id) {
            self.session.subscribed_threads.insert(next_id.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::shell::client::ClientOutbound;
    use crate::shell::event_loop::apply::apply;
    use crate::shell::event_loop::{AppMutation, TranscriptEdit, UiRuntime};
    use crate::shell::tests::new_app_for_relay_tests;
    use tool::Role;
    use view::model::document::TranscriptMessage;

    /// ADR-0251 / ADR-0253: switching between two resident thread tabs moves whole
    /// `ThreadInstance`s in O(1) — preserving messages, scroll, follow_bottom,
    /// disclosure state, composer drafts, and model attribution with zero network
    /// traffic and zero field-by-field reconciliation.
    #[test]
    fn switching_between_resident_tabs_preserves_state_without_rebuild() {
        let mut app = new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;

        app.session.current_session_id = "thread-a".to_string();
        app.session.live_session_id = "thread-a".to_string();
        app.threads.sync_foreground_id("thread-a");
        app.session
            .subscribed_threads
            .insert("thread-a".to_string());
        app.open_tab(crate::shell::surfaces::ClientTab::thread(
            "thread-a",
            Some("A"),
        ));
        app.open_tab(crate::shell::surfaces::ClientTab::thread(
            "thread-b",
            Some("B"),
        ));
        app.open_tab(crate::shell::surfaces::ClientTab::thread(
            "thread-a",
            Some("A"),
        ));
        app.threads.current_model = "deepseek-v4.1-flash".to_string();
        app.threads
            .messages
            .push(TranscriptMessage::new(Role::User, "Question in thread A"));
        app.threads.scroll = 42;
        app.threads.follow_bottom = false;
        app.composer.input = "Draft for A".to_string();

        // First switch to cold thread-b: mounts B, sends one Subscribe.
        app.surfaces.surfaces.select_tab(1);
        app.activate_thread_workspace("thread-b");
        assert_eq!(app.session.current_session_id, "thread-b");
        assert!(app.threads.messages.is_empty());
        assert_eq!(app.composer.input, "");
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientOutbound::Subscribe {
                action: protocol::AttachAction::Attach(Some(ref tid)),
                ..
            }) if tid == "thread-b"
        ));

        // Populate thread-b.
        app.threads.current_model = "gemini-4-neon".to_string();
        app.threads
            .messages
            .push(TranscriptMessage::new(Role::User, "Question in thread B"));
        app.threads.scroll = 17;
        app.composer.input = "Draft for B".to_string();

        // Switch back to thread-a: ZERO Subscribe frames, exact state restored!
        app.surfaces.surfaces.select_tab(0);
        app.activate_thread_workspace("thread-a");
        assert!(
            rx.try_recv().is_err(),
            "switching to an already-subscribed resident tab must send zero wire frames"
        );
        assert_eq!(app.session.current_session_id, "thread-a");
        assert_eq!(app.threads.current_model, "deepseek-v4.1-flash");
        assert_eq!(app.threads.messages.len(), 1);
        assert_eq!(app.threads.messages[0].raw, "Question in thread A");
        assert_eq!(app.threads.scroll, 42);
        assert!(!app.threads.follow_bottom);
        assert_eq!(app.composer.input, "Draft for A");

        // Switch back to thread-b: ZERO Subscribe frames, exact state restored!
        app.surfaces.surfaces.select_tab(1);
        app.activate_thread_workspace("thread-b");
        assert!(rx.try_recv().is_err());
        assert_eq!(app.threads.current_model, "gemini-4-neon");
        assert_eq!(app.threads.messages.len(), 1);
        assert_eq!(app.threads.messages[0].raw, "Question in thread B");
        assert_eq!(app.threads.scroll, 17);
        assert_eq!(app.composer.input, "Draft for B");
    }

    /// ADR-0251 / ADR-0253: background thread streaming mutations land directly in
    /// the background `ThreadInstance` through the same unified applier path.
    #[test]
    fn background_thread_mutations_update_resident_workspace_live() {
        let mut app = new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.session.current_session_id = "thread-a".to_string();
        app.session.live_session_id = "thread-a".to_string();
        app.threads.sync_foreground_id("thread-a");
        app.session
            .subscribed_threads
            .insert("thread-a".to_string());
        app.session
            .subscribed_threads
            .insert("thread-b".to_string());
        app.threads
            .messages
            .push(TranscriptMessage::new(Role::User, "A prompt"));

        // Switch to thread-b while thread-a stays resident in background.
        app.activate_thread_workspace("thread-b");
        assert_eq!(app.session.current_session_id, "thread-b");
        assert!(app.threads.messages.is_empty());

        // A live assistant message arrives for background thread-a.
        let bg_changed = apply(
            &mut app,
            &runtime,
            AppMutation::Transcript {
                thread_id: Some("thread-a".to_string()),
                edit: TranscriptEdit::Append {
                    class: protocol::EventClass::Dialogue,
                    message: TranscriptMessage::new(Role::Assistant, "Live reply for A"),
                },
            },
        );
        assert!(
            !bg_changed,
            "background transcript mutation must not mark foreground dirty"
        );
        assert!(
            app.threads.messages.is_empty(),
            "foreground thread-b must remain untouched"
        );

        // Switching back to thread-a immediately reveals the live reply!
        app.activate_thread_workspace("thread-a");
        assert_eq!(app.threads.messages.len(), 2);
        assert_eq!(app.threads.messages[1].raw, "Live reply for A");
    }

    /// Closing a thread tab drops its resident instance and emits
    /// `ClientOutbound::Unsubscribe`.
    #[test]
    fn closing_thread_workspace_unsubscribes_and_frees_memory() {
        let mut app = new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;

        app.session.current_session_id = "thread-a".to_string();
        app.session.live_session_id = "thread-a".to_string();
        app.threads.sync_foreground_id("thread-a");
        app.session
            .subscribed_threads
            .insert("thread-a".to_string());
        app.session
            .subscribed_threads
            .insert("thread-b".to_string());
        app.activate_thread_workspace("thread-b");
        app.activate_thread_workspace("thread-a");
        assert!(app.threads.contains("thread-b"));

        app.close_thread_workspace("thread-b");
        assert!(!app.threads.contains("thread-b"));
        assert!(!app.session.subscribed_threads.contains("thread-b"));
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientOutbound::Unsubscribe { thread_id }) if thread_id == "thread-b"
        ));
    }

    /// ADR-0253 §5: emptiness predicate agrees with durable authority — command rows
    /// do not make a thread non-empty, whereas dialogue or a started round does.
    #[test]
    fn emptiness_predicate_excludes_command_rows_and_includes_dialogue_and_rounds() {
        let mut inst = super::ThreadInstance::new("t1", 1, protocol::ThreadPhase::Ready);
        assert!(inst.is_dialogue_empty(), "fresh thread is empty");

        // Command rows are not dialogue (ADR-0252 §2, ADR-0253 §5).
        inst.messages
            .push(TranscriptMessage::pending_command("status", ""));
        assert!(
            inst.is_dialogue_empty(),
            "command-only thread must still be empty"
        );

        // Started round makes it non-empty even with zero dialogue rows.
        inst.round_count = 1;
        assert!(!inst.is_dialogue_empty(), "started round is non-empty");
        inst.round_count = 0;

        // Dialogue message makes it non-empty.
        inst.messages
            .push(TranscriptMessage::new(Role::User, "hello"));
        assert!(!inst.is_dialogue_empty(), "dialogue row is non-empty");
    }
}
