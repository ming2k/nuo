//! Automation and background-job commands.

use super::super::record::{record_ack, record_command, record_error};
use super::super::session_route::{parse_confinement_arg, parse_unattended_arg};
use crate::handlers::handlers_slash::SlashEnv;
use nuo_engine::orchestration::round_response;
use nuo_protocol::{CommandResult, RoundEvent};

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
                let jid = nuo_protocol::JobId(target_id.to_string());
                match background_jobs.kill_job(&jid) {
                    Ok(()) => {
                        record_ack(
                            session,
                            resp_tx,
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
                let jid = nuo_protocol::JobId(target_id.to_string());
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
                        nuo_protocol::JobSpec::Process { command, label, .. } => (
                            label.clone().unwrap_or_else(|| "process".to_string()),
                            command.clone(),
                        ),
                        nuo_protocol::JobSpec::Timer { label, prompt, .. } => (
                            label.clone().unwrap_or_else(|| "timer".to_string()),
                            prompt.clone(),
                        ),
                    };
                    let status_str = match &j.state {
                        nuo_protocol::JobState::Queued => "Queued".to_string(),
                        nuo_protocol::JobState::Running { pid, .. } => {
                            if let Some(p) = pid {
                                format!("Running (PID {p})")
                            } else {
                                "Running".to_string()
                            }
                        }
                        nuo_protocol::JobState::Ready { .. } => "Ready (service)".to_string(),
                        nuo_protocol::JobState::Succeeded { duration_ms, .. } => {
                            format!("✓ Passed ({}s)", duration_ms / 1000)
                        }
                        nuo_protocol::JobState::Failed {
                            duration_ms,
                            exit_code,
                            ..
                        } => {
                            format!("✗ Failed (Exit {exit_code}, {}s)", duration_ms / 1000)
                        }
                        nuo_protocol::JobState::Killed { duration_ms } => {
                            format!("Killed ({}s)", duration_ms / 1000)
                        }
                        nuo_protocol::JobState::TimedOut { duration_ms } => {
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
