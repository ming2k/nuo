//! Pure per-frame projections for the TUI event loop's draw pass.
//!
//! Functions in this module derive transient display models (lifecycle status,
//! empty-state hero guidance, TabBar live activity/hover props, and MCP
//! bootstrap status) from `App` state without mutating state or touching a
//! terminal frame.

use crate::shell::App;
use crate::shell::app::state::ClientLinkPhase;
use crate::shell::app::workspaces::UnseenOutcome;
use crate::shell::render::{self, TabBarItem, TabStatus};
use crate::shell::surfaces::{ClientTab, SceneKind};

/// Display hysteresis for transient non-error waiting states
/// (ADR-0260 `[INV-SRVPHASE-05]`): warm local connects and fast local
/// thread preparations complete under this threshold and transition seamlessly
/// without flashing intermediate labels.
pub(crate) const TRANSIENT_WAIT_HYSTERESIS: std::time::Duration =
    std::time::Duration::from_millis(120);

fn format_reconnecting_label(
    endpoint: &str,
    attempt: u32,
    max_attempts: u32,
    delay_ms: Option<u64>,
) -> String {
    match delay_ms {
        Some(ms) if ms > 0 => format!(
            "Reconnecting to {endpoint} ({attempt}/{max_attempts}, retrying in {:.1}s · Esc to cancel)…",
            (ms as f64) / 1000.0
        ),
        _ => format!("Reconnecting to {endpoint} ({attempt}/{max_attempts} · Esc to cancel)…"),
    }
}

/// Select the lifecycle status-bar override for the current frame, if any.
///
/// Priority (ADR-0260 `[INV-SRVPHASE-05]`):
/// 1. Client transport phase (`Connecting` after 120ms hysteresis,
///    `Reconnecting` / `Failed` immediately) or dead link latch.
/// 2. Non-`Ready` server lifecycle phase (`Initializing` after 120ms
///    hysteresis; `Reloading`, `Draining`, `Failed` immediately).
/// 3. Explicit user-initiated thread switch/resume — composed with the live
///    [`nuo_protocol::ThreadPhase`].
/// 4. Non-`Ready` foreground thread phase (`Preparing` after 120ms hysteresis;
///    `Failed` immediately).
#[allow(clippy::too_many_arguments)]
pub(crate) fn select_lifecycle_status(
    link_down: bool,
    link_phase: &ClientLinkPhase,
    link_phase_elapsed: std::time::Duration,
    server_phase: &nuo_protocol::ServerPhase,
    server_phase_elapsed: std::time::Duration,
    switching_session: Option<&str>,
    session_phase: &nuo_protocol::ThreadPhase,
    thread_phase_elapsed: std::time::Duration,
) -> Option<String> {
    match link_phase {
        ClientLinkPhase::Connecting { label } => {
            if link_phase_elapsed >= TRANSIENT_WAIT_HYSTERESIS {
                return Some(label.to_lowercase());
            }
            return None;
        }
        ClientLinkPhase::Reconnecting {
            attempt,
            max_attempts,
            ..
        } => {
            return Some(format!("reconnecting ({attempt}/{max_attempts})…"));
        }
        ClientLinkPhase::Failed { .. } => {
            return Some("server connection failed".to_string());
        }
        ClientLinkPhase::Connected { .. } => {}
    }
    if link_down {
        return Some("server link lost".to_string());
    }
    if let Some(server_status) = render::server_phase_status(server_phase) {
        let is_transient = matches!(server_phase, nuo_protocol::ServerPhase::Initializing { .. });
        if !is_transient || server_phase_elapsed >= TRANSIENT_WAIT_HYSTERESIS {
            return Some(server_status.to_string());
        }
        return None;
    }
    if let Some(target) = switching_session {
        return Some(render::resuming_thread_status(target, session_phase));
    }
    if let Some(thread_status) = render::thread_phase_status(session_phase) {
        let is_transient = matches!(session_phase, nuo_protocol::ThreadPhase::Preparing { .. });
        if !is_transient || thread_phase_elapsed >= TRANSIENT_WAIT_HYSTERESIS {
            return Some(thread_status.to_string());
        }
    }
    None
}

/// Select the empty-state hero guidance for the foreground thread.
///
/// Priority (ADR-0260 `[INV-SRVPHASE-05]`):
/// 1. Client transport non-connected phase (`Connecting` uses 120ms
///    `QuietWait` hysteresis; `Reconnecting` and `Failed` surface immediately).
/// 2. Blocking server lifecycle phase (`Initializing` uses 120ms `QuietWait`
///    hysteresis; `Draining` and `Failed` surface immediately; `Reloading` is
///    non-blocking and appears in the status bar only).
/// 3. Explicit user-initiated thread switch/resume (`ResumingThread`).
/// 4. Non-`Ready` foreground thread phase (`Preparing` uses 120ms `QuietWait`
///    hysteresis; `Failed` surfaces immediately).
/// 5. Ready thread: rotating capability tour or `NeedsProvider` setup blocker.
#[allow(clippy::too_many_arguments)]
pub(crate) fn select_empty_state_guidance(
    link_phase: &ClientLinkPhase,
    link_phase_elapsed: std::time::Duration,
    server_phase: &nuo_protocol::ServerPhase,
    server_phase_elapsed: std::time::Duration,
    switching_session: Option<&str>,
    session_phase: &nuo_protocol::ThreadPhase,
    thread_phase_elapsed: std::time::Duration,
    has_keyed_provider: bool,
) -> render::EmptyStateGuidance {
    match link_phase {
        ClientLinkPhase::Connecting { label } => {
            return if link_phase_elapsed >= TRANSIENT_WAIT_HYSTERESIS {
                render::EmptyStateGuidance::Connecting {
                    label: label.clone(),
                }
            } else {
                render::EmptyStateGuidance::QuietWait
            };
        }
        ClientLinkPhase::Reconnecting {
            endpoint,
            attempt,
            max_attempts,
            delay_ms,
        } => {
            return render::EmptyStateGuidance::Connecting {
                label: format_reconnecting_label(endpoint, *attempt, *max_attempts, *delay_ms),
            };
        }
        ClientLinkPhase::Failed { message } => {
            return render::EmptyStateGuidance::ServerPhase(nuo_protocol::ServerPhase::Failed {
                code: None,
                message: message.clone(),
            });
        }
        ClientLinkPhase::Connected { .. } => {}
    }

    if !server_phase.admits_work() {
        let is_transient = matches!(server_phase, nuo_protocol::ServerPhase::Initializing { .. });
        return if !is_transient || server_phase_elapsed >= TRANSIENT_WAIT_HYSTERESIS {
            render::EmptyStateGuidance::ServerPhase(server_phase.clone())
        } else {
            render::EmptyStateGuidance::QuietWait
        };
    }

    if let Some(target) = switching_session {
        render::EmptyStateGuidance::ResumingThread {
            short_id: target.to_string(),
            phase: session_phase.clone(),
        }
    } else if *session_phase != nuo_protocol::ThreadPhase::Ready {
        let is_transient = matches!(session_phase, nuo_protocol::ThreadPhase::Preparing { .. });
        if !is_transient || thread_phase_elapsed >= TRANSIENT_WAIT_HYSTERESIS {
            render::EmptyStateGuidance::ThreadPhase(session_phase.clone())
        } else {
            render::EmptyStateGuidance::QuietWait
        }
    } else if has_keyed_provider {
        render::EmptyStateGuidance::Tour
    } else {
        render::EmptyStateGuidance::NeedsProvider
    }
}

/// Project one thread scene container's transient live activity or unseen
/// terminal outcome for the Tier 1 TabBar.
///
/// Priority:
/// 1. [`TabStatus::Attention`] — blocked on human approval, question, or stdin.
/// 2. [`TabStatus::Running`] — actively executing a round (local direct-stream
///    status takes precedence over lagging `host_sessions` for subscribed
///    resident threads; cold tabs fall back to `host_sessions`).
/// 3. [`TabStatus::Failed`] — unseen terminal failure recorded in the
///    background (or cold `host_sessions` `Failed` / failed `ThreadPhase`).
/// 4. [`TabStatus::Succeeded`] — unseen clean round completion recorded in the
///    background.
/// 5. [`TabStatus::Idle`] — resting, or terminal outcome already viewed.
fn project_thread_tab_status(app: &App, thread_id: &str, viewed_session_id: &str) -> TabStatus {
    let host_status = app
        .session
        .host_sessions
        .iter()
        .find(|row| row.id == thread_id)
        .map(|row| row.status);
    let inst = app.threads.get(thread_id);

    // 1. Attention (human gate) wins over everything.
    let viewed_awaiting_human = thread_id == viewed_session_id
        && (app.human.pending_permission.is_some()
            || app.human.question.is_some()
            || app.human.pending_input.is_some());
    if viewed_awaiting_human
        || matches!(
            host_status,
            Some(
                nuo_protocol::ThreadActivity::NeedsApproval
                    | nuo_protocol::ThreadActivity::NeedsInput
            )
        )
        || inst.is_some_and(|t| t.phase == Some(nuo_view::phase::Phase::AwaitingUser))
    {
        return TabStatus::Attention;
    }

    // 2. Active round execution.
    if app.session.running_sessions.contains(thread_id) || inst.is_some_and(|t| t.is_responding()) {
        return TabStatus::Running;
    }

    // Cold/unsubscribed thread tabs fall back to the server monitor stream.
    let subscribed = inst.is_some() && app.session.subscribed_threads.contains(thread_id);
    if !subscribed && host_status == Some(nuo_protocol::ThreadActivity::Running) {
        return TabStatus::Running;
    }

    // 3. Unseen terminal outcome recorded while this thread was not viewed
    //    (single source of truth covering both subscribed and cold tabs).
    if let Some(outcome) = app.session.unseen_outcomes.get(thread_id).copied() {
        return match outcome {
            UnseenOutcome::Failed => TabStatus::Failed,
            UnseenOutcome::Succeeded => TabStatus::Succeeded,
        };
    }

    // Cold/unsubscribed thread tabs that were already failed on the server.
    if !subscribed && host_status == Some(nuo_protocol::ThreadActivity::Failed) {
        return TabStatus::Failed;
    }

    TabStatus::Idle
}

/// Resolve a thread tab's authentic title using the full authority ladder:
/// 1. Tab's cached authentic title (`tab.display_title()`),
/// 2. Live monitor snapshot (`app.session.host_sessions`),
/// 3. Active thread detail (`app.session.session_detail`).
///
/// Returns `None` while the thread is still untitled.
fn resolve_authentic_thread_title(app: &App, tab: &ClientTab, thread_id: &str) -> Option<String> {
    if let Some(title) = tab.display_title() {
        return Some(title.to_string());
    }
    if let Some(row) = app
        .session
        .host_sessions
        .iter()
        .find(|row| row.id == thread_id)
        && let Some(ref title) = row.title
    {
        let trimmed = title.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(ref detail) = app.session.session_detail
        && detail.id == thread_id
        && let Some(ref title) = detail.title
    {
        let trimmed = title.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Format an untitled thread tab label without exposing raw or tail-sliced UUIDs
/// (ADR-0268 `[INV-HEAD-12]`):
/// - First untitled thread tab: `"new thread"`
/// - Subsequent concurrent untitled thread tabs: `"new thread 2"`, `"new thread 3"`, …
fn untitled_thread_tab_label(ordinal: usize) -> String {
    if ordinal == 0 {
        "new thread".to_string()
    } else {
        format!("new thread {}", ordinal + 1)
    }
}

/// Resolve a thread tab's display title, falling back to a human-readable
/// untitled label (`new thread` / `new thread N`) instead of a random UUID tail
/// (ADR-0268 `[INV-HEAD-12]`).
fn resolve_thread_tab_title(
    app: &App,
    tab: &ClientTab,
    thread_id: &str,
    untitled_ordinal: usize,
) -> String {
    resolve_authentic_thread_title(app, tab, thread_id)
        .unwrap_or_else(|| untitled_thread_tab_label(untitled_ordinal))
}

/// Thread scene projector: decides the semantic title and live/outcome status
/// for a root `SceneKind::Thread` tab container.
fn project_thread_scene_tab(
    app: &App,
    tab: &ClientTab,
    viewed_session_id: &str,
    untitled_ordinal: usize,
) -> TabBarItem<'static> {
    let Some(thread_id) = tab.thread_id() else {
        return TabBarItem::new(untitled_thread_tab_label(untitled_ordinal));
    };
    let title = resolve_thread_tab_title(app, tab, thread_id, untitled_ordinal);
    let status = project_thread_tab_status(app, thread_id, viewed_session_id);
    TabBarItem::with_status(title, status)
}

/// Subagent drill-in scene projector: reflects the focused subagent task's
/// description when inspecting a live child stream inside the viewed tab,
/// falling back cleanly to the parent thread identity while preserving its
/// live/outcome status.
fn project_subagent_scene_tab(
    app: &App,
    tab: &ClientTab,
    viewed_session_id: &str,
    untitled_ordinal: usize,
) -> TabBarItem<'static> {
    let Some(thread_id) = tab.thread_id() else {
        return TabBarItem::new("subagent");
    };
    let subagent_title = app.threads.get(thread_id).and_then(|inst| {
        let frame = inst.focus_stack.last()?;
        let task = inst.messages.iter().find(|m| {
            m.is_subagent_task() && m.tool_step_call_id() == Some(frame.call_id.as_str())
        })?;
        let desc = task.subagent_description();
        let trimmed = desc.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });
    let title = subagent_title
        .unwrap_or_else(|| resolve_thread_tab_title(app, tab, thread_id, untitled_ordinal));
    let status = project_thread_tab_status(app, thread_id, viewed_session_id);
    TabBarItem::with_status(title, status)
}

/// CommandLog drill-in scene projector: retains the owning thread container's
/// semantic identity and live status (Tier 2 `SceneBar` names `log` + command).
fn project_command_log_scene_tab(
    app: &App,
    tab: &ClientTab,
    viewed_session_id: &str,
    untitled_ordinal: usize,
) -> TabBarItem<'static> {
    project_thread_scene_tab(app, tab, viewed_session_id, untitled_ordinal)
}

/// Diff drill-in scene projector: retains the owning thread container's
/// semantic identity and live status (Tier 2 `SceneBar` names `diff`).
fn project_diff_scene_tab(
    app: &App,
    tab: &ClientTab,
    viewed_session_id: &str,
    untitled_ordinal: usize,
) -> TabBarItem<'static> {
    project_thread_scene_tab(app, tab, viewed_session_id, untitled_ordinal)
}

/// Dashboard scene projector: owns its own TabBar slot representation.
fn project_dashboard_scene_tab() -> TabBarItem<'static> {
    TabBarItem::with_status("dashboard", TabStatus::Idle)
}

/// Settings scene projector: owns its own TabBar slot representation.
fn project_settings_scene_tab() -> TabBarItem<'static> {
    TabBarItem::with_status("settings", TabStatus::Idle)
}

fn project_tab_item_with_ordinal(
    app: &App,
    tab: &ClientTab,
    viewed_session_id: &str,
    untitled_ordinal: usize,
) -> TabBarItem<'static> {
    match tab.current_scene() {
        SceneKind::Thread => {
            project_thread_scene_tab(app, tab, viewed_session_id, untitled_ordinal)
        }
        SceneKind::Subagent => {
            project_subagent_scene_tab(app, tab, viewed_session_id, untitled_ordinal)
        }
        SceneKind::CommandLog => {
            project_command_log_scene_tab(app, tab, viewed_session_id, untitled_ordinal)
        }
        SceneKind::Diff => project_diff_scene_tab(app, tab, viewed_session_id, untitled_ordinal),
        SceneKind::Dashboard => project_dashboard_scene_tab(),
        SceneKind::Settings => project_settings_scene_tab(),
    }
}

/// Project one open tab container's active scene into a domain-agnostic
/// [`TabBarItem`] (DIP boundary between scenes and the TabBar renderer).
///
/// Pure per-frame projection — never persisted and never computed inside
/// `draw_tab_bar`. Each [`SceneKind`] owns its own title and transient status.
#[cfg(test)]
pub(crate) fn project_tab_item(
    app: &App,
    tab: &ClientTab,
    viewed_session_id: &str,
) -> TabBarItem<'static> {
    let mut untitled_ordinal = 0usize;
    for candidate in app.surfaces.surfaces.tabs() {
        if std::ptr::eq(candidate, tab)
            || (candidate.thread_id().is_some() && candidate.thread_id() == tab.thread_id())
        {
            break;
        }
        if let Some(tid) = candidate.thread_id()
            && resolve_authentic_thread_title(app, candidate, tid).is_none()
        {
            untitled_ordinal += 1;
        }
    }
    project_tab_item_with_ordinal(app, tab, viewed_session_id, untitled_ordinal)
}

/// Project all open tab containers into ordered [`TabBarItem`] slots aligned
/// 1:1 with `app.surfaces.surfaces.tabs()`.
pub(crate) fn project_tab_items(app: &App, viewed_session_id: &str) -> Vec<TabBarItem<'static>> {
    let mut untitled_count = 0usize;
    app.surfaces
        .surfaces
        .tabs()
        .iter()
        .map(|tab| {
            let ordinal = if let Some(tid) = tab.thread_id()
                && resolve_authentic_thread_title(app, tab, tid).is_none()
            {
                let ord = untitled_count;
                untitled_count += 1;
                ord
            } else {
                0
            };
            project_tab_item_with_ordinal(app, tab, viewed_session_id, ordinal)
        })
        .collect()
}

/// Project one open tab's transient live status (test convenience wrapper over
/// [`project_tab_item`]).
#[cfg(test)]
pub(super) fn project_tab_status(app: &App, tab: &ClientTab, viewed_session_id: &str) -> TabStatus {
    project_tab_item(app, tab, viewed_session_id).status
}

/// The TabBar's client-global projection for this frame (ADR-0223, ADR-0245).
///
/// Every page renderer that draws Tier 1 fills its own scene-projected items
/// and fault count but shares this one hover + live help-key + animation clock
/// fact, so the strip behaves identically on the transcript, dashboard,
/// settings, command log, and diff pages. Elapsed time is measured against the
/// *anchor* the pointer arrived on — the app owns that instant, not the
/// renderer, so a repaint never restarts the reveal clock.
pub(super) fn tab_hover_projection(app: &App) -> render::TabBarProps<'static> {
    render::TabBarProps {
        animation_ms: app
            .viewport
            .spinner_epoch
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64,
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
