//! App-owned reconciliation passes (M1).
//!
//! The old per-frame mirror (`sync_runtime_state_to_app` hydrating `App`
//! from ~45 shared cells) is gone: the response translator produces
//! `AppMutation`s and `apply` (the sole `App` writer) lands them directly.
//! What remains here is the reconciliation that is genuinely a *loop-side
//! state machine* — mounting/dismissing sheets in reaction to request
//! queues, projecting viewed-session data, and the scroll-relevant
//! transcript-change bookkeeping — all reading and writing `App` alone.
use nuo_tool::Role;

use crate::shell::app::App;
use crate::shell::event_loop::runtime::{UiRuntime, now_epoch_ms};
use nuo_view::model::document::{TranscriptMessage, UserMessageOrigin};

/// The per-frame reconciliation over App-owned request queues and sheets.
pub(crate) fn sync_request_surfaces(app: &mut App, runtime: &UiRuntime) {
    // Publish the add-flow fact to the translator (loop → translator): the
    // OAuth add-flow surfaces the URL in the modal; the translator suppresses
    // the duplicate transcript notice while it is in flight.
    runtime.awaiting_oauth_add.store(
        app.provider_editor.awaiting_oauth_add,
        std::sync::atomic::Ordering::SeqCst,
    );

    let request_sheet_open = app.active_sheet().is_some();

    // Project the mounted front of the permission queue.
    app.human.pending_permission = app.human.pending_permissions.front().cloned();

    if app.human.pending_permission.is_some() && !request_sheet_open {
        app.push_sheet_surface(crate::shell::sheet::Permission);
        app.surfaces.modal_index = 0;
        app.human.permission_scroll = 0;
        app.human.permission_show_details = false;
        app.park_focus_for_sheet();
    } else if app.human.pending_permission.is_none()
        && app.active_sheet() == Some(crate::shell::sheet::Permission)
    {
        app.dismiss_sheet();
        app.surfaces.modal_index = 0;
        app.human.permission_confirm_always = false;
        app.human.permission_scroll = 0;
        app.human.permission_max_scroll = 0;
        app.human.permission_show_details = false;
    }
    app.human.pending_permission_depth = app.human.pending_permissions.len();

    // Question modal sync
    {
        let front = app.human.pending_questions.front().cloned();
        // The badge reports items queued *behind* the one
        // already mounted on the sheet — not counting the mounted one
        // itself. The permission sheet's predicate is `> 1` for the
        // same reason; the question sheet's renderer uses `> 0`, so the
        // depth must subtract the front item here.
        app.human.pending_question_depth = app.human.pending_questions.len().saturating_sub(1);
        let model_matches_front = match (&app.human.question, &front) {
            (Some(m), Some(req)) => m.request().id == req.id,
            (None, None) => true,
            _ => false,
        };
        if !model_matches_front {
            if let Some(req) = front {
                app.human.question = Some(crate::shell::question_model::QuestionModel::open(req));
                app.human.question_scroll = 0;
                app.human.question_modal_follow = true;
                app.surfaces.modal_index = 0;
                app.park_focus_for_sheet();
            } else {
                app.human.question = None;
                if app.active_sheet() == Some(crate::shell::sheet::Question) {
                    app.dismiss_sheet();
                    app.surfaces.modal_index = 0;
                }
            }
        }
        if app.human.question.is_some() && !request_sheet_open {
            app.push_sheet_surface(crate::shell::sheet::Question);
            app.surfaces.modal_index = 0;
            app.park_focus_for_sheet();
        }
    }

    // Input-injection modal sync
    {
        let front = app.human.pending_inputs.front().cloned();
        let matches_front = match (&app.human.pending_input, &front) {
            (Some(cur), Some(req)) => cur.id == req.id,
            (None, None) => true,
            _ => false,
        };
        if !matches_front {
            if let Some(req) = front {
                app.human.pending_input = Some(req);
                app.surfaces.modal_index = 0;
                app.park_focus_for_sheet();
            } else {
                app.human.pending_input = None;
                if app.active_sheet() == Some(crate::shell::sheet::InputInjection) {
                    app.restore_input_draft();
                    app.dismiss_sheet();
                    app.surfaces.modal_index = 0;
                }
            }
        }
        if app.human.pending_input.is_some() && !request_sheet_open {
            app.park_input_draft();
            app.push_sheet_surface(crate::shell::sheet::InputInjection);
            app.surfaces.modal_index = 0;
            app.park_focus_for_sheet();
        }
    }
}

pub(crate) fn tick_toast_timers(app: &mut App) -> bool {
    let mut changed = false;
    if let Some(until) = app.surfaces.copy_toast_until
        && std::time::Instant::now() >= until
    {
        app.surfaces.copy_toast_until = None;
        changed = true;
    }

    if let Some(until) = app.surfaces.notice_toast_until
        && std::time::Instant::now() >= until
    {
        app.surfaces.notice_toast_until = None;
        changed = true;
    }

    if !app.composer.pending_images.is_empty() && !app.ctrl_c_armed() {
        let n = app.composer.pending_images.len();
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

/// Show a local toast.
///
/// A **failed** local toast is diagnostic content: it goes through the notice
/// funnel first (`App::record_local_fault`), which retains it in the log and
/// paints the fault bubble. A successful one is an acknowledgement of an action
/// the user can repeat, so it is only a bubble — but it is still recorded in the
/// log at `Info`, because "what did that say?" is asked a minute later.
pub(crate) fn show_local_toast(
    app: &mut App,
    message: impl Into<String>,
    failed: bool,
    duration: std::time::Duration,
) {
    let message = message.into();
    if failed {
        app.record_local_fault(
            crate::shell::app::notice_catalog::ClientTopic::Client,
            message,
        );
        return;
    }
    // Retained as an acknowledgement (no bubble from the funnel — the caller is
    // painting one), then the caller's own duration wins.
    app.record_client_notice(crate::shell::app::notice_catalog::ClientNotice::LocalAck {
        topic: crate::shell::app::notice_catalog::ClientTopic::Client,
        message: message.clone(),
    });
    app.surfaces.copy_toast_message = message;
    app.surfaces.copy_toast_failed = false;
    app.surfaces.copy_toast_until = Some(std::time::Instant::now() + duration);
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

/// The per-frame session/view reconciliation (M1): project the
/// viewed session's state, publish the routing fact to the translator, and
/// run the history backfill. Returns whether the *displayed* transcript
/// changed shape (bottom-follow staging) and the viewed session id.
pub(crate) async fn sync_transcripts_and_session(
    app: &mut App,
    runtime: &UiRuntime,
) -> (bool, String) {
    let transcript_changed = std::mem::take(&mut app.transcript.transcript_changed_pending);
    let displayed_transcript_changed = transcript_changed;

    let viewed_session_id = app.session.live_session_id.clone();

    if app.session.current_session_id != viewed_session_id {
        app.session.current_session_id = viewed_session_id.clone();
        app.on_viewed_session_changed();
        app.session.switching_session = None;
    }

    let backfill_from = app.composer.session_history_backfill_cursor;
    let viewed_len = app.transcript.messages.len();
    if backfill_from < viewed_len {
        let tail: Vec<(String, bool, u64)> =
            user_prompt_tail(&app.transcript.messages[backfill_from..]);
        app.composer.session_history_backfill_cursor = viewed_len;
        app.backfill_session_history(&tail, now_epoch_ms());
    }

    // Publish the routing fact to the translator (loop → translator).
    *runtime.viewed_session_id.lock().await = Some(viewed_session_id.clone());

    if app.session.current_workspace.is_empty() && app.session.current_role.is_none() {
        app.session.current_workspace = crate::shell::chrome::tilde_home(&app.session.cwd);
    }

    app.telemetry.context_tokens = app
        .telemetry
        .context_tokens_by_session
        .get(&viewed_session_id)
        .copied();

    (displayed_transcript_changed, viewed_session_id)
}

/// Consume the one-shot backend navigation signals the applier latched
/// (M1: formerly `AtomicBool` swaps on shared cells).
pub(crate) fn consume_navigation_signals(app: &mut App, runtime: &UiRuntime) -> (bool, bool) {
    let can_apply_backend_navigation = app.can_accept_navigation_signal();
    let open_sessions =
        can_apply_backend_navigation && std::mem::take(&mut app.link.open_sessions_signal);
    let open_tree = can_apply_backend_navigation && std::mem::take(&mut app.link.open_tree_signal);
    if !can_apply_backend_navigation {
        // Drop the signals anyway: a navigation the surface cannot accept
        // right now must not silently queue behind the next frame forever.
        app.link.open_sessions_signal = false;
        app.link.open_tree_signal = false;
    }
    let _ = runtime;
    (open_sessions, open_tree)
}

/// Consume the backend completion round-trip the applier latched.
pub(crate) fn consume_completion_signal(app: &mut App) -> bool {
    if let Some(signal) = app.composer.backend_completion_signal.take() {
        app.apply_backend_completions(signal.request_id, signal.input, signal.cursor, signal.items);
        true
    } else {
        false
    }
}
