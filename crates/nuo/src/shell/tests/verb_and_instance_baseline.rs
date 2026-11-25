//! `[INV-VERB-02]` / `[INV-INSTANCE-01]` acceptance and baseline tests
//! (ADR-0252 §1b, ADR-0253 §5).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::shell::client::ClientOutbound;
use crate::shell::surfaces::ClientTab;

/// All five trigger families that reach "create a thread" (ADR-0252 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// `actions::tabs::handle_add_tab` — the TabBar's `+` (ADR-0235).
    TabBar,
    /// `InputAction::CreateNewSession` — `Alt-t`.
    AltT,
    /// `actions::palette::execute_command_by_id(..., CommandId::NewTab)`.
    Palette,
    /// `actions::mouse::execute_help_action(..., HelpActionKind::NewTab)`.
    HelpPortal,
    /// `handle_send_slash("/new")` — the composer.
    Composer,
}

impl Trigger {
    const ALL: [Self; 5] = [
        Self::TabBar,
        Self::AltT,
        Self::Palette,
        Self::HelpPortal,
        Self::Composer,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::TabBar => "TabBar +",
            Self::AltT => "Alt-t",
            Self::Palette => "Palette NewTab",
            Self::HelpPortal => "HelpPortal NewTab",
            Self::Composer => "composer /new",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mechanism {
    Resident,
    TeardownReattach,
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    wire: Vec<String>,
    transcript: (usize, usize),
    mechanism: Mechanism,
}

fn outcome(wire: &[&str], transcript: (usize, usize), mechanism: Mechanism) -> Outcome {
    Outcome {
        wire: wire.iter().map(|s| (*s).to_string()).collect(),
        transcript,
        mechanism,
    }
}

fn app_with_tab(session_id: &str) -> App {
    let mut app = new_app_for_relay_tests();
    app.session.current_session_id = session_id.to_string();
    app.session.live_session_id = session_id.to_string();
    app.open_tab(ClientTab::thread(session_id, None::<String>));
    app.hand_composer_to_current_thread();
    app
}

fn app_with_dialogue(session_id: &str) -> App {
    let mut app = app_with_tab(session_id);
    app.threads
        .messages
        .push(view::model::document::TranscriptMessage::new(
            tool::Role::User,
            "an admitted prompt",
        ));
    app
}

fn describe(frame: &ClientOutbound) -> String {
    match frame {
        ClientOutbound::Subscribe { action, .. } => format!("Subscribe({action:?})"),
        ClientOutbound::Unsubscribe { thread_id } => format!("Unsubscribe({thread_id})"),
        ClientOutbound::Request { request, .. } => match request {
            protocol::AgentRequest::SlashCommand(cmd) => format!("SlashCommand({cmd})"),
            other => format!("{other:?}")
                .split(['{', '(', ' '])
                .next()
                .unwrap_or("?")
                .to_string(),
        },
    }
}

async fn drive(app: &mut App, trigger: Trigger, session_id: &str) -> Outcome {
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;
    let hook_session = crate::shell::HookStartSource::Remote {
        session_id: session_id.to_string(),
    };

    match trigger {
        Trigger::TabBar => {
            crate::shell::event_loop::actions::tabs::handle_add_tab(app, &runtime, session_id);
        }
        Trigger::AltT => {
            crate::shell::event_loop::actions::dispatch_action_for_test(
                app,
                &runtime,
                crate::shell::input::InputAction::CreateNewSession,
                session_id,
            )
            .await;
        }
        Trigger::Palette => {
            let (copy_tx, _copy_rx) = tokio::sync::mpsc::unbounded_channel();
            let (paste_tx, _paste_rx) = tokio::sync::mpsc::unbounded_channel();
            let copy_pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ctx = crate::shell::event_loop::actions::ActionContext {
                runtime: &runtime,
                session: &hook_session,
                viewed_session_id: session_id,
                copy_tx: &copy_tx,
                copy_pending: &copy_pending,
                paste_tx: &paste_tx,
                key_event: true,
            };
            crate::shell::event_loop::actions::palette::execute_command_by_id(
                app,
                &ctx,
                crate::shell::keymap::CommandId::NewTab,
            )
            .await;
        }
        Trigger::HelpPortal => {
            crate::shell::event_loop::actions::mouse::execute_help_action(
                app,
                &runtime,
                crate::shell::overlays::help::HelpActionKind::NewTab,
                session_id,
            )
            .await;
        }
        Trigger::Composer => {
            crate::shell::event_loop::handle_send_slash(
                app,
                &runtime,
                &hook_session,
                "/new".to_string(),
            )
            .await;
        }
    }

    let wire: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|frame| describe(&frame))
        .collect();
    let dialogue_rows = app
        .threads
        .messages
        .iter()
        .filter(|m| !m.is_command_result())
        .count();
    let mechanism = if app
        .link
        .should_quit
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        Mechanism::TeardownReattach
    } else {
        Mechanism::Resident
    };
    Outcome {
        wire,
        transcript: (app.threads.messages.len(), dialogue_rows),
        mechanism,
    }
}

async fn measure_all(session_id: &str, seed_dialogue: bool) -> Vec<(Trigger, Outcome)> {
    let mut outcomes = Vec::with_capacity(Trigger::ALL.len());
    for trigger in Trigger::ALL {
        let mut app = if seed_dialogue {
            app_with_dialogue(session_id)
        } else {
            app_with_tab(session_id)
        };
        let outcome = drive(&mut app, trigger, session_id).await;
        outcomes.push((trigger, outcome));
    }
    outcomes
}

fn report(outcomes: &[(Trigger, Outcome)]) -> String {
    outcomes
        .iter()
        .map(|(trigger, outcome)| format!("    {:<19} {outcome:?}", trigger.label()))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------------
// `[INV-VERB-02]` — one action, one outcome across all five triggers.
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn all_triggers_agree_on_a_non_empty_thread() {
    let outcomes = measure_all("verb-nonempty-target", true).await;
    let expected = outcome(&["Subscribe(New(None))"], (1, 1), Mechanism::Resident);
    for (trigger, actual) in &outcomes {
        assert_eq!(
            actual,
            &expected,
            "[INV-VERB-02] (ADR-0252 §1): trigger `{}` diverged on non-empty thread:\n{}",
            trigger.label(),
            report(&outcomes)
        );
    }
}

#[tokio::test]
async fn all_triggers_agree_on_an_empty_thread() {
    let outcomes = measure_all("verb-empty-target", false).await;
    let expected = outcome(&["SlashCommand(/new)"], (0, 0), Mechanism::Resident);
    for (trigger, actual) in &outcomes {
        assert_eq!(
            actual,
            &expected,
            "[INV-VERB-02] (ADR-0252 §1): trigger `{}` diverged on empty thread:\n{}",
            trigger.label(),
            report(&outcomes)
        );
    }
}

// ---------------------------------------------------------------------------------
// `[INV-INSTANCE-01]` — one owner per fact (ADR-0253).
// ---------------------------------------------------------------------------------

#[test]
fn scroll_has_one_owner_per_field() {
    let sid = "instance-scroll";
    let mut app = app_with_tab(sid);
    app.threads.sync_foreground_id(sid);
    app.threads.scroll = 42;

    // Switching away and back moves the single `ThreadInstance` atomically:
    // no duplicate copies on `TabContextState` or `ThreadWorkspace` exist.
    app.activate_thread_workspace("other-thread");
    assert_eq!(app.threads.scroll, 0);
    assert_eq!(app.threads.get(sid).map(|t| t.scroll), Some(42));

    app.activate_thread_workspace(sid);
    assert_eq!(app.threads.scroll, 42);
    app.threads.scroll = 7;
    assert_eq!(app.threads.get(sid).map(|t| t.scroll), Some(7));
}

/// `[INV-INSTANCE-05]` (ADR-0253 §4): the draft's *content* has exactly one owner, and
/// the emptiness answer derives from it rather than from a copy on the tab.
#[test]
fn a_parked_draft_keeps_the_thread_out_of_the_recycle_path() {
    let sid = "instance-draft";
    let mut app = app_with_tab(sid);

    assert!(
        app.is_current_thread_empty(),
        "a fresh thread with no dialogue is empty"
    );

    app.surfaces.drafts.park(
        sid,
        view::composer_draft::ComposerDraft::capture(
            "unsent".to_string(),
            6,
            Vec::new(),
            Vec::new(),
            0,
        ),
    );
    assert!(
        !app.is_current_thread_empty(),
        "[ADR-0247]: a thread holding an unsent draft is not empty and must never be \
         recycled in place"
    );
    assert!(
        app.surfaces.drafts.has_stored(sid),
        "the answer comes from the one durable owner, not from a copy on the tab"
    );
}
