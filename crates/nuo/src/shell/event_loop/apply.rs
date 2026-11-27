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
        AppMutation::Transcript { thread_id, edit } => {
            apply_transcript_for(app, thread_id.as_deref(), edit)
        }

        // ---- Chrome / round lifecycle -------------------------------------
        AppMutation::ChromeEdit { thread_id, edit } => {
            apply_chrome_mutation(app, runtime, thread_id.as_deref(), edit)
        }
        AppMutation::SetRoundCount { thread_id, round } => {
            let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id.as_deref()) else {
                return false;
            };
            inst.round_count = round;
            is_fg
        }

        // ---- Session / thread lifecycle -----------------------------------
        AppMutation::Harness {
            thread_id,
            snapshot,
        } => apply_harness_snapshot(app, runtime, thread_id.as_deref(), snapshot),
        AppMutation::SessionPhase { thread_id, phase } => {
            apply_session_phase(app, thread_id.as_deref(), phase)
        }
        AppMutation::ThreadSubscribed { thread_id, phase } => {
            super::apply_link::apply_thread_subscribed(app, thread_id, phase)
        }
        AppMutation::CommandCatalog(catalog) => {
            app.composer.command_catalog = catalog;
            true
        }
        AppMutation::HarnessUnattended { thread_id, enabled } => {
            let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id.as_deref()) else {
                return false;
            };
            inst.unattended = enabled;
            is_fg
        }
        AppMutation::HarnessConfined {
            thread_id,
            confined,
        } => {
            let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id.as_deref()) else {
                return false;
            };
            inst.confined = confined;
            is_fg
        }
        AppMutation::ClearSwitchingSession => {
            app.session.switching_session = None;
            true
        }
        AppMutation::LiveSession {
            previous_id,
            next_id,
        } => apply_live_session(app, previous_id, next_id),
        AppMutation::SessionsOverview(sessions) => apply_sessions_overview(app, sessions),
        AppMutation::ReconcileMissingDrafts(missing) => {
            let active = app.session.current_session_id.clone();
            app.surfaces
                .drafts
                .reconcile_missing(&missing, Some(active.as_str()));
            false
        }
        AppMutation::ForgetDeletedThreadDraft(thread_id) => {
            app.surfaces.drafts.forget(&thread_id);
            app.surfaces.drafts.flush();
            false
        }
        AppMutation::OpenSessionsPanel => {
            app.link.open_sessions_signal = true;
            true
        }
        AppMutation::OpenTreePanel => {
            app.link.open_tree_signal = true;
            true
        }
        AppMutation::SessionDetail(detail) => apply_session_detail(app, detail),
        AppMutation::ConnectionDetail(detail) => apply_connection_detail(app, detail),
        AppMutation::SessionTree(tree) => {
            app.threads.session_tree = tree;
            true
        }
        AppMutation::SessionContext(snapshot) => {
            app.threads.session_context = Some(snapshot);
            true
        }
        AppMutation::CompletionSignal(signal) => {
            app.composer.backend_completion_signal = Some(signal);
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
            app.human.pending_inputs.push_back(request);
            true
        }
        AppMutation::ClearPermissions => {
            app.human.pending_permissions.clear();
            app.threads.clear_awaiting_user_phases();
            app.session
                .running_sessions
                .retain(|id| app.threads.get(id).is_none_or(|t| t.is_responding()));
            sync_runtime_responding(app, runtime);
            true
        }
        AppMutation::BackgroundTaskStarted {
            thread_id,
            id,
            label,
            started_at_ms,
        } => {
            let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id.as_deref()) else {
                return false;
            };
            inst.upsert_background_task(id, label, started_at_ms);
            is_fg
        }
        AppMutation::BackgroundTaskCompleted {
            thread_id,
            id,
            success,
            exit_code,
            duration_secs,
        } => {
            let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id.as_deref()) else {
                return false;
            };
            inst.complete_background_task(&id, success, exit_code, duration_secs);
            is_fg
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
            app.surfaces.key_status = status;
            true
        }
        AppMutation::ProviderPicker(snapshot) => apply_provider_picker(app, snapshot),
        AppMutation::ProviderSwitched {
            thread_id,
            provider,
            model,
        } => {
            let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id.as_deref()) else {
                return false;
            };
            inst.current_provider = provider;
            inst.current_model = model;
            is_fg
        }
        AppMutation::WebSearchConfig(snapshot) => {
            app.surfaces.websearch_config = snapshot;
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
            app.threads.token_report = report;
            true
        }
        AppMutation::ContextPlan(plan) => {
            app.threads.context_plan = plan;
            app.threads.context_plan_loaded = true;
            true
        }
        AppMutation::UsageStats(report) => apply_usage_stats(app, report),
        AppMutation::UsageFailed(_error) => {
            app.telemetry.usage_loading = false;
            true
        }
        AppMutation::ProviderQuotas(snapshot) => apply_provider_quotas(app, snapshot),
        AppMutation::ContextTokens {
            session_id,
            snapshot,
        } => {
            app.telemetry
                .context_tokens_by_session
                .insert(session_id, snapshot);
            true
        }
        AppMutation::ClearContextTokens { thread_id } => {
            if let Some(ref tid) = thread_id {
                app.telemetry.context_tokens_by_session.remove(tid);
            } else {
                app.telemetry.context_tokens_by_session.clear();
            }
            // A plan belongs to one thread's `Agent`. Leaving it installed across
            // a session change is the exact failure the reply's `thread_id` guard
            // exists to prevent — the guard rejects the *reply*, so the stale
            // value must go with the session it described (ADR-0220 §3).
            let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id.as_deref()) else {
                return false;
            };
            inst.context_plan = None;
            inst.context_plan_loaded = false;
            is_fg
        }

        // ---- Notices / link / process -------------------------------------
        AppMutation::NoticeDisclosure(rendered) => apply_notice_disclosure(app, rendered),
        AppMutation::Quit => {
            app.link
                .should_quit
                .store(true, std::sync::atomic::Ordering::SeqCst);
            true
        }
        AppMutation::LinkLost { reason } => apply_link_lost(app, reason),
        AppMutation::PersistenceHealth(health) => {
            app.session.persistence_health = health;
            true
        }
        AppMutation::ServerPhase(phase) => super::apply_link::apply_server_phase(app, phase),
        AppMutation::LinkPhase(phase) => {
            if app.link.phase != phase {
                app.link.phase = phase;
                app.link.phase_since = std::time::Instant::now();
            }
            true
        }
        AppMutation::LinkEstablished {
            server_id,
            server_phase,
            persistence_health,
            req_tx,
        } => super::apply_link::apply_link_established(
            app,
            server_id,
            server_phase,
            persistence_health,
            req_tx,
        ),
        AppMutation::SubscribeRefused(error) => {
            super::apply_link::apply_subscribe_refused(app, error)
        }
        AppMutation::PickRequired(threads) => super::apply_link::apply_pick_required(app, threads),
        AppMutation::HostConsole(line) => {
            app.viewport.host_console_log.push(line);
            true
        }
    }
}

/// Mirror the aggregate liveness hint into `UiRuntime::is_responding` so
/// stream delta coalescing stays active whenever *any* resident thread is live.
fn sync_runtime_responding(app: &App, runtime: &UiRuntime) {
    let any = app.threads.any_responding() || !app.session.running_sessions.is_empty();
    runtime
        .is_responding
        .store(any, std::sync::atomic::Ordering::SeqCst);
}

/// Whether `thread_id` belongs to the tab the user is currently looking at.
///
/// When tabs are mounted, only the active tab's thread is viewed — so a
/// resident foreground thread that finishes while the user is on `Dashboard`
/// or `Settings` is treated as unseen and earns its TabBar outcome tint.
/// When no tabs are mounted (minimal headless test harnesses), falls back to
/// `is_fg`.
fn is_viewed_thread_tab(app: &App, thread_id: &str, is_fg: bool) -> bool {
    match app
        .surfaces
        .surfaces
        .active_tab()
        .and_then(|tab| tab.thread_id())
    {
        Some(active_id) => active_id == thread_id,
        None => app.surfaces.surfaces.tabs().is_empty() && is_fg,
    }
}

/// Apply a chrome edit directly to the target [`ThreadInstance`] and mirror the
/// resulting running state into the session-keyed bookkeeping the keymap and
/// queue bar consume (ADR-0253 `[INV-INSTANCE-01]` / `[INV-INSTANCE-02]`).
fn apply_chrome_mutation(
    app: &mut App,
    runtime: &UiRuntime,
    thread_id: Option<&str>,
    edit: ChromeEdit,
) -> bool {
    let is_turn_perf = matches!(edit, ChromeEdit::TurnPerformance(_));
    let is_round_started = matches!(
        edit,
        ChromeEdit::RoundLifecycle { running: true, .. }
            | ChromeEdit::TurnStarted { .. }
            | ChromeEdit::StreamStarted
    );
    let is_round_completed = matches!(edit, ChromeEdit::RoundCompleted(_));
    let is_round_failed = matches!(edit, ChromeEdit::RoundFailed);
    let is_lifecycle_stop = matches!(edit, ChromeEdit::RoundLifecycle { running: false, .. });
    let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id) else {
        return false;
    };
    let prev_running = inst.is_responding();
    let prev_attention = inst.phase == Some(Phase::AwaitingUser);
    let resolved_id = inst.key.id.clone();

    inst.apply_chrome_edit(edit);

    let now_running = inst.is_responding();
    let now_attention = inst.phase == Some(Phase::AwaitingUser);

    let prev_outcome = app.session.unseen_outcomes.get(&resolved_id).copied();
    let is_viewed_tab = is_viewed_thread_tab(app, &resolved_id, is_fg);
    if !resolved_id.is_empty() {
        if is_round_started {
            app.session.unseen_outcomes.remove(&resolved_id);
        } else if !is_viewed_tab {
            if is_round_failed {
                app.session.unseen_outcomes.insert(
                    resolved_id.clone(),
                    crate::shell::app::workspaces::UnseenOutcome::Failed,
                );
            } else if is_round_completed
                || (is_lifecycle_stop && prev_running && !now_running && prev_outcome.is_none())
            {
                app.session.unseen_outcomes.insert(
                    resolved_id.clone(),
                    crate::shell::app::workspaces::UnseenOutcome::Succeeded,
                );
            }
        }
    }
    let now_outcome = app.session.unseen_outcomes.get(&resolved_id).copied();

    if !is_turn_perf && !resolved_id.is_empty() {
        if now_running {
            app.session.running_sessions.insert(resolved_id);
        } else {
            app.session.running_sessions.remove(&resolved_id);
        }
    }
    sync_runtime_responding(app, runtime);

    // Foreground chrome changes repaint the activity/model bar. Background
    // threads request a repaint whenever their Tier 1 TabBar status changes
    // (Running, Attention, Succeeded, Failed, or Idle).
    is_fg
        || prev_running != now_running
        || prev_attention != now_attention
        || prev_outcome != now_outcome
}

/// Fold a harness snapshot into the loop-status and trust-gate projection.
fn apply_harness_snapshot(
    app: &mut App,
    runtime: &UiRuntime,
    thread_id: Option<&str>,
    snapshot: nuo_protocol::HarnessSnapshot,
) -> bool {
    let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id) else {
        return false;
    };
    inst.loop_status = snapshot.loop_status;
    inst.unattended = snapshot.unattended;
    inst.confined = snapshot.confined;
    inst.harness_retry_pending = snapshot.retry_pending;
    if let Some(role) = snapshot.role {
        inst.current_role = Some(role);
    }
    if let Some(ws) = snapshot.workspace {
        inst.current_workspace = nuo_view::chrome::tilde_home(std::path::Path::new(&ws));
    } else if inst.current_role.is_some() {
        inst.current_workspace.clear();
    }
    if !is_fg {
        return false;
    }
    app.session.workspace_security = snapshot.workspace_security.clone();
    // PreAttach unmount: when the snapshot transitions
    // to having no unresolved quarantined/changed/expired domains (or the
    // workspace is no longer quarantined), clear the interstitial and latch
    // the per-run gate so a subsequent periodic republish cannot re-mount
    // within this run.
    if let Some(pa) = app.surfaces.pre_attach.as_mut() {
        let gate_needed =
            nuo_view::trust_gate::gate_request(&snapshot.workspace_security).is_some();
        if !gate_needed {
            tracing::info!("nuo: clearing PreAttach interstitial (workspace review resolved)");
            app.surfaces.pre_attach = None;
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

fn apply_session_phase(
    app: &mut App,
    thread_id: Option<&str>,
    phase: nuo_protocol::ThreadPhase,
) -> bool {
    let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id) else {
        return false;
    };
    let resolved_id = inst.key.id.clone();
    let is_failed = matches!(phase, nuo_protocol::ThreadPhase::Failed { .. });
    if !is_fg {
        let ready = phase == nuo_protocol::ThreadPhase::Ready;
        inst.session_phase = phase;
        let is_viewed_tab = is_viewed_thread_tab(app, &resolved_id, false);
        let prev_outcome = app.session.unseen_outcomes.get(&resolved_id).copied();
        if is_failed && !is_viewed_tab && !resolved_id.is_empty() {
            app.session.unseen_outcomes.insert(
                resolved_id.clone(),
                crate::shell::app::workspaces::UnseenOutcome::Failed,
            );
        }
        let outcome_changed =
            app.session.unseen_outcomes.get(&resolved_id).copied() != prev_outcome;
        if ready && let Some(tid) = thread_id {
            let _ = app.send_intent_to(
                tid,
                nuo_protocol::AgentRequest::QueueSnapshot {
                    thread_id: tid.to_string(),
                },
            );
        }
        return outcome_changed;
    }
    let failed = match &phase {
        nuo_protocol::ThreadPhase::Failed { message, .. } => Some(message.clone()),
        _ => None,
    };
    let ready = phase == nuo_protocol::ThreadPhase::Ready;
    if app.threads.session_phase != phase {
        app.threads.session_phase = phase;
        app.session.thread_phase_since = std::time::Instant::now();
    }
    if is_failed && !is_viewed_thread_tab(app, &resolved_id, true) && !resolved_id.is_empty() {
        app.session.unseen_outcomes.insert(
            resolved_id,
            crate::shell::app::workspaces::UnseenOutcome::Failed,
        );
    }
    if ready {
        app.session.switching_session = None;
        let queued: Vec<_> = std::mem::take(&mut app.composer.pending_prompts);
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
            thread_id: app.session.current_session_id.clone(),
        });
    }
    if let Some(message) = failed {
        // A post-Welcome failure is a disclosure, not a death
        // (ADR-0133, extending `[INV-ATTACH-02]`).
        app.record_client_notice(
            crate::shell::app::notice_catalog::ClientNotice::SessionPrepFailed { detail: message },
        );
        app.session.switching_session = None;
    }
    true
}

/// Record the live session move and rebase only the tab carrying the session
/// being left (ADR-0123 `[INV-TAB-13]`).
fn apply_live_session(app: &mut App, previous_id: String, next_id: String) -> bool {
    app.rekey_thread_workspace(&previous_id, &next_id);
    app.session.live_session_id = next_id.clone();
    if app.session.current_session_id == previous_id {
        app.session.current_session_id = next_id.clone();
    }
    // `[INV-TAB-13]`: address the tab by the session it is *leaving*.
    // Rebasing "the active tab" is the ghost-tab generator — the active
    // tab may already be a replacement that has nothing to do with this
    // switch. No matching tab means this receipt is for a session the
    // client no longer shows, so there is nothing to rebase.
    if app
        .surfaces
        .surfaces
        .rebase_thread_tab(&previous_id, &next_id)
    {
        // Only the rebased tab loses its title: the new session has no
        // authentic title yet (ADR-0050). Titles of untouched tabs are
        // never collateral damage.
        app.surfaces.surfaces.clear_tab_title(&next_id);
    }
    true
}

fn apply_queue_permission(
    app: &mut App,
    request: nuo_tool::PermissionRequest,
    parent_call_id: Option<String>,
) -> bool {
    if let Some(parent) = parent_call_id {
        app.human
            .subagent_permission_parent
            .insert(request.id.clone(), parent);
    }
    app.human.pending_permissions.push_back(request);
    true
}

fn apply_queue_question(
    app: &mut App,
    request: nuo_tool::UserQuestionRequest,
    parent_call_id: Option<String>,
) -> bool {
    if let Some(parent) = parent_call_id {
        app.human
            .subagent_question_parent
            .insert(request.id.clone(), parent);
    }
    app.human.pending_questions.push_back(request);
    true
}

/// The single terminal path for an outbox item.
///
/// Clearing the deletion-intent flag here (rather than in each fate arm) is
/// what makes the flag structurally unable to outlive its item: whichever
/// event ends this id — admitted, missed, cancelled, cancel-failed — the row
/// leaves through this mutation, so the flag leaves with it.
fn apply_dispatch_removed(app: &mut App, session_id: String, input_id: String) -> bool {
    app.composer
        .suppress_steer_restore
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
        .composer
        .suppress_steer_restore
        .remove(&(session_id.clone(), input_id.clone()));
    // The **row is the payload** (text + attachments), so the restore
    // must read it before it leaves. If it is already gone the message
    // is unrecoverable and the user must be told: silence here is a
    // prompt vanishing with no transcript row, no draft, and no
    // receipt. (A full-replace `QueueSnapshot` used to delete pending
    // steers, which made this arm silently drop the text.)
    let restored = app.take_steer_row(&session_id, &input_id);
    if app.session.live_session_id == session_id && !suppress {
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
        .composer
        .suppress_steer_restore
        .remove(&(session_id.to_string(), input_id.to_string()));
    let restored = app.take_steer_row(&session_id, &input_id);
    if app.session.live_session_id == session_id && !suppress {
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
    let row_alive = app.composer.pending_dispatch.iter().any(|item| {
        item.session_id == session_id
            && item.id == input_id
            && item.state == crate::shell::app::QueuedDispatchState::Steering
    });
    if !row_alive {
        app.composer
            .suppress_steer_restore
            .remove(&(session_id.clone(), input_id.clone()));
    }
    // One outcome, one disclosure. The row being gone means the user's
    // own gesture (delete / clear) already consumed the content and said
    // so; the admission that follows will end the id again and must not
    // add a "text lost" receipt for a loss that never happened. So the
    // race is disclosed only while the item is still live.
    if app.session.live_session_id == session_id && row_alive {
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
        .composer
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
        .composer
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
    for item in app.composer.pending_dispatch.drain(..) {
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
    app.composer.pending_dispatch = rebuilt;
    if paused {
        app.session.queue_blocked_sessions.insert(session_id);
    } else {
        app.session.queue_blocked_sessions.remove(&session_id);
    }
    true
}

/// Replace the persisted prompt history, scrubbing legacy slash invocations
/// when command recording is disabled.
fn apply_input_history(app: &mut App, rows: Vec<nuo_protocol::HistoryEntry>) -> bool {
    app.composer.input_history = if app.composer.input_history_record_commands {
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
    if app.provider_editor.editor_target.as_deref() == Some(provider_id.as_str())
        && app.provider_editor.editor_model == model
    {
        app.provider_editor.editor_vision_override = overrides.as_ref().and_then(|o| o.vision);
        app.provider_editor.editor_tool_override = overrides.as_ref().and_then(|o| o.tool_call);
        true
    } else {
        false
    }
}

fn apply_provider_picker(app: &mut App, snapshot: nuo_protocol::ProviderPickerSnapshot) -> bool {
    app.surfaces.provider_picker = snapshot;
    app.surfaces.provider_picker_synced = true;
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
        .refreshing = false;
    app.surfaces
        .surfaces
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
                app.surfaces.surfaces.update_tab_title(&s.id, trimmed);
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
    app.surfaces.modal_index = app
        .surfaces
        .modal_index
        .min(sessions.len().saturating_sub(1));
    app.session.sessions_overview = sessions;
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
        .loading = false;
    true
}

fn apply_session_detail(app: &mut App, detail: nuo_protocol::ThreadDetail) -> bool {
    if let Some(ref t) = detail.title {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            app.surfaces.surfaces.update_tab_title(&detail.id, trimmed);
        }
    }
    let same_id = app.session.session_detail.as_ref().map(|s| &s.id) == Some(&detail.id);
    app.session.session_detail = Some(detail);
    if !same_id {
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
            .info_scroll = 0;
    }
    true
}

fn apply_connection_detail(app: &mut App, detail: nuo_protocol::ConnectionDetail) -> bool {
    let same_id = app.surfaces.connection_detail.as_ref().map(|c| &c.name) == Some(&detail.name);
    app.surfaces.connection_detail = Some(detail);
    if !same_id {
        app.surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
            .info_scroll = 0;
    }
    true
}

fn apply_host_sessions(app: &mut App, rows: Vec<nuo_protocol::MonitoredThread>) -> bool {
    let open_thread_ids: std::collections::HashSet<String> = app
        .surfaces
        .surfaces
        .tabs()
        .iter()
        .filter_map(|tab| tab.thread_id().map(str::to_string))
        .collect();
    let prev_statuses: std::collections::HashMap<String, nuo_protocol::ThreadActivity> = app
        .session
        .host_sessions
        .iter()
        .map(|row| (row.id.clone(), row.status))
        .collect();

    for row in &rows {
        if let Some(ref t) = row.title {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                app.surfaces.surfaces.update_tab_title(&row.id, trimmed);
            }
        }
        // Cold/unsubscribed open background tabs get their terminal outcomes
        // from monitor transitions (`Running` -> `Idle` / `Failed`).
        let is_subscribed =
            app.threads.get(&row.id).is_some() && app.session.subscribed_threads.contains(&row.id);
        if !is_subscribed && open_thread_ids.contains(&row.id) {
            let viewed = is_viewed_thread_tab(app, &row.id, false);
            let prev = prev_statuses.get(&row.id).copied();
            if row.status == nuo_protocol::ThreadActivity::Running {
                app.session.unseen_outcomes.remove(&row.id);
            } else if !viewed && prev == Some(nuo_protocol::ThreadActivity::Running) {
                match row.status {
                    nuo_protocol::ThreadActivity::Idle => {
                        app.session.unseen_outcomes.insert(
                            row.id.clone(),
                            crate::shell::app::workspaces::UnseenOutcome::Succeeded,
                        );
                    }
                    nuo_protocol::ThreadActivity::Failed => {
                        app.session.unseen_outcomes.insert(
                            row.id.clone(),
                            crate::shell::app::workspaces::UnseenOutcome::Failed,
                        );
                    }
                    _ => {}
                }
            }
        }
    }
    app.session.host_sessions = rows;
    true
}

fn apply_server_endpoints(app: &mut App, report: nuo_protocol::ServerEndpointsReport) -> bool {
    let d = app
        .surfaces
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
        && app.surfaces.pre_attach.is_none()
    {
        tracing::info!("nuo: mounting PreAttach interstitial");
        app.surfaces.pre_attach = Some(state);
        // PreAttach claims the keyboard; reset the composer/sheet
        // state the way an ordinary sheet mount does.
        app.surfaces.modal_index = 0;
        app.park_focus_for_sheet();
    }
    true
}

fn apply_pre_attach_attestation_error(app: &mut App, message: String) -> bool {
    // Gate on the surface being mounted: with no gate up, a trust
    // notice is ordinary session flow (in-session `/trust`, the
    // attach banner) and must keep its transcript/toast path.
    if let Some(pa) = app.surfaces.pre_attach.as_mut() {
        tracing::warn!(
            %message,
            "nuo: PreAttach trust grant failed; resuming the decision surface"
        );
        pa.note_attestation_error(message);
    }
    true
}

fn apply_usage_stats(app: &mut App, report: nuo_protocol::usage_stats::UsageStatsReport) -> bool {
    app.telemetry.usage_stats = Some(report);
    app.telemetry.usage_loading = false;
    true
}

fn apply_provider_quotas(app: &mut App, snapshot: nuo_protocol::ProviderQuotaSnapshot) -> bool {
    for entry in &snapshot.entries {
        app.telemetry
            .connection_usages
            .insert(entry.name.clone(), entry.state.clone());
    }
    app.telemetry.provider_quotas = Some(snapshot);
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
    app.link.link_down = true;
    app.link.relink_requested = true;
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
            if app.provider_editor.awaiting_oauth_add {
                app.provider_editor.oauth_pending_url = url;
                app.provider_editor.oauth_pending_user_code = user_code;
                app.provider_editor.oauth_pending_message = message;
                app.provider_editor.oauth_pending_error = None;
                if !app
                    .surfaces
                    .surfaces
                    .contains_sheet(crate::shell::surfaces::SheetKind::OAuthPending)
                {
                    app.surfaces
                        .surfaces
                        .present_sheet(crate::shell::surfaces::SheetKind::OAuthPending);
                }
            }
        }
        Signal::Done => {
            if app.provider_editor.awaiting_oauth_add {
                app.open_oauth_instance_name_editor();
            }
        }
        Signal::Failed { message } => {
            if app.provider_editor.awaiting_oauth_add {
                app.provider_editor.oauth_pending_error = Some(message);
                if !app
                    .surfaces
                    .surfaces
                    .contains_sheet(crate::shell::surfaces::SheetKind::OAuthPending)
                {
                    app.surfaces
                        .surfaces
                        .present_sheet(crate::shell::surfaces::SheetKind::OAuthPending);
                }
            }
        }
    }
}

/// Execute one transcript edit against the target [`ThreadInstance`] (ADR-0253 §6).
///
/// Foreground and background threads take the exact same code path via
/// `app.threads.resolve_mut(thread_id)`. An unknown `thread_id` (never explicitly
/// registered) is dropped rather than implicitly materialised (`[INV-INSTANCE-03]`).
pub(crate) fn apply_transcript_for(
    app: &mut App,
    thread_id: Option<&str>,
    edit: TranscriptEdit,
) -> bool {
    let tui_config = app.session.tui_config.clone();
    let mut apply = TranscriptApply::new(tui_config);
    let (Some(inst), is_fg) = app.threads.resolve_mut(thread_id) else {
        return false;
    };
    apply_transcript_edit(&mut inst.messages, edit, &mut apply);
    match apply.post {
        PostEdit::None => {}
        PostEdit::Invalidate(id) => {
            inst.layout_height_cache
                .invalidate_messages(std::iter::once(id));
        }
        PostEdit::ClearCache => inst.layout_height_cache.clear(),
    }
    if apply.changed {
        inst.transcript_changed_pending = true;
    }
    if is_fg {
        if let Some(rejected) = apply.rejected_invocation {
            restore_rejected_command_draft(app, rejected);
        }
        apply.changed
    } else {
        false
    }
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
    tui_config: nuo_view::tui_config::TuiConfig,
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
    fn new(tui_config: nuo_view::tui_config::TuiConfig) -> Self {
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
    messages: &mut [TranscriptMessage],
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
    messages: &mut [TranscriptMessage],
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
    messages: &mut [TranscriptMessage],
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

#[allow(clippy::too_many_arguments)]
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

#[allow(clippy::ptr_arg)]
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
    messages: &mut [TranscriptMessage],
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
    messages: &mut [TranscriptMessage],
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
    if !app.composer.input.is_empty() {
        return;
    }
    app.composer.input = invocation;
    app.set_cursor_end();
    app.composer.suggestion_index = None;
    app.composer.completion_dismissed = true;
    app.refresh_backend_completion_request();
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
        assert!(app.composer.input.is_empty());
        restore_rejected_command_draft(&mut app, "/modles".to_string());
        assert_eq!(app.composer.input, "/modles");
        assert_eq!(
            app.composer.cursor_position,
            app.composer.input.len(),
            "caret is retype-ready"
        );
        assert!(
            app.composer.completion_dismissed,
            "the popup must not flash over a restored draft"
        );
    }

    #[test]
    fn rejected_command_never_clobbers_text_the_user_already_typed() {
        let mut app = new_app_for_relay_tests();
        app.composer.input = "next question".to_string();
        app.composer.cursor_position = app.composer.input.len();
        restore_rejected_command_draft(&mut app, "/modles".to_string());
        assert_eq!(
            app.composer.input, "next question",
            "the user's newer draft wins; the rejection stays in the transcript and in ↑ history"
        );
    }
}
