//! Resident per-thread workspaces for O(1) tab switching (ADR-0251).
//!
//! Each open thread tab owns its own `ThreadWorkspace` containing the live
//! transcript document, layout/wrap/diff caches, scroll & follow-bottom
//! posture, focus plane, subagent zoom stack, search state, and per-thread
//! session projection. The active thread lives in `App`'s live slots
//! (`app.transcript`, `app.session`, `app.telemetry`); inactive subscribed
//! threads reside in `app.session.workspaces`.
//!
//! Switching between two resident tabs is an O(1) pointer swap (`std::mem::take`
//! / `std::mem::replace`) with zero cloning, zero Markdown re-parsing, zero
//! cache eviction, and zero network round-trip.

use protocol::{AttachAction, LoopStatus};
use view::model::document::TranscriptMessage;
use view::model::selection::{SelectionDrag, SelectionState};

use crate::shell::app::state::TranscriptSearchState;
use crate::shell::app::{App, ProviderRetryState, ZoomFrame};
use crate::shell::focus::Focus;

/// Resident in-memory state of one subscribed thread tab (ADR-0251 `[INV-MUX-02]`).
pub struct ThreadWorkspace {
    // ---- Transcript document & layout caches ------------------------------
    pub messages: Vec<TranscriptMessage>,
    pub layout_height_cache: crate::shell::render::HeightCache,
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
    pub command_log: Option<view::model::command_log::CommandLog>,
    pub diff_viewer: Option<crate::shell::overlays::DiffViewerState>,

    // ---- Per-thread session & telemetry projection ------------------------
    pub provider: String,
    pub model: String,
    pub workspace: String,
    pub role: Option<String>,
    pub loop_status: LoopStatus,
    pub harness_retry_pending: bool,
    pub phase: Option<view::phase::Phase>,
    pub provider_retry: Option<ProviderRetryState>,
    pub unattended: bool,
    pub confined: bool,
    pub round_count: u64,
    pub current_turn: u64,
    pub round_started_at: Option<std::time::Instant>,
    pub session_phase: protocol::ThreadPhase,
    pub session_tree: protocol::ThreadTree,
    pub session_context: Option<protocol::ThreadContextSnapshot>,
    pub token_report: Option<protocol::TokenSourceReport>,
    pub context_plan: Option<protocol::ContextPlanView>,
    pub context_plan_loaded: bool,
}

impl Default for ThreadWorkspace {
    fn default() -> Self {
        Self::new_binding()
    }
}

impl ThreadWorkspace {
    /// A fresh empty workspace awaiting initial `ThreadSnapshot` hydration.
    pub fn new_binding() -> Self {
        Self {
            messages: Vec::new(),
            layout_height_cache: crate::shell::render::HeightCache::default(),
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
            focus: Focus::default(),
            focus_stack: Vec::new(),
            search: TranscriptSearchState::default(),
            command_log: None,
            diff_viewer: None,
            provider: String::new(),
            model: String::new(),
            workspace: String::new(),
            role: None,
            loop_status: LoopStatus::Idle,
            harness_retry_pending: false,
            phase: None,
            provider_retry: None,
            unattended: false,
            confined: true,
            round_count: 0,
            current_turn: 0,
            round_started_at: None,
            session_phase: protocol::ThreadPhase::Binding,
            session_tree: protocol::ThreadTree::default(),
            session_context: None,
            token_report: None,
            context_plan: None,
            context_plan_loaded: false,
        }
    }
}

impl App {
    /// Park the currently foreground thread's live state into
    /// `self.session.workspaces` in O(1) without cloning.
    pub(crate) fn park_active_workspace(&mut self) {
        let current_id = self.session.current_session_id.clone();
        if current_id.is_empty() {
            return;
        }
        let ws = ThreadWorkspace {
            messages: std::mem::take(&mut self.transcript.messages),
            layout_height_cache: std::mem::take(&mut self.transcript.layout_height_cache),
            scroll: std::mem::take(&mut self.transcript.scroll),
            follow_bottom: std::mem::replace(&mut self.transcript.follow_bottom, true),
            content_lines: std::mem::take(&mut self.transcript.content_lines),
            max_scroll: std::mem::take(&mut self.transcript.max_scroll),
            sticky_step: self.transcript.sticky_step.take(),
            transcript_changed_pending: std::mem::take(
                &mut self.transcript.transcript_changed_pending,
            ),
            sticky_summary_line: self.transcript.sticky_summary_line.take(),
            pin_summary_line: self.transcript.pin_summary_line.take(),
            scroll_settle_pending: std::mem::take(&mut self.transcript.scroll_settle_pending),
            selection: std::mem::replace(&mut self.transcript.selection, SelectionState::None),
            focus: std::mem::take(&mut self.transcript.focus),
            focus_stack: std::mem::take(&mut self.transcript.focus_stack),
            search: std::mem::take(&mut self.transcript.search),
            command_log: self.transcript.command_log.take(),
            diff_viewer: self.transcript.diff_viewer.take(),
            provider: std::mem::take(&mut self.session.current_provider),
            model: std::mem::take(&mut self.session.current_model),
            workspace: std::mem::take(&mut self.session.current_workspace),
            role: self.session.current_role.take(),
            loop_status: std::mem::replace(&mut self.session.loop_status, LoopStatus::Idle),
            harness_retry_pending: std::mem::take(&mut self.session.harness_retry_pending),
            phase: self.session.phase.take(),
            provider_retry: self.session.provider_retry.take(),
            unattended: std::mem::take(&mut self.session.unattended),
            confined: std::mem::replace(&mut self.session.confined, true),
            round_count: std::mem::take(&mut self.session.round_count),
            current_turn: std::mem::take(&mut self.session.current_turn),
            round_started_at: self.session.round_started_at.take(),
            session_phase: std::mem::replace(
                &mut self.session.session_phase,
                protocol::ThreadPhase::Binding,
            ),
            session_tree: std::mem::take(&mut self.session.session_tree),
            session_context: self.session.session_context.take(),
            token_report: self.telemetry.token_report.take(),
            context_plan: self.telemetry.context_plan.take(),
            context_plan_loaded: std::mem::take(&mut self.telemetry.context_plan_loaded),
        };
        self.session.workspaces.insert(current_id, ws);
    }

    /// Install a `ThreadWorkspace` into `App`'s live foreground slots in O(1).
    fn install_workspace(&mut self, ws: ThreadWorkspace) {
        self.transcript.messages = ws.messages;
        self.transcript.layout_height_cache = ws.layout_height_cache;
        self.transcript.scroll = ws.scroll;
        self.transcript.follow_bottom = ws.follow_bottom;
        self.transcript.content_lines = ws.content_lines;
        self.transcript.max_scroll = ws.max_scroll;
        self.transcript.sticky_step = ws.sticky_step;
        self.transcript.transcript_changed_pending = ws.transcript_changed_pending;
        self.transcript.sticky_summary_line = ws.sticky_summary_line;
        self.transcript.pin_summary_line = ws.pin_summary_line;
        self.transcript.scroll_settle_pending = ws.scroll_settle_pending;
        self.transcript.selection = ws.selection;
        self.transcript.drag = SelectionDrag::default();
        self.transcript.hovered_step = None;
        self.transcript.focus = ws.focus;
        self.transcript.focus_stack = ws.focus_stack;
        self.transcript.search = ws.search;
        self.transcript.command_log = ws.command_log;
        self.transcript.diff_viewer = ws.diff_viewer;

        self.session.current_provider = ws.provider;
        self.session.current_model = ws.model;
        self.session.current_workspace = ws.workspace;
        self.session.current_role = ws.role;
        self.session.loop_status = ws.loop_status;
        self.session.harness_retry_pending = ws.harness_retry_pending;
        self.session.phase = ws.phase;
        self.session.provider_retry = ws.provider_retry;
        self.session.unattended = ws.unattended;
        self.session.confined = ws.confined;
        self.session.round_count = ws.round_count;
        self.session.current_turn = ws.current_turn;
        self.session.round_started_at = ws.round_started_at;
        self.session.session_phase = ws.session_phase;
        self.session.session_tree = ws.session_tree;
        self.session.session_context = ws.session_context;
        self.telemetry.token_report = ws.token_report;
        self.telemetry.context_plan = ws.context_plan;
        self.telemetry.context_plan_loaded = ws.context_plan_loaded;
    }

    /// Switch the foreground workspace to `target_id` in O(1) without tearing
    /// down `App` or reconnecting (ADR-0251 `[INV-MUX-02]`).
    ///
    /// If `target_id` is already subscribed on this connection, its resident
    /// workspace (already updated live in the background) is installed with
    /// zero network traffic. Otherwise, a cold binding workspace is mounted and
    /// `Wire::Subscribe` is sent on the existing connection.
    pub(crate) fn activate_thread_workspace(&mut self, target_id: &str) {
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

        self.park_active_workspace();
        self.park_current_thread_draft();

        let ws = self
            .session
            .workspaces
            .remove(target_id)
            .unwrap_or_else(ThreadWorkspace::new_binding);
        self.install_workspace(ws);

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

    /// Release a closed thread tab's resident workspace and unsubscribe it on
    /// the multiplexed connection (ADR-0251).
    pub(crate) fn close_thread_workspace(&mut self, thread_id: &str) {
        self.session.workspaces.remove(thread_id);
        if self.session.subscribed_threads.remove(thread_id) {
            self.unsubscribe_thread(thread_id);
        }
    }

    /// Re-key a resident workspace and subscription when `/new` or `/resume`
    /// replaces a thread's id in place.
    pub(crate) fn rekey_thread_workspace(&mut self, previous_id: &str, next_id: &str) {
        if previous_id == next_id {
            return;
        }
        if let Some(ws) = self.session.workspaces.remove(previous_id) {
            self.session.workspaces.insert(next_id.to_string(), ws);
        }
        if self.session.subscribed_threads.remove(previous_id) {
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

    /// ADR-0251 `[INV-MUX-02]`: switching between two resident thread tabs swaps
    /// their workspaces in O(1) — preserving messages, scroll, follow_bottom,
    /// disclosure state, composer drafts, and model attribution with zero
    /// network traffic or rebuild.
    #[test]
    fn switching_between_resident_tabs_preserves_state_without_rebuild() {
        let mut app = new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;

        app.session.current_session_id = "thread-a".to_string();
        app.session.live_session_id = "thread-a".to_string();
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
        app.surfaces.surfaces.select_tab(0);
        app.hand_composer_to_current_thread();
        app.session.current_model = "deepseek-v4.1-flash".to_string();
        app.transcript
            .messages
            .push(TranscriptMessage::new(Role::User, "Question in thread A"));
        app.transcript.scroll = 42;
        app.transcript.follow_bottom = false;
        app.composer.input = "Draft for A".to_string();

        // First switch to cold thread-b: parks A, mounts B, sends one Subscribe.
        app.surfaces.surfaces.select_tab(1);
        app.activate_thread_workspace("thread-b");
        assert_eq!(app.session.current_session_id, "thread-b");
        assert!(app.transcript.messages.is_empty());
        assert_eq!(app.composer.input, "");
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientOutbound::Subscribe {
                action: protocol::AttachAction::Attach(Some(ref tid)),
                ..
            }) if tid == "thread-b"
        ));

        // Populate thread-b.
        app.session.current_model = "gemini-4-neon".to_string();
        app.transcript
            .messages
            .push(TranscriptMessage::new(Role::User, "Question in thread B"));
        app.transcript.scroll = 17;
        app.composer.input = "Draft for B".to_string();

        // Switch back to thread-a: ZERO Subscribe frames, exact state restored!
        app.surfaces.surfaces.select_tab(0);
        app.activate_thread_workspace("thread-a");
        assert!(
            rx.try_recv().is_err(),
            "switching to an already-subscribed resident tab must send zero wire frames"
        );
        assert_eq!(app.session.current_session_id, "thread-a");
        assert_eq!(app.session.current_model, "deepseek-v4.1-flash");
        assert_eq!(app.transcript.messages.len(), 1);
        assert_eq!(app.transcript.messages[0].raw, "Question in thread A");
        assert_eq!(app.transcript.scroll, 42);
        assert!(!app.transcript.follow_bottom);
        assert_eq!(app.composer.input, "Draft for A");

        // Switch back to thread-b: ZERO Subscribe frames, exact state restored!
        app.surfaces.surfaces.select_tab(1);
        app.activate_thread_workspace("thread-b");
        assert!(rx.try_recv().is_err());
        assert_eq!(app.session.current_model, "gemini-4-neon");
        assert_eq!(app.transcript.messages.len(), 1);
        assert_eq!(app.transcript.messages[0].raw, "Question in thread B");
        assert_eq!(app.transcript.scroll, 17);
        assert_eq!(app.composer.input, "Draft for B");
    }

    /// ADR-0251 `[INV-MUX-03]`: background thread streaming mutations land
    /// directly in the background workspace without disturbing the foreground
    /// tab, and are immediately visible when switching back.
    #[test]
    fn background_thread_mutations_update_resident_workspace_live() {
        let mut app = new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.session.current_session_id = "thread-a".to_string();
        app.session.live_session_id = "thread-a".to_string();
        app.session
            .subscribed_threads
            .insert("thread-a".to_string());
        app.session
            .subscribed_threads
            .insert("thread-b".to_string());
        app.transcript
            .messages
            .push(TranscriptMessage::new(Role::User, "A prompt"));

        // Switch to thread-b while thread-a is parked in background.
        app.activate_thread_workspace("thread-b");
        assert_eq!(app.session.current_session_id, "thread-b");
        assert!(app.transcript.messages.is_empty());

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
            app.transcript.messages.is_empty(),
            "foreground thread-b must remain untouched"
        );

        // Switching back to thread-a immediately reveals the live reply!
        app.activate_thread_workspace("thread-a");
        assert_eq!(app.transcript.messages.len(), 2);
        assert_eq!(app.transcript.messages[1].raw, "Live reply for A");
    }

    /// Closing a thread tab drops its resident workspace and emits
    /// `ClientOutbound::Unsubscribe`.
    #[test]
    fn closing_thread_workspace_unsubscribes_and_frees_memory() {
        let mut app = new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.link.tx = tx;

        app.session.current_session_id = "thread-a".to_string();
        app.session.live_session_id = "thread-a".to_string();
        app.session
            .subscribed_threads
            .insert("thread-a".to_string());
        app.session
            .subscribed_threads
            .insert("thread-b".to_string());
        app.activate_thread_workspace("thread-b");
        app.activate_thread_workspace("thread-a");
        assert!(app.session.workspaces.contains_key("thread-b"));

        app.close_thread_workspace("thread-b");
        assert!(!app.session.workspaces.contains_key("thread-b"));
        assert!(!app.session.subscribed_threads.contains("thread-b"));
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientOutbound::Unsubscribe { thread_id }) if thread_id == "thread-b"
        ));
    }
}
