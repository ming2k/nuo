//! The mutation applier (ADR-0197 M1): the **sole writer** of `App`.
//!
//! Every [`AppMutation`] produced by the response translator (and the
//! monitor client) lands here. The applier is deliberately mechanical: the
//! translator pre-builds payloads (attribution, effort, positions, fallback
//! messages, disclosure defaults); the applier executes the edit against
//! `App` and maintains the two bookkeeping facts the render loop consumes —
//! the height-cache invalidation and the "transcript changed" flag that
//! drives bottom-follow scrolling.

use nuo_wire::Role;

use crate::app::App;
use crate::event_loop::mutations::{AppMutation, Buffer, ChromeEdit, TranscriptEdit};
use crate::event_loop::runtime::{SideViewSignal, UiRuntime};
use crate::model::document::{MessageKind, TranscriptMessage};
use crate::phase::Phase;

fn buffer_messages(app: &mut App, buffer: Buffer) -> &mut Vec<TranscriptMessage> {
    match buffer {
        Buffer::Primary => &mut app.messages,
        Buffer::Side => &mut app.side_messages,
    }
}

/// Height-cache bookkeeping deferred until the document borrow is dropped.
enum PostEdit {
    None,
    Invalidate(u64),
    ClearCache,
}

/// ADR-0026: resolve a *still-pending* announced tool step by its provider
/// `slot`. Pending steps carry an empty call id and are stamped with their slot
/// at announcement, so the match is exact and unique per turn.
fn find_pending_announced_tool_step_mut(
    messages: &mut [crate::model::document::TranscriptMessage],
    slot: usize,
) -> Option<&mut crate::model::document::TranscriptMessage> {
    messages
        .iter_mut()
        .rfind(|message| message.is_announced_pending_tool_step_for_slot(slot))
}

/// Apply one mutation. Returns whether anything changed that a frame must
/// observe (the loop ORs this into its dirty computation).
pub(crate) fn apply(app: &mut App, runtime: &UiRuntime, mutation: AppMutation) -> bool {
    match mutation {
        AppMutation::Transcript { buffer, edit } => apply_transcript(app, buffer, edit),
        AppMutation::ChromeEdit { session_id, edit } => {
            let is_phase_only = matches!(edit, ChromeEdit::PhaseOnly(_));
            apply_chrome(app, &session_id, edit);
            // The round-boundary chrome edit is also the live-session fact:
            // the queue bar's running rows and the keymap's has-running-task
            // guard read `App::running_sessions` (ADR-0197 M4: formerly the
            // deleted dispatch `HarnessState` signal).
            if let Some(chrome) = app.session_chrome.get(&session_id) {
                let running = chrome.responding;
                if running {
                    app.running_sessions.insert(session_id.clone());
                } else if !is_phase_only {
                    app.running_sessions.remove(&session_id);
                }
            }
            true
        }

        AppMutation::Harness(snapshot) => {
            app.loop_status = snapshot.loop_status;
            app.unattended = snapshot.unattended;
            app.confined = snapshot.confined;
            app.harness_retry_pending = snapshot.retry_pending;
            app.workspace_security = snapshot.workspace_security.clone();
            if let Some(role) = snapshot.role {
                app.current_role = Some(role);
            }
            if let Some(ws) = snapshot.workspace {
                app.current_workspace = crate::chrome::tilde_home(std::path::Path::new(&ws));
            } else if app.current_role.is_some() {
                app.current_workspace.clear();
            }
            // PreAttach unmount (ADR-0175, ADR-0253): when the snapshot transitions
            // to having no unresolved quarantined/changed/expired domains (or the
            // workspace is no longer quarantined), clear the interstitial and latch
            // the per-run gate so a subsequent periodic republish cannot re-mount
            // within this run.
            if let Some(pa) = app.pre_attach.as_mut() {
                let gate_needed =
                    crate::trust_gate::gate_request(&snapshot.workspace_security).is_some();
                if !gate_needed {
                    tracing::info!(
                        "nuo: clearing PreAttach interstitial (workspace review resolved)"
                    );
                    app.pre_attach = None;
                    runtime
                        .trust_gate_dismissed
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                } else if pa.submitting() {
                    // The snapshot round-tripped after submission, but the gate
                    // is still not resolved (e.g. backend attestation failure).
                    // Refresh the question model from the latest snapshot and reset
                    // submitting so the user is never permanently stuck in a submission spinner.
                    if let Some(new_state) =
                        crate::PreAttachState::from_snapshot(&snapshot.workspace_security)
                    {
                        *pa = new_state;
                    }
                }
            }
            true
        }
        AppMutation::HarnessUnattended(enabled) => {
            app.unattended = enabled;
            true
        }
        AppMutation::HarnessConfined(confined) => {
            app.confined = confined;
            true
        }
        AppMutation::ClearSwitchingSession => {
            app.switching_session = None;
            true
        }
        AppMutation::LiveSession(session_id) => {
            app.live_session_id = session_id;
            true
        }

        AppMutation::SetPhase(phase) => {
            // The primary's half of the setback-clause lifetime: this mirror is
            // written by every primary phase move, so retiring the clause here
            // is what makes "the clause ends with its phase" true by
            // construction (ADR-0235).
            app.set_phase(phase);
            true
        }
        AppMutation::SetResponding(responding) => {
            runtime
                .is_responding
                .store(responding, std::sync::atomic::Ordering::SeqCst);
            true
        }
        AppMutation::SetRoundCount(round) => {
            app.round_count = round;
            true
        }
        AppMutation::SetCurrentTurn(turn) => {
            app.current_turn = turn;
            true
        }
        AppMutation::SetRoundStartedAt(started) => {
            app.round_started_at = started;
            true
        }
        AppMutation::SetProviderRetry(retry) => {
            app.provider_retry = Some(retry);
            true
        }

        AppMutation::QueuePermission {
            request,
            parent_call_id,
        } => {
            if let Some(parent) = parent_call_id {
                app.subagent_permission_parent
                    .insert(request.id.clone(), parent);
            }
            app.pending_permissions.push_back(request);
            true
        }
        AppMutation::QueueQuestion {
            request,
            parent_call_id,
        } => {
            if let Some(parent) = parent_call_id {
                app.subagent_question_parent
                    .insert(request.id.clone(), parent);
            }
            app.pending_questions.push_back(request);
            true
        }
        AppMutation::QueueInput(request) => {
            app.pending_inputs.push_back(request);
            true
        }
        AppMutation::ClearPermissions => {
            app.pending_permissions.clear();
            true
        }
        AppMutation::BackgroundTaskStarted {
            id,
            label,
            started_at_ms,
        } => {
            app.upsert_background_task(id, label, started_at_ms);
            true
        }
        AppMutation::BackgroundTaskCompleted {
            id,
            success,
            exit_code,
            duration_secs,
        } => {
            app.complete_background_task(&id, success, exit_code, duration_secs);
            true
        }
        AppMutation::BackgroundTaskDismissSettled => {
            app.dismiss_settled_background_tasks();
            true
        }

        AppMutation::DispatchRemoved {
            session_id,
            input_id,
        } => {
            app.remove_dispatch(&session_id, &input_id);
            true
        }
        AppMutation::SteerMissed {
            session_id,
            input_id,
        } => {
            // ADR-0212: Steer is ephemeral to the targeted round. If admission
            // missed the round window, purge the optimistic entry from transcript
            // and restore text to the composer draft — never re-queue as follow-up.
            let mut text_to_restore = None;
            if let Some(pos) = app.messages.iter().rposition(|m| {
                m.insert_id.as_deref() == Some(input_id.as_str()) && m.role == Role::User
            }) {
                let msg = app.messages.remove(pos);
                text_to_restore = Some(msg.raw);
            } else if let Some(pos) = app.side_messages.iter().rposition(|m| {
                m.insert_id.as_deref() == Some(input_id.as_str()) && m.role == Role::User
            }) {
                let msg = app.side_messages.remove(pos);
                text_to_restore = Some(msg.raw);
            }
            if let Some(text) = text_to_restore
                && app.live_session_id == session_id
            {
                app.adopt_as_draft(
                    text,
                    Vec::new(),
                    Vec::new(),
                    crate::app::DraftAdoption::OnlyIfIdle,
                );
                app.notice_toast_severity = crate::model::document::NoticeSeverity::Info;
                app.notice_toast_message =
                    "Steer missed active round; restored to composer".to_string();
                app.notice_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(2600));
            }
            app.remove_dispatch(&session_id, &input_id);
            app.layout_height_cache.clear();
            app.transcript_changed_pending = true;
            true
        }
        AppMutation::DispatchQueued {
            session_id,
            input_id,
        } => {
            if let Some(item) = app
                .pending_dispatch
                .iter_mut()
                .find(|item| item.session_id == session_id && item.id == input_id)
            {
                item.state = crate::app::QueuedDispatchState::Waiting;
            }
            true
        }
        AppMutation::QueueSnapshot {
            session_id,
            items,
            paused,
        } => {
            // Authoritative replace, preserving in-flight optimistic entries
            // (state `Dispatching`, id not yet in the snapshot — the server
            // ack for those is still in flight).
            let optimistic: Vec<crate::app::QueuedDispatch> = app
                .pending_dispatch
                .iter()
                .filter(|item| {
                    item.session_id == session_id
                        && item.state == crate::app::QueuedDispatchState::Dispatching
                        && !items.iter().any(|queued| queued.id == item.id)
                })
                .cloned()
                .collect();
            app.pending_dispatch
                .retain(|item| item.session_id != session_id);
            for item in optimistic {
                app.pending_dispatch.push_back(item);
            }
            for queued in items {
                app.pending_dispatch.push_back(crate::app::QueuedDispatch {
                    id: queued.id,
                    session_id: session_id.clone(),
                    state: crate::app::QueuedDispatchState::Waiting,
                    text: queued.display_text.unwrap_or(queued.text),
                    queued_at_ms: queued.sent_at_ms.unwrap_or(0),
                    images: queued.images,
                    text_pastes: Vec::new(),
                });
            }
            if paused {
                app.queue_blocked_sessions.insert(session_id);
            } else {
                app.queue_blocked_sessions.remove(&session_id);
            }
            true
        }

        AppMutation::ParentStatus(status) => {
            app.parent_status = status;
            true
        }
        AppMutation::SideView(signal) => match signal {
            SideViewSignal::Opened { side_id, .. } => {
                app.enter_side_view(side_id);
                app.view_transitioned = true;
                true
            }
            SideViewSignal::Closed => {
                app.exit_side_view();
                app.view_transitioned = true;
                true
            }
        },
        AppMutation::BtwList(rows) => {
            app.btw_list = rows;
            true
        }
        AppMutation::InputHistory(rows) => {
            app.input_history = if app.input_history_record_commands {
                rows
            } else {
                // `[input_history] record_commands = false`: scrub any legacy
                // `/slash` invocations from the server snapshot so they stop
                // showing in the picker.
                rows.into_iter()
                    .filter(|e| !e.text.starts_with('/'))
                    .collect()
            };
            true
        }
        AppMutation::RouteSettings {
            provider_id,
            model,
            overrides,
        } => {
            // Prefill only if the editor is still open on the same route —
            // the answer is async, the operator may have moved on.
            if app.editor_target.as_deref() == Some(provider_id.as_str())
                && app.editor_model == model
            {
                app.editor_vision_override = overrides.as_ref().and_then(|o| o.vision);
                app.editor_tool_override = overrides.as_ref().and_then(|o| o.tool_call);
                true
            } else {
                false
            }
        }
        AppMutation::KeyStatus(status) => {
            app.key_status = status;
            true
        }
        AppMutation::ProviderPicker(snapshot) => {
            app.provider_picker = snapshot;
            app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().refreshing = false;
            app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().refreshing = false;
            true
        }
        AppMutation::SessionsOverview(mut sessions) => {
            // Self-exclusion (ADR-0250): remove the active session from the switch candidates list
            // so the cursor immediately targets the most recently active alternative session (MRU top).
            sessions.retain(|s| !s.active);
            app.modal_index = app.modal_index.min(sessions.len().saturating_sub(1));
            app.sessions_overview = sessions;
            app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().loading = false;
            true
        }
        AppMutation::OpenSessionsPanel => {
            app.open_sessions_signal = true;
            true
        }
        AppMutation::OpenTreePanel => {
            app.open_tree_signal = true;
            true
        }
        AppMutation::OpenHostPanel => {
            app.open_host_signal = true;
            true
        }
        AppMutation::SessionDetail(detail) => {
            let same_id = app.session_detail.as_ref().map(|s| &s.id) == Some(&detail.id);
            app.session_detail = Some(detail);
            if !same_id {
                app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_scroll = 0;
            }
            true
        }
        AppMutation::ConnectionDetail(detail) => {
            let same_id = app.connection_detail.as_ref().map(|c| &c.name) == Some(&detail.name);
            app.connection_detail = Some(detail);
            if !same_id {
                app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_scroll = 0;
            }
            true
        }
        AppMutation::TokenReport(report) => {
            app.token_report = report;
            true
        }
        AppMutation::UsageStats(report) => {
            app.usage_stats = Some(report);
            true
        }
        AppMutation::SessionTree(tree) => {
            app.session_tree = tree;
            true
        }
        AppMutation::SessionContext(snapshot) => {
            app.session_context = Some(snapshot);
            true
        }
        AppMutation::CompletionSignal(signal) => {
            app.backend_completion_signal = Some(signal);
            true
        }
        AppMutation::NoticeToast { severity, text } => {
            app.copy_toast_until = None;
            app.notice_toast_severity = severity;
            app.notice_toast_message = text;
            app.notice_toast_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(2600));
            true
        }
        AppMutation::Oauth(signal) => {
            apply_oauth(app, signal);
            true
        }
        AppMutation::WebSearchConfig(snapshot) => {
            app.websearch_config = snapshot;
            true
        }
        AppMutation::PreAttach(signal) => {
            let dismissed = runtime
                .trust_gate_dismissed
                .load(std::sync::atomic::Ordering::SeqCst);
            if !dismissed
                && let Some(state) = crate::PreAttachState::from_snapshot(&signal.snapshot)
                && app.pre_attach.is_none()
            {
                tracing::info!("nuo: mounting PreAttach interstitial");
                app.pre_attach = Some(state);
                // PreAttach claims the keyboard; reset the composer/sheet
                // state the way an ordinary sheet mount does.
                app.modal_index = 0;
                app.park_transcript_focus_for_sheet();
            }
            true
        }
        AppMutation::ProviderSwitched { provider, model } => {
            app.current_provider = provider;
            app.current_model = model;
            true
        }
        AppMutation::ContextTokens {
            session_id,
            snapshot,
        } => {
            app.context_tokens_by_session.insert(session_id, snapshot);
            true
        }
        AppMutation::ClearContextTokens => {
            app.context_tokens_by_session.clear();
            true
        }
        AppMutation::Quit => {
            app.should_quit
                .store(true, std::sync::atomic::Ordering::SeqCst);
            true
        }
        AppMutation::HostSessions(rows) => {
            app.host_sessions = rows;
            true
        }
        AppMutation::PersistenceHealth(health) => {
            app.persistence_health = health;
            true
        }
        AppMutation::HostConsole(line) => {
            app.host_console_log.push(line);
            true
        }
    }
}

/// The OAuth add-flow handoff (formerly the `oauth_add_signal` cell and its
/// per-frame drain).
fn apply_oauth(app: &mut App, signal: crate::app::OauthAddSignal) {
    use crate::app::OauthAddSignal as Signal;
    match signal {
        Signal::Pending {
            url,
            user_code,
            message,
        } => {
            if app.awaiting_oauth_add {
                app.oauth_pending_url = url;
                app.oauth_pending_user_code = user_code;
                app.oauth_pending_message = message;
                app.oauth_pending_error = None;
                app.surfaces
                    .present_sheet(crate::surfaces::SheetKind::OAuthPending);
            }
        }
        Signal::Done => {
            if app.awaiting_oauth_add {
                app.open_oauth_instance_name_editor();
            }
        }
        Signal::Failed { message } => {
            if app.awaiting_oauth_add {
                app.oauth_pending_error = Some(message);
                app.surfaces
                    .present_sheet(crate::surfaces::SheetKind::OAuthPending);
            }
        }
    }
}

/// Execute one transcript edit against the target document. Invalidation
/// discipline mirrors the old `Versioned` write guards, made honest: a
/// streaming-targeted content edit invalidates exactly the touched message's
/// cached height; a structural replace clears the cache; metadata-only
/// edits (delivery badges, round stamps) touch nothing, and newly appended
/// messages have no cached entry to evict.
fn apply_transcript(app: &mut App, buffer: Buffer, edit: TranscriptEdit) -> bool {
    let mut post = PostEdit::None;
    let mut changed = true;
    // Snapshot the disclosure input the tool arms need before the document borrow.
    let tui_config = app.tui_config.clone();
    {
        let messages = buffer_messages(app, buffer);
        match edit {
            TranscriptEdit::BeginStream => {
                crate::begin_stream(messages);
            }
            TranscriptEdit::StreamTextDelta {
                round,
                turn,
                delta,
                created,
                clear_retry,
            } => {
                let _ = clear_retry; // the translator bundles the retry reset as its own mutation
                match crate::append_stream_text_delta(messages, round, turn, &delta) {
                    Some(id) => post = PostEdit::Invalidate(id),
                    None => {
                        if let Some(message) = created {
                            messages.push(message);
                        }
                    }
                }
            }
            TranscriptEdit::StreamTextFinalize {
                round,
                turn,
                content,
                created,
            } => {
                let target = messages.iter_mut().rfind(|message| {
                    message.role == Role::Assistant
                        && matches!(&message.kind, MessageKind::Text)
                        && message.round == round
                        && message.turn == turn
                });
                match target {
                    Some(message) => {
                        message.raw = content;
                        message.reparse();
                        let id = message.id;
                        post = PostEdit::Invalidate(id);
                    }
                    None => {
                        // Defensive fallback for providers that deliver only
                        // a final payload with no preceding deltas.
                        if let Some(message) = created {
                            messages.push(message);
                        }
                    }
                }
            }
            TranscriptEdit::StreamDiscard { round, turn } => {
                let pops = messages.last().is_some_and(|message| {
                    message.role == Role::Assistant
                        && message.round == round
                        && message.turn == turn
                });
                if pops {
                    messages.pop();
                } else {
                    changed = false;
                }
            }
            TranscriptEdit::ReasoningDelta {
                round,
                turn,
                delta,
                created,
            } => {
                // Reasoning traces carry no height-cache entry while
                // streaming; their deltas need no invalidation.
                match crate::append_reasoning_delta(messages, round, turn, &delta) {
                    Some(_) => {}
                    None => {
                        if let Some(message) = created {
                            messages.push(message);
                        }
                    }
                }
            }
            TranscriptEdit::ReasoningFinalize {
                round,
                turn,
                content,
                duration_ms,
            } => {
                let target = messages.iter_mut().rfind(|message| {
                    message.is_reasoning_streaming()
                        && message.round == round
                        && message.turn == turn
                });
                if let Some(last) = target {
                    last.raw = content.clone();
                    last.reparse();
                    if let MessageKind::Reasoning {
                        content: current,
                        duration_ms: d,
                        ..
                    } = &mut last.kind
                    {
                        *current = content;
                        if d.is_none() {
                            *d = Some(duration_ms.unwrap_or(0));
                        }
                    }
                    let id = last.id;
                    post = PostEdit::Invalidate(id);
                }
            }
            TranscriptEdit::ToolStart { message } => {
                messages.push(message);
            }
            TranscriptEdit::ToolAnnounce {
                slot,
                name,
                message,
            } => {
                let _ = slot;
                // Insert the announced (pending) step, applying the same
                // lifecycle-aware disclosure default a finished step gets; the
                // following `ToolCallCollapse` re-keys it onto the dispatch id.
                // A just-pushed message has no cached height to evict.
                let mut message = message;
                if let Some(status) = message.tool_step_status() {
                    let default = crate::step_interaction::default_tool_expanded(
                        status,
                        &name,
                        &tui_config,
                    );
                    message.set_tool_step_expanded(default);
                }
                messages.push(message);
            }
            TranscriptEdit::ToolCallCollapse { slot, call_id } => {
                if let Some(step) = find_pending_announced_tool_step_mut(messages, slot) {
                    // Re-key onto the dispatch id and clear the pre-dispatch
                    // slot/byte state in one transition.
                    step.rekey_tool_step(call_id);
                    post = PostEdit::Invalidate(step.id);
                }
            }
            TranscriptEdit::ToolInputProgress { slot, bytes } => {
                if let Some(step) = find_pending_announced_tool_step_mut(messages, slot) {
                    let id = step.tool_step_call_id().unwrap_or_default().to_string();
                    if step.set_tool_input_bytes(&id, bytes) {
                        post = PostEdit::Invalidate(step.id);
                    }
                }
            }
            TranscriptEdit::FrozenOrphanToolSteps => {
                for message in messages.iter_mut() {
                    if message.cancel_pending_announced_tool_step() {
                        post = PostEdit::Invalidate(message.id);
                    }
                }
            }
            TranscriptEdit::ToolResult {
                id,
                name,
                output,
                structured,
                duration_ms,
                fallback,
            } => {
                let mut finished: Option<u64> = None;
                for existing in messages.iter_mut() {
                    if existing.finish_tool_step(
                        &id,
                        output.clone(),
                        structured.clone(),
                        duration_ms,
                    ) {
                        if let Some(status) = existing.tool_step_status() {
                            let default = crate::step_interaction::default_tool_expanded(
                                status,
                                &name,
                                &tui_config,
                            );
                            existing.set_tool_step_expanded(default);
                        }
                        finished = Some(existing.id);
                        break;
                    }
                }
                match finished {
                    Some(id) => post = PostEdit::Invalidate(id),
                    None => {
                        if let Some(mut message) = fallback {
                            if let Some(status) = message.tool_step_status() {
                                let default = crate::step_interaction::default_tool_expanded(
                                    status,
                                    &name,
                                    &tui_config,
                                );
                                message.set_tool_step_expanded(default);
                            }
                            messages.push(message);
                        }
                    }
                }
            }
            TranscriptEdit::ToolCancel { id, fallback } => {
                let mut cancelled_id: Option<u64> = None;
                for message in messages.iter_mut() {
                    if message.cancel_tool_step(&id) {
                        message.set_tool_step_expanded(false);
                        cancelled_id = Some(message.id);
                        break;
                    }
                }
                match cancelled_id {
                    Some(id) => post = PostEdit::Invalidate(id),
                    None => {
                        if let Some(message) = fallback {
                            messages.push(message);
                        }
                    }
                }
            }
            TranscriptEdit::ToolStream { id, stream } => {
                let _ = messages
                    .iter_mut()
                    .any(|message| message.push_tool_stream(&id, &stream));
            }
            TranscriptEdit::SubagentEvent {
                parent_call_id,
                event,
            } => {
                let _ = messages
                    .iter_mut()
                    .find(|message| message.tool_step_call_id() == Some(parent_call_id.as_str()))
                    .is_some_and(|message| message.push_subagent_event(&event));
            }
            TranscriptEdit::SettleInserted {
                insert_id,
                origin,
                sent_at_ms,
                fallback,
            } => {
                let mut settled = false;
                if let Some(entry) = messages
                    .iter_mut()
                    .rev()
                    .find(|m| m.insert_id.as_deref() == Some(insert_id.as_str()))
                {
                    entry.delivery = crate::model::document::DeliveryStatus::Delivered;
                    entry.origin = origin;
                    if entry.sent_at_ms.is_none() {
                        entry.sent_at_ms = sent_at_ms;
                    }
                    settled = true;
                }
                if !settled && let Some(message) = fallback {
                    messages.push(message);
                }
            }
            TranscriptEdit::HoldInserted { insert_id } => {
                if let Some(entry) = messages
                    .iter_mut()
                    .rev()
                    .find(|m| m.insert_id.as_deref() == Some(insert_id.as_str()))
                {
                    entry.hold_pending_round();
                }
            }
            TranscriptEdit::SettleCommandResult {
                invocation,
                result,
                fallback,
            } => {
                let invocation = invocation.trim().to_string();
                let mut settled_id: Option<u64> = None;
                if let Some(message) = messages.iter_mut().rev().find(|message| {
                    message.is_command_result()
                        && message.raw == invocation
                        && message.command_result_phase()
                            == Some(crate::model::document::CommandPhase::Pending)
                }) {
                    message.settle_command_result(result.clone());
                    settled_id = Some(message.id);
                }
                match settled_id {
                    Some(id) => post = PostEdit::Invalidate(id),
                    None => {
                        if let Some(message) = fallback {
                            messages.push(message);
                        }
                    }
                }
            }
            TranscriptEdit::Interrupted { record } => {
                let at_ms = record.at_ms;
                for m in messages.iter_mut().filter(|m| m.is_provider_retry()) {
                    m.settle_interrupted_provider_retry();
                    post = PostEdit::Invalidate(m.id);
                }
                if let Some(user_msg) = messages.iter_mut().rev().find(|m| {
                    m.role == Role::User
                        && (m.is_sending()
                            || (record.round.is_some() && m.round == record.round)
                            || (record.round.is_none() && m.round.is_none()))
                }) {
                    if user_msg.round.is_none() && record.round.is_some() {
                        user_msg.round = record.round;
                    }
                    user_msg.cancel_prompt();
                }
                messages.push(TranscriptMessage::round_interrupted(record).with_sent_at_ms(at_ms));
            }
            TranscriptEdit::CancelLastUserPrompt => {
                if let Some(user_msg) = messages.iter_mut().rev().find(|m| m.role == Role::User) {
                    user_msg.cancel_prompt();
                }
            }
            TranscriptEdit::RetainNotRetry => {
                messages.retain(|m| !m.is_provider_retry());
            }
            TranscriptEdit::UpsertRetry {
                attempt,
                max_attempts,
                retry_at,
                failure,
                fallback,
            } => match messages.last_mut().filter(|m| m.is_provider_retry()) {
                Some(last) => {
                    last.update_provider_retry(attempt, max_attempts, retry_at, failure.clone());
                    let id = last.id;
                    post = PostEdit::Invalidate(id);
                }
                None => messages.push(fallback),
            },
            TranscriptEdit::Append { message } => {
                messages.push(message);
            }
            TranscriptEdit::FinalizeOrphanedReasoning { duration_ms } => {
                // Freeze every still-streaming trace (its round ended
                // mid-trace; the spinner would breathe forever). Streaming
                // traces carry no height-cache entries — no invalidation.
                crate::finalize_streaming_reasoning(messages, duration_ms);
            }
            TranscriptEdit::CancelPendingCommands => {
                for message in messages.iter_mut() {
                    message.cancel_pending_command();
                }
            }
            TranscriptEdit::RebaseRounds { round_counter } => {
                crate::rebase_transcript_rounds(messages, round_counter);
            }
            TranscriptEdit::StampTurnPrompt { round } => {
                // The composer cannot know the authoritative round until
                // admission; stamp its latest unpositioned driving prompt.
                if let Some(prompt) = messages.iter_mut().rev().find(|message| {
                    message.role == Role::User
                        && (message.origin == crate::model::document::UserMessageOrigin::Chat
                            || message.origin
                                == crate::model::document::UserMessageOrigin::FollowUp)
                        && (message.round.is_none() || message.is_sending())
                }) {
                    prompt.round = Some(round);
                    prompt.settle_delivered();
                }
            }
            TranscriptEdit::ReplaceAll { messages: rebuilt } => {
                *messages = rebuilt;
                post = PostEdit::ClearCache;
            }
            TranscriptEdit::Clear => {
                messages.clear();
                post = PostEdit::ClearCache;
            }
        }
    }

    match post {
        PostEdit::None => {}
        PostEdit::Invalidate(id) => {
            app.layout_height_cache
                .invalidate_messages(std::iter::once(id));
        }
        PostEdit::ClearCache => app.layout_height_cache.clear(),
    }
    if changed {
        match buffer {
            Buffer::Primary => app.transcript_changed_pending = true,
            Buffer::Side => app.side_transcript_changed_pending = true,
        }
    }
    changed
}

/// Execute one view-scoped chrome edit against `App::session_chrome`
/// (formerly `ChromeUpdate` closures over a shared map).
fn apply_chrome(app: &mut App, session_id: &str, edit: ChromeEdit) {
    let chrome = app
        .session_chrome
        .entry(session_id.to_string())
        .or_default();
    match edit {
        ChromeEdit::RoundLifecycle {
            round_count,
            running,
            can_retry,
        } => {
            chrome.round_count = round_count;
            chrome.responding = running;
            chrome.can_retry = can_retry;
            if running {
                chrome.current_turn = 0;
                if chrome.round_started_at.is_none() {
                    chrome.round_started_at = Some(std::time::Instant::now());
                }
                if chrome.phase.is_none() {
                    chrome.set_phase(Some(Phase::Preparing));
                }
            } else {
                chrome.set_phase(None);
                chrome.current_turn = 0;
                chrome.round_started_at = None;
            }
        }
        ChromeEdit::ActivityFolded(phase) => {
            chrome.set_phase(Some(phase));
            chrome.responding = true;
        }
        ChromeEdit::PhaseOnly(phase) => {
            chrome.set_phase(phase);
        }
        ChromeEdit::StreamStarted => {
            chrome.responding = true;
            if chrome.round_started_at.is_none() {
                chrome.round_started_at = Some(std::time::Instant::now());
            }
            if !matches!(chrome.phase, Some(Phase::Reasoning | Phase::Answering)) {
                chrome.set_phase(Some(Phase::Answering));
            }
        }
        ChromeEdit::TurnStarted { round, turn } => {
            chrome.round_count = round;
            chrome.current_turn = turn;
            chrome.set_phase(Some(Phase::AwaitingModel));
        }
        ChromeEdit::RoundEnded => {
            chrome.set_phase(None);
            chrome.responding = false;
            chrome.current_turn = 0;
            chrome.round_started_at = None;
        }
        ChromeEdit::TurnPerformance(performance) => {
            chrome.last_turn_performance = Some(*performance);
        }
        ChromeEdit::TransportSetback(setback) => {
            // Publish-only: the clause is retired by this session's next phase
            // write (`SessionChrome::set_phase`), never by another mutation
            // (ADR-0235).
            chrome.transport_setback = Some(*setback);
        }
    }
}
