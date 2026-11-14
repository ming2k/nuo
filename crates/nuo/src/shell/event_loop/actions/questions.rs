//! User interaction interstitials: question sheets, input injection, and permission gates.
use super::modals::{apply_pre_attach_decision, handle_permission_submit, question_effects};
use crate::shell::App;
use crate::shell::event_loop::runtime::UiRuntime;
use crate::shell::input::InputAction;
use nuo_protocol::{AgentRequest, PermissionDecision};
use nuo_tool::{PermissionRequest};


pub(super) async fn handle_question_action(
    app: &mut App,
    runtime: &UiRuntime,
    action: InputAction,
) {
    match action {
        InputAction::QuestionUp => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Up)
                        .0,
                );
                // Moving the highlight re-enables follow so the body
                // scrolls to keep the cursor visible.
                app.question_modal_follow = true;
            }
        }
        InputAction::QuestionDown => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Down)
                        .0,
                );
                app.question_modal_follow = true;
            }
        }
        InputAction::QuestionToggle => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Toggle)
                        .0,
                );
            }
        }
        InputAction::QuestionSelect(n) => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Select(n))
                        .0,
                );
                // A digit jump moves the highlight, so follow it.
                app.question_modal_follow = true;
            }
        }
        InputAction::QuestionSubmit => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                let (qm, effects) = qm.update(crate::shell::question_model::QuestionAction::Submit);
                // Keep the model until the per-frame queue sync clears
                // it; the Closed effect drives the channel reply + drain.
                app.question = Some(qm);
                question_effects::apply(&effects, app, runtime).await;
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        InputAction::QuestionPrevious => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Previous)
                        .0,
                );
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        InputAction::QuestionNext => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Next)
                        .0,
                );
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        InputAction::QuestionCancel => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                let (_qm, effects) =
                    qm.update(crate::shell::question_model::QuestionAction::Cancel);
                // Cancel discards the model immediately; the Closed
                // effect drives the (empty-answers) reply + drain.
                question_effects::apply(&effects, app, runtime).await;
            }
        }
        InputAction::PreAttachUp => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::shell::question_model::QuestionAction::Up);
            }
        }
        InputAction::PreAttachDown => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::shell::question_model::QuestionAction::Down);
            }
        }
        InputAction::PreAttachToggle => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::shell::question_model::QuestionAction::Toggle);
            }
        }
        InputAction::PreAttachSubmit => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let decision = pa.apply(crate::shell::question_model::QuestionAction::Submit);
                apply_pre_attach_decision(decision, app, runtime).await;
            }
        }
        InputAction::PreAttachCancel => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let decision = pa.apply(crate::shell::question_model::QuestionAction::Cancel);
                apply_pre_attach_decision(decision, app, runtime).await;
            }
        }
        InputAction::InputSubmit => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::InputInjection) {
                let text = std::mem::take(&mut app.input);
                if let Some(req) = app.pending_input.take() {
                    // Drain the matching front so the per-frame sync
                    // closes the modal and restores the composer draft.
                    app.pending_inputs.pop_front();
                    let parent_call_id = app.subagent_question_parent.remove(&req.id);
                    app.send_intent(AgentRequest::StdinReply {
                        request_id: req.id.clone(),
                        text,
                        parent_call_id,
                    });
                }
                let next = app.pending_inputs.front().cloned();
                if let Some(next) = next {
                    app.pending_input = Some(next);
                    app.input.clear();
                    app.set_cursor(0);
                } else {
                    app.restore_input_draft();
                    app.dismiss_sheet();
                }
            }
        }
        InputAction::InputCancel => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::InputInjection)
                && let Some(req) = app.pending_input.take()
            {
                // Empty reply = cancel → the command runs with closed
                // stdin and fails fast with a non-interactive remedy.
                app.pending_inputs.pop_front();
                let next = app.pending_inputs.front().cloned();
                let parent_call_id = app.subagent_question_parent.remove(&req.id);
                app.send_intent(AgentRequest::StdinReply {
                    request_id: req.id.clone(),
                    text: String::new(),
                    parent_call_id,
                });
                if let Some(next) = next {
                    app.pending_input = Some(next);
                    app.input.clear();
                    app.set_cursor(0);
                } else {
                    app.restore_input_draft();
                    app.dismiss_sheet();
                }
            }
        }
        InputAction::QuestionInsertChar(c) => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::InsertChar(c))
                        .0,
                );
                // Typing into the "Other" field may grow it onto a new
                // wrapped line, pushing the caret below the viewport.
                // Re-arm follow so the body scrolls to track the
                // caret (not just the "Other" label row).
                app.question_modal_follow = true;
            }
        }
        InputAction::QuestionBackspace => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Backspace)
                        .0,
                );
                // Backspace can collapse the field back up a line;
                // re-arm follow so the caret stays on screen.
                app.question_modal_follow = true;
            }
        }
        InputAction::PermissionPrevOption => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Permission) {
                let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
                let total = crate::shell::overlays::permission_action_count(
                    app.permission_confirm_always,
                    one_off,
                );
                if total > 0 {
                    app.modal_index = if app.modal_index == 0 {
                        total - 1
                    } else {
                        app.modal_index - 1
                    };
                }
            }
        }
        InputAction::PermissionNextOption => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Permission) {
                let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
                let total = crate::shell::overlays::permission_action_count(
                    app.permission_confirm_always,
                    one_off,
                );
                if total > 0 {
                    app.modal_index = (app.modal_index + 1) % total;
                }
            }
        }
        InputAction::PermissionSubmit => {
            handle_permission_submit(app, runtime).await;
        }
        InputAction::PermissionReject => {
            // Rejecting settles the whole concurrent permission batch;
            // resolve every queued request so its tool futures finish.
            let queued: Vec<PermissionRequest> = app.pending_permissions.drain(..).collect();
            app.pending_permission = None;
            app.dismiss_sheet();
            app.modal_index = 0;
            app.permission_confirm_always = false;
            app.permission_show_details = false;
            for pending in queued {
                let parent_call_id = app.subagent_permission_parent.remove(&pending.id);
                app.send_intent(AgentRequest::PermissionReply {
                    request_id: pending.id,
                    decision: PermissionDecision::Reject,
                    parent_call_id,
                });
            }
        }
        InputAction::PermissionBack => {
            app.permission_confirm_always = false;
            app.modal_index = 1;
        }
        _ => {}
    }
}
