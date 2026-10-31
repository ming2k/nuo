//! Composer-submission and interrupt handlers for the input dispatch match
//! (`SendChat` / `SendSlash` / `CtrlC`).
//! Extracted verbatim from the corresponding arms of `dispatch_action`'s
//! match; only the arm-level `continue` / `return Ok(())` control flow became
//! [`ActionFlow`] values (it already was, inside `dispatch_action`).
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc;

use nuo_protocol::{AgentRequest, Role};

use crate::shell::surfaces::{DialogKind, SceneKind, SheetKind};
use crate::shell::{App, clipboard, clipboard_ops};
use nuo_view::composer_attachments;
use nuo_view::model::document::{DeliveryStatus, TranscriptMessage};
use nuo_view::model::selection::SelectionState;

use super::super::runtime::{UiRuntime, now_epoch_ms};
use super::super::sync::show_local_toast;
use super::super::transcript::{extract_selection_text, resolve_focused_mut};
use super::ActionFlow;

/// Split a raw composer command into the ledger identity (`name`, `args`)
/// used by [`TranscriptMessage::pending_command`]: the command word without
/// the leading slash, and the raw argument remainder. Mirrors the runtime's
/// parse in `handlers_slash::dispatch` so the optimistic row and the eventual
/// `RoundEvent::CommandResult` agree on the invocation text.
pub(in crate::shell::event_loop) fn split_command_word(cmd: &str) -> (&str, &str) {
    let trimmed = cmd.trim();
    let first = trimmed.split_whitespace().next().unwrap_or_default();
    let name = first.trim_start_matches('/');
    let args = trimmed.strip_prefix(first).unwrap_or("").trim();
    (name, args)
}

/// Loop stage (input dispatch): the `SendChat` arm of the action match.
pub(super) async fn handle_send_chat(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    text: String,
) {
    // Note: history-search selection no longer flows through
    // here — Enter in `Modal::HistorySearch` emits the dedicated
    // `HistoryInsert` action so the chosen entry lands in the
    // input box for editing instead of being sent immediately.
    app.reset_to_thread();
    app.suggestion_index = None;
    app.input_scroll = 0;
    // The latency timeline starts here: the server records dispatch, this
    // records the moment the user pressed Enter.
    app.last_submit_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis() as u64);

    let images = std::mem::take(&mut app.pending_images);
    let text_pastes = std::mem::take(&mut app.pending_text_pastes);

    // Stage the chips' backing payloads so they ship with
    // this message. The text is expanded into the real paste
    // contents at the moment of dispatch — either inline
    // (immediate send) or when the queue drains (queued
    // send). For queue recall, the raw chip text and the
    // staged vectors are restored verbatim so the user can
    // keep editing the placeholder.
    let has_images = !images.is_empty();

    if !text.is_empty() || has_images {
        // `[INV-ATTACH-04]` (ADR-0133): a turn must not be submitted before the
        // server attests `Ready`. Queue it locally (the server also buffers
        // defensively) and flush on the transition.
        if app.session_phase != nuo_protocol::ThreadPhase::Ready {
            let expanded = composer_attachments::expand_paste_chips(&text, &text_pastes);
            let expanded = composer_attachments::strip_orphan_image_chips(&expanded, images.len());
            app.record_input_history(text.clone(), images.clone(), text_pastes);
            app.pending_prompts.push(AgentRequest::Prompt {
                text: expanded,
                images,
                sent_at_ms: Some(now_epoch_ms()),
            });
            app.clear_history_draft();
            app.record_notice(
                nuo_view::model::document::NoticeSeverity::Info,
                "attach",
                "Session is still preparing; your message will be sent when it is ready.",
                now_epoch_ms(),
            );
            return;
        }
        if app.running_sessions.contains(viewed_session_id) {
            match app.composer_send_mode {
                crate::shell::app::ComposerSendMode::Steer => {
                    let expanded = composer_attachments::expand_paste_chips(&text, &text_pastes);
                    let expanded =
                        composer_attachments::strip_orphan_image_chips(&expanded, images.len());
                    let id = app.new_insert_id();
                    app.record_input_history(text.clone(), images.clone(), text_pastes);
                    app.clear_history_draft();
                    app.follow_bottom = true;
                    app.pin_summary_line = None;
                    let sent_at_ms = now_epoch_ms();
                    let entry = TranscriptMessage::new(Role::User, text.clone())
                        .with_origin(nuo_view::model::document::UserMessageOrigin::Steer)
                        .with_sent_at_ms(sent_at_ms)
                        .with_insert_id(id.clone())
                        .queued();
                    app.messages.push(entry);
                    app.layout_height_cache.clear();
                    app.transcript_changed_pending = true;
                    app.send_intent(AgentRequest::Steer {
                        thread_id: viewed_session_id.to_string(),
                        message: nuo_protocol::QueuedMessage {
                            id,
                            text: expanded,
                            display_text: Some(text),
                            images,
                            sent_at_ms: Some(sent_at_ms),
                        },
                    });
                }
                crate::shell::app::ComposerSendMode::FollowUp => {
                    // M4: one verb, server decides. The frontend
                    // sends `FollowUp` unconditionally — an idle target
                    // starts immediately, a running one enqueues in the
                    // driver's queue. The optimistic queue-bar entry rides
                    // the `QueueUpdated` snapshot / `FollowUpQueued` ack.
                    let id = uuid::Uuid::new_v4().to_string();
                    let queued_at_ms = now_epoch_ms();
                    let expanded = composer_attachments::expand_paste_chips(&text, &text_pastes);
                    let expanded =
                        composer_attachments::strip_orphan_image_chips(&expanded, images.len());
                    app.pending_dispatch
                        .push_back(crate::shell::app::QueuedDispatch {
                            id: id.clone(),
                            session_id: viewed_session_id.to_string(),
                            state: crate::shell::app::QueuedDispatchState::Dispatching,
                            text: text.clone(),
                            queued_at_ms,
                            images: images.clone(),
                            text_pastes: text_pastes.clone(),
                        });
                    app.record_input_history(text.clone(), images.clone(), text_pastes.clone());
                    app.clear_history_draft();
                    app.follow_bottom = true;
                    app.pin_summary_line = None;
                    app.send_intent(AgentRequest::FollowUp {
                        thread_id: viewed_session_id.to_string(),
                        message: nuo_protocol::QueuedMessage {
                            id,
                            text: expanded,
                            display_text: Some(text),
                            images,
                            sent_at_ms: Some(queued_at_ms),
                        },
                    });
                }
            }
            app.composer_send_mode = crate::shell::app::ComposerSendMode::Steer;
        } else {
            // Expand `[Pasted text #N +M lines]` chips into
            // their full staged text right before dispatch so
            // the model receives the real paste contents
            // rather than the chip label. Image chips stay
            // in the text as positional labels.
            let expanded = composer_attachments::expand_paste_chips(&text, &text_pastes);
            // An image chip with no staged payload (e.g.
            // recalled from a history entry recorded before
            // attachment staging) is a bare label — drop it
            // so the model never receives a phantom
            // `[Image #N …]` it cannot see.
            let expanded = composer_attachments::strip_orphan_image_chips(&expanded, images.len());
            runtime.is_responding.store(true, Ordering::SeqCst);
            // A new dispatch opens a new attempt window: `Queued` is not
            // the transport-wait phase, so this also retires any setback
            // clause left over from an earlier round.
            app.set_phase(Some(nuo_view::phase::Phase::Queued));
            app.running_sessions.insert(viewed_session_id.to_string());
            let sent_at_ms = now_epoch_ms();
            let target_round = app.round_count.saturating_add(1);
            let sent = TranscriptMessage::new(Role::User, text.clone())
                .with_sent_at_ms(sent_at_ms)
                .with_round(target_round)
                .sending();
            app.messages.push(sent);
            app.layout_height_cache.clear();
            app.transcript_changed_pending = true;
            app.record_input_history(text.clone(), images.clone(), text_pastes.clone());
            // The draft's content has been sent — it is now a
            // history row, not the unsent slot.
            app.clear_history_draft();
            app.follow_bottom = true;
            app.pin_summary_line = None;
            app.send_intent(AgentRequest::Prompt {
                text: expanded,
                images,
                sent_at_ms: Some(sent_at_ms),
            });
        }
    } else if let Some((start, end)) = app.selection.active_normalized_range() {
        // Enter on a selected step: navigate into a subagent
        // task, otherwise toggle that step's expansion.
        if start.message_idx == end.message_idx {
            let mi = start.message_idx;
            let mut messages = std::mem::take(&mut app.messages);
            // An subagent task navigates into its view instead
            // of expanding.
            let enter_id =
                resolve_focused_mut(&mut messages, &app.focus_stack, mi).and_then(|message| {
                    if message.is_subagent_task() {
                        message.tool_step_call_id().map(String::from)
                    } else {
                        None
                    }
                });
            if let Some(id) = enter_id {
                app.messages = messages;
                app.enter_subagent(id);
            } else {
                let toggled = app.toggle_step_pinned(&mut messages, mi);
                app.messages = messages;
                app.layout_height_cache.clear();
                app.transcript_changed_pending = true;
                if toggled {
                    app.selection = SelectionState::None;
                }
            }
        }
    }
}

/// Loop stage (input dispatch): the `SendSlash` arm of the action match.
/// `pub(crate)` so behavior-lock tests in `crate::shell::tests` can drive it directly
pub(crate) async fn handle_send_slash(
    app: &mut App,
    runtime: &UiRuntime,
    _session: &crate::shell::SessionSource,
    cmd: String,
) -> ActionFlow {
    app.suggestion_index = None;
    app.input_scroll = 0;
    // A command is a synchronous control-plane operation, not a round
    // it never enters the round state machine, so it must not
    // arm the activity bar's liveness surface — no `is_responding`, no
    // optimistic "queued", no fabricated `Esc Esc interrupt` affordance over
    // a dispatch that cannot be interrupted. The pending command row pushed
    // below is the in-flight feedback for a command; a running
    // round keeps owning the bar through its own events.
    app.follow_bottom = true;
    app.pin_summary_line = None;
    let sent_at_ms = now_epoch_ms();
    // The command component owns its input AND its output: the
    // optimistic row pushed here is the input half — `⌘ /cmd` in the muted
    // running tone — and the `RoundEvent::CommandResult` handler settles the
    // same row in place when the typed reply arrives. A command is therefore
    // never echoed as a user bubble (the old `▌ cmd` panel duplicated the
    // invocation in a second row and split the effect across a seam), and the
    // transcript keeps one row per command, live and after resume alike.
    // The invocation is still recorded in input history for ↑/Ctrl+R recall.
    let (cmd_name, cmd_args) = split_command_word(&cmd);

    let is_new_session_cmd = cmd_name == "new"
        || (cmd_name == "session" && cmd_args.split_whitespace().next() == Some("new"));

    if is_new_session_cmd {
        app.record_input_history(cmd.clone(), Vec::new(), Vec::new());
        let prompt_args = if cmd_name == "new" {
            cmd_args.trim()
        } else if cmd_name == "session" {
            cmd_args.strip_prefix("new").map(str::trim).unwrap_or("")
        } else {
            ""
        };

        if app.is_current_thread_empty() {
            if !prompt_args.is_empty() {
                // If thread is already empty and user provided an initial prompt,
                // submit the prompt directly in place.
                handle_send_chat(
                    app,
                    runtime,
                    &app.current_session_id.clone(),
                    prompt_args.to_string(),
                )
                .await;
                return ActionFlow::Handled;
            }
            // Bare reset in place without adding a new tab.
            app.messages.push(
                TranscriptMessage::pending_command(cmd_name, cmd_args).with_sent_at_ms(sent_at_ms),
            );
            app.send_intent(AgentRequest::SlashCommand("/new".to_string()));
            return ActionFlow::Handled;
        } else {
            // Current thread has conversations: preserve current tab and create a fresh thread in a new tab.
            app.switch_to_new = true;
            if !prompt_args.is_empty() {
                app.switch_to_new_prompt = Some(prompt_args.to_string());
            }
            app.should_quit.store(true, Ordering::SeqCst);
            return ActionFlow::Handled;
        }
    }

    if (cmd_name == "sessions" || cmd_name == "resume" || cmd_name == "session")
        && !cmd_args.trim().is_empty()
        && !cmd_args.trim().starts_with("list")
    {
        let target = cmd_args
            .split_whitespace()
            .next()
            .unwrap_or(cmd_args.trim());
        let short_id = crate::shell::session::short_session_id(target);
        app.switching_session = Some(short_id);
        app.messages.clear();
        app.layout_height_cache.clear();
        app.transcript_changed_pending = true;
        app.scroll = 0;
    }
    app.messages
        .push(TranscriptMessage::pending_command(cmd_name, cmd_args).with_sent_at_ms(sent_at_ms));
    app.record_input_history(cmd.clone(), Vec::new(), Vec::new());
    app.send_intent(AgentRequest::SlashCommand(cmd));
    ActionFlow::Handled
}

pub(super) async fn handle_send_steer(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    text: String,
) {
    app.composer_send_mode = crate::shell::app::ComposerSendMode::Steer;
    handle_send_chat(app, runtime, viewed_session_id, text).await;
}

pub(super) async fn handle_queue_follow_up(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    text: String,
) {
    app.composer_send_mode = crate::shell::app::ComposerSendMode::FollowUp;
    handle_send_chat(app, runtime, viewed_session_id, text).await;
}

/// Loop stage (input dispatch): the `CtrlC` arm of the action match (copy
/// selection, close overlay, clear input, armed double-press quit).
pub(crate) fn handle_ctrl_c(
    app: &mut App,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
) -> ActionFlow {
    if let Some(text) = extract_selection_text(
        &app.selection,
        app.focused_messages(),
        &app.input,
        &app.ui.document,
        app.drag.cell_info.as_ref(),
    ) {
        clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
    } else if app.active_dialog() == Some(DialogKind::HistorySearch) {
        // [ADR-0044] Single Input SSOT: Composer is the live query input.
        // Ctrl+C clears the query, or cancels history search and restores draft if already empty.
        if !app.input.is_empty() {
            app.input.clear();
            app.cursor_position = 0;
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                .index = 0;
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                .follow = true;
        } else {
            app.dismiss_surface();
        }
    } else if app.startup_overlay == crate::shell::StartupOverlay::SessionsPicker
        && app.active_dialog() == Some(DialogKind::Threads)
    {
        // `nuo attach` (no id) opened the picker at startup:
        // there is no thread behind it, so Ctrl+C — like
        // Esc and an outside click — quits the program rather
        // than dropping into an empty session. Without this,
        // Ctrl+C used to close the modal and land the user in a
        // bare empty chat (which a stray /models then persisted
        // as an empty-session file).
        tracing::info!(reason = "startup_picker_cancelled", "app exiting");
        app.should_quit.store(true, Ordering::SeqCst);
    } else if app.surfaces.contains_sheet(SheetKind::OAuthPending) {
        let text = if !app.oauth_pending_url.is_empty() {
            app.oauth_pending_url.clone()
        } else if !app.oauth_pending_user_code.is_empty() {
            app.oauth_pending_user_code.clone()
        } else {
            String::new()
        };
        if !text.is_empty() {
            clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
            show_local_toast(
                app,
                "Link copied to clipboard",
                false,
                std::time::Duration::from_millis(2000),
            );
        }
    } else if app.current_scene() == SceneKind::Dashboard {
        // The session dashboard owns Ctrl+C: it is a first-class
        // screen, not a transient modal, so Ctrl+C never closes
        // it into the thread behind it. The gesture is the
        // app-wide double-press — first press arms a 2s quit
        // window (the "press Ctrl+C again to exit" toast), the
        // second exits the whole TUI.
        if app.host_prompting && !app.input.is_empty() {
            // First Ctrl+C clears the staged text. Clearing does not
            // arm quit, avoiding accidental exits on rapid clear gestures.
            app.input.clear();
            app.set_cursor(0);
            app.arm_ctrl_c(None);
            show_local_toast(
                app,
                "input cleared",
                false,
                std::time::Duration::from_millis(1800),
            );
        } else if app.ctrl_c_armed() {
            tracing::info!(reason = "dashboard_ctrl_c_double_press", "app exiting");
            if app.startup_overlay == crate::shell::StartupOverlay::Dashboard
                || matches!(
                    app.startup_overlay,
                    crate::shell::StartupOverlay::Settings { .. }
                )
            {
                // Standalone entry (`nuo dashboard` or `nuo settings`) opened this
                // screen without an attached thread: quit cleanly.
                app.should_quit.store(true, Ordering::SeqCst);
            } else {
                // Client detach (ADR-0038 [INV-ACTION-02]).
                tracing::info!(reason = "ctrl_c_dashboard", "client detaching");
                return ActionFlow::Exit;
            }
        } else {
            // Arm the real 2s window in which a second Ctrl+C
            // quits (the toast renders over the dashboard).
            app.copy_toast_until = None;
            app.arm_ctrl_c(Some(std::time::Instant::now() + App::CTRL_C_ARM_WINDOW));
        }
    } else if app.surfaces.active_overlay().is_some() {
        // Ctrl+C over a surface is the same dismiss as Esc:
        // retained browse views hide with state saved, the quick switcher
        // cancels to its origin, everything else falls to plain close.
        // A coexisting sheet does not downgrade this: the modal is the
        // visual foreground, so Ctrl+C closes it first (the sheet beneath
        // keeps its pending decision) — mirroring the Esc arms.
        super::modals::handle_close_modal(app, viewed_session_id);
    } else if app.is_composer_focused() && app.has_composer_content() {
        // Ctrl+C clears the composer content only when the composer has input focus.
        // Clearing text consumes this shortcut and does NOT arm
        // the quit window. If the user wants to exit, they must
        // press Ctrl+C twice with an empty input.
        app.clear_composer();
        show_local_toast(
            app,
            "input cleared",
            false,
            std::time::Duration::from_millis(1800),
        );
    } else if app.ctrl_c_armed() {
        // Double Ctrl+C inside the thread is a client detach intent (ADR-0038 [INV-ACTION-02]).
        // Quits the TUI gracefully without sending EndSession, leaving the thread running.
        tracing::info!(reason = "ctrl_c_double_press", "client detaching");
        return ActionFlow::Exit;
    } else {
        // Arm a real 2s window (wall-clock) in which a second Ctrl+C
        // quits. Clear any transient informational toast so the armed
        // confirmation shows immediately.
        app.copy_toast_until = None;
        app.arm_ctrl_c(Some(std::time::Instant::now() + App::CTRL_C_ARM_WINDOW));
    }
    ActionFlow::Handled
}

/// Who a double-Esc (`Esc Esc`) interrupt targets. The chord is **scene-scoped**
/// the scene the user stands in decides which round the confirmation
/// window stops, so an interrupt fired inside a nested view can never penetrate
/// its scene boundary and stop an outer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterruptTarget {
    /// The primary session's running round. The Conversation scene's Esc.
    Primary,
    /// One subagent spawned by the session, addressed by its parent tool-call
    /// id. The Subagent scene's Esc: it stops only the viewed child
    /// and leaves the enclosing primary round and every sibling running.
    Subagent,
}

/// Loop stage (input dispatch): the shared `Interrupt` / `InterruptSubagent` arm of the action match.
pub(crate) fn handle_esc_interrupt(app: &mut App, target: InterruptTarget) -> bool {
    if !app.esc_press() {
        return false;
    }
    match target {
        InterruptTarget::Subagent => {
            if let Some(call_id) = app.focused_subagent_call_id().map(str::to_owned) {
                app.send_intent(AgentRequest::InterruptSubagent { call_id });
            }
            return true;
        }
        InterruptTarget::Primary => {}
    }
    let target_session_id = if !app.current_session_id.is_empty() {
        Some(app.current_session_id.clone())
    } else {
        None
    };
    if let Some(ref sid) = target_session_id {
        app.block_queue(sid);
        app.running_sessions.remove(sid);
    }
    app.clear_responding();
    // Immediately mark any in-flight prompt in `app.messages` as cancelled
    // so the TUI updates its status and styling with zero blocking.
    if let Some(msg) = app.messages.iter_mut().rev().find(|m| {
        m.role == Role::User
            && (m.is_sending() || (m.delivery == DeliveryStatus::Delivered && m.round.is_none()))
    }) {
        if msg.round.is_none() {
            msg.round = Some(app.round_count.saturating_add(1));
        }
        msg.cancel_prompt();
    }
    app.send_intent(AgentRequest::Interrupt);
    true
}

/// Shared input dispatch: handles the double-Esc interrupt confirmation and
/// immediately updates both `App` and `UiRuntime` state without latency.
pub(crate) async fn handle_esc_interrupt_with_runtime(
    app: &mut App,
    runtime: &UiRuntime,
    target: InterruptTarget,
) {
    if !handle_esc_interrupt(app, target) {
        return;
    }
    // The scene-scoped subagent interrupt leaves the primary runtime running:
    // only the chat paths clear the global responding flag and reset
    // the viewed transcript's in-flight prompt row.
    if matches!(target, InterruptTarget::Subagent) {
        return;
    }
    runtime.is_responding.store(false, Ordering::SeqCst);
    // The round is being stopped: no request is in flight any more, so the
    // transport-setback clause goes with the phase.
    app.set_phase(None);
    let msgs = &mut app.messages;
    if let Some(m) = msgs.iter_mut().rev().find(|m| {
        m.role == Role::User
            && (m.is_sending() || (m.delivery == DeliveryStatus::Delivered && m.round.is_none()))
    }) {
        if m.round.is_none() {
            m.round = Some(app.round_count.saturating_add(1));
        }
        m.cancel_prompt();
    }
}
