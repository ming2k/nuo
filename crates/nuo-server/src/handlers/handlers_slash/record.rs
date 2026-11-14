//! Slash-command reply funnel: emit always, persist iff durable (ADR-0140).
//!
//! # Why one funnel and not a helper per reply shape
//!
//! Every slash handler used to pick its own recording helper, and each helper
//! independently decided what to persist. That made "is this command remembered?"
//! a property of which function a handler happened to call — a judgement re-made
//! at forty call sites, with no mechanism behind it. The helpers also disagreed
//! about the wire: `record_ack` persisted an ack and emitted *nothing*, so
//! `/stats`, `/trace`, `/usage` and `/quotas` were silent to every client that
//! could not intercept the command locally, and the ledger was being used as a
//! message bus.
//!
//! The funnel inverts that: **the reply always reaches the client; whether it is
//! also remembered is decided once, from the command's declared effect.**
//!
//! # The three conditions
//!
//! `is_durable` is the whole residency policy, and each condition exists
//! because of a distinct failure:
//!
//! 1. **The command is an act.** A reading (`/usage`, `/diff`, `/jobs`, `/help`)
//!    reports the world at the moment it is asked; a stored copy asserts
//!    something false the moment the world moves, and carries no way to signal
//!    its own staleness.
//! 2. **An act actually happened here.** The four surface-owned commands
//!    (`/models`, `/connections`, `/tools`, `/mcp`) reach the server only from a
//!    client that does not own the modal; the server performs nothing, and
//!    fabricating a record for state this process never touched is a lie in the
//!    audit.
//! 3. **The reply is not a rejection.** Nothing ran and nothing reached the
//!    model (ADR-0119 §3), so nothing is owed to the ledger — no matter what the
//!    command's declared class is.

use tokio::sync::mpsc;

use nuo_engine::orchestration::round_response;
use nuo_persistence::thread::ThreadSession;
use nuo_protocol::{AgentResponse, CommandRecord, CommandResult, RoundEvent};
use nuo_tool::CommandEffect;

/// Where the act happened.
///
/// `Surface` means the client owns the surface that performs it: the server's
/// arm is then a no-op that only answers, and must not record state it never
/// touched. This is not a policy knob — it is a fact about the arm, so the arm
/// that knows it states it rather than having it inferred.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Performed {
    /// The act (if any) happened in this process.
    Here,
    /// The client owns the surface; this process only answers.
    Surface,
}

/// Whether a reply is remembered as a durable record.
fn is_durable(performed: Performed, effect: CommandEffect, result: Option<&CommandResult>) -> bool {
    performed == Performed::Here
        && effect.is_durable()
        && !matches!(result, Some(CommandResult::UnknownCommand { .. }))
}

/// The declared effect of one invocation.
///
/// The decomposition mirrors `handlers_slash::dispatch` (command word with its
/// leading slash, then the argument tokens) so the class is resolved from exactly
/// what the router routed on — the vocabulary's single owner does the deciding,
/// and no handler can opt in or out.
fn effect_of(name: &str, args: &str, result: Option<&CommandResult>) -> CommandEffect {
    if matches!(result, Some(CommandResult::UnknownCommand { .. })) {
        return CommandEffect::Reading;
    }
    let slash = format!("/{name}");
    let mut parts: Vec<&str> = Vec::with_capacity(2);
    parts.push(&slash);
    if let Some(verb) = args.split_whitespace().next() {
        parts.push(verb);
    }
    crate::composition::startup::effect_for(&parts)
}

/// The single reply path: emit always, persist iff durable.
///
/// `result: None` is the invocation-only shape — no wire frame is emitted because
/// there is no reply to emit. A mutation whose success is carried by its own
/// state event (`/compact`, `/retry`) and a command that ends the process
/// (`/exit`) take this path.
async fn emit(
    session: &ThreadSession,
    resp_tx: Option<&mpsc::UnboundedSender<AgentResponse>>,
    performed: Performed,
    name: &str,
    args: &str,
    result: Option<CommandResult>,
    duration_ms: Option<u64>,
) {
    let effect = effect_of(name, args, result.as_ref());
    if is_durable(performed, effect, result.as_ref()) {
        let mut record = CommandRecord::new(name, args);
        if let Some(result) = result.clone() {
            record = record.with_result(result);
        }
        if let Some(ms) = duration_ms {
            record = record.with_duration_ms(ms);
        }
        if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
            tracing::warn!(?error, name, "could not persist command record");
        }
    }
    if let (Some(resp_tx), Some(result)) = (resp_tx, result) {
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::CommandResult {
                name: name.to_string(),
                args: args.to_string(),
                result,
                effect,
            },
        ));
    }
}

/// Surface a typed reply, and record it iff the command is an act performed here.
pub(crate) async fn record_command(
    session: &ThreadSession,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    result: CommandResult,
) {
    record_command_with_duration(session, resp_tx, name, args, result, None).await;
}

pub(crate) async fn record_command_with_duration(
    session: &ThreadSession,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    result: CommandResult,
    duration_ms: Option<u64>,
) {
    emit(
        session,
        Some(resp_tx),
        Performed::Here,
        name,
        args,
        Some(result),
        duration_ms,
    )
    .await;
}

/// Surface an acknowledgment (the durable twin of a `CommandAck` toast).
///
/// Gains `resp_tx` under ADR-0140: an ack used to be persisted *silently*, which
/// left four commands answering nothing at all to a non-TUI client.
pub(crate) async fn record_ack(
    session: &ThreadSession,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    title: impl Into<String>,
) {
    record_ack_with_duration(session, resp_tx, name, args, title, None).await;
}

pub(crate) async fn record_ack_with_duration(
    session: &ThreadSession,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    title: impl Into<String>,
    duration_ms: Option<u64>,
) {
    emit(
        session,
        Some(resp_tx),
        Performed::Here,
        name,
        args,
        Some(CommandResult::Ack {
            title: title.into(),
            detail: None,
        }),
        duration_ms,
    )
    .await;
}

/// Record the invocation of an act whose success is carried by its own state
/// event, with no textual reply (`/compact`).
pub(crate) async fn record_invocation(session: &ThreadSession, name: &str, args: &str) {
    record_invocation_with_duration(session, name, args, None).await;
}

pub(crate) async fn record_invocation_with_duration(
    session: &ThreadSession,
    name: &str,
    args: &str,
    duration_ms: Option<u64>,
) {
    emit(
        session,
        None,
        Performed::Here,
        name,
        args,
        None,
        duration_ms,
    )
    .await;
}

/// Record an act whose reply is carried by another channel, so this path must not
/// emit a `CommandResult` frame of its own.
///
/// The durability decision is still the funnel's — that is the whole point of
/// routing here rather than calling `mutate_commands` directly: a second writer
/// with its own judgement is exactly the drift this funnel exists to prevent.
/// `/models` is the live case: a model switch is performed by the picker surface
/// and acknowledged with a `CommandAck` notice, so the ledger row is written
/// without a second, duplicate reply.
pub(crate) async fn record_act_only(
    session: &ThreadSession,
    name: &str,
    args: &str,
    result: CommandResult,
) {
    let effect = effect_of(name, args, Some(&result));
    if !is_durable(Performed::Here, effect, Some(&result)) {
        return;
    }
    let record = CommandRecord::new(name, args).with_result(result);
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command record");
    }
}

/// Surface a failed act (or a failed reading, which is a receipt only).
pub(crate) async fn record_error(
    session: &ThreadSession,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    message: impl Into<String>,
) {
    record_error_with_duration(session, resp_tx, name, args, message, None).await;
}

pub(crate) async fn record_error_with_duration(
    session: &ThreadSession,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    message: impl Into<String>,
    duration_ms: Option<u64>,
) {
    emit(
        session,
        Some(resp_tx),
        Performed::Here,
        name,
        args,
        Some(CommandResult::Error {
            message: message.into(),
            detail: None,
        }),
        duration_ms,
    )
    .await;
}

/// Answer a command whose surface the client owns (`/models`, `/connections`,
/// `/tools`, `/mcp`).
///
/// The client performs the act; this process performs nothing, so nothing is
/// recorded. The reply exists so the command is never silently dropped from a
/// client that reached the dispatcher without owning the surface.
pub(crate) async fn record_surface_only(
    session: &ThreadSession,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    guidance: impl Into<String>,
) {
    emit(
        session,
        Some(resp_tx),
        Performed::Surface,
        name,
        args,
        Some(CommandResult::Text(guidance.into())),
        None,
    )
    .await;
}
