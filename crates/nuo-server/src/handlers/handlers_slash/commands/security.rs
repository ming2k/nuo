//! Permission and tool-security commands.

use super::super::record::{record_ack, record_command, record_error};
use crate::handlers::handlers_slash::SlashEnv;
use nuo_engine::cognition::skills::ListSkillsTool;
use nuo_protocol::CommandResult;
use nuo_tool::Tool;

pub(crate) async fn permissions(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        agent,
        resp_tx,
        session,
        ..
    } = env;
    if parts.get(1) == Some(&"clear") {
        agent.clear_allowed_tools();
        record_ack(
            session,
            resp_tx,
            name,
            args,
            "Always-allowed tool rules cleared.",
        )
        .await;
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
                    .filter(|s| s.scope == nuo_engine::cognition::skills::SkillScope::User)
                    .count();
                let repo_count = list
                    .iter()
                    .filter(|s| s.scope == nuo_engine::cognition::skills::SkillScope::Repo)
                    .count();
                let extra_count = list
                    .iter()
                    .filter(|s| s.scope == nuo_engine::cognition::skills::SkillScope::Extra)
                    .count();
                let remote_count = list
                    .iter()
                    .filter(|s| s.scope == nuo_engine::cognition::skills::SkillScope::Remote)
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
                    "Manual `/skills reload` has been retired.\n\
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
