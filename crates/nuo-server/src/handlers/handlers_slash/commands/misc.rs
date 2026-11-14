//! Remaining commands: overlays, extension/unknown dispatch, help, and exit.

use super::super::record::{record_ack, record_command, record_invocation};
use super::super::session_ops::fork_retired_aside;
use crate::composition::startup::BuiltinCmd;
use crate::handlers::handlers_slash::SlashEnv;
use crate::handlers::slash_handler::SlashContext;
use nuo_protocol::{AgentResponse, CommandResult};

pub(crate) async fn stats(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        session, resp_tx, ..
    } = env;
    record_ack(
        session,
        resp_tx,
        name,
        args,
        "Thread stats are shown by the TUI overlay — run /stats in the terminal app.",
    )
    .await;
}

pub(crate) async fn trace(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        session, resp_tx, ..
    } = env;
    record_ack(
        session,
        resp_tx,
        name,
        args,
        "Session execution trace is shown by the TUI overlay — run /trace in the terminal app.",
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
        provider_for_task,
        ref mut provider_usage,
        skills_registry,
        req_tx_for_commands,
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
            provider_holder: provider_for_task,
            provider_usage,
            skills_registry,
            req_tx: req_tx_for_commands,
            project_root: None,
            startup,
            ui,
        };
        if handler.handle(ctx).await {
            return;
        }
    }
    // Retired-command steering (ADR-0058 §4, `[INV-DEPR-01]`): the `/btw` aside
    // substrate is gone. Non-destructively redirect a submission to a fork at
    // head instead of dead-ending it as an unknown command.
    if matches!(name_no_slash, "btw" | "aside" | "side") {
        fork_retired_aside(session, resp_tx, name, args).await;
        return;
    }
    // Unknown command: a `/`-prefixed draft is a command, not a prompt, so it
    // must never be silently appended to the transcript as a user turn (the
    // client sends it here precisely because it is spelled as a command). Answer
    // with the dedicated `UnknownCommand` result — `startup` owns the vocabulary,
    // so this is also the only place that can name the command that was meant.
    record_unknown_command(session, resp_tx, name, args, parts[0]).await;
}

/// Record an unrecognized `/name` and hand the caller a computable reply.
///
/// Distinct from [`record_error`] on purpose: the reply is machine-readable
/// (`CommandResult::UnknownCommand`), so a frontend can return the draft to the
/// composer instead of only painting a message. A typo is a *correctable*
/// gesture — the text is intact and nothing reached the model — and treating it
/// like a failed side effect (a plain error) is what made it feel unrecoverable.
pub(crate) async fn record_unknown_command(
    session: &std::sync::Arc<nuo_persistence::thread::ThreadSession>,
    resp_tx: &tokio::sync::mpsc::UnboundedSender<nuo_protocol::AgentResponse>,
    name: &str,
    args: &str,
    typed: &str,
) {
    record_command(
        session,
        resp_tx,
        name,
        args,
        nuo_protocol::CommandResult::UnknownCommand {
            name: typed.to_string(),
            suggestion: crate::composition::startup::suggest_command(typed).map(str::to_string),
        },
    )
    .await;
}
