//! Per-command handlers for `AgentRequest::SlashCommand`.
//!
//! Each built-in lives in its own async fn so the dispatcher stays a thin
//! router. One giant async fn accumulated a ~1.6 MiB debug-build stack frame
//! (rustc does not reuse stack slots across a 1700-line state machine), which
//! overflowed the server's 2 MiB Tokio worker stack on every slash command.

use std::sync::Arc;

use super::SlashEnv;
use super::record::{
    record_ack, record_command, record_command_with_duration, record_error, record_invocation,
};
use super::security_ops::{TrustRoute, reload_trusted_assets, trust_route};
use super::session_ops::{
    fork_current_session, restore_session_runtime, start_fresh_session,
    supersede_for_session_switch, switch_or_start_session_with_role,
    teardown_sides_for_session_switch,
};
use super::session_route::{
    SessionRoute, parse_confinement_arg, parse_unattended_arg, session_route,
};
use crate::agent_setup::active_context_window;
use crate::project::init_nuo_config;
use crate::session_view::{build_sessions_overview, short_session_id};
use crate::side::{
    SideEnv, SideSession, publish_btw_list, refuse_if_no_provider, spawn_parent_status_watcher,
    start_active_turn,
};
use crate::slash_handler::SlashContext;
use crate::startup::BuiltinCmd;

use crate::session_driver::{compact_round_history, send_harness_state_for_session};
use nuo_harness::orchestration::{
    ContextProjectionSettings, RoundInput, round_response, send_compaction,
};
use nuo_wire::{
    AgentResponse, CommandResult, LoopStatus, Message, RoundEvent, Tool, TrustDomain,
    estimate_tokens,
};
use nuo_harness::skills::ListSkillsTool;

pub(crate) async fn permissions(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        agent,
        resp_tx,
        session,
        ..
    } = env;
    if parts.get(1) == Some(&"clear") {
        agent.clear_allowed_tools();
        record_ack(session, name, args, "Always-allowed tool rules cleared.").await;
    } else {
        let allowed = agent.allowed_tools();
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::PermissionList { allowed },
        )
        .await;
    }
}

pub(crate) async fn unattended(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        agent,
        resp_tx,
        session,
        ..
    } = env;
    let arg = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
    let next = match parse_unattended_arg(&arg) {
        Ok(next) => next,
        Err(msg) => {
            record_error(session, resp_tx, name, args, msg).await;
            return;
        }
    };
    // A bare `/unattended` (`None`) toggles the current state.
    let enabled = next.unwrap_or_else(|| !agent.unattended());
    agent.set_unattended(enabled);
    if let Err(error) = session.set_unattended(enabled).await {
        tracing::warn!(
            error = %error,
            "could not persist unattended posture; it will not survive a restart"
        );
    }
    // The ack is a headline plus dimmed explanation lines (never a
    // `•`-joined one-row squeeze); the command entry settles in place
    // with this body, so the mode change owns its own durable row.
    // The current posture is carried by the `UnattendedChanged` chip, not a
    // second transcript entry.
    let (title, detail) = if enabled {
        (
            "Unattended mode ON",
            vec![
                "Autonomous decision-making & tool execution enabled".to_string(),
                "Ambiguities resolved self-reliantly without interruptions".to_string(),
            ],
        )
    } else {
        (
            "Unattended mode OFF",
            vec![
                "Interactive confirmation prompts restored".to_string(),
                "Questions and approval prompts are available".to_string(),
            ],
        )
    };
    record_command(
        session,
        resp_tx,
        name,
        args,
        CommandResult::Ack {
            title: title.to_string(),
            detail: Some(detail.clone()),
        },
    )
    .await;
    let _ = resp_tx.send(round_response(
        &session.id().await,
        RoundEvent::UnattendedChanged(enabled),
    ));
}

pub(crate) async fn confinement(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        shared_confinement,
        resp_tx,
        session,
        ..
    } = env;
    let arg = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
    let next = match parse_confinement_arg(&arg) {
        Ok(next) => next,
        Err(msg) => {
            record_error(session, resp_tx, name, args, msg).await;
            return;
        }
    };
    // A bare `/confinement` (`None`) toggles the current state.
    let next_confined = next.unwrap_or_else(|| !shared_confinement.is_confined());
    shared_confinement.set_confined(next_confined);

    let (title, detail) = if next_confined {
        (
            "Workspace Confinement ON",
            vec![
                "File tools are confined to workspace root and temp paths".to_string(),
                "Escapes outside admitted roots will be blocked".to_string(),
            ],
        )
    } else {
        (
            "Workspace Confinement OFF (Unconfined File Access)",
            vec![
                "Tools may access and edit any file on the host system".to_string(),
                "Constrained only by server OS user permissions".to_string(),
            ],
        )
    };
    record_command(
        session,
        resp_tx,
        name,
        args,
        CommandResult::Ack {
            title: title.to_string(),
            detail: Some(detail),
        },
    )
    .await;
    let _ = resp_tx.send(round_response(
        &session.id().await,
        RoundEvent::ConfinementChanged(next_confined),
    ));
}

pub(crate) async fn role(mut env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        resp_tx, session, ..
    } = env;
    // /role [id] [workspace] — switch the live agent role.
    // Resolves the role or preset onto the live agent, applies the
    // resulting profile (identity preamble, capability scope, extensions,
    // runtime knobs, unattended posture), updates session workspace if applicable,
    // and records the session metadata.
    // With no argument, lists the available built-in and user roles.
    let target = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty());
    match target {
        None => {
            let roles_config = nuo_persistence::roles::RolesConfig::load_for_workspace(
                session.workspace_root().as_deref(),
            );
            let mut lines = Vec::new();

            let current = session
                .active_role()
                .await
                .or_else(|| session.role())
                .unwrap_or_else(|| "developer".to_string());
            let ws_str = session
                .workspace_root()
                .map(|r| format!(" ({})", r.display()))
                .unwrap_or_default();
            lines.push(format!("Active role: `{current}`{ws_str}\n"));

            lines.push("Available roles:".to_string());
            lines.push("  Built-in roles:".to_string());
            for preset in nuo_wire::MainAgentRole::ALL {
                lines.push(format!(
                    "    • `{}` — {}",
                    preset.as_str(),
                    preset.description()
                ));
            }

            if !roles_config.is_empty() {
                lines.push(String::new());
                lines.push("  User roles (~/.config/nuo/roles.toml):".to_string());
                for (id, p) in &roles_config.roles {
                    let desc = p.description.as_deref().unwrap_or(p.name.as_str());
                    let mcp_info =
                        if !p.admit_mcp.is_empty() && p.admit_mcp != vec!["*".to_string()] {
                            format!(" [mcp: {}]", p.admit_mcp.join(", "))
                        } else {
                            String::new()
                        };
                    let tools_info = if !p.tools.is_empty() && p.tools != vec!["*".to_string()] {
                        format!(" [tools: {}]", p.tools.join(", "))
                    } else {
                        String::new()
                    };
                    lines.push(format!("    • `{id}`{tools_info}{mcp_info} — {desc}"));
                }
            }

            lines.push(String::new());
            lines.push(
                "Usage: `/role ops`, `/role philosophist`, or `/role developer <workspace>`"
                    .to_string(),
            );

            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(lines.join("\n")),
            )
            .await;
        }
        Some(role_id) => {
            let roles_config = nuo_persistence::roles::RolesConfig::load_for_workspace(
                session.workspace_root().as_deref(),
            );
            let user_role = roles_config.get(role_id);
            let builtin = nuo_wire::MainAgentRole::parse(role_id);

            if user_role.is_none() && builtin.is_none() {
                let mut available: Vec<String> = nuo_wire::MainAgentRole::ALL
                    .iter()
                    .map(|p| format!("`{}`", p.as_str()))
                    .collect();
                for id in roles_config.ids() {
                    available.push(format!("`{id}`"));
                }
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    format!(
                        "Unknown role `{role_id}`. Available roles: {}.",
                        available.join(", ")
                    ),
                )
                .await;
                return;
            }

            let target_workspace = if builtin == Some(nuo_wire::MainAgentRole::Developer) {
                let ws_arg = parts
                    .get(2..)
                    .map(|p| p.join(" "))
                    .filter(|s| !s.trim().is_empty());
                let target_path = match ws_arg {
                    Some(ws_str) => {
                        let trimmed = ws_str.trim();
                        let raw_path = std::path::PathBuf::from(trimmed);
                        let resolved = if raw_path.starts_with("~") {
                            if let Some(home) = dirs::home_dir() {
                                if let Ok(stripped) = raw_path.strip_prefix("~") {
                                    home.join(stripped)
                                } else {
                                    raw_path
                                }
                            } else {
                                raw_path
                            }
                        } else {
                            raw_path
                        };
                        match std::fs::canonicalize(&resolved) {
                            Ok(canon) if canon.is_dir() => canon,
                            Ok(_) => {
                                record_error(
                                    session,
                                    resp_tx,
                                    name,
                                    args,
                                    format!("Workspace path `{trimmed}` is not a directory"),
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
                                    format!(
                                        "Workspace path `{trimmed}` does not exist or is inaccessible: {e}"
                                    ),
                                )
                                .await;
                                return;
                            }
                        }
                    }
                    None => {
                        if let Some(ws_root) = session.workspace_root() {
                            ws_root
                        } else {
                            record_error(
                                session,
                                resp_tx,
                                name,
                                args,
                                "Developer role requires a workspace path. Usage: `/role developer <workspace>`"
                                    .to_string(),
                            )
                            .await;
                            return;
                        }
                    }
                };
                Some(nuo_wire::WorkspaceBinding::new(target_path))
            } else if builtin.map(|b| !b.requires_workspace()).unwrap_or(false) {
                None
            } else if let Some(user_role) = user_role {
                match user_role.resolved_workspace() {
                    nuo_persistence::roles::RoleWorkspace::None => None,
                    nuo_persistence::roles::RoleWorkspace::Inherit => session.workspace(),
                    nuo_persistence::roles::RoleWorkspace::Fixed(root) => {
                        Some(nuo_wire::WorkspaceBinding::new(root))
                    }
                }
            } else {
                None
            };

            let force_new = parts.iter().any(|&p| p == "--new" || p == "-n");
            switch_or_start_session_with_role(
                &mut env,
                role_id,
                target_workspace,
                force_new,
                name,
                args,
            )
            .await;
        }
    }
}

pub(crate) async fn search(
    env: SlashEnv<'_>,
    cmd: &str,
    name: &str,
    args: &str,
    _parts: &[&str],
    start_instant: std::time::Instant,
) {
    let SlashEnv {
        resp_tx, session, ..
    } = env;
    let query = cmd.strip_prefix("/search").unwrap_or("").trim();
    if query.is_empty() {
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::Text("Usage: /search <query>".to_string()),
        )
        .await;
    } else {
        // Lexical ranking over the live transcript + command ledger.
        // The embedding-index machinery (persisted vectors from a
        // hash-based mock provider) was real cost with no semantics;
        // until a real embedding provider exists, `/search` is
        // deterministic lexical scoring over the in-memory transcript
        // — no index file, no rewrite per search.
        let messages = session.full_transcript().await;
        let commands = session.commands().await;
        let hits = crate::search_lexical::search(query, &messages, &commands, 5);
        let hits = hits
            .into_iter()
            .map(|hit| nuo_wire::SearchHit {
                text: hit.text,
                score: hit.score,
            })
            .collect::<Vec<_>>();
        if hits.is_empty() {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text("No matching messages in this session.".to_string()),
            )
            .await;
        } else {
            record_command_with_duration(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Search {
                    query: query.to_string(),
                    hits,
                },
                Some(start_instant.elapsed().as_millis() as u64),
            )
            .await;
        }
    }
}

pub(crate) async fn sessions(mut env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        config,
        agent,
        resp_tx,
        session,
        lifecycle,
        side,
        provider_for_task,
        ref mut provider_usage,
        ..
    } = env;
    // `/sessions` opens the picker; `/sessions <id>` opens that
    // session directly. The retired `/resume` and `/session`
    // spellings resolve here through the alias table, so their legacy
    // grammar is translated first:
    //   `/resume [id]`             → picker, or open <id>
    //   `/session`                  → picker
    //   `/session open|resume <id>`  → open <id> (picker without one)
    //   `/session list`             → picker
    //   `/session new`              → fresh session (same as `/new`)
    //   `/session fork`             → fork (same as `/fork`)
    //   `/session status`           → retired; error with guidance
    let route = match session_route(name, parts) {
        Ok(route) => route,
        Err(message) => {
            record_error(session, resp_tx, name, args, message).await;
            return;
        }
    };
    match route {
        SessionRoute::New => {
            // Rebuild a SlashEnv whose mutable usage cell points at
            // the same place, without partially moving `env` while
            // fields are still read for the rest of the match.
            let provider_usage = &mut *env.provider_usage;
            let mut fresh_env = SlashEnv {
                side: env.side,
                session: env.session,
                config: env.config,
                agent: env.agent,
                lifecycle: env.lifecycle,
                resp_tx: env.resp_tx,
                provider_for_task: env.provider_for_task,
                provider_usage,
                mcp_runtime: env.mcp_runtime,
                workspace_security: env.workspace_security,
                shared_additional_roots: env.shared_additional_roots,
                shared_confinement: env.shared_confinement,
                base_tools_for_side: env.base_tools_for_side,
                skills_registry: env.skills_registry,
                req_tx_for_commands: env.req_tx_for_commands,
                project_root_for_side: env.project_root_for_side,
                startup: env.startup,
                ui: env.ui,
                extra_commands: env.extra_commands,
                websearch_shared: env.websearch_shared,
                background_jobs: env.background_jobs,
            };
            start_fresh_session(&mut fresh_env, name, args).await;
        }
        SessionRoute::Fork => {
            fork_current_session(lifecycle, agent, session, side, resp_tx, name, args).await;
        }
        SessionRoute::Status => {
            record_error(
                session,
                resp_tx,
                name,
                args,
                "/session status is retired. Session id, counts, and timestamps now live \
                 in the /sessions info view (press i in the picker).",
            )
            .await;
        }
        SessionRoute::Open(target_id) => match target_id {
            // `/sessions <id>` (and the legacy `/resume <id>`,
            // `/session open <id>`, `/session list`) — the same flow
            // the picker's Enter key drives. Without an id, the
            // picker (the old "resume most recent" guess is gone).
            Some(id) => {
                supersede_for_session_switch(lifecycle, agent, resp_tx).await;
                teardown_sides_for_session_switch(side, resp_tx).await;
                match session.open(id).await {
                    Ok(()) => {
                        // Full restore of the session-scoped runtime the
                        // bootstrap skipped in Picker mode: todos,
                        // disabled tools, round counter, and SessionStart
                        // hooks. Opening a prior session is a resume.
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
                        // The live provider tracks the opened session's
                        // own provider pin (or the global default).
                        crate::handlers_provider::reapply_session_selection(
                            config,
                            agent,
                            provider_for_task,
                            session,
                            resp_tx,
                            provider_usage,
                        )
                        .await;
                        record_command(
                            session,
                            resp_tx,
                            name,
                            args,
                            CommandResult::Text(format!(
                                "Opened session {}.",
                                short_session_id(&session.id().await)
                            )),
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
            None => {
                // No id (bare `/sessions`, `/session list`, or a
                // legacy open/resume without one): open the picker.
                let overview_fut = build_sessions_overview(session);
                let record_fut = record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(String::new()),
                );
                let (overview, ()) = tokio::join!(overview_fut, record_fut);
                let _ = resp_tx.send(AgentResponse::SessionsOverview(overview));
                let _ = resp_tx.send(AgentResponse::OpenSessionsPanel);
            }
        },
    }
}

pub(crate) async fn fork(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        agent,
        resp_tx,
        session,
        lifecycle,
        side,
        ..
    } = env;
    fork_current_session(lifecycle, agent, session, side, resp_tx, name, args).await;
}

pub(crate) async fn tree(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        resp_tx, session, ..
    } = env;
    let tree = session.tree().await;
    let _ = resp_tx.send(AgentResponse::SessionTreeSnapshot {
        session_id: session.id().await,
        tree: tree.clone(),
    });
    let _ = resp_tx.send(AgentResponse::OpenTreePanel);
    record_ack(
        session,
        name,
        args,
        &format!(
            "Session DAG Tree has {} nodes and {} branch leaves.",
            tree.entries.len(),
            tree.leaves().len()
        ),
    )
    .await;
}

pub(crate) async fn diff(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv { session, .. } = env;
    record_ack(
        session,
        name,
        args,
        "Workspace diff tracking is active on the current conversation branch.",
    )
    .await;
}

pub(crate) async fn undo(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv { session, .. } = env;
    let tree = session.tree().await;
    if let Some(current_leaf) = tree.active_leaf()
        && let Some(parent_id) = tree
            .entries
            .get(&current_leaf)
            .and_then(|e| e.parent_id.clone())
    {
        let _ = session.switch_tree_leaf(&parent_id).await;
        record_ack(
            session,
            name,
            args,
            &format!(
                "Rolled back active conversation branch to parent node {}.",
                parent_id
            ),
        )
        .await;
    } else {
        record_ack(
            session,
            name,
            args,
            "Cannot undo: already at the root of the conversation tree.",
        )
        .await;
    }
}

pub(crate) async fn dashboard(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        resp_tx, session, ..
    } = env;
    record_invocation(session, name, args).await;
    // The session dashboard renders the monitor stream the TUI
    // maintains client-side (ADR-0096); this is only the open signal.
    let _ = resp_tx.send(AgentResponse::OpenHostPanel);
}

pub(crate) async fn usage(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv { session, .. } = env;
    // Handled in TUI: `/usage` opens the usage-statistics overlay
    // locally (intercepted in input.rs as `InputAction::OpenUsage`)
    // and issues `AgentRequest::QueryUsageStats`; it never arrives
    // here as a SlashCommand. Reaching this arm means a non-TUI
    // client (Web app) typed it — answer inline with a pointer so
    // the command is never silently dropped.
    record_ack(
        session,
        name,
        args,
        "Usage statistics are shown by the TUI overlay — open the terminal app and run /usage there.",
    )
    .await;
}

pub(crate) async fn quota(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv { session, .. } = env;
    record_ack(
        session,
        name,
        args,
        "Provider quotas are shown by CLI or TUI — run `nuo quota [provider]` in your terminal.",
    )
    .await;
}

pub(crate) async fn btw(env: SlashEnv<'_>, cmd: &str, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        config,
        agent,
        resp_tx,
        session,
        lifecycle,
        side,
        base_tools_for_side,
        provider_for_task,
        skills_registry,
        project_root_for_side,
        ..
    } = env;
    // `/btw` grammar (ADR-0103 §4):
    //   `/btw`        — open a NEW aside view (no round yet);
    //   `/btw <text>` — open a new aside and auto-send <text> as its
    //                   first turn;
    //   `/btw list`   — open the asides modal (same as F5).
    //
    // Opening forks the primary into a self-contained side file,
    // builds a fresh aside `Agent` + store, and switches the view.
    // The primary round keeps running untouched — unlike
    // `/session open`, we deliberately do NOT bump the generation
    // counter, reject permissions, or cancel the primary token.
    // Existing asides stay live in the registry: each `/btw` creates
    // an additional aside (ADR-0103 lifts ADR-0017's single slot).
    let rest = cmd.strip_prefix("/btw").unwrap_or("").trim();
    if rest == "list" {
        record_invocation(session, name, args).await;
        publish_btw_list(side, resp_tx).await;
        return;
    }
    let prompt = rest;
    let side_session = match SideSession::build(
        session,
        base_tools_for_side,
        provider_for_task,
        Arc::unwrap_or_clone((*skills_registry).clone()),
        project_root_for_side,
        agent.identity().clone(),
        agent.workspace_security_handle(),
    )
    .await
    {
        Ok(s) => s,
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
            return;
        }
    };
    let side_id = side_session.id.clone();
    if let Some(ledger) = agent.token_ledger() {
        side_session.agent.install_token_ledger(ledger.clone());
        ledger.restore_session(&side_id, side_session.store.request_usage_records().await);
    }
    let side_context = side_session
        .agent
        .estimate_next_request_tokens(&side_session.store.model_window().await)
        .total_tokens;
    let _ = resp_tx.send(round_response(
        &side_id,
        RoundEvent::ContextTokens(nuo_wire::ContextTokenSnapshot::new(
            side_context,
            nuo_wire::ContextTokenSource::Projection,
        )),
    ));
    // Register + make it the active view, then tell the TUI to enter
    // the aside view — `SideViewOpened` carries the aside's full
    // transcript (inherited parent context included, ADR-0103 §6)
    // and lands before the first aside round starts streaming.
    let side_for_titler = Arc::clone(side);
    let resp_tx_for_titler = (*resp_tx).clone();
    side_session
        .agent
        .set_title_established(std::sync::Arc::new(move |_title| {
            let side_registry = Arc::clone(&side_for_titler);
            let resp_tx = resp_tx_for_titler.clone();
            Box::pin(async move {
                publish_btw_list(&side_registry, &resp_tx).await;
            }) as futures::future::BoxFuture<'static, ()>
        }));

    side.write().await.open(side_session);
    crate::handlers_session::emit_side_view_opened(side, session, resp_tx, &side_id).await;
    publish_btw_list(side, resp_tx).await;
    record_invocation(session, name, args).await;
    // Stream coarse primary-status updates to the aside banner while
    // any aside is live. The watcher spans the whole registry and
    // self-terminates when the last aside closes; spawning a second
    // one while the first is still alive is harmless (both emit only
    // on change and dedupe through the shared last-value cell on the
    // TUI side).
    spawn_parent_status_watcher((*side).clone(), (*lifecycle).clone(), (*resp_tx).clone());
    if !prompt.is_empty() {
        start_active_turn(
            SideEnv {
                side,
                agent,
                primary_session: session,
                primary_lifecycle: lifecycle,
                tx: resp_tx,
                config,
            },
            RoundInput {
                prompt: prompt.to_string(),
                hidden: false,
                display_prompt: None,
                sent_at_ms: None,
                images: Vec::new(),
                driver: nuo_harness::orchestration::RoundDriver::Fresh,
            },
        )
        .await;
    }
}

pub(crate) async fn compact(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        config,
        agent,
        resp_tx,
        session,
        ..
    } = env;
    let mut current = session.model_window().await;
    let settings = ContextProjectionSettings::from_context_policy(&config.context, active_context_window(agent))
        .for_request(agent.estimate_next_request_tokens(&current));

    let _ = resp_tx.send(round_response(
        &session.id().await,
        nuo_wire::RoundEvent::Notice(
            nuo_wire::AgentNotice::new(
                nuo_wire::NoticeKind::CommandAck,
                nuo_wire::NoticeSeverity::Info,
                "Compacting Context",
                nuo_wire::NoticeSource::Harness,
            )
            .with_body("Synthesizing completed rounds into structured checkpoint..."),
        ),
    ));

    let mut extra = agent.fire_pre_compact().await;
    let trimmed = args.trim();
    if !trimmed.is_empty() {
        extra.push(format!("User Focus / Instructions for Compaction: {}", trimmed));
    }

    // ADR-0296: Universal Causal Compaction folds [0..N-1] and preserves round N
    match compact_round_history(
        &mut current,
        session,
        &settings,
        Some(agent.provider.clone()),
        extra,
    )
    .await
    {
        Ok(Some(checkpoint)) => {
            record_invocation(session, name, args).await;
            send_compaction(resp_tx, &session.id().await, &checkpoint);
        }
        Ok(None) => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text("No completed dialogue history to compact yet.".to_string()),
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
    agent.fire_post_compact().await;
}

pub(crate) async fn jobs(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        resp_tx,
        session,
        background_jobs,
        ..
    } = env;
    let sub = parts.get(1).copied().unwrap_or("list");
    match sub {
        "kill" => {
            if let Some(target_id) = parts.get(2) {
                let jid = nuo_wire::JobId(target_id.to_string());
                match background_jobs.kill_job(&jid) {
                    Ok(()) => {
                        record_ack(
                            session,
                            name,
                            args,
                            format!("Terminated background job {}.", target_id),
                        )
                        .await;
                    }
                    Err(err) => {
                        record_error(
                            session,
                            resp_tx,
                            name,
                            args,
                            &format!("Failed to kill job {}: {}", target_id, err),
                        )
                        .await;
                    }
                }
            } else {
                record_error(session, resp_tx, name, args, "Usage: /jobs kill <job_id>").await;
            }
        }
        "logs" => {
            if let Some(target_id) = parts.get(2) {
                let jid = nuo_wire::JobId(target_id.to_string());
                match background_jobs.get_logs(&jid, 50) {
                    Some(lines) => {
                        let output = if lines.is_empty() {
                            "(no logs recorded yet)".to_string()
                        } else {
                            lines.join("\n")
                        };
                        record_command(
                            session,
                            resp_tx,
                            name,
                            args,
                            CommandResult::Text(format!(
                                "Logs for {}:\n```\n{}\n```",
                                target_id, output
                            )),
                        )
                        .await;
                    }
                    None => {
                        record_error(
                            session,
                            resp_tx,
                            name,
                            args,
                            &format!("Job not found: {}", target_id),
                        )
                        .await;
                    }
                }
            } else {
                record_error(session, resp_tx, name, args, "Usage: /jobs logs <job_id>").await;
            }
        }
        _ => {
            let jobs = background_jobs.list_jobs();
            if jobs.is_empty() {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text("No active or recent background jobs.".to_string()),
                )
                .await;
            } else {
                let mut table = String::from(
                    "### Background Jobs\n\n| ID | Type | State | Latest Output |\n|---|---|---|---|\n",
                );
                for j in jobs {
                    let (kind_str, detail) = match &j.spec {
                        nuo_wire::JobSpec::Process { command, label, .. } => (
                            label.clone().unwrap_or_else(|| "process".to_string()),
                            command.clone(),
                        ),
                        nuo_wire::JobSpec::Timer { label, prompt, .. } => (
                            label.clone().unwrap_or_else(|| "timer".to_string()),
                            prompt.clone(),
                        ),
                    };
                    let status_str = match &j.state {
                        nuo_wire::JobState::Queued => "Queued".to_string(),
                        nuo_wire::JobState::Running { pid, .. } => {
                            if let Some(p) = pid {
                                format!("Running (PID {p})")
                            } else {
                                "Running".to_string()
                            }
                        }
                        nuo_wire::JobState::Ready { .. } => "Ready (service)".to_string(),
                        nuo_wire::JobState::Succeeded { duration_ms, .. } => {
                            format!("✓ Passed ({}s)", duration_ms / 1000)
                        }
                        nuo_wire::JobState::Failed {
                            duration_ms,
                            exit_code,
                            ..
                        } => {
                            format!("✗ Failed (Exit {exit_code}, {}s)", duration_ms / 1000)
                        }
                        nuo_wire::JobState::Killed { duration_ms } => {
                            format!("Killed ({}s)", duration_ms / 1000)
                        }
                        nuo_wire::JobState::TimedOut { duration_ms } => {
                            format!("Timed Out ({}s)", duration_ms / 1000)
                        }
                    };
                    let latest = j.latest_output.as_deref().unwrap_or(detail.as_str());
                    let truncated_latest = if latest.len() > 60 {
                        format!("{}...", &latest[..57])
                    } else {
                        latest.to_string()
                    };
                    table.push_str(&format!(
                        "| `{}` | {} | {} | `{}` |\n",
                        j.id.0, kind_str, status_str, truncated_latest
                    ));
                }
                record_command(session, resp_tx, name, args, CommandResult::Text(table)).await;
            }
        }
    }
}

pub(crate) async fn init(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        resp_tx, session, ..
    } = env;
    let target = parts.get(1).copied().unwrap_or(".");
    match init_nuo_config(std::path::Path::new(target)) {
        Ok(created) if created.is_empty() => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "nuo is already configured in '{}'. Nothing to do.",
                    target
                )),
            )
            .await;
        }
        Ok(created) => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "Initialized nuo configuration in '{}'.\nCreated:\n{}",
                    target,
                    created
                        .iter()
                        .map(|path| format!("- {}", path))
                        .collect::<Vec<_>>()
                        .join("\n")
                )),
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

pub(crate) async fn trust(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        agent,
        mcp_runtime,
        workspace_security,
        shared_additional_roots,
        resp_tx,
        session,
        skills_registry,
        project_root_for_side,
        ..
    } = env;
    let route = match trust_route(name, parts) {
        Ok(route) => route,
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
            return;
        }
    };
    let Some(project_root_for_side) = project_root_for_side else {
        match route {
            TrustRoute::GrantAll | TrustRoute::Grant(TrustDomain::UserAssets) => {
                let user_granted = crate::handlers_slash::security_ops::trust_user_assets();
                let effective = nuo_persistence::config::Config::load();
                let mcp_report = mcp_runtime.reconfigure(effective.mcp.clone()).await;
                let mut snap = nuo_wire::WorkspaceSecuritySnapshot::new("workspace-free");
                snap.user_assets = crate::handlers_slash::security_ops::compute_user_assets_trust();
                agent.set_workspace_security(snap.clone());
                let user_granted_str = if user_granted.is_empty() {
                    "none".to_string()
                } else {
                    user_granted.join(", ")
                };
                let message = format!(
                    "User asset trust recorded (30-day lease granted).\n\
                     - Granted: {}\n\
                     - Connected MCP: {}",
                    user_granted_str,
                    if mcp_report.connected.is_empty() {
                        "none".to_string()
                    } else {
                        mcp_report
                            .connected
                            .iter()
                            .filter_map(|(n, ok)| ok.then_some(n.as_str()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                );
                record_command(session, resp_tx, name, args, CommandResult::Text(message)).await;
            }
            TrustRoute::Status => {
                let mut snapshot = nuo_wire::WorkspaceSecuritySnapshot::new("workspace-free");
                snapshot.user_assets =
                    crate::handlers_slash::security_ops::compute_user_assets_trust();
                agent.set_workspace_security(snapshot.clone());
                let message = format!(
                    "Session Asset Trust (Workspace-Free)\n\
                     - User Assets: {}\n\
                     - Aggregate: {}\n\
                     Asset trust does not grant runtime execution permission.",
                    snapshot.user_assets.as_str(),
                    snapshot.aggregate().as_str(),
                );
                record_command(session, resp_tx, name, args, CommandResult::Text(message)).await;
            }
            _ => {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "project asset trust is unavailable in a workspace-free session".to_string(),
                )
                .await;
            }
        }
        send_harness_state_for_session(
            resp_tx,
            &session.id().await,
            agent,
            session,
            LoopStatus::Idle,
        )
        .await;
        return;
    };
    match route {
        TrustRoute::Status => {
            let mut snapshot = workspace_security.snapshot(project_root_for_side);
            snapshot.user_assets = crate::handlers_slash::security_ops::compute_user_assets_trust();
            agent.set_workspace_security(snapshot.clone());
            let message = format!(
                "Workspace Asset Trust\n\
                 - Root: {}\n\
                 - Instructions: {}\n\
                 - Ex-Workspace: {}\n\
                 - MCP: {}\n\
                 - Skills: {}\n\
                 - Hooks: {}\n\
                 - User Assets: {}\n\
                 - Aggregate: {}\n\
                 Asset trust does not grant runtime execution permission.",
                snapshot.root,
                snapshot.instructions.as_str(),
                snapshot.ex_workspace.as_str(),
                snapshot.mcp.as_str(),
                snapshot.skills.as_str(),
                snapshot.hooks.as_str(),
                snapshot.user_assets.as_str(),
                snapshot.aggregate().as_str(),
            );
            record_command(session, resp_tx, name, args, CommandResult::Text(message)).await;
        }
        TrustRoute::GrantAll | TrustRoute::Grant(_) => {
            let domains: &[TrustDomain] = match route {
                TrustRoute::GrantAll => &TrustDomain::ALL,
                TrustRoute::Grant(ref domain) => std::slice::from_ref(domain),
                _ => unreachable!(),
            };
            let should_trust_user = route == TrustRoute::GrantAll
                || matches!(route, TrustRoute::Grant(TrustDomain::UserAssets));
            let user_granted = if should_trust_user {
                crate::handlers_slash::security_ops::trust_user_assets()
            } else {
                Vec::new()
            };

            let ws_domains: Vec<TrustDomain> = domains
                .iter()
                .copied()
                .filter(|d| *d != TrustDomain::UserAssets)
                .collect();
            let mut granted = if ws_domains.is_empty() {
                Vec::new()
            } else {
                match workspace_security.trust_domains(project_root_for_side, &ws_domains) {
                    Ok(granted) => granted,
                    Err(error) => {
                        record_error(session, resp_tx, name, args, error).await;
                        return;
                    }
                }
            };
            if !user_granted.is_empty() {
                granted.push(TrustDomain::UserAssets);
            }
            let report = match reload_trusted_assets(
                agent,
                mcp_runtime,
                workspace_security,
                project_root_for_side,
                skills_registry,
                shared_additional_roots,
            )
            .await
            {
                Ok(report) => report,
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                    return;
                }
            };
            let granted_str = if granted.is_empty() {
                "none (the selected domains have no assets)".to_string()
            } else {
                granted
                    .iter()
                    .map(|domain| domain.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mcp = if report.connected_mcp.is_empty() {
                String::new()
            } else {
                format!("\n- MCP connected: {}", report.connected_mcp.join(", "))
            };
            let message = format!(
                "Asset trust recorded (30-day lease granted).\n\
                 - Root: {}\n\
                 - Granted: {}\n\
                 - Instructions: {}; Ex-Workspace: {}; MCP: {}; Skills: {}; Hooks: {}; User Assets: {}{}",
                report.snapshot.root,
                granted_str,
                report.snapshot.instructions.as_str(),
                report.snapshot.ex_workspace.as_str(),
                report.snapshot.mcp.as_str(),
                report.snapshot.skills.as_str(),
                report.snapshot.hooks.as_str(),
                report.snapshot.user_assets.as_str(),
                mcp,
            );
            record_command(session, resp_tx, name, args, CommandResult::Text(message)).await;
        }
        TrustRoute::Revoke => {
            let revoked = match workspace_security.revoke_workspace(project_root_for_side) {
                Ok(revoked) => revoked,
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                    return;
                }
            };
            let report = match reload_trusted_assets(
                agent,
                mcp_runtime,
                workspace_security,
                project_root_for_side,
                skills_registry,
                shared_additional_roots,
            )
            .await
            {
                Ok(report) => report,
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                    return;
                }
            };
            let removed = if report.removed_mcp.is_empty() {
                String::new()
            } else {
                format!(" Disconnected MCP: {}.", report.removed_mcp.join(", "))
            };
            let message = if revoked {
                format!(
                    "Project asset trust revoked. Instructions, external workspaces, MCP, skills, and hooks were unloaded.{removed}"
                )
            } else {
                "No project asset grants were recorded for this workspace.".to_string()
            };
            record_command(session, resp_tx, name, args, CommandResult::Text(message)).await;
        }
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

pub(crate) async fn skills(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        resp_tx,
        session,
        skills_registry,
        ..
    } = env;
    let sub = parts.get(1).copied().unwrap_or("list");
    match sub {
        "list" => {
            let tool = ListSkillsTool {
                registry: (*skills_registry).clone(),
            };
            match tool.call("{}").await {
                Ok(output) => {
                    record_command(session, resp_tx, name, args, CommandResult::Text(output)).await;
                }
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                }
            }
        }
        "status" => {
            let status_lines = {
                let guard = skills_registry.lock();
                let list = guard.list();
                let total = list.len();
                let quarantined = list.iter().filter(|s| s.quarantined).count();
                let enabled = list.iter().filter(|s| s.enabled && !s.quarantined).count();
                let user_count = list
                    .iter()
                    .filter(|s| s.scope == nuo_harness::skills::SkillScope::User)
                    .count();
                let repo_count = list
                    .iter()
                    .filter(|s| s.scope == nuo_harness::skills::SkillScope::Repo)
                    .count();
                let extra_count = list
                    .iter()
                    .filter(|s| s.scope == nuo_harness::skills::SkillScope::Extra)
                    .count();
                let remote_count = list
                    .iter()
                    .filter(|s| s.scope == nuo_harness::skills::SkillScope::Remote)
                    .count();

                let mut lines = vec![
                    format!(
                        "Skills Status: {total} total ({enabled} enabled, {quarantined} quarantined)"
                    ),
                    format!("  • User: {user_count}"),
                    format!("  • Repo: {repo_count}"),
                    format!("  • Extra: {extra_count}"),
                    format!("  • Remote: {remote_count}"),
                ];
                if quarantined > 0 {
                    lines.push("\nNote: Quarantined skills require authorization. Run `/trust skills` or `/trust` to enable them.".to_string());
                }
                lines.join("\n")
            };
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(status_lines),
            )
            .await;
        }
        "reload" => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(
                    "Manual `/skills reload` has been retired (ADR-0165).\n\
                     Skills update automatically via reactive file watching.\n\
                     To authorize project skills in a new workspace, run `/trust skills`."
                        .to_string(),
                ),
            )
            .await;
        }
        other => {
            record_error(
                session,
                resp_tx,
                name,
                args,
                format!(
                    "Unknown skills command '{}'. Use 'list' or 'status'.",
                    other
                ),
            )
            .await;
        }
    }
}

pub(crate) async fn new_session(mut env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    // `/new` never wipes anything in place: it starts a fresh session
    // and leaves the current one on disk (resumable via `/sessions`
    // or `/session open`). The retired `/clear` resolves here through
    // the alias table, so old muscle memory gets the safe semantics.
    start_fresh_session(&mut env, name, args).await;
}

pub(crate) async fn export(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        agent,
        resp_tx,
        session,
        ui,
        ..
    } = env;
    let messages = session.model_window().await;
    let commands = session.commands().await;
    let session_id = session.id().await;
    let provider_id = agent.provider.provider_id();
    let model_name = agent.provider.model();
    let markdown = crate::export::format_export_markdown(
        crate::export::ExportContext {
            session_id: &session_id,
            provider: &provider_id,
            model: &model_name,
        },
        &messages,
        &commands,
    );
    let char_count = markdown.chars().count();
    match ui.copy_to_clipboard(&markdown).await {
        Ok(crate::CopyOutcome::Native) => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "Session exported to clipboard ({} messages, {} chars). \
                                     Paste it into another agent to continue this work.",
                    messages.len(),
                    char_count
                )),
            )
            .await;
        }
        Ok(crate::CopyOutcome::Osc52) => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "Session exported via OSC52 ({} messages, {} chars). \
                                     If your terminal did not capture it, run nuo in a \
                                     clipboard-capable environment.",
                    messages.len(),
                    char_count
                )),
            )
            .await;
        }
        Err(_) => {
            let _ = resp_tx.send(AgentResponse::CopyToClipboard { text: markdown });
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "Session exported to clipboard ({} messages, {} chars). \
                     Paste it into another agent to continue this work.",
                    messages.len(),
                    char_count
                )),
            )
            .await;
        }
    }
}

pub(crate) async fn debug(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        agent,
        resp_tx,
        session,
        project_root_for_side,
        ..
    } = env;
    // /debug trace on|off — arm/disarm semantic call tracing at
    // the ProxyProvider layer. Each provider round-trip (request
    // messages + streamed/returned response) is then written as one
    // JSON file under the per-project `network/` directory. Captures
    // the `Vec<Message>` exchange — not raw HTTP bytes — so API keys
    // in headers/query strings never land on disk.
    match parts.get(1).copied() {
        Some("trace") => {
            let next = match parts.get(2).map(|s| s.to_lowercase()).as_deref() {
                Some("on") | Some("true") | Some("1") => Some(true),
                Some("off") | Some("false") | Some("0") => Some(false),
                Some(other) => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!("Unknown value '{other}'. Use `/debug trace on|off`."),
                    )
                    .await;
                    return;
                }
                None => None,
            };
            let enabled = next.unwrap_or_else(|| !agent.provider.debug_capture_enabled());
            let Some(debug_root) = project_root_for_side else {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "`/debug trace` requires a workspace".to_string(),
                )
                .await;
                return;
            };
            let dir = nuo_persistence::paths::get().project_network_dir(debug_root);
            agent.provider.set_debug_capture(enabled, dir.clone());
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "Trace {}: each provider round-trip {} written to\n  {}",
                    if enabled { "ON" } else { "OFF" },
                    if enabled { "is" } else { "will no longer be" },
                    dir.display(),
                )),
            )
            .await;
        }
        Some("preview") => {
            // /debug preview — dev-only dry run. Projects the *wire*
            // body of the next request (the minimal shape the provider
            // serializes) to disk, with a simulated `This is a test.`
            // probe user message appended so the snapshot reflects
            // "what the LLM context would look like if the user sent
            // this now". Out-of-band fields (nested subagent children,
            // subagent_meta, attribution, origin, hidden) are stripped via
            // `Message::to_wire` — the dump shows what the model
            // actually sees, not the internal `Message` struct that
            // also carries durable-session sidecars. NO provider call
            // is made; nothing is mutated. Reported to the transcript
            // as a single summary line — the on-disk JSON is the source
            // of truth for details.
            let messages = {
                let mut snapshot = session.model_window().await;
                // Append the probe BEFORE prepare so it participates in
                // implicit-skill injection and lands as the final wire
                // user message the provider would receive.
                snapshot.push(Message::new(nuo_wire::Role::User, "This is a test."));
                agent.prepare_request_messages_debug(&mut snapshot);
                // Project to the wire form: this is what the provider
                // request body would contain (no children / sidecars).
                snapshot
                    .into_iter()
                    .map(|m| m.to_wire())
                    .collect::<Vec<_>>()
            };
            let provider_id = agent.provider.provider_id();
            let model_name = agent.provider.model();
            let window = active_context_window(agent);
            let tokens = estimate_tokens(&messages);
            let wire_bytes = messages.iter().map(|m| m.content.len()).sum::<usize>();
            let session_id = session.id().await;
            let timestamp = chrono::Utc::now();
            let pressure_pct = if window > 0 {
                (tokens as f64 / window as f64 * 100.0).round() as u64
            } else {
                0
            };
            let n_tools = agent.installed_tools().len();

            // Persist the full record (raw messages + tool schemas)
            // for offline inspection.
            let Some(debug_root) = project_root_for_side else {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "`/debug preview` requires a workspace".to_string(),
                )
                .await;
                return;
            };
            let dir = nuo_persistence::paths::get().project_debug_dir(debug_root);
            let stamp = timestamp.format("%Y%m%d-%H%M%S%.3f");
            let file = dir.join(format!("{stamp}_preview.json"));
            let record = serde_json::json!({
                "timestamp": timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "session_id": session_id,
                "provider": provider_id,
                "model": model_name,
                "context_window_tokens": window,
                "estimated_tokens": tokens,
                // wire-size diagnostic: bytes remain the honest unit
                // for the transport view (ADR-0120 keeps this one).
                "estimated_wire_bytes": wire_bytes,
                "pressure_pct": pressure_pct,
                "tools": agent
                    .installed_tools()
                    .iter()
                    .map(|tool| tool.to_openai_function())
                    .collect::<Vec<_>>(),
                "messages": messages,
            });
            let file_path = file.display().to_string();
            match serde_json::to_vec_pretty(&record) {
                Ok(bytes) => {
                    if let Err(error) = nuo_host::fsutil::atomic_write_bytes(&file, &bytes)
                    {
                        record_error(
                            session,
                            resp_tx,
                            name,
                            args,
                            format!("Preview write failed: {error}"),
                        )
                        .await;
                        return;
                    }
                }
                Err(error) => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!("Preview serialize failed: {error}"),
                    )
                    .await;
                    return;
                }
            }

            let window_str = if window > 0 {
                format!("of {window} ({pressure_pct}%)")
            } else {
                "of unknown window".to_string()
            };
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "Preview (dry run, wire body, probe \"This is a test.\") — \
                     {provider_id}/{model_name}: ~{tokens} tokens {window_str}, {} \
                     message(s), {n_tools} tool(s). Full JSON: {file_path}",
                    messages.len(),
                )),
            )
            .await;
        }
        Some(other) => {
            record_error(
                session,
                resp_tx,
                name,
                args,
                format!(
                    "Unknown debug target '{other}'. Available: trace, preview. \
                     Usage: `/debug trace on|off` or `/debug preview`."
                ),
            )
            .await;
        }
        None => {
            let trace_on = agent.provider.debug_capture_enabled();
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!(
                    "Debug status:\n- trace: {}\n\nUsage:\n\
                     - `/debug trace on|off` — trace each provider round-trip\n\
                     - `/debug preview` — dry-run the next request to disk",
                    if trace_on { "ON" } else { "OFF" },
                )),
            )
            .await;
        }
    }
}

pub(crate) async fn retry(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        config,
        agent,
        resp_tx,
        session,
        lifecycle,
        side,
        ..
    } = env;
    if lifecycle.is_running().await {
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::Text("Cannot retry while a round is already running.".to_string()),
        )
        .await;
        return;
    }
    // `/retry` exists to let a stopped round finish itself
    // (ADR-0128): it resumes the parked round with the same number
    // and an unbroken turn sequence. It is deliberately a no-op for
    // a round that completed naturally — re-sending a finished
    // round's history would mint a new round and duplicate the
    // assistant's answer, so without an armed resume point there is
    // simply nothing to do.
    let Some(point) = session.retry_pending().await else {
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::Text("Nothing to retry — the last round already completed.".to_string()),
        )
        .await;
        return;
    };
    // A newer round (or `/new`) retires the point; a stale point for
    // an older round must never resurrect a round the session has
    // moved past.
    if point.round != session.round_counter().await {
        if let Err(error) = session.clear_retry_pending().await {
            tracing::warn!(%error, "could not clear stale retry point");
        }
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::Text("Nothing to retry — the last round already completed.".to_string()),
        )
        .await;
        return;
    }
    if refuse_if_no_provider(resp_tx, agent, session, &session.id().await).await {
        return;
    }
    record_invocation(session, name, args).await;
    start_active_turn(
        SideEnv {
            side,
            agent,
            primary_session: session,
            primary_lifecycle: lifecycle,
            tx: resp_tx,
            config,
        },
        RoundInput::resume(point),
    )
    .await;
}

pub(crate) async fn help(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        resp_tx,
        session,
        extra_commands,
        ..
    } = env;
    let extra_help = if extra_commands.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nExtension commands:\n{}",
            extra_commands
                .list()
                .into_iter()
                .map(|(name, desc)| format!("/{name} — {desc}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let mut lines = vec!["Slash commands:".to_string()];
    for (name, desc) in BuiltinCmd::ALL {
        lines.push(format!("{name:<13} — {desc}"));
    }
    record_command(
        session,
        resp_tx,
        name,
        args,
        CommandResult::Text(format!("{}\n{extra_help}", lines.join("\n\n"))),
    )
    .await;
}

pub(crate) async fn exit(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        resp_tx, session, ..
    } = env;
    record_invocation(session, name, args).await;
    let _ = resp_tx.send(AgentResponse::Exit);
}

pub(crate) async fn user_command(
    mut env: SlashEnv<'_>,
    cmd: &str,
    name: &str,
    args: &str,
    parts: &[&str],
) {
    let SlashEnv {
        config,
        agent,
        resp_tx,
        session,
        lifecycle,
        side,
        base_tools_for_side,
        provider_for_task,
        ref mut provider_usage,
        skills_registry,
        req_tx_for_commands,
        project_root_for_side,
        startup,
        ui,
        extra_commands,
        ..
    } = env;
    // Application-registered Rust handlers (extension point): try the
    // extra-commands registry. A handler that returns `true` fully handled
    // it; otherwise fall through to the unknown-command path.
    let name_no_slash = parts[0].strip_prefix('/').unwrap_or(parts[0]);
    if let Some(handler) = extra_commands.get(name_no_slash) {
        let ctx = SlashContext {
            cmd,
            parts,
            config,
            agent,
            resp_tx,
            session,
            lifecycle,
            side,
            base_tools: base_tools_for_side,
            provider_holder: provider_for_task,
            provider_usage,
            skills_registry,
            req_tx: req_tx_for_commands,
            project_root: project_root_for_side,
            startup,
            ui,
        };
        if handler.handle(ctx).await {
            return;
        }
    }
    record_error(
        session,
        resp_tx,
        name,
        args,
        format!("Unknown command: {}", parts[0]),
    )
    .await;
}
