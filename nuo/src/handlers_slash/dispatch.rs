//! The `AgentRequest::SlashCommand` router.
//!
//! Parsing and routing only. Each built-in delegates to a focused async fn in
//! [`super::commands`] so no single frame is large enough to overflow a Tokio
//! worker stack (a monolithic dispatcher reached ~1.6 MiB in debug builds).

use super::SlashEnv;
use super::commands;
use crate::startup::BuiltinCmd;

/// `AgentRequest::SlashCommand` — parse the command, dispatch to the matching
/// built-in handler, or fall through to the user-defined project-command path.
pub async fn dispatch(cmd: String, env: SlashEnv<'_>) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }
    let start_instant = std::time::Instant::now();
    let name = parts[0].trim_start_matches('/');
    let args = cmd.strip_prefix(parts[0]).unwrap_or("").trim();
    match BuiltinCmd::from_slash(parts[0]) {
        Some(BuiltinCmd::Models) | Some(BuiltinCmd::Connections) | Some(BuiltinCmd::Settings) => {
            // Handled in client UI / overlay
        }
        Some(BuiltinCmd::Tools) => {
            // Handled in TUI (`/tools` opens the tools manager modal
            // locally; it is never forwarded here as a SlashCommand).
        }
        Some(BuiltinCmd::Mcp) => {
            // Handled in TUI: `/mcp` opens the MCP manager modal locally
            // (intercepted in input.rs as `InputAction::OpenMcp`) and is never
            // forwarded here as a SlashCommand. The modal reads the live
            // session-context snapshot, whose MCP pane the harness keeps current
            // via the shared `McpRuntime`.
        }
        Some(BuiltinCmd::Permissions) => commands::permissions(env, name, args, &parts).await,
        Some(BuiltinCmd::Unattended) => commands::unattended(env, name, args, &parts).await,
        Some(BuiltinCmd::Confinement) => commands::confinement(env, name, args, &parts).await,
        Some(BuiltinCmd::Role) => commands::role(env, name, args, &parts).await,
        Some(BuiltinCmd::Search) => {
            commands::search(env, &cmd, name, args, &parts, start_instant).await
        }
        Some(BuiltinCmd::Sessions) => commands::sessions(env, name, args, &parts).await,
        Some(BuiltinCmd::Fork) => commands::fork(env, name, args, &parts).await,
        Some(BuiltinCmd::Tree) => commands::tree(env, name, args, &parts).await,
        Some(BuiltinCmd::Diff) => commands::diff(env, name, args, &parts).await,
        Some(BuiltinCmd::Undo) => commands::undo(env, name, args, &parts).await,
        Some(BuiltinCmd::Dashboard) => commands::dashboard(env, name, args, &parts).await,
        Some(BuiltinCmd::Usage) => commands::usage(env, name, args, &parts).await,
        Some(BuiltinCmd::Btw) => commands::btw(env, &cmd, name, args, &parts).await,
        Some(BuiltinCmd::Compact) => commands::compact(env, name, args, &parts).await,
        Some(BuiltinCmd::Jobs) => commands::jobs(env, name, args, &parts).await,
        Some(BuiltinCmd::Init) => commands::init(env, name, args, &parts).await,
        Some(BuiltinCmd::Trust) | Some(BuiltinCmd::Untrust) => {
            commands::trust(env, name, args, &parts).await
        }
        Some(BuiltinCmd::Skills) => commands::skills(env, name, args, &parts).await,
        Some(BuiltinCmd::New) => commands::new_session(env, name, args, &parts).await,
        Some(BuiltinCmd::Export) => commands::export(env, name, args, &parts).await,
        Some(BuiltinCmd::Debug) => commands::debug(env, name, args, &parts).await,
        Some(BuiltinCmd::Retry) => commands::retry(env, name, args, &parts).await,
        Some(BuiltinCmd::Help) => commands::help(env, name, args, &parts).await,
        Some(BuiltinCmd::Exit) => commands::exit(env, name, args, &parts).await,
        None => commands::user_command(env, &cmd, name, args, &parts).await,
    }
}
