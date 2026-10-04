//! Session lifecycle, creation, resumption, branching, and runtime teardown operations.

use std::sync::Arc;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use super::SlashEnv;
use super::record::{record_command, record_error};
use crate::side::{SideRegistry, publish_btw_list};
use nuo_harness::Agent;
use nuo_harness::RoundLifecycle;
use crate::session_driver::send_harness_state_for_session;
use nuo_harness::orchestration::round_response;
use nuo_wire::{AgentNotice, AgentResponse, CommandResult, LoopStatus, RoundEvent};
use nuo_persistence::config::Config;
use nuo_persistence::session::SessionStore;

pub(crate) async fn supersede_for_session_switch(
    lifecycle: &RoundLifecycle,
    agent: &Agent,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::Superseded);
    lifecycle.supersede();
    agent.reject_pending_permissions();
    agent.reject_pending_user_questions();
    agent.reject_pending_inputs();
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);
    lifecycle.cancel_current().await;
}

pub(crate) fn apply_additional_roots(
    handle: &nuo_wire::SharedAdditionalRoots,
    effective: &Config,
    project_root: &std::path::Path,
) {
    let resolved = effective
        .resolve_workspace_additional_roots(project_root)
        .unwrap_or_default();
    handle.store(resolved);
}

pub async fn teardown_sides_for_session_switch(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let ids: Vec<String> = side.read().await.iter().map(|s| s.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    let was_active = side.read().await.active().is_some();
    for id in ids {
        if let Some(s) = side.write().await.remove(&id) {
            s.lifecycle
                .record_interrupt(nuo_wire::RoundInterruptReason::Superseded);
            s.agent.reject_pending_permissions();
            s.agent.reject_pending_user_questions();
            s.agent.reject_pending_inputs();
            s.lifecycle.cancel_current().await;
            let _ = s.store.delete(&s.id).await;
        }
    }
    publish_btw_list(side, resp_tx).await;
    if was_active {
        let _ = resp_tx.send(AgentResponse::SideViewClosed);
    }
}

pub(crate) async fn start_fresh_session(env: &mut SlashEnv<'_>, name: &str, args: &str) {
    let (side, session, config, agent, lifecycle, resp_tx, provider_for_task, shared_confinement) = (
        env.side,
        env.session,
        env.config,
        env.agent,
        env.lifecycle,
        env.resp_tx,
        env.provider_for_task,
        env.shared_confinement,
    );
    let provider_usage = &mut *env.provider_usage;
    supersede_for_session_switch(lifecycle, agent, resp_tx).await;
    teardown_sides_for_session_switch(side, resp_tx).await;
    agent.clear_todos();
    match session.reset().await {
        Ok(id) => {
            agent.set_thread_id(&id);
            let fresh_posture = session.unattended().await;
            if agent.unattended() != fresh_posture {
                agent.set_unattended(fresh_posture);
                let _ = resp_tx.send(round_response(
                    &id,
                    RoundEvent::UnattendedChanged(fresh_posture),
                ));
            }
            if !shared_confinement.is_confined() {
                shared_confinement.set_confined(true);
                let _ = resp_tx.send(round_response(&id, RoundEvent::ConfinementChanged(true)));
            }
            agent.restore_round_count(session.round_counter().await);
            crate::handlers_provider::reapply_session_selection(
                config,
                agent,
                provider_for_task,
                session,
                resp_tx,
                provider_usage,
            )
            .await;
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::TodosUpdated(nuo_wire::TodoList::default()),
            ));
            let _ = resp_tx.send(AgentResponse::ConversationCleared {
                session_id: session.id().await,
            });
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::HarnessState(nuo_wire::HarnessSnapshot {
                    loop_status: nuo_wire::LoopStatus::Idle,
                    round_counter: 0,
                    unattended: fresh_posture,
                    confined: shared_confinement.is_confined(),
                    workspace_security: agent.workspace_security(),
                    retry_pending: false,
                    role: session.role(),
                    workspace: session
                        .workspace()
                        .map(|w| w.root.to_string_lossy().to_string()),
                }),
            ));
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!("Started new session: {}", id)),
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

pub(crate) async fn switch_or_start_session_with_role(
    env: &mut SlashEnv<'_>,
    role_id: &str,
    target_workspace: Option<nuo_wire::WorkspaceBinding>,
    force_new: bool,
    name: &str,
    args: &str,
) {
    let partition =
        nuo_wire::SessionPartition::from_binding(target_workspace.as_ref(), Some(role_id));
    let latest_existing_id = if !force_new {
        nuo_persistence::db::get_persistence_handle()
            .reader()
            .ok()
            .and_then(|r| r.latest_session_in_partition(&partition).ok().flatten())
    } else {
        None
    };

    let active_id = env.session.id().await;
    if let Some(target_id) = latest_existing_id.filter(|id| id != &active_id) {
        // Resume existing session for this role
        let (
            side,
            session,
            config,
            agent,
            lifecycle,
            resp_tx,
            provider_for_task,
            shared_confinement,
            workspace_security,
        ) = (
            env.side,
            env.session,
            env.config,
            env.agent,
            env.lifecycle,
            env.resp_tx,
            env.provider_for_task,
            env.shared_confinement,
            env.workspace_security,
        );
        let provider_usage = &mut *env.provider_usage;
        supersede_for_session_switch(lifecycle, agent, resp_tx).await;
        teardown_sides_for_session_switch(side, resp_tx).await;
        agent.clear_todos();

        if let Some(switched) = agent.apply_role(role_id) {
            agent.set_project_root(target_workspace.as_ref().map(|w| w.root.clone()));
            if let Some(ws) = &target_workspace {
                let sec_snapshot = workspace_security.snapshot(&ws.root);
                agent.set_workspace_security(sec_snapshot);
            } else {
                agent.set_workspace_security(nuo_wire::WorkspaceSecuritySnapshot::new(
                    "workspace-free",
                ));
            }

            match session.open(&target_id).await {
                Ok(()) => {
                    restore_session_runtime(
                        session,
                        agent,
                        resp_tx,
                        nuo_wire::SessionSource::Resume,
                    )
                    .await;
                    let transcript = session.full_transcript().await;
                    let _ = resp_tx.send(AgentResponse::ConversationReplaced {
                        session_id: session.id().await,
                        messages: transcript,
                        commands: session.commands().await,
                        round_interrupts: session.round_interrupts().await,
                        retry_resolutions: session.retry_resolutions().await,
                    });
                    crate::handlers_provider::reapply_session_selection(
                        config,
                        agent,
                        provider_for_task,
                        session,
                        resp_tx,
                        provider_usage,
                    )
                    .await;
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::HarnessState(nuo_wire::HarnessSnapshot {
                            loop_status: nuo_wire::LoopStatus::Idle,
                            round_counter: session.round_counter().await,
                            unattended: session.unattended().await,
                            confined: shared_confinement.is_confined(),
                            workspace_security: agent.workspace_security(),
                            retry_pending: false,
                            role: Some(role_id.to_string()),
                            workspace: target_workspace
                                .as_ref()
                                .map(|w| w.root.to_string_lossy().to_string()),
                        }),
                    ));
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Resumed session {} with role `{}` (`{}`). Use `/role {role_id} --new` to start a fresh dialogue.",
                            crate::session_view::short_session_id(&target_id),
                            switched.name,
                            switched.id,
                        )),
                    )
                    .await;
                    return;
                }
                Err(e) => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!("Could not open session {target_id}: {e}"),
                    )
                    .await;
                    return;
                }
            }
        }
    }

    start_fresh_session_with_role(env, role_id, target_workspace, name, args).await;
}

pub(crate) async fn start_fresh_session_with_role(
    env: &mut SlashEnv<'_>,
    role_id: &str,
    target_workspace: Option<nuo_wire::WorkspaceBinding>,
    name: &str,
    args: &str,
) {
    let (
        side,
        session,
        config,
        agent,
        lifecycle,
        resp_tx,
        provider_for_task,
        shared_confinement,
        workspace_security,
    ) = (
        env.side,
        env.session,
        env.config,
        env.agent,
        env.lifecycle,
        env.resp_tx,
        env.provider_for_task,
        env.shared_confinement,
        env.workspace_security,
    );
    let provider_usage = &mut *env.provider_usage;
    supersede_for_session_switch(lifecycle, agent, resp_tx).await;
    teardown_sides_for_session_switch(side, resp_tx).await;
    agent.clear_todos();

    let switched = match agent.apply_role(role_id) {
        Some(s) => s,
        None => {
            record_error(
                session,
                resp_tx,
                name,
                args,
                format!("Could not apply role `{role_id}`"),
            )
            .await;
            return;
        }
    };
    agent.set_project_root(target_workspace.as_ref().map(|w| w.root.clone()));
    if let Some(ws) = &target_workspace {
        let sec_snapshot = workspace_security.snapshot(&ws.root);
        agent.set_workspace_security(sec_snapshot);
    } else {
        agent.set_workspace_security(nuo_wire::WorkspaceSecuritySnapshot::new(
            "workspace-free",
        ));
    }

    match session
        .reset_with(target_workspace.clone(), Some(role_id.to_string()))
        .await
    {
        Ok(id) => {
            agent.set_thread_id(&id);
            let fresh_posture = session.unattended().await;
            if agent.unattended() != fresh_posture {
                agent.set_unattended(fresh_posture);
                let _ = resp_tx.send(round_response(
                    &id,
                    RoundEvent::UnattendedChanged(fresh_posture),
                ));
            }
            let default_confined =
                if let Some(builtin) = nuo_wire::MainAgentRole::parse(role_id) {
                    builtin.default_confined()
                } else {
                    true
                };
            if shared_confinement.is_confined() != default_confined {
                shared_confinement.set_confined(default_confined);
                let _ = resp_tx.send(round_response(
                    &id,
                    RoundEvent::ConfinementChanged(default_confined),
                ));
            }
            agent.restore_round_count(session.round_counter().await);
            crate::handlers_provider::reapply_session_selection(
                config,
                agent,
                provider_for_task,
                session,
                resp_tx,
                provider_usage,
            )
            .await;
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::TodosUpdated(nuo_wire::TodoList::default()),
            ));
            let _ = resp_tx.send(AgentResponse::ConversationCleared {
                session_id: session.id().await,
            });
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::HarnessState(nuo_wire::HarnessSnapshot {
                    loop_status: nuo_wire::LoopStatus::Idle,
                    round_counter: 0,
                    unattended: fresh_posture,
                    confined: shared_confinement.is_confined(),
                    workspace_security: agent.workspace_security(),
                    retry_pending: false,
                    role: Some(role_id.to_string()),
                    workspace: target_workspace
                        .as_ref()
                        .map(|w| w.root.to_string_lossy().to_string()),
                }),
            ));
            let ws_info = if let Some(ws) = &target_workspace {
                format!(" with workspace `{}`", ws.root.display())
            } else {
                String::new()
            };
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "Started new session {id} with role `{}` (`{}`){ws_info} — {}. Conversation reset for this role.",
                    switched.name,
                    switched.id,
                    switched.description,
                )),
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

pub(crate) async fn restore_session_runtime(
    session: &Arc<SessionStore>,
    agent: &Arc<Agent>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    source: nuo_wire::SessionSource,
) {
    // Restore role and identity from the session's manifest (ADR-0245, ADR-0246) or role metadata
    if let Some(manifest) = session.role_manifest().await {
        let role_identity = manifest.identity.clone();
        let mut role = nuo_wire::AgentRoleProfile::with_identity(
            manifest.role_id.clone(),
            role_identity.clone(),
        );
        role.tools = nuo_wire::ToolSelection::from_allowlist(&manifest.tools);
        role.admit_mcp = manifest.admit_mcp.clone();
        if session.workspace_root().is_some() {
            role.extensions.push(Arc::new(
                nuo_harness::extension::CodeIntelligenceExtension::new(),
            ));
        }
        agent.apply_profile(&role);
        agent.set_active_role(Some(manifest.role_id));
    } else if let Some(role_id) = session.role() {
        let _ = agent.apply_role(&role_id);
    }
    agent.set_project_root(session.workspace_root());

    let todos = session.todos().await;
    agent.set_todos(todos.clone());
    let _ = resp_tx.send(round_response(
        &session.id().await,
        RoundEvent::TodosUpdated(todos),
    ));

    agent.restore_disabled_tools(session.disabled_tools().await);
    agent.restore_round_count(session.round_counter().await);

    let mut restored_unattended = session.unattended().await;
    let mut restored_from_ledger = false;
    if !restored_unattended {
        let commands = session.commands().await;
        let was_unattended_on = commands
            .iter()
            .rev()
            .find_map(|rec| {
                if (rec.name == "unattended"
                    || rec.name == "auto"
                    || rec.name == "delegate"
                    || rec.name == "autopilot"
                    || rec.name == "yolo")
                    && let Some(CommandResult::Ack { title, .. }) = &rec.result
                {
                    let title = title.to_lowercase();
                    if title.contains("on") {
                        return Some(true);
                    } else if title.contains("off") {
                        return Some(false);
                    }
                }
                None
            })
            .unwrap_or(false);
        if was_unattended_on {
            restored_unattended = true;
            restored_from_ledger = true;
            let _ = session.set_unattended(true).await;
        }
    }

    if agent.unattended() != restored_unattended {
        agent.set_unattended(restored_unattended);
        if restored_from_ledger {
            let notice = AgentNotice::new(
                nuo_wire::NoticeKind::CommandAck,
                nuo_wire::NoticeSeverity::Warning,
                "Unattended mode restored",
                nuo_wire::NoticeSource::Harness,
            )
            .with_surface(nuo_wire::NoticeSurface::Inline)
            .with_body(
                "This session was previously running in unattended execution mode. \
                 Use `/unattended off` to return to interactive mode.",
            );
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Notice(notice),
            ));
        }
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::UnattendedChanged(restored_unattended),
        ));
    }

    let mut messages = session.model_window().await;
    let before_len = messages.len();
    agent.fire_session_start(source, &mut messages).await;
    if messages.len() > before_len
        && let Err(err) = session.append_turn(&messages).await
    {
        tracing::warn!(error = %err, "failed to persist SessionStart hook context");
    }

    send_harness_state_for_session(
        resp_tx,
        &session.id().await,
        agent,
        session,
        LoopStatus::Idle,
    )
    .await;
}

pub(crate) async fn fork_current_session(
    lifecycle: &Arc<RoundLifecycle>,
    agent: &Agent,
    session: &Arc<SessionStore>,
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
) {
    supersede_for_session_switch(lifecycle, agent, resp_tx).await;
    teardown_sides_for_session_switch(side, resp_tx).await;
    match session.fork().await {
        Ok((id, parent_id)) => {
            agent.restore_round_count(session.round_counter().await);
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!("Forked session {} from {}.", id, parent_id)),
            )
            .await;
            send_harness_state_for_session(
                resp_tx,
                &session.id().await,
                agent,
                session,
                LoopStatus::Idle,
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}
