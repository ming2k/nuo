//! Provider- and model-facing commands.
//!
//! `/models` and `/connections` are client-owned surfaces answered in
//! [`super::super::dispatch`], so no server handler exists here for them. What
//! remains is server-side provider usage/quota reporting plus the
//! provider-bound `/compact` and `/retry` acts.

use super::super::record::{record_ack, record_command, record_error, record_invocation};
use crate::composition::agent_setup::active_context_window;
use crate::handlers::handlers_chat::refuse_if_no_provider;
use crate::handlers::handlers_slash::SlashEnv;
use crate::runtime::thread_driver::compact_round_history;
use nuo_engine::orchestration::{
    ContextProjectionSettings, RoundInput, round_response, send_compaction,
};
use nuo_protocol::CommandResult;

pub(crate) async fn usage(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        session, resp_tx, ..
    } = env;
    // Handled in TUI: `/usage` opens the usage-statistics overlay
    // locally (intercepted in input.rs as `InputAction::OpenUsage`)
    // and issues `AgentRequest::QueryUsageStats`; it never arrives
    // here as a SlashCommand. Reaching this arm means a non-TUI
    // client (Web app) typed it — answer inline with a pointer so
    // the command is never silently dropped.
    record_ack(
        session,
        resp_tx,
        name,
        args,
        "Usage statistics are shown by the TUI overlay — open the terminal app and run /usage there.",
    )
    .await;
}

pub(crate) async fn quota(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        session, resp_tx, ..
    } = env;
    record_ack(
        session,
        resp_tx,
        name,
        args,
        "Provider quotas are shown by CLI or TUI — run `nuo quota [provider]` in your terminal.",
    )
    .await;
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
    let settings = ContextProjectionSettings::from_context_policy(
        &config.context,
        active_context_window(agent),
    )
    .for_request(agent.estimate_next_request_tokens(&current));

    let _ = resp_tx.send(round_response(
        &session.id().await,
        nuo_protocol::RoundEvent::Notice(
            nuo_tool::AgentNotice::new(
                nuo_tool::NoticeKind::CommandAck,
                nuo_tool::NoticeSeverity::Info,
                "Compacting Context",
                nuo_tool::NoticeSource::Harness,
            )
            .with_body("Synthesizing completed rounds into structured checkpoint..."),
        ),
    ));

    let mut extra = agent.fire_pre_compact().await;
    let trimmed = args.trim();
    if !trimmed.is_empty() {
        extra.push(format!(
            "User Focus / Instructions for Compaction: {}",
            trimmed
        ));
    }

    // Universal Causal Compaction folds [0..N-1] and preserves round N
    match compact_round_history(
        &mut current,
        session,
        &settings,
        Some(agent.provider.clone()),
        extra,
    )
    .await
    {
        Ok(Some(report)) => {
            record_invocation(session, name, args).await;
            let session_id = session.id().await;
            send_compaction(resp_tx, &session_id, &report.checkpoint);
            if !report.satisfied {
                let _ = resp_tx.send(round_response(
                    &session_id,
                    nuo_protocol::RoundEvent::Notice(
                        nuo_tool::AgentNotice::new(
                            nuo_tool::NoticeKind::ReviewAlert,
                            nuo_tool::NoticeSeverity::Warning,
                            "Compaction could not bring the window under the limit",
                            nuo_tool::NoticeSource::Harness,
                        )
                        .with_body(format!(
                            "The compacted window is {} tokens; this model's compaction gate is {}. \
                             Run /compact again or switch to a larger window.",
                            report.checkpoint.window_tokens_after,
                            settings.budget.compaction_threshold_tokens
                        )),
                    ),
                ));
            }
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

pub(crate) async fn retry(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        config,
        agent,
        resp_tx,
        session,
        lifecycle,
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
    // it resumes the parked round with the same number
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
    crate::handlers::handlers_chat::start_turn(
        crate::handlers::handlers_chat::ChatEnv {
            agent,
            session,
            lifecycle,
            tx: resp_tx,
            config,
        },
        RoundInput::resume(point),
    )
    .await;
}
