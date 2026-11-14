//! The mutation applier (M1): the **sole writer** of `App`.
//!
//! Every [`AppMutation`] produced by the response translator (and the
//! monitor client) lands here. The applier is deliberately mechanical: the
//! translator pre-builds payloads (attribution, effort, positions, fallback
//! messages, disclosure defaults); the applier executes the edit against
//! `App` and maintains the two bookkeeping facts the render loop consumes —
//! the height-cache invalidation and the "transcript changed" flag that
//! drives bottom-follow scrolling.
use nuo_tool::Role;

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

/// ADR-0166: the **only** tool-step resolver. A call is addressed by its call
/// id; an id identifies one row, so there is no tie-break to get wrong.
///
/// `rfind` is the conservative spelling of that fact: if a duplicate ever
/// reached the buffer by another route, the live row is the most recent one.
fn find_tool_step_mut<'a>(
    messages: &'a mut [TranscriptMessage],
    call_id: &str,
) -> Option<&'a mut TranscriptMessage> {
    messages
        .iter_mut()
        .rfind(|message| message.tool_step_call_id() == Some(call_id))
}

/// Apply one mutation. Returns whether anything changed that a frame must
/// observe (the loop ORs this into its dirty computation).
pub(crate) fn apply(app: &mut App, runtime: &UiRuntime, mutation: AppMutation) -> bool {
    match mutation {
        AppMutation::Transcript { edit } => apply_transcript(app, edit),

        // ---- Chrome / round lifecycle -------------------------------------
        AppMutation::ChromeEdit { session_id, edit } => {
            apply_chrome_mutation(app, &session_id, edit)
        }
        AppMutation::SetPhase(phase) => apply_phase(app, phase),
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

        // ---- Session / thread lifecycle -----------------------------------
        AppMutation::Harness(snapshot) => apply_harness_snapshot(app, runtime, snapshot),
        AppMutation::SessionPhase(phase) => apply_session_phase(app, phase),
        AppMutation::CommandCatalog(catalog) => {
            app.command_catalog = catalog;
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
        } => apply_live_session(app, previous_id, next_id),
        AppMutation::SessionsOverview(sessions) => apply_sessions_overview(app, sessions),
        AppMutation::OpenSessionsPanel => {
            app.open_sessions_signal = true;
            true
        }
        AppMutation::OpenTreePanel => {
            app.open_tree_signal = true;
            true
        }
        AppMutation::SessionDetail(detail) => apply_session_detail(app, detail),
        AppMutation::ConnectionDetail(detail) => apply_connection_detail(app, detail),
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
        AppMutation::HostSessions(rows) => apply_host_sessions(app, rows),

        // ---- Permission / question / human-in-the-loop --------------------
        AppMutation::QueuePermission {
            request,
            parent_call_id,
        } => apply_queue_permission(app, request, parent_call_id),
        AppMutation::QueueQuestion {
            request,
            parent_call_id,
        } => apply_queue_question(app, request, parent_call_id),
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

        // ---- Dispatch queue / steers --------------------------------------
        AppMutation::DispatchRemoved {
            session_id,
            input_id,
        } => apply_dispatch_removed(app, session_id, input_id),
        AppMutation::SteerMissed {
            session_id,
            input_id,
        } => apply_steer_missed(app, session_id, input_id),
        AppMutation::SteerCancelledRestored {
            session_id,
            input_id,
        } => apply_steer_cancelled_restored(app, session_id, input_id),
        AppMutation::SteerCancelFailedCleared {
            session_id,
            input_id,
        } => apply_steer_cancel_failed_cleared(app, session_id, input_id),
        AppMutation::DispatchQueued {
            session_id,
            input_id,
        } => apply_dispatch_queued(app, session_id, input_id),
        AppMutation::QueueSnapshot {
            session_id,
            items,
            paused,
        } => apply_queue_snapshot(app, session_id, items, paused),

        // ---- Provider / config --------------------------------------------
        AppMutation::InputHistory(rows) => apply_input_history(app, rows),
        AppMutation::RouteSettings {
            provider_id,
            model,
            overrides,
        } => apply_route_settings(app, provider_id, model, overrides),
        AppMutation::KeyStatus(status) => {
            app.key_status = status;
            true
        }
        AppMutation::ProviderPicker(snapshot) => apply_provider_picker(app, snapshot),
        AppMutation::ProviderSwitched { provider, model } => {
            app.current_provider = provider;
            app.current_model = model;
            true
        }
        AppMutation::WebSearchConfig(snapshot) => {
            app.websearch_config = snapshot;
            true
        }
        AppMutation::ServerEndpoints(report) => apply_server_endpoints(app, report),

        // ---- Overlay / dialog / trust -------------------------------------
        AppMutation::Oauth(signal) => {
            apply_oauth(app, signal);
            true
        }
        AppMutation::PreAttach(signal) => apply_pre_attach(app, runtime, signal),
        AppMutation::PreAttachAttestationError(message) => {
            apply_pre_attach_attestation_error(app, message)
        }

        // ---- Telemetry / usage --------------------------------------------
        AppMutation::TokenReport(report) => {
            app.token_report = report;
            true
        }
        AppMutation::UsageStats(report) => apply_usage_stats(app, report),
        AppMutation::UsageFailed(_error) => {
            app.usage_loading = false;
            true
        }
        AppMutation::ProviderQuotas(snapshot) => apply_provider_quotas(app, snapshot),
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

        // ---- Notices / link / process -------------------------------------
        AppMutation::NoticeDisclosure(rendered) => apply_notice_disclosure(app, rendered),
        AppMutation::Quit => {
            app.should_quit
                .store(true, std::sync::atomic::Ordering::SeqCst);
            true
        }
        AppMutation::LinkLost { reason } => apply_link_lost(app, reason),
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

/// Apply a chrome edit and mirror the resulting running state into the
/// session-keyed bookkeeping the keymap and queue bar consume.
fn apply_chrome_mutation(app: &mut App, session_id: &str, edit: ChromeEdit) -> bool {
    let is_phase_only = matches!(edit, ChromeEdit::PhaseOnly(_));
    apply_chrome(app, session_id, edit);
    // The round-boundary chrome edit is also the live-session fact:
    // the queue bar's running rows and the keymap's has-running-task
    // guard read `App::running_sessions` (M4: formerly the
    // deleted dispatch `HarnessState` signal).
    if let Some(chrome) = app.session_chrome.get(session_id) {
        let running = chrome.responding;
        if running {
            app.running_sessions.insert(session_id.to_string());
        } else if !is_phase_only {
            app.running_sessions.remove(session_id);
        }
    }
    true
}

/// Fold a harness snapshot into the loop-status and trust-gate projection.
fn apply_harness_snapshot(
    app: &mut App,
    runtime: &UiRuntime,
    snapshot: nuo_protocol::HarnessSnapshot,
) -> bool {
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
            tracing::info!("nuo: clearing PreAttach interstitial (workspace review resolved)");
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

/// Apply a server-attested readiness transition, flushing queued prompts and
/// disclosing a post-Welcome failure as a notice rather than a death.
fn apply_session_phase(app: &mut App, phase: nuo_protocol::ThreadPhase) -> bool {
    let failed = match &phase {
        nuo_protocol::ThreadPhase::Failed { message, .. } => Some(message.clone()),
        _ => None,
    };
    let ready = phase == nuo_protocol::ThreadPhase::Ready;
    app.session_phase = phase;
    if ready {
        app.switching_session = None;
        let queued: Vec<_> = std::mem::take(&mut app.pending_prompts);
        for request in queued {
            let _ = app.send_intent(request);
        }
        // A snapshot is an event, so a client that attaches to an
        // already-live session never saw the queue changes that
        // produced the driver's current queue: without this read the
        // projection stays empty while items sit queued and ship at the
        // next boundary — the user is shown "nothing pending" and then
        // gets prompt they did not see coming.
        let _ = app.send_intent(nuo_protocol::AgentRequest::QueueSnapshot {
            thread_id: app.current_session_id.clone(),
        });
    }
    if let Some(message) = failed {
        // A post-Welcome failure is a disclosure, not a death
        // (ADR-0133, extending `[INV-ATTACH-02]`).
        app.record_client_notice(
            crate::shell::app::notice_catalog::ClientNotice::SessionPrepFailed { detail: message },
        );
        app.switching_session = None;
    }
    true
}

/// Record the live session move and rebase only the tab carrying the session
/// being left (ADR-0123 `[INV-TAB-13]`).
fn apply_live_session(app: &mut App, previous_id: String, next_id: String) -> bool {
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

/// Mirror the primary phase, retiring the provider-retry clause with it.
fn apply_phase(app: &mut App, phase: Option<Phase>) -> bool {
    // The primary's half of the setback-clause lifetime: this mirror is
    // written by every primary phase move, so retiring the clause here
    // is what makes "the clause ends with its phase" true by
    // construction.
    app.set_phase(phase);
    true
}

fn apply_queue_permission(
    app: &mut App,
    request: nuo_tool::PermissionRequest,
    parent_call_id: Option<String>,
) -> bool {
    if let Some(parent) = parent_call_id {
        app.subagent_permission_parent
            .insert(request.id.clone(), parent);
    }
    app.pending_permissions.push_back(request);
    true
}

fn apply_queue_question(
    app: &mut App,
    request: nuo_tool::UserQuestionRequest,
    parent_call_id: Option<String>,
) -> bool {
    if let Some(parent) = parent_call_id {
        app.subagent_question_parent
            .insert(request.id.clone(), parent);
    }
    app.pending_questions.push_back(request);
    true
}

/// The single terminal path for an outbox item.
///
/// Clearing the deletion-intent flag here (rather than in each fate arm) is
/// what makes the flag structurally unable to outlive its item: whichever
/// event ends this id — admitted, missed, cancelled, cancel-failed — the row
/// leaves through this mutation, so the flag leaves with it.
fn apply_dispatch_removed(app: &mut App, session_id: String, input_id: String) -> bool {
    app.suppress_steer_restore
        .remove(&(session_id.clone(), input_id.clone()));
    app.remove_dispatch(&session_id, &input_id);
    true
}

/// A steer missed the targeted round: restore its row to the composer draft,
/// never re-queue it as follow-up (ADR-0137).
fn apply_steer_missed(app: &mut App, session_id: String, input_id: String) -> bool {
    // Steer is ephemeral to the targeted round. If admission
    // missed the round window, the outbox `Steering` row is removed
    // and the text is restored to the composer draft — never
    // re-queued as follow-up (ADR-0137). There is no transcript
    // entry to purge: busy-Enter steers are never staged in the
    // thread until `SteerAdmitted`.
    //
    // The deletion-intent flag cannot outlive the item: this arm ends
    // the id directly (it does not route through `DispatchRemoved`), so
    // it clears the flag itself — **after** consulting it. A steer the
    // user deleted (or cleared) must not resurrect its text here; the
    // old code removed the flag first and then restored unconditionally,
    // which made every deletion of a missed steer undo itself.
    let suppress = app
        .suppress_steer_restore
        .remove(&(session_id.clone(), input_id.clone()));
    // The **row is the payload** (text + attachments), so the restore
    // must read it before it leaves. If it is already gone the message
    // is unrecoverable and the user must be told: silence here is a
    // prompt vanishing with no transcript row, no draft, and no
    // receipt. (A full-replace `QueueSnapshot` used to delete pending
    // steers, which made this arm silently drop the text.)
    let restored = app.take_steer_row(&session_id, &input_id);
    if app.live_session_id == session_id && !suppress {
        match restored {
            Some(item) => {
                app.adopt_as_draft(
                    item.text,
                    item.images,
                    item.text_pastes,
                    crate::shell::app::DraftAdoption::StashCurrent,
                );
                // Through the catalog funnel ([INV-NOTICE-08]), so the
                // ack is retained in the notice log rather than flashed
                // from a raw field write and lost.
                app.record_client_notice(
                    crate::shell::app::notice_catalog::ClientNotice::SteerMissedRoundRestored,
                );
            }
            None => {
                app.record_client_notice(
                    crate::shell::app::notice_catalog::ClientNotice::SteerContentLost,
                );
            }
        }
    }
    app.remove_dispatch(&session_id, &input_id);
    true
}

/// A pending steer was cancelled before admission: remove the outbox row and
/// restore the text only for a recall, never for a deletion (ADR-0137).
fn apply_steer_cancelled_restored(app: &mut App, session_id: String, input_id: String) -> bool {
    // A pending steer was cancelled before admission (SteerCancelled).
    // Remove the outbox `Steering` row; restore the text to the
    // composer only for a *recall*, never for a deletion — the modal's
    // `D` flags `suppress_steer_restore` so a deleted steer cannot
    // resurrect itself (ADR-0137).
    let suppress = app
        .suppress_steer_restore
        .remove(&(session_id.to_string(), input_id.to_string()));
    let restored = app.take_steer_row(&session_id, &input_id);
    if app.live_session_id == session_id && !suppress {
        match restored {
            Some(item) => {
                app.adopt_as_draft(
                    item.text,
                    item.images,
                    item.text_pastes,
                    crate::shell::app::DraftAdoption::StashCurrent,
                );
            }
            // A *recall* that finds no row lost the content the user
            // asked to get back. Say so instead of no-oping. (A
            // deletion is different: `suppress` is set, and consuming
            // the content is exactly what was asked for.)
            None => {
                app.record_client_notice(
                    crate::shell::app::notice_catalog::ClientNotice::SteerContentLost,
                );
            }
        }
    }
    app.remove_dispatch(&session_id, &input_id);
    true
}

/// The cancel lost the race: the steer will be admitted after all.
fn apply_steer_cancel_failed_cleared(app: &mut App, session_id: String, input_id: String) -> bool {
    // The cancel lost the race: the steer will be admitted after all.
    //
    // The deletion-intent flag may not outlive its **item**
    // (`[INV-STEER-07]`). While the row is still staged, the admission
    // that follows ends the id and clears the flag in `DispatchRemoved`,
    // so clearing it here would be premature: it would make "the cancel
    // lost" indistinguishable from "no cancel was requested", and a
    // later `SteerCancelled` in the same id's life would restore text
    // the user had deliberately deleted. If the row is already gone
    // (a delete removed it before the race resolved), nothing will ever
    // end this id again, so the flag must go now or it leaks.
    let row_alive = app.pending_dispatch.iter().any(|item| {
        item.session_id == session_id
            && item.id == input_id
            && item.state == crate::shell::app::QueuedDispatchState::Steering
    });
    if !row_alive {
        app.suppress_steer_restore
            .remove(&(session_id.clone(), input_id.clone()));
    }
    // One outcome, one disclosure. The row being gone means the user's
    // own gesture (delete / clear) already consumed the content and said
    // so; the admission that follows will end the id again and must not
    // add a "text lost" receipt for a loss that never happened. So the
    // race is disclosed only while the item is still live.
    if app.live_session_id == session_id && row_alive {
        // Through the catalog funnel (ADR-0138 / [INV-NOTICE-05]), so
        // the disclosure is *retained* in the notice log rather than
        // flashed and lost, and its wording/topic live in one place.
        app.record_client_notice(crate::shell::app::notice_catalog::ClientNotice::SteerCancelRaced);
    }
    true
}

/// The server admitted an optimistic follow-up: settle the row back to
/// `Waiting` so it is once again server-owned.
fn apply_dispatch_queued(app: &mut App, session_id: String, input_id: String) -> bool {
    if let Some(item) = app
        .pending_dispatch
        .iter_mut()
        .find(|item| item.session_id == session_id && item.id == input_id)
    {
        item.state = crate::shell::app::QueuedDispatchState::Waiting;
    }
    true
}

/// Authoritative replace of the rows the server owns, preserving every
/// client-owned row it has never heard of.
fn apply_queue_snapshot(
    app: &mut App,
    session_id: String,
    items: Vec<nuo_protocol::QueuedMessage>,
    paused: bool,
) -> bool {
    // Authoritative replace **of the rows the server owns**, preserving
    // every client-owned row it has never heard of.
    //
    // Ownership is a property of the state, not a heuristic:
    // `Waiting` is a projection of the server's `FollowUpQueue` (the
    // snapshot is the only writer that creates those rows outside a
    // test), while `Dispatching` (a `FollowUp` whose ack is still in
    // flight) and `Steering` (a busy-Enter steer, which the server
    // tracks in the *round*, never in this queue) exist only here.
    //
    // The old merge kept `Dispatching` alone, so any snapshot — and the
    // server sends one after *every* queue change, including the
    // `QueuePaused(true)` echo of merely opening the modal — silently
    // deleted every pending steer. The text then had nowhere to come
    // back from when `SteerUnavailable` arrived: the user's message was
    // lost with no transcript row, no composer draft, and no notice.
    let server_ids: std::collections::HashSet<&str> =
        items.iter().map(|queued| queued.id.as_str()).collect();
    let client_owned: Vec<crate::shell::app::QueuedDispatch> = app
        .pending_dispatch
        .iter()
        .filter(|item| {
            item.session_id == session_id
                && match item.state {
                    crate::shell::app::QueuedDispatchState::Waiting => false,
                    crate::shell::app::QueuedDispatchState::Dispatching
                    | crate::shell::app::QueuedDispatchState::Steering => {
                        !server_ids.contains(item.id.as_str())
                    }
                }
        })
        .cloned()
        .collect();
    // Preserve the other sessions' relative order verbatim; this
    // session's block is rebuilt at the tail (all addressing is
    // session-filtered, so the global slot is not observable).
    let mut rebuilt: std::collections::VecDeque<crate::shell::app::QueuedDispatch> =
        std::collections::VecDeque::new();
    for item in app.pending_dispatch.drain(..) {
        if item.session_id != session_id {
            rebuilt.push_back(item);
        }
    }
    for queued in items {
        rebuilt.push_back(crate::shell::app::QueuedDispatch {
            id: queued.id,
            session_id: session_id.clone(),
            state: crate::shell::app::QueuedDispatchState::Waiting,
            text: queued.display_text.unwrap_or(queued.text),
            queued_at_ms: queued.sent_at_ms,
            images: queued.images,
            text_pastes: Vec::new(),
        });
    }
    // Client-owned rows last: they are the newest, and a pending steer
    // is not the next item to ship (it waits on a turn boundary, not on
    // a round boundary).
    for item in client_owned {
        rebuilt.push_back(item);
    }
    app.pending_dispatch = rebuilt;
    if paused {
        app.queue_blocked_sessions.insert(session_id);
    } else {
        app.queue_blocked_sessions.remove(&session_id);
    }
    true
}

/// Replace the persisted prompt history, scrubbing legacy slash invocations
/// when command recording is disabled.
fn apply_input_history(app: &mut App, rows: Vec<nuo_protocol::HistoryEntry>) -> bool {
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

/// Prefill the model editor only while it is still open on the same route.
fn apply_route_settings(
    app: &mut App,
    provider_id: String,
    model: String,
    overrides: Option<nuo_protocol::model::CapabilityOverrides>,
) -> bool {
    // Prefill only if the editor is still open on the same route —
    // the answer is async, the operator may have moved on.
    if app.editor_target.as_deref() == Some(provider_id.as_str()) && app.editor_model == model {
        app.editor_vision_override = overrides.as_ref().and_then(|o| o.vision);
        app.editor_tool_override = overrides.as_ref().and_then(|o| o.tool_call);
        true
    } else {
        false
    }
}

fn apply_provider_picker(app: &mut App, snapshot: nuo_protocol::ProviderPickerSnapshot) -> bool {
    app.provider_picker = snapshot;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
        .refreshing = false;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .refreshing = false;
    true
}

/// Replace the thread overview, carrying through authentic titles and
/// clamping the cursor to the new list.
fn apply_sessions_overview(app: &mut App, sessions: Vec<nuo_protocol::ThreadOverview>) -> bool {
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

fn apply_session_detail(app: &mut App, detail: nuo_protocol::ThreadDetail) -> bool {
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

fn apply_connection_detail(app: &mut App, detail: nuo_protocol::ConnectionDetail) -> bool {
    let same_id = app.connection_detail.as_ref().map(|c| &c.name) == Some(&detail.name);
    app.connection_detail = Some(detail);
    if !same_id {
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
            .info_scroll = 0;
    }
    true
}

fn apply_host_sessions(app: &mut App, rows: Vec<nuo_protocol::MonitoredThread>) -> bool {
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

fn apply_server_endpoints(app: &mut App, report: nuo_protocol::ServerEndpointsReport) -> bool {
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

/// Mount the trust-gate interstitial from a snapshot unless the per-run gate
/// already latched it dismissed.
fn apply_pre_attach(
    app: &mut App,
    runtime: &UiRuntime,
    signal: crate::shell::pre_attach::PreAttachSignal,
) -> bool {
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
        app.park_focus_for_sheet();
    }
    true
}

fn apply_pre_attach_attestation_error(app: &mut App, message: String) -> bool {
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

fn apply_usage_stats(app: &mut App, report: nuo_protocol::usage_stats::UsageStatsReport) -> bool {
    app.usage_stats = Some(report);
    app.usage_loading = false;
    true
}

fn apply_provider_quotas(app: &mut App, snapshot: nuo_protocol::ProviderQuotaSnapshot) -> bool {
    for entry in &snapshot.entries {
        app.connection_usages
            .insert(entry.name.clone(), entry.state.clone());
    }
    app.provider_quotas = Some(snapshot);
    true
}

/// Record the disclosure once, then let only a fault raise a bubble.
fn apply_notice_disclosure(
    app: &mut App,
    rendered: crate::shell::app::notice_catalog::RenderedNotice,
) -> bool {
    // Residency is decided here, once, for every producer: the notice
    // is retained first (the log is the home), and only a fault
    // additionally raises a bubble. A producer therefore cannot show a
    // bubble without leaving a record — the defect that made a missed
    // two-second toast unrecoverable.
    app.record_notice(rendered);
    true
}

/// A dead link is not an exit: retain the reason, surface it, and let the
/// attach loop re-dial (ADR-0136).
fn apply_link_lost(app: &mut App, reason: String) -> bool {
    // ADR-0136: a dead link is not an exit. The session is still hosted
    // on the server; only this client's reader stopped, so intents can
    // no longer be delivered — which is exactly what `link_down` already
    // renders. Retaining the reason is what makes the fault answerable
    // after a window that previously closed with no explanation at all.
    app.link_down = true;
    app.relink_requested = true;
    nuo_host::diagnostics::record_event(
        "client",
        "link_lost",
        &format!("session server link lost: {reason}"),
        None,
    );
    app.record_client_notice(crate::shell::app::notice_catalog::ClientNotice::LinkLost { reason });
    true
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
pub(crate) fn apply_transcript(app: &mut App, edit: TranscriptEdit) -> bool {
    // Snapshot the disclosure input the tool arms need before the document borrow.
    let tui_config = app.tui_config.clone();
    let mut apply = TranscriptApply::new(tui_config);
    apply_transcript_edit(&mut app.messages, edit, &mut apply);
    apply.finish(app)
}

/// The turn position and send time an edit's payload *would* join, captured
/// before the document borrow. A freshly created component that arrives
/// unstamped adopts its turn's time ([`TranscriptMessage::adopt_sent_at_ms`]),
/// so a header can never lose its only temporal fact to a forgotten builder
/// call — the invariant is repaired here rather than trusted at every arm.
fn turn_time_adoption(
    messages: &[TranscriptMessage],
    round: Option<u64>,
    turn: Option<u64>,
) -> Option<u64> {
    let (round, turn) = (round?, turn?);
    messages
        .iter()
        .rev()
        .filter(|message| message.turn_position() == Some((round, turn)))
        .find_map(|message| message.sent_at_ms)
}

/// Per-edit bookkeeping accumulated while the document is mutably borrowed.
struct TranscriptApply {
    tui_config: crate::shell::config::TuiConfig,
    post: PostEdit,
    changed: bool,
    /// A command name the dispatcher did not recognize. Its draft was consumed
    /// by the Enter that submitted it, and nothing was sent to the model, so
    /// the text is handed back to the composer after the document borrow ends
    /// (see [`restore_rejected_command_draft`]) — a typo must cost one edit,
    /// not a retype.
    rejected_invocation: Option<String>,
}

impl TranscriptApply {
    fn new(tui_config: crate::shell::config::TuiConfig) -> Self {
        Self {
            tui_config,
            post: PostEdit::None,
            changed: true,
            rejected_invocation: None,
        }
    }

    fn invalidate(&mut self, id: u64) {
        self.post = PostEdit::Invalidate(id);
    }

    fn clear_cache(&mut self) {
        self.post = PostEdit::ClearCache;
    }

    fn mark_unchanged(&mut self) {
        self.changed = false;
    }

    /// Flush deferred cache invalidation, any restored draft, and the
    /// transcript-changed flag once the document borrow has ended.
    fn finish(self, app: &mut App) -> bool {
        match self.post {
            PostEdit::None => {}
            PostEdit::Invalidate(id) => {
                app.layout_height_cache
                    .invalidate_messages(std::iter::once(id));
            }
            PostEdit::ClearCache => app.layout_height_cache.clear(),
        }
        if let Some(rejected) = self.rejected_invocation {
            restore_rejected_command_draft(app, rejected);
        }
        if self.changed {
            app.transcript_changed_pending = true;
        }
        self.changed
    }
}

/// Dispatch one transcript edit to its named mutation handler.
fn apply_transcript_edit(
    messages: &mut Vec<TranscriptMessage>,
    edit: TranscriptEdit,
    apply: &mut TranscriptApply,
) {
    match edit {
        // ---- Streaming text -------------------------------------------------
        TranscriptEdit::BeginStream => apply_begin_stream(messages),
        TranscriptEdit::StreamTextDelta {
            round,
            turn,
            delta,
            created,
            clear_retry,
        } => {
            let _ = clear_retry; // the translator bundles the retry reset as its own mutation
            apply_stream_text_delta(messages, round, turn, delta, created, apply);
        }
        TranscriptEdit::StreamTextFinalize {
            round,
            turn,
            content,
            created,
        } => apply_stream_text_finalize(messages, round, turn, content, created, apply),
        TranscriptEdit::StreamDiscard { round, turn } => {
            apply_stream_discard(messages, round, turn, apply);
        }

        // ---- Reasoning ------------------------------------------------------
        TranscriptEdit::ReasoningDelta {
            round,
            turn,
            delta,
            created,
        } => apply_reasoning_delta(messages, round, turn, delta, created),
        TranscriptEdit::ReasoningFinalize {
            round,
            turn,
            content,
            duration_ms,
        } => apply_reasoning_finalize(messages, round, turn, content, duration_ms, apply),
        TranscriptEdit::FinalizeOrphanedReasoning { duration_ms } => {
            crate::shell::finalize_streaming_reasoning(messages, duration_ms);
        }

        // ---- Tool steps -----------------------------------------------------
        TranscriptEdit::ToolStepUpsert { message } => {
            apply_tool_step_upsert(messages, message, apply);
        }
        TranscriptEdit::ToolCallsRetracted { ids } => {
            apply_tool_calls_retracted(messages, ids, apply);
        }
        TranscriptEdit::RetractPendingToolSteps => {
            apply_retract_pending_tool_steps(messages, apply);
        }
        TranscriptEdit::ToolPreview { id, preview } => {
            apply_tool_preview(messages, id, preview, apply);
        }
        TranscriptEdit::ToolInputProgress {
            id,
            bytes,
            arguments,
        } => apply_tool_input_progress(messages, id, bytes, arguments, apply),
        TranscriptEdit::ToolResult {
            id,
            name,
            output,
            structured,
            duration_ms,
            fallback,
        } => apply_tool_result(
            messages,
            id,
            name,
            output,
            structured,
            duration_ms,
            fallback,
            apply,
        ),
        TranscriptEdit::ToolCancel { id, fallback } => {
            apply_tool_cancel(messages, id, fallback, apply);
        }
        TranscriptEdit::ToolStream { id, stream } => {
            apply_tool_stream(messages, id, stream, apply);
        }
        TranscriptEdit::SubagentEvent {
            parent_call_id,
            event,
        } => apply_subagent_event(messages, parent_call_id, event, apply),

        // ---- Turn / command projections ------------------------------------
        TranscriptEdit::InsertAdmittedSeam { message } => {
            // ADR-0137 [INV-STEER-02]: an admitted steer lands at the turn
            // seam, which is the buffer tail at admission time (the harness
            // persisted it after the finished turn, and the next turn's
            // components have not arrived yet). Appending reproduces the
            // durable order; head insertion would put a round-N steer above
            // round 1 in any thread with history.
            messages.push(message);
        }
        TranscriptEdit::SettleCommandResult {
            invocation,
            result,
            fallback,
        } => apply_settle_command_result(messages, invocation, result, fallback, apply),
        TranscriptEdit::Interrupted { record } => apply_interrupted(messages, record),
        TranscriptEdit::CancelLastUserPrompt => apply_cancel_last_user_prompt(messages),
        TranscriptEdit::Append { class, message } => apply_append(messages, class, message),
        TranscriptEdit::WithdrawStagedCommand {
            invocation,
            restore_draft,
        } => apply_withdraw_staged_command(messages, invocation, restore_draft, apply),
        TranscriptEdit::CancelPendingCommands => apply_cancel_pending_commands(messages),
        TranscriptEdit::RebaseRounds { round_counter } => {
            crate::shell::rebase_transcript_rounds(messages, round_counter);
        }
        TranscriptEdit::StampTurnPrompt { round } => apply_stamp_turn_prompt(messages, round),

        // ---- Structural replacement ----------------------------------------
        TranscriptEdit::ReplaceAll { messages: rebuilt } => {
            *messages = rebuilt;
            apply.clear_cache();
        }
        TranscriptEdit::Clear => {
            messages.clear();
            apply.clear_cache();
        }
    }
}

fn apply_begin_stream(messages: &mut [TranscriptMessage]) {
    crate::shell::begin_stream(messages);
}

fn apply_stream_text_delta(
    messages: &mut Vec<TranscriptMessage>,
    round: Option<u64>,
    turn: Option<u64>,
    delta: String,
    created: Option<TranscriptMessage>,
    apply: &mut TranscriptApply,
) {
    match crate::shell::append_stream_text_delta(messages, round, turn, &delta) {
        Some(id) => apply.invalidate(id),
        None => {
            if let Some(mut message) = created {
                message.adopt_sent_at_ms(turn_time_adoption(messages, round, turn));
                messages.push(message);
            }
        }
    }
}

fn apply_stream_text_finalize(
    messages: &mut Vec<TranscriptMessage>,
    round: Option<u64>,
    turn: Option<u64>,
    content: String,
    created: Option<TranscriptMessage>,
    apply: &mut TranscriptApply,
) {
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
            apply.invalidate(id);
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

fn apply_stream_discard(
    messages: &mut Vec<TranscriptMessage>,
    round: u64,
    turn: u64,
    apply: &mut TranscriptApply,
) {
    // A discarded attempt can leave **more than one** provisional
    // row behind: a thinking trace, the finalized copy of that
    // trace, and the answer text that was still streaming when the
    // transport died. A retry reuses the same `(round, turn)`, so
    // every row this attempt streamed has to go — otherwise the
    // next attempt's deltas append into the failed attempt's row
    // (`append_reasoning_delta` matches on position alone) and two
    // attempts fuse into one block, e.g.
    // "attempt one reasoningattempt two reasoning".
    //
    // Only a *trailing* run of this attempt's provisional rows is
    // swept. Once the attempt has committed dialogue — a dispatched
    // tool step, a notice, a command result — it is no longer
    // discardable, and truncating beneath that row would reorder
    // history.
    //
    // A tool step whose arguments are still ingesting counts as
    // provisional, not committed: it cannot become a call, the
    // harness has already retracted it by id (`[INV-TOOL-ID-05]`),
    // and the `RetryScheduled` sweep deletes it a moment later
    // anyway. It must not shield the answer text beneath it — the
    // discard is emitted *before* that retry.
    //
    // The position is carried by the edit (ADR-0167), so there is no
    // "unaddressed discard" case to guard against: the state is not
    // representable.
    let mut cut = messages.len();
    while cut > 0 {
        let message = &messages[cut - 1];
        let owned = message.round == Some(round) && message.turn == Some(turn);
        let provisional = message.role == Role::Assistant
            || message.is_reasoning()
            || message.is_pending_tool_step();
        if owned && provisional {
            cut -= 1;
        } else {
            break;
        }
    }
    if cut == messages.len() {
        apply.mark_unchanged();
    } else {
        messages.truncate(cut);
        // Rows moved, so every cached height below the cut is stale.
        apply.clear_cache();
    }
}

fn apply_reasoning_delta(
    messages: &mut Vec<TranscriptMessage>,
    round: Option<u64>,
    turn: Option<u64>,
    delta: String,
    created: Option<TranscriptMessage>,
) {
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

fn apply_reasoning_finalize(
    messages: &mut Vec<TranscriptMessage>,
    round: Option<u64>,
    turn: Option<u64>,
    content: String,
    duration_ms: Option<u64>,
    apply: &mut TranscriptApply,
) {
    let target = messages.iter_mut().rfind(|message| {
        message.is_reasoning_streaming() && message.round == round && message.turn == turn
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
        apply.invalidate(id);
    }
}

fn apply_tool_step_upsert(
    messages: &mut Vec<TranscriptMessage>,
    mut message: TranscriptMessage,
    apply: &mut TranscriptApply,
) {
    // ADR-0166: one id, one row. Resolve first, merge if present,
    // insert if not. Applying the same fact twice is a no-op, which
    // is what makes a duplicated or replayed frame harmless.
    let (round, turn) = (message.round, message.turn);
    message.adopt_sent_at_ms(turn_time_adoption(messages, round, turn));
    let name = message.tool_step_name().unwrap_or_default().to_string();
    if let Some(status) = message.tool_step_status() {
        let default =
            crate::shell::step_interaction::default_tool_expanded(status, &name, &apply.tui_config);
        message.set_tool_step_expanded(default);
    }
    let call_id = message.tool_step_call_id().unwrap_or_default().to_string();
    match find_tool_step_mut(messages, &call_id) {
        Some(existing) => {
            if existing.merge_tool_step(&message) {
                // The lifecycle may have advanced, and the
                // lifecycle-aware disclosure default follows it.
                if let Some(status) = existing.tool_step_status() {
                    let default = crate::shell::step_interaction::default_tool_expanded(
                        status,
                        &name,
                        &apply.tui_config,
                    );
                    existing.set_tool_step_expanded(default);
                }
                apply.invalidate(existing.id);
            } else {
                apply.mark_unchanged();
            }
        }
        // A just-pushed message has no cached height to evict.
        None => messages.push(message),
    }
}

fn apply_tool_calls_retracted(
    messages: &mut Vec<TranscriptMessage>,
    ids: Vec<String>,
    apply: &mut TranscriptApply,
) {
    if ids.is_empty() {
        apply.mark_unchanged();
    } else {
        let ids: std::collections::HashSet<String> = ids.into_iter().collect();
        let before = messages.len();
        // Only a provisional row is retractable. A call that
        // dispatched is real, even if the retraction crossed it in
        // flight (`[INV-TOOL-ID-04]`).
        messages.retain(|message| {
            !message
                .tool_step_call_id()
                .is_some_and(|id| ids.contains(id))
                || !message.is_pending_tool_step()
        });
        if messages.len() == before {
            apply.mark_unchanged();
        } else {
            apply.clear_cache();
        }
    }
}

fn apply_retract_pending_tool_steps(
    messages: &mut Vec<TranscriptMessage>,
    apply: &mut TranscriptApply,
) {
    // `[INV-TOOL-ID-05]`: a round that is not running cannot be
    // ingesting a call. Ids are not needed, so this sweep is correct
    // even in a listener that was spawned by a relink.
    let before = messages.len();
    messages.retain(|message| !message.is_pending_tool_step());
    if messages.len() == before {
        apply.mark_unchanged();
    } else {
        apply.clear_cache();
    }
}

fn apply_tool_preview(
    messages: &mut Vec<TranscriptMessage>,
    id: String,
    preview: nuo_tool::PreviewResult,
    apply: &mut TranscriptApply,
) {
    // Match by call id: the announcement (or the up-front `ToolCall`
    // for a whole-argument provider) already created the step.
    if let Some(message) = find_tool_step_mut(messages, &id) {
        if message.set_tool_preview(&id, preview) {
            apply.invalidate(message.id);
        } else {
            apply.mark_unchanged();
        }
    } else {
        apply.mark_unchanged();
    }
}

fn apply_tool_input_progress(
    messages: &mut Vec<TranscriptMessage>,
    id: String,
    bytes: usize,
    arguments: Option<String>,
    apply: &mut TranscriptApply,
) {
    match find_tool_step_mut(messages, &id) {
        Some(step) => {
            let mut moved = step.set_tool_input_bytes(bytes);
            if let Some(args) = arguments
                && step.set_tool_streaming_arguments(args)
            {
                moved = true;
            }
            if moved {
                apply.invalidate(step.id);
            } else {
                apply.mark_unchanged();
            }
        }
        None => apply.mark_unchanged(),
    }
}

fn apply_tool_result(
    messages: &mut Vec<TranscriptMessage>,
    id: String,
    name: String,
    output: String,
    structured: nuo_tool::ToolOutput,
    duration_ms: u64,
    fallback: Option<TranscriptMessage>,
    apply: &mut TranscriptApply,
) {
    // Newest-first: an id identifies one row, and `rfind` is the
    // same "live row wins" convention every other tool arm uses.
    let mut finished: Option<u64> = None;
    for existing in messages.iter_mut().rev() {
        if existing.finish_tool_step(&id, output.clone(), structured.clone(), duration_ms) {
            if let Some(status) = existing.tool_step_status() {
                let default = crate::shell::step_interaction::default_tool_expanded(
                    status,
                    &name,
                    &apply.tui_config,
                );
                existing.set_tool_step_expanded(default);
            }
            finished = Some(existing.id);
            break;
        }
    }
    match finished {
        Some(id) => apply.invalidate(id),
        None => {
            if let Some(mut message) = fallback {
                if let Some(status) = message.tool_step_status() {
                    let default = crate::shell::step_interaction::default_tool_expanded(
                        status,
                        &name,
                        &apply.tui_config,
                    );
                    message.set_tool_step_expanded(default);
                }
                messages.push(message);
            } else {
                apply.mark_unchanged();
            }
        }
    }
}

fn apply_tool_cancel(
    messages: &mut Vec<TranscriptMessage>,
    id: String,
    fallback: Option<TranscriptMessage>,
    apply: &mut TranscriptApply,
) {
    let mut cancelled_id: Option<u64> = None;
    for message in messages.iter_mut().rev() {
        if message.cancel_tool_step(&id) {
            message.set_tool_step_expanded(false);
            cancelled_id = Some(message.id);
            break;
        }
    }
    match cancelled_id {
        Some(id) => apply.invalidate(id),
        None => {
            if let Some(message) = fallback {
                messages.push(message);
            } else {
                apply.mark_unchanged();
            }
        }
    }
}

fn apply_tool_stream(
    messages: &mut Vec<TranscriptMessage>,
    id: String,
    stream: nuo_tool::ToolStream,
    apply: &mut TranscriptApply,
) {
    if !messages
        .iter_mut()
        .rev()
        .any(|message| message.push_tool_stream(&id, &stream))
    {
        apply.mark_unchanged();
    }
}

fn apply_subagent_event(
    messages: &mut Vec<TranscriptMessage>,
    parent_call_id: String,
    event: nuo_tool::SubagentEvent,
    apply: &mut TranscriptApply,
) {
    if !messages
        .iter_mut()
        .rev()
        .find(|message| message.tool_step_call_id() == Some(parent_call_id.as_str()))
        .is_some_and(|message| message.push_subagent_event(&event, &apply.tui_config))
    {
        apply.mark_unchanged();
    }
}

fn apply_settle_command_result(
    messages: &mut Vec<TranscriptMessage>,
    invocation: String,
    result: nuo_protocol::CommandResult,
    fallback: Option<TranscriptMessage>,
    apply: &mut TranscriptApply,
) {
    let invocation = invocation.trim().to_string();
    // A rejected command name is the one command result that gives
    // the user something back: nothing was dispatched, so the draft
    // is still theirs. Hold it for the post-borrow restore below.
    if let nuo_protocol::CommandResult::UnknownCommand { .. } = &result {
        apply.rejected_invocation = Some(invocation.clone());
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
        Some(id) => apply.invalidate(id),
        None => {
            if let Some(message) = fallback {
                messages.push(message);
            }
        }
    }
}

fn apply_interrupted(messages: &mut Vec<TranscriptMessage>, record: nuo_protocol::RoundInterrupt) {
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

fn apply_cancel_last_user_prompt(messages: &mut [TranscriptMessage]) {
    if let Some(user_msg) = messages.iter_mut().rev().find(|m| m.role == Role::User) {
        user_msg.cancel_prompt();
    }
}

fn apply_append(
    messages: &mut Vec<TranscriptMessage>,
    class: nuo_protocol::EventClass,
    message: TranscriptMessage,
) {
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

fn apply_withdraw_staged_command(
    messages: &mut Vec<TranscriptMessage>,
    invocation: String,
    restore_draft: bool,
    apply: &mut TranscriptApply,
) {
    // Drop the staged optimistic row: the server's event declared the
    // reply a reading or failure, so no thread row will ever settle it. Removing
    // it HERE — rather than leaving it for `CancelPendingCommands` to
    // mark `Cancelled` — is what keeps the thread's contents identical
    // whether or not the catalog had arrived before dispatch (ADR-0140, ADR-0149).
    let invocation = invocation.trim().to_string();
    if restore_draft {
        apply.rejected_invocation = Some(invocation.clone());
    }
    let before = messages.len();
    messages.retain(|message| {
        !(message.raw == invocation
            && message.command_result_phase()
                == Some(nuo_view::model::document::CommandPhase::Pending))
    });
    if messages.len() != before {
        // Every following row shifted a line; the layout cache is keyed
        // by message id, so a wholesale clear is the honest answer.
        apply.clear_cache();
    }
}

fn apply_cancel_pending_commands(messages: &mut [TranscriptMessage]) {
    for message in messages.iter_mut() {
        message.cancel_pending_command();
    }
}

fn apply_stamp_turn_prompt(messages: &mut [TranscriptMessage], round: u64) {
    // The composer cannot know the authoritative round until
    // admission; stamp its latest unpositioned driving prompt.
    if let Some(prompt) = messages.iter_mut().rev().find(|message| {
        message.role == Role::User
            && (message.origin == nuo_view::model::document::UserMessageOrigin::Chat
                || message.origin == nuo_view::model::document::UserMessageOrigin::FollowUp)
            && (message.round.is_none() || message.is_sending())
    }) {
        prompt.round = Some(round);
        prompt.settle_delivered();
    }
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
                    chrome.set_phase(Some(Phase::Admitting));
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
