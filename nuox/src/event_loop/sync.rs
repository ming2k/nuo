//! App-owned reconciliation passes (ADR-0197 M1).
//!
//! The old per-frame mirror (`sync_runtime_state_to_app` hydrating `App`
//! from ~45 shared cells) is gone: the response translator produces
//! `AppMutation`s and `apply` (the sole `App` writer) lands them directly.
//! What remains here is the reconciliation that is genuinely a *loop-side
//! state machine* — mounting/dismissing sheets in reaction to request
//! queues, projecting viewed-session data, and the scroll-relevant
//! transcript-change bookkeeping — all reading and writing `App` alone.

use nuo_contracts::Role;

use crate::app::App;
use crate::event_loop::runtime::{UiRuntime, now_epoch_ms};
use crate::model::document::{TranscriptMessage, UserMessageOrigin};

/// The per-frame reconciliation over App-owned request queues and sheets.
pub(crate) fn sync_request_surfaces(app: &mut App, runtime: &UiRuntime) {
    // Publish the add-flow fact to the translator (loop → translator): the
    // OAuth add-flow surfaces the URL in the modal; the translator suppresses
    // the duplicate transcript notice while it is in flight.
    runtime
        .awaiting_oauth_add
        .store(app.awaiting_oauth_add, std::sync::atomic::Ordering::SeqCst);

    let request_sheet_open = app.active_sheet().is_some();

    // Project the mounted front of the permission queue.
    app.pending_permission = app.pending_permissions.front().cloned();

    if app.pending_permission.is_some() && !request_sheet_open {
        app.push_sheet_surface(crate::sheet::Permission);
        app.modal_index = 0;
        app.permission_scroll = 0;
        app.permission_show_details = false;
        app.park_transcript_focus_for_sheet();
    } else if app.pending_permission.is_none()
        && app.active_sheet() == Some(crate::sheet::Permission)
    {
        app.dismiss_sheet();
        app.modal_index = 0;
        app.permission_confirm_always = false;
        app.permission_scroll = 0;
        app.permission_max_scroll = 0;
        app.permission_show_details = false;
    }
    app.pending_permission_depth = app.pending_permissions.len();

    // Question modal sync
    {
        let front = app.pending_questions.front().cloned();
        // ADR-0175 §7: the badge reports items queued *behind* the one
        // already mounted on the sheet — not counting the mounted one
        // itself. The permission sheet's predicate is `> 1` for the
        // same reason; the question sheet's renderer uses `> 0`, so the
        // depth must subtract the front item here.
        app.pending_question_depth = app.pending_questions.len().saturating_sub(1);
        let model_matches_front = match (&app.question, &front) {
            (Some(m), Some(req)) => m.request().id == req.id,
            (None, None) => true,
            _ => false,
        };
        if !model_matches_front {
            if let Some(req) = front {
                app.question = Some(crate::question_model::QuestionModel::open(req));
                app.question_scroll = 0;
                app.question_modal_follow = true;
                app.modal_index = 0;
                app.park_transcript_focus_for_sheet();
            } else {
                app.question = None;
                if app.active_sheet() == Some(crate::sheet::Question) {
                    app.dismiss_sheet();
                    app.modal_index = 0;
                }
            }
        }
        if app.question.is_some() && !request_sheet_open {
            app.push_sheet_surface(crate::sheet::Question);
            app.modal_index = 0;
            app.park_transcript_focus_for_sheet();
        }
    }

    // Input-injection modal sync
    {
        let front = app.pending_inputs.front().cloned();
        let matches_front = match (&app.pending_input, &front) {
            (Some(cur), Some(req)) => cur.id == req.id,
            (None, None) => true,
            _ => false,
        };
        if !matches_front {
            if let Some(req) = front {
                app.pending_input = Some(req);
                app.modal_index = 0;
                app.park_transcript_focus_for_sheet();
            } else {
                app.pending_input = None;
                if app.active_sheet() == Some(crate::sheet::InputInjection) {
                    app.restore_input_draft();
                    app.dismiss_sheet();
                    app.modal_index = 0;
                }
            }
        }
        if app.pending_input.is_some() && !request_sheet_open {
            app.park_input_draft();
            app.push_sheet_surface(crate::sheet::InputInjection);
            app.modal_index = 0;
            app.park_transcript_focus_for_sheet();
        }
    }
}

pub(crate) fn tick_toast_timers(app: &mut App) -> bool {
    let mut changed = false;
    if let Some(until) = app.copy_toast_until
        && std::time::Instant::now() >= until
    {
        app.copy_toast_until = None;
        changed = true;
    }

    if let Some(until) = app.notice_toast_until
        && std::time::Instant::now() >= until
    {
        app.notice_toast_until = None;
        changed = true;
    }

    if !app.pending_images.is_empty() && !app.ctrl_c_armed() {
        let n = app.pending_images.len();
        show_local_toast(
            app,
            format!(
                "{n} image{} attached — enter to send",
                if n == 1 { "" } else { "s" }
            ),
            false,
            std::time::Duration::from_millis(600),
        );
        changed = true;
    }

    let before_esc = app.esc_armed();
    app.tick_esc_arm();
    if before_esc != app.esc_armed() {
        changed = true;
    }

    changed
}

pub(crate) fn show_local_toast(
    app: &mut App,
    message: impl Into<String>,
    failed: bool,
    duration: std::time::Duration,
) {
    app.copy_toast_message = message.into();
    app.copy_toast_failed = failed;
    app.copy_toast_until = Some(std::time::Instant::now() + duration);
}

fn user_prompt_tail(messages: &[TranscriptMessage]) -> Vec<(String, bool, u64)> {
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

/// The per-frame session/view reconciliation (ADR-0197 M1): project the
/// viewed session's state, publish the routing fact to the translator, and
/// run the history backfill. Returns whether the *displayed* transcript
/// changed shape (bottom-follow staging) and the viewed session id.
pub(crate) async fn sync_transcripts_and_session(
    app: &mut App,
    runtime: &UiRuntime,
) -> (bool, String) {
    let side_view_transitioned = std::mem::take(&mut app.view_transitioned);
    let transcript_changed = std::mem::take(&mut app.transcript_changed_pending);
    let side_transcript_changed = std::mem::take(&mut app.side_transcript_changed_pending);

    let displayed_transcript_changed =
        crate::event_loop::transcript::displayed_transcript_did_change(
            app.in_side_view,
            transcript_changed,
            side_transcript_changed,
            side_view_transitioned,
        );

    let primary_session_id = app.live_session_id.clone();
    let viewed_session_id = if app.in_side_view {
        app.side_session_id
            .as_deref()
            .unwrap_or(primary_session_id.as_str())
    } else {
        primary_session_id.as_str()
    }
    .to_string();

    if app.current_session_id != viewed_session_id {
        app.current_session_id = viewed_session_id.clone();
        app.on_viewed_session_changed();
        app.switching_session = None;
    }

    let backfill_from = app.session_history_backfill_cursor;
    let viewed_len = if app.in_side_view {
        app.side_messages.len()
    } else {
        app.messages.len()
    };
    if backfill_from < viewed_len {
        let tail: Vec<(String, bool, u64)> = if app.in_side_view {
            user_prompt_tail(&app.side_messages[backfill_from..])
        } else {
            user_prompt_tail(&app.messages[backfill_from..])
        };
        app.session_history_backfill_cursor = viewed_len;
        app.backfill_session_history(&tail, now_epoch_ms());
    }

    // Publish the routing fact to the translator (loop → translator).
    *runtime.viewed_session_id.lock().await = Some(viewed_session_id.clone());

    if app.current_workspace.is_empty() && app.current_role.is_none() {
        app.current_workspace = crate::chrome::tilde_home(&app.cwd);
    }

    app.context_tokens = app
        .context_tokens_by_session
        .get(&viewed_session_id)
        .copied();

    (displayed_transcript_changed, viewed_session_id)
}

/// Consume the one-shot backend navigation signals the applier latched
/// (ADR-0197 M1: formerly `AtomicBool` swaps on shared cells).
pub(crate) fn consume_navigation_signals(app: &mut App, runtime: &UiRuntime) -> (bool, bool, bool) {
    let can_apply_backend_navigation = app.can_accept_navigation_signal();
    let open_sessions =
        can_apply_backend_navigation && std::mem::take(&mut app.open_sessions_signal);
    let open_tree = can_apply_backend_navigation && std::mem::take(&mut app.open_tree_signal);
    let open_host = can_apply_backend_navigation && std::mem::take(&mut app.open_host_signal);
    if !can_apply_backend_navigation {
        // Drop the signals anyway: a navigation the surface cannot accept
        // right now must not silently queue behind the next frame forever.
        app.open_sessions_signal = false;
        app.open_tree_signal = false;
        app.open_host_signal = false;
    }
    let _ = runtime;
    (open_sessions, open_tree, open_host)
}

/// Consume the backend completion round-trip the applier latched.
pub(crate) fn consume_completion_signal(app: &mut App) -> bool {
    if let Some(signal) = app.backend_completion_signal.take() {
        app.apply_backend_completions(signal.request_id, signal.input, signal.cursor, signal.items);
        true
    } else {
        false
    }
}
