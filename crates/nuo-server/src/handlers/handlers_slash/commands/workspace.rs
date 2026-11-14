//! Workspace, project, and reversibility commands.

use super::super::record::{record_command, record_error};
use super::super::security_ops::{TrustRoute, reload_trusted_assets, trust_route};
use crate::handlers::handlers_slash::SlashEnv;
use crate::product::project::init_nuo_config;
use crate::runtime::thread_driver::send_harness_state_for_session;
use nuo_protocol::{AgentResponse, CommandResult, LoopStatus, TrustDomain};

pub(crate) async fn diff(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        session, resp_tx, ..
    } = env;
    let session_id = session.id().await;

    let effects = match session
        .persistence()
        .list_workspace_effects(session_id.clone())
        .await
    {
        Ok(effects) => effects,
        Err(error) => {
            record_error(
                session,
                resp_tx,
                name,
                args,
                format!("could not read the workspace journal: {error}"),
            )
            .await;
            return;
        }
    };

    let workspace_root = session.workspace_root();
    let (reversible, irreversible, unconfirmed) =
        project_changes(&effects, workspace_root.as_deref());
    record_command(
        session,
        resp_tx,
        name,
        args,
        CommandResult::WorkspaceChanges {
            reversible,
            irreversible,
            unconfirmed,
        },
    )
    .await;
}

pub(crate) async fn undo(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        session, resp_tx, ..
    } = env;
    let session_id = session.id().await;
    let workspace_root = session.workspace_root();

    // The undo unit is the round: it is the boundary the user perceives ("what did
    // that turn do?") and the boundary the causal graph already uses.
    let candidate_limit = UNDO_CANDIDATE_LIMIT;
    let candidates = match session
        .persistence()
        .undoable_workspace_effects(session_id.clone(), candidate_limit)
        .await
    {
        Ok(effects) => effects,
        Err(error) => {
            record_error(
                session,
                resp_tx,
                name,
                args,
                format!("could not read the workspace journal: {error}"),
            )
            .await;
            return;
        }
    };

    // Only the newest round's rows: undoing "the last turn" must not silently
    // reach back into earlier rounds.
    let newest_round = candidates.first().map(|effect| effect.round_id.clone());
    let targets: Vec<_> = candidates
        .into_iter()
        .filter(|effect| Some(&effect.round_id) == newest_round.as_ref())
        .collect();

    if targets.is_empty() {
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::UndoOutcome {
                restored: Vec::new(),
                refused: Vec::new(),
                nothing_to_undo: true,
            },
        )
        .await;
        return;
    }

    // Reverse through the same content-addressed store the journal wrote through,
    // so a pre-image is looked up by the hash that was recorded for it.
    let store =
        crate::runtime::workspace_journal::SessionBlobAdapter::new(session.blob_store().clone());
    let outcomes = nuo_engine::runtime::workspace_journal::reversal::reverse_all(
        &targets,
        workspace_root.as_deref(),
        &store,
    );

    let mut restored = Vec::new();
    let mut refused = Vec::new();
    for (stamp, outcome) in outcomes {
        match outcome {
            nuo_engine::runtime::workspace_journal::reversal::Reversal::Restored(file) => {
                // Stamp before reporting: a reversal the journal does not know
                // about would be offered again on the next `/undo`, and the second
                // attempt would either refuse (post-image no longer matches) or,
                // worse, act twice.
                if let Err(error) = session
                    .persistence()
                    .mark_workspace_effect_reversed(
                        stamp.effect_id,
                        unix_ms(),
                        Some(stamp.note.clone()),
                    )
                    .await
                {
                    refused.push(nuo_protocol::UndoRefusal {
                        path: file.path.clone(),
                        reason: format!(
                            "restored, but the journal could not record it: {error}. \
                             Do not run /undo again for this round; the file is correct."
                        ),
                    });
                }
                restored.push(file);
            }
            nuo_engine::runtime::workspace_journal::reversal::Reversal::Refused(refusal) => {
                // A refusal is stamped too, with its reason, so a later `/diff`
                // explains why the change stayed instead of silently dropping it.
                let _ = session
                    .persistence()
                    .mark_workspace_effect_reversed(
                        stamp.effect_id,
                        unix_ms(),
                        Some(stamp.note.clone()),
                    )
                    .await;
                refused.push(refusal);
            }
            nuo_engine::runtime::workspace_journal::reversal::Reversal::Skipped => {}
        }
    }

    record_command(
        session,
        resp_tx,
        name,
        args,
        CommandResult::UndoOutcome {
            restored,
            refused,
            nothing_to_undo: false,
        },
    )
    .await;
}

/// How many recent journal rows `/undo` considers before narrowing to the newest
/// round. A round that touched more files than this is not silently truncated: the
/// cap is far above any plausible round and exists only so a runaway journal cannot
/// turn `/undo` into an unbounded read.
const UNDO_CANDIDATE_LIMIT: u32 = 512;

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Project journal rows into the three groups `/diff` reports.
///
/// The grouping is the point: a change the journal cannot reverse must not appear
/// in the same list as one it can, and a row whose mutation may never have run must
/// not be presented as either.
fn project_changes(
    effects: &[nuo_persistence::workspace_effects::WorkspaceEffect],
    workspace_root: Option<&std::path::Path>,
) -> (
    Vec<nuo_protocol::ChangedFile>,
    Vec<nuo_protocol::ChangedFile>,
    Vec<nuo_protocol::ChangedFile>,
) {
    let mut reversible = Vec::new();
    let mut irreversible = Vec::new();
    let mut unconfirmed = Vec::new();
    for effect in effects {
        let file = nuo_protocol::ChangedFile {
            path: display_path(effect.path.as_deref(), workspace_root),
            tool: effect.tool_name.clone(),
            round_id: effect.round_id.clone(),
            line_counts: None,
            reason: effect.irreversible_reason.clone(),
            created: effect.op == nuo_persistence::workspace_effects::JournalOp::Create,
            deleted: effect.op == nuo_persistence::workspace_effects::JournalOp::Delete,
        };
        if effect.effect_class == "file_bytes" {
            if effect.completed_at_ms.is_none() {
                // Journaled but never confirmed: the journal cannot say whether
                // the mutation ran, so it belongs in its own group.
                unconfirmed.push(file);
            } else if effect.reversed_at_ms.is_some() {
                // Already undone: reporting it as a pending change would invite a
                // second `/undo` for work that is no longer there.
            } else {
                reversible.push(file);
            }
        } else {
            irreversible.push(file);
        }
    }
    (reversible, irreversible, unconfirmed)
}

fn display_path(path: Option<&str>, workspace_root: Option<&std::path::Path>) -> String {
    let Some(path) = path else {
        return "<no path>".to_string();
    };
    if let Some(root) = workspace_root
        && let Ok(relative) = std::path::Path::new(path).strip_prefix(root)
        && !relative.as_os_str().is_empty()
    {
        return relative.to_string_lossy().into_owned();
    }
    path.to_string()
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
        ..
    } = env;
    let ws_opt = session.workspace();
    let project_root = ws_opt.as_ref().map(|w| w.root.as_path());
    let route = match trust_route(name, parts) {
        Ok(route) => route,
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
            return;
        }
    };
    let Some(project_root) = project_root else {
        match route {
            TrustRoute::GrantAll | TrustRoute::Grant(TrustDomain::UserAssets) => {
                let user_granted =
                    crate::handlers::handlers_slash::security_ops::trust_user_assets();
                let effective = nuo_persistence::config::Config::load();
                let mcp_report = mcp_runtime.reconfigure(effective.mcp.clone()).await;
                let mut snap = nuo_protocol::WorkspaceSecuritySnapshot::new("workspace-free");
                snap.user_assets =
                    crate::handlers::handlers_slash::security_ops::compute_user_assets_trust();
                agent.set_workspace_security(snap.clone());
                let user_granted_str = if user_granted.is_empty() {
                    "none".to_string()
                } else {
                    user_granted.join(", ")
                };
                let message = format!(
                    "User asset trust updated.\n\
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
            TrustRoute::DenyAll | TrustRoute::Deny(TrustDomain::UserAssets) => {
                // A workspace-free session's only project-supplied assets are
                // user-level MCP servers, so a refusal here is exactly the
                // `deny_user_assets` path — the same one the pre-attach gate
                // uses for unselected user rows.
                let denied = crate::handlers::handlers_slash::security_ops::deny_user_assets();
                let effective = nuo_persistence::config::Config::load();
                let mcp_report = mcp_runtime.reconfigure(effective.mcp.clone()).await;
                let mut snap = nuo_protocol::WorkspaceSecuritySnapshot::new("workspace-free");
                snap.user_assets =
                    crate::handlers::handlers_slash::security_ops::compute_user_assets_trust();
                agent.set_workspace_security(snap.clone());
                let message = if denied.is_empty() {
                    "Nothing to refuse: no user-level asset is awaiting review.".to_string()
                } else {
                    format!(
                        "User asset trust refused.\n\
                         - Refused: {}\n\
                         - Connected MCP: {}\n\
                         Refused assets stay inactive until their declaration changes.",
                        denied.join(", "),
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
                    )
                };
                record_command(session, resp_tx, name, args, CommandResult::Text(message)).await;
            }
            TrustRoute::Status => {
                let mut snapshot = nuo_protocol::WorkspaceSecuritySnapshot::new("workspace-free");
                snapshot.user_assets =
                    crate::handlers::handlers_slash::security_ops::compute_user_assets_trust();
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
            let mut snapshot = workspace_security.snapshot(project_root);
            snapshot.user_assets =
                crate::handlers::handlers_slash::security_ops::compute_user_assets_trust();
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
        TrustRoute::DenyAll | TrustRoute::Deny(_) => {
            // A refusal is a decision, not an absence of one. Recording it is
            // what stops the workspace asking again: `deny_domains` stores the
            // digest of the content that was refused, so the next snapshot
            // reports `Denied` (settled, no gate) until the bytes change — at
            // which point it flips to `Changed` and asks again. That is the
            // exact "stop nagging me about *this* content" semantic the attach
            // banner used to promise while pointing at `/untrust`, which
            // revoked everything and re-armed the gate instead.
            let snapshot_before = workspace_security.snapshot(project_root);
            let requested: Vec<TrustDomain> = match route {
                TrustRoute::DenyAll => snapshot_before.domains_needing_review(),
                TrustRoute::Deny(domain) => vec![domain],
                _ => unreachable!(),
            };
            if requested.is_empty() {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(
                        "Nothing to refuse: no project asset domain is awaiting review."
                            .to_string(),
                    ),
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
                return;
            }

            let user_domains: Vec<TrustDomain> = requested
                .iter()
                .copied()
                .filter(|d| *d == TrustDomain::UserAssets)
                .collect();
            let ws_domains: Vec<TrustDomain> = requested
                .iter()
                .copied()
                .filter(|d| *d != TrustDomain::UserAssets)
                .collect();
            let user_denied = if user_domains.is_empty() {
                Vec::new()
            } else {
                crate::handlers::handlers_slash::security_ops::deny_user_assets()
            };
            let denied = if ws_domains.is_empty() {
                Vec::new()
            } else {
                match workspace_security.deny_domains(project_root, &ws_domains) {
                    Ok(denied) => denied,
                    Err(error) => {
                        record_error(session, resp_tx, name, args, error).await;
                        return;
                    }
                }
            };
            let report = match reload_trusted_assets(
                agent,
                mcp_runtime,
                workspace_security,
                project_root,
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
            // The refusal is a decision the user must be able to see the
            // *consequence* of, so report both what was refused and what the
            // workspace now looks like — an unloaded domain is the point, not a
            // side effect.
            let denied_str = denied
                .iter()
                .chain(user_denied.iter().map(|_| &TrustDomain::UserAssets))
                .map(|domain| domain.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let still_pending = report
                .snapshot
                .domains_needing_review()
                .into_iter()
                .map(|domain| domain.as_str())
                .collect::<Vec<_>>();
            let message = format!(
                "Project asset trust refused.\n\
                 - Root: {}\n\
                 - Refused: {denied_str}\n\
                 - Still awaiting review: {}\n\
                 Refused content stays inactive. This will not be asked again until the content \
                 changes on disk.",
                report.snapshot.root,
                if still_pending.is_empty() {
                    "none".to_string()
                } else {
                    still_pending.join(", ")
                },
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
                crate::handlers::handlers_slash::security_ops::trust_user_assets()
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
                match workspace_security.trust_domains(project_root, &ws_domains) {
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
                project_root,
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
                "Workspace trust updated.\n\
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
            let revoked = match workspace_security.revoke_workspace(project_root) {
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
                project_root,
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
    let markdown = crate::product::export::format_export_markdown(
        crate::product::export::ExportContext {
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
