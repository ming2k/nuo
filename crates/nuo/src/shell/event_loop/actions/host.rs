//! Dashboard console dispatch: turning the console's
//! command grammar into control-plane verbs and receipts. Extracted from
//! the event-loop action arms so the grammar, the verb mapping, and the
//! receipt plumbing read as one unit.
//!
//! The dispatch is fire-and-forget with a *reply receipt*: the verb ships
//! on a spawned one-shot control connection and its outcome lands in the
//! console transcript (via `AppMutation::HostConsole`) rather than
//! a toast — the cockpit log is the feedback surface.
use std::sync::Arc;
use std::sync::atomic::Ordering;

use nuo_protocol::AgentRequest;

use crate::shell::App;
use crate::shell::input::InputAction;
use crate::shell::overlays::{ConsoleCommand, ConsoleLine, ConsoleVerb};

use super::{ActionFlow, UiRuntime};

/// The `#N` sequence number → session id for the current dock selection.
/// The selection indexes the creation-ordered entries (`#seq` = index + 1),
/// exactly like every other selection-driven action.
fn selection_seq(app: &App) -> Option<(usize, String)> {
    let idx = app
        .modal_index
        .min(app.host_sessions.len().saturating_sub(1));
    let order = crate::shell::overlays::creation_order(&app.host_sessions);
    order
        .get(idx)
        .map(|&i| (idx + 1, app.host_sessions[i].id.clone()))
}

/// Resolve a `#N` address to a session id against the live snapshot.
/// `None` when no session holds that number (the receipt names it).
fn seq_to_id(app: &App, n: usize) -> Option<String> {
    let order = crate::shell::overlays::creation_order(&app.host_sessions);
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
fn verb_request(verb: ConsoleVerb, id: String) -> crate::shell::client::ControlRequest {
    match verb {
        ConsoleVerb::Kill => crate::shell::client::ControlRequest::KillThread { thread_id: id },
        ConsoleVerb::Interrupt => crate::shell::client::ControlRequest::Interrupt { thread_id: id },
        ConsoleVerb::Suspend => {
            crate::shell::client::ControlRequest::SuspendThread { thread_id: id }
        }
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

/// Send one control verb and push its receipt when the server answers.
/// `target` is the `#N` the receipt names (the server gets the id).
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
            .send(crate::shell::event_loop::AppMutation::HostConsole(line))
            .await;
        wake(&dirty);
    });
}

/// Discover the server and issue one control verb. `Err` carries either the
/// discovery failure or the server's rejection, both receipt-ready.
async fn discover_and_control(request: crate::shell::client::ControlRequest) -> Result<(), String> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match crate::shell::client::discover(&project_root).await {
        Some(info) => crate::shell::client::control(&info, request).await,
        None => {
            tracing::warn!("dashboard control: no server discovered");
            Err("no server is running".to_string())
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

/// `s` on the dock: suspend the selection. The server refuses a session
/// with an attached client or an active round; the receipt explains.
pub(super) fn suspend_selected(app: &mut App, runtime: &UiRuntime) {
    let Some((seq, id)) = selection_seq(app) else {
        return;
    };
    log_dispatch(app, &format!("/suspend @{seq}"), vec![seq], "suspend");
    spawn_control_verb(runtime, ConsoleVerb::Suspend, Some(seq), id);
}

/// Ask the Archivist one question: a synchronous control round
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
        let outcome = match crate::shell::client::discover(&project_root).await {
            Some(info) => {
                crate::shell::client::control_with_reply(
                    &info,
                    crate::shell::client::ControlRequest::AskArchivist { text: text.clone() },
                )
                .await
            }
            None => Err("no server is running".to_string()),
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
            .send(crate::shell::event_loop::AppMutation::HostConsole(line))
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
        let outcome = discover_and_control(crate::shell::client::ControlRequest::CreateThread {
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
            .send(crate::shell::event_loop::AppMutation::HostConsole(line))
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
        "  ? text        ask the Archivist to find sessions across all projects",
    );
}

/// Dispatch one parsed console line (see
/// [`crate::shell::overlays::parse_console_command`]). `create_when_bare` is the
/// fallback role of bare text: `true` when the prompt was opened with `n`
/// (create a session), `false` when opened with `p` or submitted straight
/// from the console (prompt the dock selection).
pub(super) async fn dispatch_console_command(
    app: &mut App,
    runtime: &UiRuntime,
    raw: &str,
    create_when_bare: bool,
) {
    match crate::shell::overlays::parse_console_command(raw) {
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
                    None => return notice(app, format!("no session #{n} on the server")),
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
                        None => return notice(app, format!("no session #{n} on the server")),
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
            let sends: Vec<(usize, crate::shell::client::ControlRequest)> = resolved
                .into_iter()
                .map(|(n, id)| {
                    (
                        n,
                        crate::shell::client::ControlRequest::SendPrompt {
                            thread_id: id,
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
                let Some(info) = crate::shell::client::discover(&project_root).await else {
                    tracing::warn!("dashboard control: no server discovered");
                    return;
                };
                for (n, request) in sends {
                    let line = match crate::shell::client::control(&info, request).await {
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
                        .send(crate::shell::event_loop::AppMutation::HostConsole(line))
                        .await;
                }
                wake(&dirty);
            });
        }
    }
}

/// How many recent terminal requests the usage view's event tail carries.
pub(crate) const USAGE_EVENT_CAP: usize = 200;

/// ADR-0129 [INV-USAGE-01]: fetch the cross-session model usage report over the
/// session-free control plane.
///
/// Usage accounting is a property of the installation, not of a thread: the
/// store it folds (`data/usage/`) outlives every session that wrote to it and
/// exists before the first one is created. Fetching it through an attached
/// session's duplex made a global statistic unusable without a thread, which is
/// why this path takes no `session_id` at all.
///
/// Fire-and-forget: the result lands as [`AppMutation::UsageStats`] and the
/// event loop repaints. A failure keeps whatever report is already on screen
/// (re-opening never flashes the loading state) and clears only the flag.
pub(crate) fn refresh_usage(app: &mut App, runtime: &UiRuntime) {
    app.usage_loading = true;
    let mutations = runtime.mutations.clone();
    let dirty = (runtime.dirty.clone(), runtime.dirty_notify.clone());
    tokio::spawn(async move {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let outcome = match crate::shell::client::discover(&project_root).await {
            Some(info) => crate::shell::client::get_usage_stats(&info, USAGE_EVENT_CAP).await,
            None => Err("no server is running".to_string()),
        };
        let mutation = match outcome {
            Ok(report) => crate::shell::event_loop::AppMutation::UsageStats(report),
            Err(error) => {
                tracing::warn!(%error, "usage report unavailable");
                crate::shell::event_loop::AppMutation::UsageFailed(error)
            }
        };
        mutations.send(mutation).await;
        wake(&dirty);
    });
}

pub(super) async fn handle_host_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    action: InputAction,
) -> ActionFlow {
    match action {
        InputAction::HostPreviewSelected => {
            let idx = app
                .modal_index
                .min(app.host_sessions.len().saturating_sub(1));
            let order = crate::shell::overlays::creation_order(&app.host_sessions);
            if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                app.host_preview = Some(row.id.clone());
                app.host_preview_scroll = 0;
            }
        }
        InputAction::HostSwitchSelected => {
            let idx = app
                .modal_index
                .min(app.host_sessions.len().saturating_sub(1));
            let order = crate::shell::overlays::creation_order(&app.host_sessions);
            let target_info = order
                .get(idx)
                .and_then(|&i| app.host_sessions.get(i))
                .map(|r| (r.id.clone(), r.title.clone()));
            if let Some((target_id, target_title)) = target_info {
                let switchable = target_id != viewed_session_id;
                if switchable {
                    let is_empty = app.is_current_thread_empty();
                    let tab = crate::shell::surfaces::ClientTab::thread(
                        &target_id,
                        target_title.as_deref(),
                    );
                    if is_empty {
                        app.send_intent(AgentRequest::DeleteThread {
                            id: viewed_session_id.to_string(),
                        });
                        let active_idx = app.surfaces.active_tab_index();
                        if let Some(existing_pos) =
                            app.surfaces.tabs().iter().position(|t| match &t.kind {
                                crate::shell::surfaces::TabKind::Thread(tid) => tid == &target_id,
                                _ => false,
                            })
                        {
                            app.close_tab(active_idx);
                            let new_pos = if existing_pos > active_idx {
                                existing_pos - 1
                            } else {
                                existing_pos
                            };
                            app.surfaces.select_tab(new_pos);
                        } else {
                            app.replace_tab(active_idx, tab);
                        }
                    } else {
                        app.open_tab(tab);
                    }
                    app.switch_to_target = Some(target_id);
                    app.should_quit.store(true, Ordering::SeqCst);
                }
                app.dismiss_active_dialog();
                app.set_active_index(0);
                app.host_prompting = false;
            }
        }
        InputAction::HostToggleUsage => {
            app.dashboard_view_usage = !app.dashboard_view_usage;
            if app.dashboard_view_usage {
                refresh_usage(app, runtime);
            }
        }
        InputAction::HostFilterOpen => {
            app.dashboard_filtering = !app.dashboard_filtering;
            if !app.dashboard_filtering {
                app.dashboard_filter.clear();
            }
        }
        InputAction::HostFocusToggle => {
            app.host_focus = match app.host_focus {
                crate::shell::overlays::DashboardFocus::List => {
                    crate::shell::overlays::DashboardFocus::Detail
                }
                crate::shell::overlays::DashboardFocus::Detail => {
                    crate::shell::overlays::DashboardFocus::List
                }
            };
        }
        InputAction::HostInterruptSelected => {
            dispatch_console_command(app, runtime, "/interrupt", false).await;
        }
        InputAction::HostKillSelected => {
            kill_selected(app, runtime);
        }
        InputAction::HostSuspendSelected => {
            suspend_selected(app, runtime);
        }
        InputAction::HostPromptOpen => {
            app.host_prompting = true;
            app.host_prompt_new = false;
            app.input.clear();
            app.set_cursor(0);
        }
        InputAction::HostNewSession => {
            app.host_prompting = true;
            app.host_prompt_new = true;
            app.input.clear();
            app.set_cursor(0);
        }
        InputAction::HostPromptSeed(c) => {
            app.host_prompting = true;
            app.host_prompt_new = false;
            app.input.clear();
            app.input.insert(0, c);
            app.set_cursor(1);
        }
        InputAction::HostPromptSubmit => {
            let text = app.input.trim().to_string();
            let create_new = app.host_prompt_new;
            app.host_prompting = false;
            app.host_prompt_new = false;
            app.input.clear();
            app.set_cursor(0);
            if text.is_empty() {
                return ActionFlow::Handled;
            }
            dispatch_console_command(app, runtime, &text, create_new).await;
        }
        _ => {}
    }
    ActionFlow::Handled
}
