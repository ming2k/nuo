//! Dashboard console dispatch (ADR-0097 §2–§3): turning the console's
//! command grammar into control-plane verbs and receipts. Extracted from
//! the event-loop action arms so the grammar, the verb mapping, and the
//! receipt plumbing read as one unit.
//!
//! The dispatch is fire-and-forget with a *reply receipt*: the verb ships
//! on a spawned one-shot control connection and its outcome lands in the
//! console transcript (via `AppMutation::HostConsole`) rather than
//! a toast — the cockpit log is the feedback surface.

use std::sync::Arc;

use nuo_client;
use crate::App;
use crate::overlays::{ConsoleCommand, ConsoleLine, ConsoleVerb};

use super::UiRuntime;

/// The `#N` sequence number → session id for the current dock selection.
/// The selection indexes the creation-ordered entries (`#seq` = index + 1),
/// exactly like every other selection-driven action.
fn selection_seq(app: &App) -> Option<(usize, String)> {
    let idx = app
        .modal_index
        .min(app.host_sessions.len().saturating_sub(1));
    let order = crate::overlays::creation_order(&app.host_sessions);
    order
        .get(idx)
        .map(|&i| (idx + 1, app.host_sessions[i].id.clone()))
}

/// Resolve a `#N` address to a session id against the live snapshot.
/// `None` when no session holds that number (the receipt names it).
fn seq_to_id(app: &App, n: usize) -> Option<String> {
    let order = crate::overlays::creation_order(&app.host_sessions);
    order
        .get(n.saturating_sub(1))
        .map(|&i| app.host_sessions[i].id.clone())
}

/// Push a local notice into the console log.
fn notice(app: &mut App, text: impl Into<String>) {
    app.host_console_log.push(ConsoleLine::Notice(text.into()));
}

/// Record a dispatch line — the "what I asked the fleet to do" half of the
/// cockpit log, written synchronously so it precedes any receipt.
fn log_dispatch(app: &mut App, raw: &str, targets: Vec<usize>, action: &'static str) {
    app.host_console_log.push(ConsoleLine::Dispatch {
        raw: raw.to_string(),
        targets,
        action,
    });
}

/// The control request a console verb maps to.
fn verb_request(verb: ConsoleVerb, id: String) -> nuo_client::ControlRequest {
    match verb {
        ConsoleVerb::Kill => nuo_client::ControlRequest::KillSession { session_id: id },
        ConsoleVerb::Interrupt => nuo_client::ControlRequest::Interrupt { session_id: id },
        ConsoleVerb::Suspend => nuo_client::ControlRequest::SuspendSession { session_id: id },
    }
}

/// What an accepted verb reports on its success receipt.
fn verb_receipt(verb: ConsoleVerb) -> &'static str {
    match verb {
        ConsoleVerb::Kill => "session ended",
        ConsoleVerb::Interrupt => "interrupt sent",
        ConsoleVerb::Suspend => "suspended — re-attaching resumes it",
    }
}

/// Send one control verb and push its receipt when the daemon answers.
/// `target` is the `#N` the receipt names (the daemon gets the id).
fn spawn_control_verb(runtime: &UiRuntime, verb: ConsoleVerb, target: Option<usize>, id: String) {
    let request = verb_request(verb, id);
    let mutations = runtime.mutations.clone();
    let dirty = (runtime.dirty.clone(), runtime.dirty_notify.clone());
    tokio::spawn(async move {
        let outcome = discover_and_control(request).await;
        let line = match outcome {
            Ok(()) => ConsoleLine::Receipt {
                ok: true,
                target,
                text: verb_receipt(verb).to_string(),
            },
            Err(e) => ConsoleLine::Receipt {
                ok: false,
                target,
                text: e,
            },
        };
        mutations
            .send(crate::event_loop::AppMutation::HostConsole(line))
            .await;
        wake(&dirty);
    });
}

/// Discover the daemon and issue one control verb. `Err` carries either the
/// discovery failure or the daemon's rejection, both receipt-ready.
async fn discover_and_control(request: nuo_client::ControlRequest) -> Result<(), String> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match nuo_client::discover(&project_root) {
        Some(info) => nuo_client::control(&info, request).await,
        None => {
            tracing::warn!("dashboard control: no daemon discovered");
            Err("no daemon is running".to_string())
        }
    }
}

/// Nudge the event loop awake so the fresh receipt paints immediately.
fn wake(
    dirty: &(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        Arc<tokio::sync::Notify>,
    ),
) {
    dirty.0.store(true, std::sync::atomic::Ordering::SeqCst);
    dirty.1.notify_one();
}

/// `k` on the dock: arm a two-press confirm; the second `k` confirms.
/// Killing a session is irreversible (its running work dies with it), so it
/// stays a deliberate two-surface gesture — the same pattern as the queue
/// modal's `Shift+D`, on a key that must be struck twice.
pub(super) fn kill_selected(app: &mut App, runtime: &UiRuntime) {
    let Some((seq, id)) = selection_seq(app) else {
        return;
    };
    if app.host_kill_confirm.is_some() && app.host_kill_confirm_id.as_deref() == Some(&id) {
        app.host_kill_confirm = None;
        app.host_kill_confirm_id = None;
        log_dispatch(app, &format!("/kill @{seq}"), vec![seq], "kill");
        spawn_control_verb(runtime, ConsoleVerb::Kill, Some(seq), id);
    } else {
        app.host_kill_confirm = Some("armed".to_string());
        app.host_kill_confirm_id = Some(id);
        notice(
            app,
            format!("kill #{seq}: press k again to confirm (any other key cancels)"),
        );
    }
}

/// Cancel an armed kill confirm (any non-`k` key or a selection move).
pub(super) fn cancel_kill_confirm(app: &mut App) {
    if app.host_kill_confirm.is_some() {
        app.host_kill_confirm = None;
        app.host_kill_confirm_id = None;
    }
}

/// `s` on the dock: suspend the selection. The daemon refuses a session
/// with an attached client or an active round; the receipt explains.
pub(super) fn suspend_selected(app: &mut App, runtime: &UiRuntime) {
    let Some((seq, id)) = selection_seq(app) else {
        return;
    };
    log_dispatch(app, &format!("/suspend @{seq}"), vec![seq], "suspend");
    spawn_control_verb(runtime, ConsoleVerb::Suspend, Some(seq), id);
}

/// Ask the Archivist (ADR-0208) one question: a synchronous control round
/// whose answer lands as a console receipt. The Archivist is the nuo-level
/// retrieval agent — it searches every project's sessions, so no `@N` target
/// and no dock selection are involved.
async fn dispatch_archivist(app: &mut App, runtime: &UiRuntime, raw: &str, text: &str) {
    log_dispatch(app, raw, Vec::new(), "ask archivist");
    let mutations = runtime.mutations.clone();
    let dirty = (runtime.dirty.clone(), runtime.dirty_notify.clone());
    let text = text.to_string();
    tokio::spawn(async move {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let outcome = match nuo_client::discover(&project_root) {
            Some(info) => {
                nuo_client::control_with_reply(
                    &info,
                    nuo_client::ControlRequest::AskArchivist { text: text.clone() },
                )
                .await
            }
            None => Err("no daemon is running".to_string()),
        };
        let line = match outcome {
            Ok(answer) => ConsoleLine::Receipt {
                ok: true,
                target: None,
                text: if answer.is_empty() {
                    "(the Archivist returned no answer)".to_string()
                } else {
                    answer
                },
            },
            Err(e) => ConsoleLine::Receipt {
                ok: false,
                target: None,
                text: e,
            },
        };
        mutations
            .send(crate::event_loop::AppMutation::HostConsole(line))
            .await;
        wake(&dirty);
    });
}

/// Create a session for the dashboard's project, optionally with an opening
/// prompt. Shared by `/new [text]` and the `n`-opened prompt's bare-text
/// submit. `raw` is the line the dispatch receipt echoes.
async fn dispatch_create(app: &mut App, runtime: &UiRuntime, raw: &str, text: Option<String>) {
    log_dispatch(app, raw, Vec::new(), "new session");
    let mutations = runtime.mutations.clone();
    let dirty = (runtime.dirty.clone(), runtime.dirty_notify.clone());
    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let project = project_root.display().to_string();
    tokio::spawn(async move {
        let outcome = discover_and_control(nuo_client::ControlRequest::CreateSession {
            project,
            prompt: text,
            init_options: None,
        })
        .await;
        let line = match outcome {
            Ok(()) => ConsoleLine::Receipt {
                ok: true,
                target: None,
                text: "session created".to_string(),
            },
            Err(e) => ConsoleLine::Receipt {
                ok: false,
                target: None,
                text: e,
            },
        };
        mutations
            .send(crate::event_loop::AppMutation::HostConsole(line))
            .await;
        wake(&dirty);
    });
}

/// The console's `/help` block: the verb table plus the address grammar.
fn help_block(app: &mut App) {
    notice(app, "verbs:");
    for verb in [
        ConsoleVerb::Interrupt,
        ConsoleVerb::Suspend,
        ConsoleVerb::Kill,
    ] {
        notice(app, format!("  {}", verb.help_line()));
    }
    notice(app, "  /new [text]   create a session for this project");
    notice(app, "addressing:");
    notice(app, "  @3 text       send the text to session #3");
    notice(
        app,
        "  @2 @3 text    fan out the same prompt to several sessions",
    );
    notice(app, "  bare text     prompt the selected session");
    notice(
        app,
        "  ? text        ask the Archivist to find sessions across all projects (ADR-0208)",
    );
}

/// Dispatch one parsed console line (see
/// [`crate::overlays::parse_console_command`]). `create_when_bare` is the
/// fallback role of bare text: `true` when the prompt was opened with `n`
/// (create a session), `false` when opened with `p` or submitted straight
/// from the console (prompt the dock selection).
pub(super) async fn dispatch_console_command(
    app: &mut App,
    runtime: &UiRuntime,
    raw: &str,
    create_when_bare: bool,
) {
    match crate::overlays::parse_console_command(raw) {
        ConsoleCommand::Help => help_block(app),
        ConsoleCommand::Unrecognized(text) => {
            if !text.is_empty() {
                notice(app, text);
            }
        }
        ConsoleCommand::New { text } => {
            dispatch_create(app, runtime, raw, text).await;
        }
        ConsoleCommand::Archivist { text } => {
            dispatch_archivist(app, runtime, raw, &text).await;
        }
        ConsoleCommand::Verb { verb, target } => {
            let (seq, id) = match target {
                Some(n) => match seq_to_id(app, n) {
                    Some(id) => (n, id),
                    None => return notice(app, format!("no session #{n} on the daemon")),
                },
                None => match selection_seq(app) {
                    Some((seq, id)) => (seq, id),
                    None => return notice(app, "no session selected"),
                },
            };
            log_dispatch(app, raw, vec![seq], verb.as_str());
            spawn_control_verb(runtime, verb, Some(seq), id);
        }
        ConsoleCommand::Prompt { targets, text } => {
            // An address with no payload is a usage notice, not a send.
            if text.trim().is_empty() {
                return notice(
                    app,
                    "nothing to send — a prompt needs text after the address",
                );
            }
            let resolved: Vec<(usize, String)> = if targets.is_empty() {
                // Bare text keeps the prompt-opened role: create when `n`
                // opened the line, else prompt the dock selection.
                if create_when_bare {
                    Vec::new()
                } else {
                    match selection_seq(app) {
                        Some((seq, id)) => vec![(seq, id)],
                        None => {
                            return notice(
                                app,
                                "no session selected — address one with @N or use /new",
                            );
                        }
                    }
                }
            } else {
                let mut out = Vec::new();
                for n in &targets {
                    match seq_to_id(app, *n) {
                        Some(id) => out.push((*n, id)),
                        None => return notice(app, format!("no session #{n} on the daemon")),
                    }
                }
                out
            };
            // `create_when_bare` with no address resolved to an empty set:
            // the directive is a create-with-prompt, not a zero-target send.
            if resolved.is_empty() {
                dispatch_create(app, runtime, raw, Some(text)).await;
                return;
            }
            log_dispatch(
                app,
                raw,
                resolved.iter().map(|(n, _)| *n).collect(),
                "prompt",
            );
            let sends: Vec<(usize, nuo_client::ControlRequest)> = resolved
                .into_iter()
                .map(|(n, id)| {
                    (
                        n,
                        nuo_client::ControlRequest::SendPrompt {
                            session_id: id,
                            text: text.clone(),
                        },
                    )
                })
                .collect();
            let mutations = runtime.mutations.clone();
            let dirty = (runtime.dirty.clone(), runtime.dirty_notify.clone());
            tokio::spawn(async move {
                let project_root =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let Some(info) = nuo_client::discover(&project_root) else {
                    tracing::warn!("dashboard control: no daemon discovered");
                    return;
                };
                for (n, request) in sends {
                    let line = match nuo_client::control(&info, request).await {
                        Ok(()) => ConsoleLine::Receipt {
                            ok: true,
                            target: Some(n),
                            text: "queued".to_string(),
                        },
                        Err(e) => ConsoleLine::Receipt {
                            ok: false,
                            target: Some(n),
                            text: e,
                        },
                    };
                    mutations
                        .send(crate::event_loop::AppMutation::HostConsole(line))
                        .await;
                }
                wake(&dirty);
            });
        }
    }
}
