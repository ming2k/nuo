//! The nuo embedded test suite, split by surface.
//!
//! Shared fixtures live here in `mod.rs`; the per-surface test groups are
//! sibling modules.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use nuo_wire::{AgentResponse, LoopStatus, Message, Role, RoundEvent, ToolCall};

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use crate::shell::app::{App, CaretOwner, QueuedDispatch, QueuedDispatchState, RecallQueued};
use crate::shell::completion::CompletionKind;
use crate::shell::completion::{
    completion_anchor, completion_anchor_x, mention_range_at, resolved_slash_command_len,
};
use crate::shell::config;
use crate::shell::event_loop::{display_status, focused_messages_mut};
use crate::shell::render::Theme;
use crate::shell::transcript::{
    finalize_streaming_reasoning, transcript_message_from_core, transcript_messages_from_core,
};
use nuo_view::model::layout::InteractiveTarget;
use nuo_view::model::selection::{SelectionDrag, SelectionState};
use nuo_wire::{AgentRequest, ProviderPickerSnapshot};

use std::collections::HashMap;

/// The vocabulary fixture is the **server's** catalog — the same publication a
/// real client receives during attach. A test-local copy was what let the TUI's
/// popup advertise `/delegate` and lose `/quotas` without any test noticing.
fn test_command_catalog() -> nuo_wire::CommandCatalog {
    nuo_server::startup::command_catalog(&[])
}

fn thread_with_subagents() -> Vec<TranscriptMessage> {
    let mut a = TranscriptMessage::tool_step(
        "task_a",
        "spawn_agent",
        r#"{"description":"explore a","prompt":"..."}"#,
    );
    a.subagent_children_mut()
        .unwrap()
        .push(TranscriptMessage::new(Role::Assistant, "child A1"));
    let mut b = TranscriptMessage::tool_step(
        "task_b",
        "spawn_agent",
        r#"{"description":"explore b","prompt":"..."}"#,
    );
    b.subagent_children_mut()
        .unwrap()
        .push(TranscriptMessage::new(Role::Assistant, "child B1"));
    vec![
        TranscriptMessage::new(Role::User, "hi"),
        a,
        TranscriptMessage::new(Role::Assistant, "ok"),
        b,
    ]
}

/// Build a minimal `App` scoped to a tempdir project so we can exercise
/// the completion pipeline end-to-end without touching the user's real
/// filesystem. Mirrors how a real session captures cwd at startup.
/// Test constructor for cross-module relay tests (the event loop's
/// input-selection tests): a default `App` in a temp dir, with no files.
/// The returned temp dir must be kept alive by the caller for the app's
/// lifetime.
#[cfg(test)]
pub(crate) fn new_app_for_relay_tests() -> App {
    let (app, _tmp) = app_in_tempdir(&[], &[]);
    // Leak the temp dir intentionally: these tests only touch in-memory
    // state, and returning `(App, TempDir)` would force every caller to
    // juggle the guard. The OS reclaims the empty dir at process exit.
    std::mem::forget(_tmp);
    app
}

fn app_in_tempdir(files: &[&str], dirs: &[&str]) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    for d in dirs {
        std::fs::create_dir_all(tmp.path().join(d)).expect("mkdir");
    }
    for f in files {
        // Create parent dirs as needed so `src/foo.rs` lays down cleanly.
        let path = tmp.path().join(f);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir for file");
        }
        std::fs::write(path, "x").expect("write file");
    }
    let cwd = tmp.path().to_path_buf();
    let app = App {
        surface_store: crate::shell::surfaces::SurfaceStore::new(),
        surfaces: crate::shell::surfaces::SurfaceRouter::new(),
        queue_exit_session: None,
        recent_commands: Vec::new(),
        input: String::new(),
        messages: Vec::new(),
        layout_height_cache: Default::default(),
        session_chrome: std::collections::HashMap::new(),
        session_tree: nuo_wire::SessionTree::default(),
        scroll: 0,
        follow_bottom: true,
        content_lines: 0,
        view_height: 0,
        max_scroll: 0,
        sticky_step: None,
        token_ledger: None,
        token_report: None,
        context_tokens: None,
        usage_stats: None,
        provider_quotas: None,
        connection_usages: std::collections::HashMap::new(),
        modal_body_height: 0,
        caret_motion: Default::default(),
        logs: Default::default(),
        sticky_summary_line: None,
        pin_summary_line: None,
        scroll_settle_pending: false,
        focus_stack: Vec::new(),
        tx: new_test_channel(),
        should_quit: Arc::new(AtomicBool::new(false)),
        suggestion_index: None,
        completion_dismissed: false,
        command_catalog: test_command_catalog(),
        backend_completions: Vec::new(),
        completion_response_input: None,
        completion_response_cursor: 0,
        completion_requested: None,
        completion_request_id: 0,
        cursor_position: 0,
        input_scroll: 0,
        input_scroll_follow_cursor: true,
        input_drag_scroll: None,
        modal_index: 0,
        last_key_press: std::time::Instant::now(),
        session_detail: None,
        connection_detail: None,
        config_scroll: 0,
        config_focus: crate::shell::overlays::ConfigFocus::Categories,
        config_category: 0,
        config_detail_index: 0,
        config_hover_index: None,
        config_detail_scroll: 0,
        websearch_config: None,
        config_dropdown: None,
        config_selected_rect: None,
        current_provider: "mock".to_string(),
        current_model: "mock".to_string(),
        cwd: cwd.clone(),
        current_session_id: "test-session".to_string(),
        current_workspace: String::new(),
        current_role: None,
        session_context: None,
        loop_status: LoopStatus::Idle,
        harness_retry_pending: false,
        phase: None,
        provider_retry: None,
        persistence_health: None,
        link_down: false,
        live_session_id: "session-test".to_string(),
        pending_permissions: std::collections::VecDeque::new(),
        pending_questions: std::collections::VecDeque::new(),
        pending_inputs: std::collections::VecDeque::new(),
        subagent_permission_parent: HashMap::new(),
        subagent_question_parent: HashMap::new(),
        workspace_security: nuo_wire::WorkspaceSecuritySnapshot::default(),
        context_tokens_by_session: HashMap::new(),
        open_sessions_signal: false,
        open_tree_signal: false,
        transcript_changed_pending: false,
        reasoning_default_expanded: false,
        backend_completion_signal: None,
        tui_config: crate::shell::config::TuiConfig::default(),
        unattended: false,
        confined: true,
        round_count: 0,
        current_turn: 0,
        round_started_at: None,
        pending_permission: None,
        active_sheet: None,
        pending_permission_depth: 0,
        pending_question_depth: 0,
        pending_input: None,
        pre_attach: None,
        question: None,
        question_scroll: 0,
        question_modal_follow: true,
        sessions_overview: Vec::new(),
        switching_session: None,
        host_sessions: Vec::new(),
        host_scroll: 0,
        host_modal_follow: true,
        host_focus: crate::shell::overlays::DashboardFocus::Detail,
        host_detail_scroll: 0,
        host_preview: None,
        host_preview_scroll: 0,
        host_prompting: false,
        host_prompt_new: false,
        host_console_log: Vec::new(),
        host_kill_confirm: None,
        host_kill_confirm_id: None,
        switch_to_target: None,
        switch_to_new: false,
        switch_to_new_prompt: None,
        switch_to_prompt: None,
        startup_overlay: crate::shell::StartupOverlay::None,
        permission_confirm_always: false,
        permission_show_details: false,
        permission_scroll: 0,
        permission_max_scroll: 0,
        input_history: Vec::new(),
        history_index: None,
        draft_capsule: nuo_view::composer_mode::DraftCapsule::default(),
        history_attachments: std::collections::HashMap::new(),
        history_attachments_order: std::collections::VecDeque::new(),
        session_history_backfill: Vec::new(),
        session_history_backfill_cursor: 0,
        input_history_dedup: true,
        input_history_record_commands: false,
        // Tests must not touch the developer's real database: with the
        // guard off, `record_input_history` writes to nuo.db
        // — a leak that polluted the file with synthetic `prompt N` rows.
        input_history_persist: false,
        pending_images: Vec::new(),
        pending_text_pastes: Vec::new(),
        pending_dispatch: std::collections::VecDeque::new(),
        composer_send_mode: crate::shell::app::ComposerSendMode::default(),
        scene_namespace_armed: false,
        queue_blocked_sessions: std::collections::HashSet::new(),
        running_sessions: std::collections::HashSet::new(),
        selection: SelectionState::None,
        drag: SelectionDrag::default(),
        ui: crate::shell::ui::ComponentTree::new(),
        hovered_step: None,
        transcript_focused: false,
        transcript_layout: crate::shell::render::layout::Strategy::default(),
        color_scheme: "zen".to_string(),
        custom_color_scheme: nuo_wire::ColorSchemeConfig::default(),

        click_outside_dismiss: false,
        expand_auto_scroll: false,
        key_overrides: crate::shell::keymap::GlobalOverrides::default(),
        surface_overrides: crate::shell::keymap::SurfaceOverrides::default(),
        focused_target: None,
        copy_toast_until: None,
        receipts: crate::shell::app::receipts::ReceiptLog::default(),
        copy_toast_message: String::new(),
        copy_toast_failed: false,
        notice_toast_until: None,
        notice_toast_message: String::new(),
        notice_toast_severity: NoticeSeverity::Info,
        dev_toast_pinned: false,
        ctrl_c_armed_until: None,
        esc_armed_until: None,
        spinner_epoch: std::time::Instant::now(),
        carousel_epoch: std::time::Instant::now(),
        last_submit_ms: None,
        injection_stashed_input: String::new(),
        editor_target: None,
        editor_field: 0,
        editor_key: String::new(),
        editor_model: String::new(),
        editor_model_settings_only: false,
        editor_target_is_builtin: false,
        editor_effort: "high".to_string(),
        editor_effort_levels: Vec::new(),
        editor_thinking_available: false,
        editor_thinking: true,
        editor_vision_override: None,
        editor_tool_override: None,
        custom_field: 0,
        custom_fields: Vec::new(),
        custom_protocol_wire: String::new(),
        custom_client_identity: nuo_wire::ClientIdentity::Native,
        custom_budget: Default::default(),
        custom_models: Vec::new(),
        custom_url_hint: String::new(),
        custom_user_agent: None,
        custom_auth: Default::default(),
        custom_provider_id: None,
        awaiting_oauth_add: false,
        oauth_pending_message: String::new(),
        oauth_pending_url: String::new(),
        oauth_pending_user_code: String::new(),
        oauth_pending_error: None,
        oauth_selected_item: 0,
        oauth_scroll: 0,
        custom_scroll: 0,
        custom_edit_id: None,
        custom_name: String::new(),
        custom_base_url: String::new(),
        custom_token: String::new(),
        custom_model: String::new(),
        preset_choice: 0,
        preset_scroll: 0,
        pending_provider_delete: None,
        provider_delete_focus: crate::shell::ProviderDeleteChoice::default(),
        key_status: HashMap::new(),
        provider_picker: ProviderPickerSnapshot::default(),
        theme: Theme::default(),
        profile: nuotc::TerminalProfile::direct_color(),
        logo: None,
        background_tasks: Vec::new(),
    };
    (app, tmp)
}

fn new_test_channel() -> mpsc::UnboundedSender<AgentRequest> {
    let (tx, _rx) = mpsc::unbounded_channel();
    tx
}

fn openai_template() -> &'static nuo_view::providers::ConnectionTemplate {
    crate::PROVIDER_PRESETS
        .iter()
        .find(|t| t.id == "openai")
        .expect("openai template")
}

fn anthropic_template() -> &'static nuo_view::providers::ConnectionTemplate {
    crate::PROVIDER_PRESETS
        .iter()
        .find(|t| t.id == "anthropic")
        .expect("anthropic template")
}

fn antigravity_template() -> &'static nuo_view::providers::ConnectionTemplate {
    crate::PROVIDER_PRESETS
        .iter()
        .find(|t| t.id == "google-antigravity")
        .expect("antigravity template")
}

fn queued_dispatch(id: &str, session_id: &str, text: &str) -> QueuedDispatch {
    QueuedDispatch {
        id: id.to_string(),
        session_id: session_id.to_string(),
        state: QueuedDispatchState::Waiting,
        text: text.to_string(),
        queued_at_ms: 0,
        images: Vec::new(),
        text_pastes: Vec::new(),
    }
}

fn prompt_tail(messages: &[TranscriptMessage]) -> Vec<(String, bool, u64)> {
    messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| {
            (
                m.raw.clone(),
                m.origin == UserMessageOrigin::Chat,
                m.sent_at_ms.unwrap_or(0),
            )
        })
        .collect()
}

pub(crate) fn overview_row(id: &str) -> nuo_wire::SessionOverview {
    nuo_wire::SessionOverview {
        parent_id: None,
        fork_kind: nuo_wire::SessionForkKind::Trunk,
        fork_point: None,
        id: id.to_string(),
        title: None,
        overview: format!("overview-{id}"),
        created_at: 0,
        updated_at: 0,
        message_count: 0,
        active: false,
        digest: None,
    }
}

fn app_with_input_selection(input: &str) -> App {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = input.to_string();
    // The drag-selection shape the composer actually records: middle-click /
    // whole-block select of the live input.
    app.selection = SelectionState::Block {
        message_idx: crate::shell::render::INPUT_MSG_IDX,
        block_idx: 0,
    };
    // The hidden caret parked where the mouse released (the drag's head).
    app.set_cursor(input.chars().count());
    app
}

fn relay_probe(
    app: &mut App,
    code: crossterm::event::KeyCode,
) -> Option<crate::shell::input::InputAction> {
    crate::shell::event_loop::component_input::route(
        app,
        &crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            code,
            crossterm::event::KeyModifiers::NONE,
        )),
        &[crate::shell::ui::UiKey::Composer],
    )
}

fn console_host_rows(app: &mut App) {
    let row = |id: &str, created: u64| nuo_wire::MonitoredSession {
        id: id.to_string(),
        title: None,
        overview: String::new(),
        created_at: created,
        updated_at: created,
        message_count: 1,
        hosting: nuo_wire::SessionHosting::Hosted,
        status: nuo_wire::SessionStatus::Idle,
        round: 1,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: "/tmp/proj".to_string(),
        parent_id: None,
        fork_kind: nuo_wire::SessionForkKind::Trunk,
        fork_point: None,
        digest: None,
    };
    app.host_sessions = vec![row("aaa", 100), row("bbb", 200)];
    app.switch_scene(crate::shell::surfaces::SceneKind::Dashboard);
    // Selection on the first creation-order entry = `#1`.
    app.modal_index = 0;
}

async fn console_dispatch(app: &mut App, line: &str, create_when_bare: bool) {
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    crate::shell::event_loop::host_test_shims::dispatch(app, &runtime, line, create_when_bare)
        .await;
}

mod completion;
mod elevation_pipeline;
mod gesture_input;
mod history_recall;
mod input;
mod modal;
mod pre_attach_gate;
mod provider_routes;
mod runtime_views;
mod transcript;
mod transport_setback;
mod view_state;
