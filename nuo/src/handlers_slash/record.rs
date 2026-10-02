//! Command execution ledger recording helpers (invocations, durable acks, errors).

use std::sync::Arc;
use tokio::sync::mpsc;

use nuo_harness::orchestration::round_response;
use nuo_contracts::{AgentResponse, CommandRecord, CommandResult, RoundEvent};
use nuo_persistence::session::SessionStore;

/// Record a successful slash-command invocation in the ledger and surface its
/// typed result as a command block.
pub(crate) async fn record_command(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    result: CommandResult,
) {
    record_command_with_duration(session, resp_tx, name, args, result, None).await;
}

pub(crate) async fn record_command_with_duration(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    result: CommandResult,
    duration_ms: Option<u64>,
) {
    let mut record = CommandRecord::new(name, args).with_result(result.clone());
    if let Some(ms) = duration_ms {
        record = record.with_duration_ms(ms);
    }
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command record");
    }
    let _ = resp_tx.send(round_response(
        &session.id().await,
        RoundEvent::CommandResult {
            name: name.to_string(),
            args: args.to_string(),
            result,
        },
    ));
}

/// Record a slash-command invocation whose reply is a special response type.
pub(crate) async fn record_invocation(session: &Arc<SessionStore>, name: &str, args: &str) {
    record_invocation_with_duration(session, name, args, None).await;
}

pub(crate) async fn record_invocation_with_duration(
    session: &Arc<SessionStore>,
    name: &str,
    args: &str,
    duration_ms: Option<u64>,
) {
    let mut record = CommandRecord::new(name, args);
    if let Some(ms) = duration_ms {
        record = record.with_duration_ms(ms);
    }
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command invocation");
    }
}

/// Record an acknowledgment — the durable twin of a `CommandAck` toast.
pub(crate) async fn record_ack(
    session: &Arc<SessionStore>,
    name: &str,
    args: &str,
    title: impl Into<String>,
) {
    record_ack_with_duration(session, name, args, title, None).await;
}

pub(crate) async fn record_ack_with_duration(
    session: &Arc<SessionStore>,
    name: &str,
    args: &str,
    title: impl Into<String>,
    duration_ms: Option<u64>,
) {
    let mut record = CommandRecord::new(name, args).with_result(CommandResult::Ack {
        title: title.into(),
        detail: None,
    });
    if let Some(ms) = duration_ms {
        record = record.with_duration_ms(ms);
    }
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command ack");
    }
}

/// Record a failed slash-command invocation and surface the error as a
/// typed `CommandResult::Error`.
pub(crate) async fn record_error(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    message: impl Into<String>,
) {
    record_error_with_duration(session, resp_tx, name, args, message, None).await;
}

pub(crate) async fn record_error_with_duration(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    message: impl Into<String>,
    duration_ms: Option<u64>,
) {
    let message = message.into();
    record_command_with_duration(
        session,
        resp_tx,
        name,
        args,
        CommandResult::Error {
            message,
            detail: None,
        },
        duration_ms,
    )
    .await;
}
