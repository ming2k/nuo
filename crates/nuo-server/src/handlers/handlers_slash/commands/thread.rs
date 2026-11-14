//! Session, thread, and role commands.

use super::super::record::{
    record_ack, record_command, record_command_with_duration, record_error,
};
use super::super::session_ops::{
    fork_current_session, restore_session_runtime, start_fresh_session,
    supersede_for_session_switch, switch_or_start_session_with_role,
};
use super::super::session_route::{SessionRoute, session_route};
use crate::handlers::handlers_slash::SlashEnv;
use crate::runtime::thread_driver::send_harness_state_for_session;
use crate::transport::thread_view::{build_sessions_overview, short_session_id};
use nuo_protocol::{AgentResponse, CommandResult, LoopStatus};

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
            for preset in nuo_protocol::MainAgentRole::ALL {
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
                "Usage: `/role ops`, `/role philosophist`, or `/role developer <workspace>`.\n\
                 A thread is owned by exactly one role: `/role` resumes or starts that role's thread; it never rebinds the current one."
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
            let role_id_owned = match nuo_persistence::roles::canonical_role_id(
                role_id,
                session.workspace_root().as_deref(),
            ) {
                Ok(role) => role,
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                    return;
                }
            };
            let role_id = role_id_owned.as_str();
            let roles_config = nuo_persistence::roles::RolesConfig::load_for_workspace(
                session.workspace_root().as_deref(),
            );
            let user_role = roles_config.get(role_id);
            let builtin = nuo_protocol::MainAgentRole::parse(role_id);

            if user_role.is_none() && builtin.is_none() {
                let mut available: Vec<String> = nuo_protocol::MainAgentRole::ALL
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

            let policy = if let Some(user_role) = user_role {
                user_role.resolved_workspace()
            } else {
                builtin
                    .map(nuo_protocol::MainAgentRole::workspace_policy)
                    .unwrap_or(nuo_persistence::roles::RoleWorkspace::Unbound)
            };
            let ws_arg = parts
                .get(2..)
                .map(|p| p.join(" "))
                .filter(|s| !s.trim().is_empty());
            let target_workspace = match policy {
                nuo_persistence::roles::RoleWorkspace::Unbound => {
                    if ws_arg.is_some() {
                        record_error(
                            session,
                            resp_tx,
                            name,
                            args,
                            format!(
                                "Role `{role_id}` is workspace-free; it does not accept a workspace path."
                            ),
                        )
                        .await;
                        return;
                    }
                    None
                }
                nuo_persistence::roles::RoleWorkspace::Fixed(root) => {
                    if ws_arg.is_some() {
                        record_error(
                            session,
                            resp_tx,
                            name,
                            args,
                            format!(
                                "Role `{role_id}` is pinned to `{}`; omit the workspace argument.",
                                root.display()
                            ),
                        )
                        .await;
                        return;
                    }
                    Some(nuo_protocol::WorkspaceBinding::new(root))
                }
                nuo_persistence::roles::RoleWorkspace::Inherit => {
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
                                    format!(
                                        "Role `{role_id}` requires a workspace path. Usage: `/role {role_id} <workspace>`"
                                    ),
                                )
                                .await;
                                return;
                            }
                        }
                    };
                    Some(nuo_protocol::WorkspaceBinding::new(target_path))
                }
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
        let hits = crate::runtime::search_lexical::search(query, &messages, &commands, 5);
        let hits = hits
            .into_iter()
            .map(|hit| nuo_protocol::SearchHit {
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
                skills_registry: env.skills_registry,
                req_tx_for_commands: env.req_tx_for_commands,
                startup: env.startup,
                ui: env.ui,
                extra_commands: env.extra_commands,
                websearch_shared: env.websearch_shared,
                background_jobs: env.background_jobs,
            };
            start_fresh_session(&mut fresh_env, name, args).await;
        }
        SessionRoute::Fork => {
            fork_current_session(session, resp_tx, name, args).await;
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
                let current_id = session.id().await;
                if current_id == id {
                    let status = if lifecycle.is_running().await {
                        LoopStatus::Running
                    } else {
                        LoopStatus::Idle
                    };
                    send_harness_state_for_session(resp_tx, &current_id, agent, session, status)
                        .await;
                    return;
                }
                supersede_for_session_switch(lifecycle, agent, resp_tx).await;
                match session.open(id).await {
                    Ok(()) => {
                        // Full restore of the session-scoped runtime the
                        // bootstrap skipped in Picker mode: todos,
                        // disabled tools, round counter, and ThreadStart
                        // hooks. Opening a prior session is a resume.
                        restore_session_runtime(
                            session,
                            agent,
                            resp_tx,
                            nuo_protocol::HookStartSource::Resume,
                        )
                        .await;
                        let transcript = session.full_transcript().await;
                        let _ = resp_tx.send(AgentResponse::ConversationReplaced {
                            // `[INV-TAB-13]`: the id being left addresses the tab.
                            previous_id: current_id.clone(),
                            thread_id: session.id().await,
                            messages: transcript,
                            commands: session.commands().await,
                            round_interrupts: session.round_interrupts().await,
                        });
                        // The live provider tracks the opened session's
                        // own provider pin (or the global default).
                        crate::handlers::handlers_provider::reapply_session_selection(
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
                let _ = resp_tx.send(AgentResponse::ThreadsOverview(overview));
                let _ = resp_tx.send(AgentResponse::OpenThreadsPanel);
            }
        },
    }
}

pub(crate) async fn fork(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        resp_tx, session, ..
    } = env;
    fork_current_session(session, resp_tx, name, args).await;
}

pub(crate) async fn tree(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        resp_tx, session, ..
    } = env;
    let tree = session.tree().await;
    let _ = resp_tx.send(AgentResponse::ThreadTreeSnapshot {
        thread_id: session.id().await,
        tree: tree.clone(),
    });
    let _ = resp_tx.send(AgentResponse::OpenTreePanel);
    record_ack(
        session,
        resp_tx,
        name,
        args,
        format!(
            "Session DAG Tree has {} nodes and {} branch leaves.",
            tree.entries.len(),
            tree.leaves().len()
        ),
    )
    .await;
}

pub(crate) async fn new_session(mut env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    // `/new` never wipes anything in place: it starts a fresh session
    // and leaves the current one on disk (resumable via `/sessions`
    // or `/session open`). The retired `/clear` resolves here through
    // the alias table, so old muscle memory gets the safe semantics.
    start_fresh_session(&mut env, name, args).await;
}
