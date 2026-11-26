//! Pure per-frame projections for the TUI event loop's draw pass.
//!
//! Functions in this module derive transient display models (lifecycle status,
//! empty-state hero guidance, TabBar live activity/hover props, and MCP
//! bootstrap status) from `App` state without mutating state or touching a
//! terminal frame.

use crate::shell::App;
use crate::shell::render::{self, TabStatus};

/// Select the lifecycle status-bar override for the current frame, if any.
///
/// Priority:
/// 1. Dead server link (D6) — always disclosed first so a lost connection
///    never masquerades as an in-flight prepare step.
/// 2. Explicit user-initiated thread switch/resume — composed with the live
///    [`protocol::ThreadPhase`] so real assembly stages remain visible.
/// 3. Non-`Ready` foreground thread phase (ADR-0133) — projects the exact
///    server-attested stage (`binding`, `opening thread store`, `preparing · …`,
///    or `thread setup failed`).
pub(crate) fn select_lifecycle_status(
    link_down: bool,
    switching_session: Option<&str>,
    session_phase: &protocol::ThreadPhase,
) -> Option<String> {
    if link_down {
        return Some("server link lost".to_string());
    }
    if let Some(target) = switching_session {
        return Some(render::resuming_thread_status(target, session_phase));
    }
    render::thread_phase_status(session_phase).map(str::to_string)
}

/// Select the empty-state hero guidance for the foreground thread.
///
/// Priority:
/// 1. Explicit user-initiated thread switch/resume (`ResumingThread`), carrying
///    the live [`protocol::ThreadPhase`] so real stages are never masked.
/// 2. Any non-`Ready` foreground thread phase (`ThreadPhase`).
/// 3. Ready thread: rotating capability tour or `NeedsProvider` setup blocker.
pub(crate) fn select_empty_state_guidance(
    switching_session: Option<&str>,
    session_phase: &protocol::ThreadPhase,
    has_keyed_provider: bool,
) -> render::EmptyStateGuidance {
    if let Some(target) = switching_session {
        render::EmptyStateGuidance::ResumingThread {
            short_id: target.to_string(),
            phase: session_phase.clone(),
        }
    } else if *session_phase != protocol::ThreadPhase::Ready {
        render::EmptyStateGuidance::ThreadPhase(session_phase.clone())
    } else if has_keyed_provider {
        render::EmptyStateGuidance::Tour
    } else {
        render::EmptyStateGuidance::NeedsProvider
    }
}

/// Project one open tab's transient live activity for the Tier 1 TabBar.
///
/// Pure per-frame projection — never persisted and never stored on `ClientTab`:
/// - Non-thread tabs (`Dashboard`, `Settings`) always project [`TabStatus::Idle`]
///   so they incur zero column tax and zero visual noise.
/// - Thread tabs prioritize [`TabStatus::Attention`] (waiting on human
///   approval/question/stdin) over [`TabStatus::Running`].
/// - For a subscribed thread with live `session_chrome`, local direct-stream
///   status takes precedence over `host_sessions` for `Running` so a slightly
///   lagging monitor snapshot cannot resurrect a spinner after `RoundEnded`,
///   while cold/unsubscribed thread tabs still fall back to `host_sessions`.
pub(crate) fn project_tab_status(
    app: &App,
    tab: &crate::shell::surfaces::ClientTab,
    viewed_session_id: &str,
) -> TabStatus {
    let Some(thread_id) = tab.thread_id() else {
        return TabStatus::Idle;
    };

    let host_status = app
        .session
        .host_sessions
        .iter()
        .find(|row| row.id == thread_id)
        .map(|row| row.status);
    let chrome = app.session.session_chrome.get(thread_id);

    // 1. Attention (human gate) wins over Running.
    let viewed_awaiting_human = thread_id == viewed_session_id
        && (app.human.pending_permission.is_some()
            || app.human.question.is_some()
            || app.human.pending_input.is_some());
    if viewed_awaiting_human
        || matches!(
            host_status,
            Some(protocol::ThreadActivity::NeedsApproval | protocol::ThreadActivity::NeedsInput)
        )
        || chrome.is_some_and(|c| c.phase == Some(view::phase::Phase::AwaitingUser))
    {
        return TabStatus::Attention;
    }

    // 2. Active round execution.
    if app.session.running_sessions.contains(thread_id)
        || chrome.is_some_and(|c| c.responding)
        || app
            .threads
            .get(thread_id)
            .is_some_and(|inst| !inst.loop_status.is_idle())
    {
        return TabStatus::Running;
    }

    // Fall back to the server monitor stream for cold/unsubscribed thread tabs.
    if chrome.is_none() && host_status == Some(protocol::ThreadActivity::Running) {
        return TabStatus::Running;
    }

    TabStatus::Idle
}

/// Project per-tab live statuses aligned 1:1 with `app.surfaces.surfaces.tabs()`.
pub(crate) fn project_tab_statuses(app: &App, viewed_session_id: &str) -> Vec<TabStatus> {
    app.surfaces
        .surfaces
        .tabs()
        .iter()
        .map(|tab| project_tab_status(app, tab, viewed_session_id))
        .collect()
}

/// The TabBar's client-global projection for this frame (ADR-0223, ADR-0245).
///
/// Every page renderer that draws Tier 1 fills its own tab list and fault count
/// but shares this one hover + live help-key + spinner-phase fact, so the strip
/// behaves identically on the transcript, dashboard, settings, command log, and
/// diff pages. Elapsed time is measured against the *anchor* the pointer arrived
/// on — the app owns that instant, not the renderer, so a repaint never restarts
/// the reveal clock.
pub(super) fn tab_hover_projection(app: &App) -> render::TabBarProps<'static> {
    render::TabBarProps {
        spinner_phase: (app.viewport.spinner_epoch.elapsed().as_millis() / 100) as usize,
        help_key: app
            .surfaces
            .key_overrides
            .effective_binding(crate::shell::keymap::CommandId::OpenHelp),
        hover: app.surfaces.tab_hover,
        hover_elapsed_ms: app
            .surfaces
            .tab_hover_since
            .map(|since| since.elapsed().as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or(0),
        ..render::TabBarProps::default()
    }
}

/// `[INV-MCP-04]`: Extract in-flight MCP connection status during system bootstrapping.
pub(super) fn mcp_connecting_status(app: &App) -> Option<String> {
    let snapshot = app.threads.session_context.as_ref()?;
    if snapshot.mcp.is_empty() {
        return None;
    }
    let connecting: Vec<&str> = snapshot
        .mcp
        .iter()
        .filter(|s| !s.connected && !s.disabled && s.failure.is_none())
        .map(|s| s.name.as_str())
        .collect();
    if connecting.is_empty() {
        return None;
    }
    let total = snapshot.mcp.iter().filter(|s| !s.disabled).count();
    let connected = snapshot.mcp.iter().filter(|s| s.connected).count();
    let names = connecting.join(", ");
    Some(format!("connecting MCP ({connected}/{total}: {names})…"))
}
