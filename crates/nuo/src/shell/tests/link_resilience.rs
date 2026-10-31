//! A protocol skew must not look like a session end (ADR-0136).
//!
//! The failure these tests pin, verbatim from a real client log:
//!
//! ```text
//! WARN nuo::transport: attach: wire recv failed
//!      error=Failed to deserialize Wire payload:
//!      unknown variant `RetryResolved`, ...
//! INFO nuo::shell::event_loop: app exiting reason="should_quit_flag"
//! ```
//!
//! A server binary that had been replaced on disk kept running and kept
//! emitting a variant the client no longer modelled. The decoder reported the
//! frame as `Err`, the reader task read `Err` as "the socket died", and the
//! synthesized `Exit` closed the window mid-round — with `port: 0` (UDS-only)
//! suppressing even the detach epilogue, so the operator's screen simply
//! vanished.
//!
//! Two independent defects let that happen, and both are locked here:
//!
//! 1. **Encoding**: an undecodable *frame* was reported as a stream error, so
//!    it was indistinguishable from a dead socket.
//! 2. **Semantics**: a dead link was spelled `AgentResponse::Exit`, the same
//!    variant the server uses to *instruct* a client to stop.

use super::*;
use crate::shell::event_loop::AppMutation;

/// `[INV-WIRE-01]`: an envelope naming a variant this build does not model is a
/// frame outcome, never a transport error — and the stream stays aligned.
#[test]
fn an_unknown_wire_variant_does_not_end_the_stream() {
    // A payload shaped exactly like the real one: a `Response` envelope whose
    // inner `AgentResponse` variant this build no longer models.
    let skewed = br#"{"type":"Response","RetryResolved":{"attempt":1,"max_attempts":30}}"#;

    match nuo_protocol::decode_wire_payload(skewed) {
        nuo_protocol::WireFrame::Undecodable { envelope, detail } => {
            assert_eq!(
                envelope.as_deref(),
                Some("Response"),
                "the disclosure must name the envelope direction, so an operator can \
                 tell which side drifted"
            );
            assert!(
                detail.contains("AgentResponse"),
                "the disclosure must name the enum that failed, got: {detail}"
            );
        }
        nuo_protocol::WireFrame::Wire(other) => {
            panic!("an unknown variant must never decode as a known envelope: {other:?}")
        }
    }
}

/// `[INV-WIRE-02]`: a dead link is disclosed and latched, never turned into a
/// quit.
///
/// The client cannot know the session ended just because its own reader
/// stopped: the session lives on the server, and `/exit` is the only thing that
/// ends it. So `LinkLost` must leave `should_quit` untouched and leave a
/// retained record behind.
#[test]
fn a_lost_link_is_disclosed_and_never_quits_the_client() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    assert!(!app.link_down, "fixture starts with a live link");
    let notices_before = app.notice_log().len();

    let repainted = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::LinkLost {
            reason: "the server closed the connection".to_string(),
        },
    );

    assert!(repainted, "a lost link changes what the chrome must show");
    assert!(
        app.link_down,
        "the dead link must latch, so the activity bar states it instead of \
         painting a live-looking chrome over a closed socket"
    );
    assert!(
        app.relink_requested,
        "the attach loop must be asked to re-dial: a latched `link_down` on its \
         own is a dead end — the chrome says the link is gone and nothing can \
         ever restore it without the operator restarting the client"
    );
    assert!(
        !app.should_quit.load(std::sync::atomic::Ordering::SeqCst),
        "a lost link must never quit the client: the session is still hosted \
         and the operator did not ask to leave"
    );

    let log = app.notice_log();
    assert_eq!(
        log.len(),
        notices_before + 1,
        "the reason must be retained, not printed to a screen that is going away"
    );
    let record = log.lines().back().expect("the notice was appended");
    assert_eq!(record.status, nuo_view::model::log::LineStatus::Failed);
    assert!(
        record.text.contains("session server"),
        "the disclosure must name what was lost, got: {}",
        record.text
    );
    assert!(
        record.text.contains("still running"),
        "the disclosure must say the work is safe, so the operator does not \
         assume the session died with the window, got: {}",
        record.text
    );
}

/// The complement: a server *instruction* to stop still quits. Collapsing both
/// into "the link is over" was the bug; keeping them separate is the fix.
#[test]
fn a_server_exit_instruction_still_quits() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    crate::shell::event_loop::apply::apply(&mut app, &runtime, AppMutation::Quit);

    assert!(
        app.should_quit.load(std::sync::atomic::Ordering::SeqCst),
        "`/exit`, a killed thread, or a server drain must still end the client"
    );
    assert!(
        !app.link_down,
        "an instructed exit is not a fault and must not be reported as one"
    );
}

/// A server instruction to stop must not request a re-dial: the link did not
/// fail, the session ended. Conflating the two would re-attach a client whose
/// session is genuinely gone, turning `/exit` into a reconnect loop.
#[test]
fn a_server_exit_does_not_request_a_relink() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    crate::shell::event_loop::apply::apply(&mut app, &runtime, AppMutation::Quit);

    assert!(
        !app.relink_requested,
        "an instructed exit ends the session; re-dialing would fight the server"
    );
    assert!(!app.link_down, "an instructed exit is not a fault");
}

/// The disclosure must carry the *real* transport error, not a canned string.
///
/// This pins the difference between "we know what broke" and "something broke":
/// the reader records its own error, and the notice repeats it verbatim so an
/// operator can act on it (ADR-0136).
#[test]
fn the_link_loss_disclosure_repeats_the_transport_error() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    const REASON: &str = "IPC read error: Connection reset by peer (os error 104)";
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        AppMutation::LinkLost {
            reason: REASON.to_string(),
        },
    );

    let log = app.notice_log();
    let record = log.lines().back().expect("the notice was appended");
    assert!(
        record.text.contains(REASON),
        "the disclosure must repeat the transport error verbatim, since that is \
         the only thing that tells an operator whether the server died, the \
         socket was reset, or a firewall dropped them; got: {}",
        record.text
    );
}
