//! Remaining commands: overlays, extension/unknown dispatch, help, and exit.

use super::super::record::{record_ack, record_command, record_error, record_invocation};
use super::super::session_ops::fork_retired_aside;
use crate::composition::startup::BuiltinCmd;
use crate::handlers::handlers_slash::SlashEnv;
use crate::handlers::slash_handler::SlashContext;
use protocol::{AgentResponse, CommandResult};

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

/// `/context` — the measured context plan for the most recent request.
///
/// Renders the same `ContextPlan` the request was built from, so the table and
/// the prompt cannot disagree ([`INV-CTXPLAN-01`], ADR-0217). It is a reading: it
/// mutates nothing and is not recorded in the ledger.
///
/// The bare form is the operator table; `--json` is the one machine form
/// (ADR-0220 §5). A terminal client intercepts the bare form locally and opens
/// the context dialog, which reads the same value over
/// [`AgentRequest::QueryContextPlan`] — so this arm serves every
/// non-terminal client and the `--json` case.
pub(crate) async fn context(env: SlashEnv<'_>, name: &str, args: &str, _parts: &[&str]) {
    let SlashEnv {
        agent,
        session,
        resp_tx,
        ..
    } = env;
    let json = args.trim() == "--json";
    let view = agent.context_plan_view();
    let body = match (&view, json) {
        (Some(view), true) => engine::render_context_json(view)
            .unwrap_or_else(|error| format!("could not render the context plan: {error}")),
        (Some(view), false) => engine::render_context_table(view),
        (None, _) => "No request has been assembled in this session yet, so there is no measured \
                      context plan. Send one message, then run `/context`."
            .to_string(),
    };
    record_command(session, resp_tx, name, args, CommandResult::Text(body)).await;
}

/// `/language [tag|off]` — declare the language the agent answers in (ADR-0222).
///
/// The bare form reads the current declaration; a tag is an **act** that
/// validates, persists, and re-seeds the live agent in that order. The order is
/// the contract: a tag the validator refuses is never written, and a write that
/// fails leaves the running agent on its previous declaration rather than on a
/// language the next restart will not reproduce.
///
/// `off` removes the declaration instead of setting "auto": there is no language
/// the harness could pick for the user, so "no declared language" is the honest
/// state and the prompt block disappears entirely.
pub(crate) async fn language(env: SlashEnv<'_>, name: &str, args: &str, parts: &[&str]) {
    let SlashEnv {
        config,
        agent,
        session,
        resp_tx,
        ..
    } = env;
    let argument = parts.get(1..).map(|rest| rest.join(" "));
    let argument = argument.as_deref().map(str::trim).unwrap_or("");

    if argument.is_empty() {
        let body = match agent.response_language() {
            Some(language) => format!(
                "Answer language: {language}\n\n\
                 Declared in `agent.toml` as `response_language`. It renders as the \
                 `agent.language` block in every request, applies to subagents, and \
                 survives `/model` switches. `/language <tag>` changes it; `/language off` \
                 removes it."
            ),
            None => "Answer language: not declared.\n\n\
                     No language rule is rendered, so the model follows its own judgement. \
                     Declare one with `/language zh-CN` (any language name or BCP-47 tag)."
                .to_string(),
        };
        record_command(session, resp_tx, name, args, CommandResult::Text(body)).await;
        return;
    }

    let next = if argument.eq_ignore_ascii_case("off") || argument.eq_ignore_ascii_case("none") {
        None
    } else {
        match protocol::ResponseLanguage::parse(argument) {
            Ok(language) => Some(language),
            Err(error) => {
                record_error(session, resp_tx, name, args, error).await;
                return;
            }
        }
    };

    // The error type is boxed and not `Send`; flatten it here so the `await`
    // below never holds a non-`Send` value across a suspension point.
    let persisted = config
        .persist_response_language(next.as_ref())
        .map_err(|error| error.to_string());
    if let Err(error) = persisted {
        record_error(
            session,
            resp_tx,
            name,
            args,
            format!("could not persist the answer language: {error}"),
        )
        .await;
        return;
    }
    // One write, three readers: the live agent for this request, the shared
    // config for every later one, and the file. Mutating the live snapshot in
    // place (rather than reloading it) is deliberate — a reload would drop the
    // project-level merges the bootstrap applied to it, which no `/language`
    // has any business discarding.
    agent.set_response_language(next.clone());

    let (title, detail) = match &next {
        Some(language) => (
            format!("Answer language: {language}"),
            vec![
                "Rendered as `agent.language` in every request; prose and the final answer \
                 follow it, code and command output keep their own language"
                    .to_string(),
                "Persisted to agent.toml; subagents inherit it".to_string(),
            ],
        ),
        None => (
            "Answer language: not declared".to_string(),
            vec![
                "The `agent.language` block is removed from every request".to_string(),
                "The model follows its own judgement again".to_string(),
            ],
        ),
    };
    record_command(
        session,
        resp_tx,
        name,
        args,
        CommandResult::Ack {
            title,
            detail: Some(detail),
        },
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
    session: &std::sync::Arc<persistence::thread::ThreadSession>,
    resp_tx: &tokio::sync::mpsc::UnboundedSender<protocol::AgentResponse>,
    name: &str,
    args: &str,
    typed: &str,
) {
    record_command(
        session,
        resp_tx,
        name,
        args,
        protocol::CommandResult::UnknownCommand {
            name: typed.to_string(),
            suggestion: crate::composition::startup::suggest_command(typed).map(str::to_string),
        },
    )
    .await;
}
