//! The mutation applier (M1): the **sole writer** of `App`.
//!
//! Every [`AppMutation`] produced by the response translator (and the
//! monitor client) lands here. The applier is deliberately mechanical: the
//! translator pre-builds payloads (attribution, effort, positions, fallback
//! messages, disclosure defaults); the applier executes the edit against
//! `App` and maintains the two bookkeeping facts the render loop consumes —
//! the height-cache invalidation and the "transcript changed" flag that
//! drives bottom-follow scrolling.
use nuo_wire::Role;

use crate::shell::app::App;
use crate::shell::event_loop::mutations::{AppMutation, ChromeEdit, TranscriptEdit};
use crate::shell::event_loop::runtime::UiRuntime;
use nuo_view::model::document::{MessageKind, TranscriptMessage};
use nuo_view::phase::Phase;

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
    messages: &mut [nuo_view::model::document::TranscriptMessage],
    slot: usize,
) -> Option<&mut nuo_view::model::document::TranscriptMessage> {
    messages
        .iter_mut()
        .rfind(|message| message.is_announced_pending_tool_step_for_slot(slot))
}

/// Apply one mutation. Returns whether anything changed that a frame must
/// observe (the loop ORs this into its dirty computation).
pub(crate) fn apply(app: &mut App, runtime: &UiRuntime, mutation: AppMutation) -> bool {
    match mutation {
        AppMutation::Transcript { edit } => apply_transcript(app, edit),
        AppMutation::ChromeEdit { session_id, edit } => {
            let is_phase_only = matches!(edit, ChromeEdit::PhaseOnly(_));
            apply_chrome(app, &session_id, edit);
            // The round-boundary chrome edit is also the live-session fact:
            // the queue bar's running rows and the keymap's has-running-task
            // guard read `App::running_sessions` (M4: formerly the
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
                app.current_workspace = crate::shell::chrome::tilde_home(std::path::Path::new(&ws));
            } else if app.current_role.is_some() {
                app.current_workspace.clear();
            }
            // PreAttach unmount: when the snapshot transitions
            // to having no unresolved quarantined/changed/expired domains (or the
            // workspace is no longer quarantined), clear the interstitial and latch
            // the per-run gate so a subsequent periodic republish cannot re-mount
            // within this run.
            if let Some(pa) = app.pre_attach.as_mut() {
                let gate_needed =
                    nuo_view::trust_gate::gate_request(&snapshot.workspace_security).is_some();
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
                        crate::shell::PreAttachState::from_snapshot(&snapshot.workspace_security)
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
        AppMutation::LiveSession {
            previous_id,
            next_id,
        } => {
            app.live_session_id = next_id.clone();
            // `[INV-TAB-13]`: address the tab by the session it is *leaving*.
            // Rebasing "the active tab" is the ghost-tab generator — the active
            // tab may already be a replacement that has nothing to do with this
            // switch. No matching tab means this receipt is for a session the
            // client no longer shows, so there is nothing to rebase.
            if app.surfaces.rebase_thread_tab(&previous_id, &next_id) {
                // Only the rebased tab loses its title: the new session has no
                // authentic title yet (ADR-0050). Titles of untouched tabs are
                // never collateral damage.
                app.surfaces.clear_tab_title(&next_id);
            }
            true
        }
        AppMutation::SessionForked {
            child_id,
            parent_id,
            fork_point,
            prompt,
        } => {
            // The tab is labelled with the address the branch diverged at
            // (ADR-0120): `⑂r2.1`. The fork no longer mints a title, so this
            // label is the only textual identity it has until the branch earns
            // its own (ADR-0050) or is identified by its first prompt. A peer
            // that predates the fork point still identifies the origin by the
            // parent's short id, so a branch never appears to have no origin.
            let label = Some(match fork_point {
                Some(point) => format!("\u{2442}{point}"),
                None => format!("\u{2442}{}", parent_id.chars().take(8).collect::<String>()),
            });
            let tab = crate::shell::surfaces::ClientTab::thread(&child_id, label);
            // Checkpoint the departing (parent) tab's context before mounting
            // the child — the same discipline every other session switch uses.
            app.open_tab(tab);
            app.switch_to_target = Some(child_id);
            // `[INV-DEPR-01]`: a retired `/btw <prompt>` steer rides its message
            // here, dispatched into the child once the runner re-attaches.
            app.switch_to_prompt = prompt;
            app.should_quit
                .store(true, std::sync::atomic::Ordering::SeqCst);
            true
        }

        AppMutation::SetPhase(phase) => {
            // The primary's half of the setback-clause lifetime: this mirror is
            // written by every primary phase move, so retiring the clause here
            // is what makes "the clause ends with its phase" true by
            // construction.
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
            // Steer is ephemeral to the targeted round. If admission
            // missed the round window, purge the optimistic entry from transcript
            // and restore text to the composer draft — never re-queue as follow-up.
            let mut text_to_restore = None;
            if let Some(pos) = app.messages.iter().rposition(|m| {
                m.insert_id.as_deref() == Some(input_id.as_str()) && m.role == Role::User
            }) {
                let msg = app.messages.remove(pos);
                text_to_restore = Some(msg.raw);
            }
            if let Some(text) = text_to_restore
                && app.live_session_id == session_id
            {
                app.adopt_as_draft(
                    text,
                    Vec::new(),
                    Vec::new(),
                    crate::shell::app::DraftAdoption::OnlyIfIdle,
                );
                app.notice_toast_severity = nuo_view::model::document::NoticeSeverity::Info;
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
                item.state = crate::shell::app::QueuedDispatchState::Waiting;
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
            let optimistic: Vec<crate::shell::app::QueuedDispatch> = app
                .pending_dispatch
                .iter()
                .filter(|item| {
                    item.session_id == session_id
                        && item.state == crate::shell::app::QueuedDispatchState::Dispatching
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
                app.pending_dispatch
                    .push_back(crate::shell::app::QueuedDispatch {
                        id: queued.id,
                        session_id: session_id.clone(),
                        state: crate::shell::app::QueuedDispatchState::Waiting,
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
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                .refreshing = false;
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                .refreshing = false;
            true
        }
        AppMutation::SessionsOverview(sessions) => {
            for s in &sessions {
                if let Some(ref t) = s.title {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        app.surfaces.update_tab_title(&s.id, trimmed);
                    }
                }
            }
            // The current thread stays in the snapshot (ADR-0120). The old
            // self-exclusion existed so the cursor landed on an *alternative*
            // thread, but it also deleted every branch forked out of the
            // attached thread: their `parent_id` named a row that was no longer
            // in the list, so the projection could not place them and promoted
            // them to unrelated roots — the exact relation the picker was
            // opened to inspect. A browser answers "where am I" as well as
            // "where can I go"; the attached row is marked current, and
            // selecting it is the idempotent focus-recovery path that already
            // existed for a re-selected session.
            app.modal_index = app.modal_index.min(sessions.len().saturating_sub(1));
            app.sessions_overview = sessions;
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                .loading = false;
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
        AppMutation::SessionDetail(detail) => {
            if let Some(ref t) = detail.title {
                let trimmed = t.trim();
                if !trimmed.is_empty() {
                    app.surfaces.update_tab_title(&detail.id, trimmed);
                }
            }
            let same_id = app.session_detail.as_ref().map(|s| &s.id) == Some(&detail.id);
            app.session_detail = Some(detail);
            if !same_id {
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                    .info_scroll = 0;
            }
            true
        }
        AppMutation::ConnectionDetail(detail) => {
            let same_id = app.connection_detail.as_ref().map(|c| &c.name) == Some(&detail.name);
            app.connection_detail = Some(detail);
            if !same_id {
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_scroll = 0;
            }
            true
        }
        AppMutation::TokenReport(report) => {
            app.token_report = report;
            true
        }
        AppMutation::UsageStats(report) => {
            app.usage_stats = Some(report);
            app.usage_loading = false;
            true
        }
        AppMutation::UsageFailed(_error) => {
            app.usage_loading = false;
            true
        }
        AppMutation::ProviderQuotas(snapshot) => {
            for entry in &snapshot.entries {
                app.connection_usages
                    .insert(entry.name.clone(), entry.state.clone());
            }
            app.provider_quotas = Some(snapshot);
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
        AppMutation::NoticeDisclosure {
            severity,
            topic,
            text,
            detail,
            at_ms,
        } => {
            // Residency is decided here, once, for every producer: the notice
            // is retained first (the log is the home), and only a fault
            // additionally raises a bubble. A producer therefore cannot show a
            // bubble without leaving a record — the defect that made a missed
            // two-second toast unrecoverable.
            app.record_notice_with_detail(severity, topic, text, detail, at_ms);
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
        AppMutation::ServerEndpoints(report) => {
            let d = app
                .surfaces
                .dlg_mut::<crate::shell::surfaces::PortShareDialog>();
            d.port = report.tcp_port;
            d.active = report.tcp_active;
            d.token = report.token;
            if let Some(endpoint) = report.local_endpoint {
                d.local_path = endpoint;
            }
            d.loading = false;
            true
        }
        AppMutation::PreAttach(signal) => {
            let dismissed = runtime
                .trust_gate_dismissed
                .load(std::sync::atomic::Ordering::SeqCst);
            if !dismissed
                && let Some(state) = crate::shell::PreAttachState::from_snapshot(&signal.snapshot)
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
        AppMutation::PreAttachAttestationError(message) => {
            // Gate on the surface being mounted: with no gate up, a trust
            // notice is ordinary session flow (in-session `/trust`, the
            // attach banner) and must keep its transcript/toast path.
            if let Some(pa) = app.pre_attach.as_mut() {
                tracing::warn!(
                    %message,
                    "nuo: PreAttach trust grant failed; resuming the decision surface"
                );
                pa.note_attestation_error(message);
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
            for row in &rows {
                if let Some(ref t) = row.title {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        app.surfaces.update_tab_title(&row.id, trimmed);
                    }
                }
            }
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
fn apply_oauth(app: &mut App, signal: crate::shell::app::OauthAddSignal) {
    use crate::shell::app::OauthAddSignal as Signal;
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
                if !app
                    .surfaces
                    .contains_sheet(crate::shell::surfaces::SheetKind::OAuthPending)
                {
                    app.surfaces
                        .present_sheet(crate::shell::surfaces::SheetKind::OAuthPending);
                }
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
                if !app
                    .surfaces
                    .contains_sheet(crate::shell::surfaces::SheetKind::OAuthPending)
                {
                    app.surfaces
                        .present_sheet(crate::shell::surfaces::SheetKind::OAuthPending);
                }
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
fn apply_transcript(app: &mut App, edit: TranscriptEdit) -> bool {
    let mut post = PostEdit::None;
    let mut changed = true;
    // A command name the dispatcher did not recognize. Its draft was consumed by
    // the Enter that submitted it, and nothing was sent to the model, so the
    // text is handed back to the composer after the document borrow ends (see
    // the restore below) — a typo must cost one edit, not a retype.
    let mut rejected_invocation: Option<String> = None;
    // Snapshot the disclosure input the tool arms need before the document borrow.
    let tui_config = app.tui_config.clone();
    // The turn position and send time this edit's payload *would* join, captured
    // before the document borrow. A freshly created component that arrives
    // unstamped adopts its turn's time ([`TranscriptMessage::adopt_sent_at_ms`]),
    // so a header can never lose its only temporal fact to a forgotten builder
    // call — the invariant is repaired here rather than trusted at every arm.
    let turn_time_adoption =
        |messages: &[TranscriptMessage], round: Option<u64>, turn: Option<u64>| -> Option<u64> {
            let (round, turn) = (round?, turn?);
            messages
                .iter()
                .rev()
                .filter(|message| message.turn_position() == Some((round, turn)))
                .find_map(|message| message.sent_at_ms)
        };
    {
        let messages = &mut app.messages;
        match edit {
            TranscriptEdit::BeginStream => {
                crate::shell::begin_stream(messages);
            }
            TranscriptEdit::StreamTextDelta {
                round,
                turn,
                delta,
                created,
                clear_retry,
            } => {
                let _ = clear_retry; // the translator bundles the retry reset as its own mutation
                match crate::shell::append_stream_text_delta(messages, round, turn, &delta) {
                    Some(id) => post = PostEdit::Invalidate(id),
                    None => {
                        if let Some(mut message) = created {
                            message.adopt_sent_at_ms(turn_time_adoption(messages, round, turn));
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
                        if let Some(mut message) = created {
                            message.adopt_sent_at_ms(turn_time_adoption(messages, round, turn));
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
                match crate::shell::append_reasoning_delta(messages, round, turn, &delta) {
                    Some(_) => {}
                    None => {
                        if let Some(mut message) = created {
                            message.adopt_sent_at_ms(turn_time_adoption(messages, round, turn));
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
                let mut message = message;
                let (round, turn) = (message.round, message.turn);
                message.adopt_sent_at_ms(turn_time_adoption(messages, round, turn));
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
                let (round, turn) = (message.round, message.turn);
                message.adopt_sent_at_ms(turn_time_adoption(messages, round, turn));
                if let Some(status) = message.tool_step_status() {
                    let default = crate::shell::step_interaction::default_tool_expanded(
                        status,
                        &name,
                        &tui_config,
                    );
                    message.set_tool_step_expanded(default);
                }
                messages.push(message);
            }
            TranscriptEdit::ToolCallCollapse {
                slot,
                call_id,
                arguments,
            } => {
                if let Some(step) = find_pending_announced_tool_step_mut(messages, slot) {
                    // Re-key onto the dispatch id, store completed arguments,
                    // and clear the pre-dispatch slot/byte state in one transition.
                    step.collapse_tool_call(call_id, arguments);
                    post = PostEdit::Invalidate(step.id);
                }
            }
            TranscriptEdit::ToolPreview { id, preview } => {
                // Match by dispatch call id: the up-front `ToolCall` events have
                // already created (or collapsed) the step this describes.
                for message in messages.iter_mut() {
                    if message.set_tool_preview(&id, preview.clone()) {
                        post = PostEdit::Invalidate(message.id);
                        break;
                    }
                }
            }
            TranscriptEdit::ToolInputProgress {
                slot,
                bytes,
                arguments,
            } => {
                if let Some(step) = find_pending_announced_tool_step_mut(messages, slot) {
                    let id = step.tool_step_call_id().unwrap_or_default().to_string();
                    let mut changed = step.set_tool_input_bytes(&id, bytes);
                    if let Some(args) = arguments {
                        if step.set_tool_streaming_arguments(&id, args) {
                            changed = true;
                        }
                    }
                    if changed {
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
                            let default = crate::shell::step_interaction::default_tool_expanded(
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
                                let default = crate::shell::step_interaction::default_tool_expanded(
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
                    entry.delivery = nuo_view::model::document::DeliveryStatus::Delivered;
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
                // A rejected command name is the one command result that gives
                // the user something back: nothing was dispatched, so the draft
                // is still theirs. Hold it for the post-borrow restore below.
                if let nuo_wire::CommandResult::UnknownCommand { .. } = &result {
                    rejected_invocation = Some(invocation.clone());
                }
                let mut settled_id: Option<u64> = None;
                if let Some(message) = messages.iter_mut().rev().find(|message| {
                    message.is_command_result()
                        && message.raw == invocation
                        && message.command_result_phase()
                            == Some(nuo_view::model::document::CommandPhase::Pending)
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
                // The interrupt used to *rewrite* the trailing provider-retry
                // entry into a permanent notice here — a second responsibility
                // hidden inside a variant named `Interrupted`, bypassing the
                // `Append` admission gate and turning a transport payload into
                // a conversation message. The retry owns no transcript row any
                // more, so there is nothing to rewrite: the round-interrupt
                // anchor below is the single record of the stop, and the last
                // fault rides its one-line detail (the producer puts it there).
                let at_ms = record.at_ms;
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
            TranscriptEdit::RetrySettled { recovered } => {
                // The round's retry episode is over. If it recovered, admit the
                // anchor through the same gate as any other turn outcome; if
                // not, there is nothing to say — the interrupt or error anchor
                // already owns the record. Either way the live countdown is
                // chrome and is retired by the phase write, not here.
                if let Some(message) = recovered {
                    messages.push(message);
                }
            }
            TranscriptEdit::Append { class, message } => {
                // The single admission gate. A row whose class is not dialogue
                // or turn structure is refused here rather than silently
                // becoming part of the conversation: operations belong to the
                // durable ledger, telemetry to chrome, and receipts/notices to
                // a surface layer (`nuo_view::model::layer`).
                if class.admits_dialogue() || class.admits_anchor() {
                    messages.push(message);
                } else {
                    tracing::debug!(
                        ?class,
                        "refused transcript admission for a non-dialogue event class"
                    );
                }
            }
            TranscriptEdit::FinalizeOrphanedReasoning { duration_ms } => {
                // Freeze every still-streaming trace (its round ended
                // mid-trace; the spinner would breathe forever). Streaming
                // traces carry no height-cache entries — no invalidation.
                crate::shell::finalize_streaming_reasoning(messages, duration_ms);
            }
            TranscriptEdit::CancelPendingCommands => {
                for message in messages.iter_mut() {
                    message.cancel_pending_command();
                }
            }
            TranscriptEdit::RebaseRounds { round_counter } => {
                crate::shell::rebase_transcript_rounds(messages, round_counter);
            }
            TranscriptEdit::StampTurnPrompt { round } => {
                // The composer cannot know the authoritative round until
                // admission; stamp its latest unpositioned driving prompt.
                if let Some(prompt) = messages.iter_mut().rev().find(|message| {
                    message.role == Role::User
                        && (message.origin == nuo_view::model::document::UserMessageOrigin::Chat
                            || message.origin
                                == nuo_view::model::document::UserMessageOrigin::FollowUp)
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
    if let Some(rejected) = rejected_invocation {
        restore_rejected_command_draft(app, rejected);
    }
    if changed {
        app.transcript_changed_pending = true;
    }
    changed
}

/// Hand a rejected slash-command draft back to the composer.
///
/// The Enter that submitted `/modles` consumed the buffer, and the dispatcher
/// answered `UnknownCommand`: nothing ran and nothing reached the model. Leaving
/// the box empty would make a one-letter typo cost a full retype, which is the
/// exact friction this path exists to remove.
///
/// Two rules keep the restore from fighting the user:
///
/// - Only an **empty** composer is filled. If the user has started typing the
///   next thing (or recalled a history row, or a modal borrowed the line), their
///   text wins and the rejected invocation is simply dropped — the transcript
///   row still shows exactly what was rejected.
/// - The caret lands at the end, ready for `Backspace`, and the completion
///   dismissal is latched exactly as a programmatic replacement would do, so the
///   popup does not flash open over the restored text.
fn restore_rejected_command_draft(app: &mut App, invocation: String) {
    if !app.input.is_empty() {
        return;
    }
    app.input = invocation;
    app.set_cursor_end();
    app.suggestion_index = None;
    app.completion_dismissed = true;
    app.refresh_backend_completion_request();
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
    }
}

#[cfg(test)]
mod rejected_command_draft_tests {
    //! A mistyped `/command` must cost one edit, not a retype.
    //!
    //! The Enter that submits a command consumes the composer buffer, and the
    //! dispatcher answers an unrecognized name with
    //! `CommandResult::UnknownCommand`. Nothing ran and nothing reached the
    //! model, so the text still belongs to the user — the applier hands it back.
    use super::*;
    use crate::shell::tests::new_app_for_relay_tests;

    /// `[INV-CMD-03]` — a rejected command hands its draft back.
    #[test]
    fn rejected_command_returns_the_draft_to_the_composer() {
        let mut app = new_app_for_relay_tests();
        // The Enter already consumed the buffer, exactly as it would live.
        assert!(app.input.is_empty());
        restore_rejected_command_draft(&mut app, "/modles".to_string());
        assert_eq!(app.input, "/modles");
        assert_eq!(
            app.cursor_position,
            app.input.len(),
            "caret is retype-ready"
        );
        assert!(
            app.completion_dismissed,
            "the popup must not flash over a restored draft"
        );
    }

    #[test]
    fn rejected_command_never_clobbers_text_the_user_already_typed() {
        let mut app = new_app_for_relay_tests();
        app.input = "next question".to_string();
        app.cursor_position = app.input.len();
        restore_rejected_command_draft(&mut app, "/modles".to_string());
        assert_eq!(
            app.input, "next question",
            "the user's newer draft wins; the rejection stays in the transcript and in ↑ history"
        );
    }
}
