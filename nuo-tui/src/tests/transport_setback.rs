//! Transport-setback clause lifetime (ADR-0235).
//!
//! The activity bar's `retry N/M (…)` clause annotates one phase —
//! `Phase::AwaitingModel` — and the bar showed it long after the retry had
//! landed: the clause was latched in a field that the response translator
//! cleared by hand at ten "this means the retry is over" arms, and the two arms
//! that carry an ordinary successful answer (`StreamStart`, `StreamDelta`)
//! were not among them. The clause therefore rode beside `answering` for the
//! whole streaming response.
//!
//! These tests pin the replacement invariant: the clause is published by the
//! producer and retired *by the session's next phase write*, whatever produced
//! it. They drive the applier with the exact mutation sequences the translator
//! emits, so a future arm that forgets something fails here rather than on a
//! user's screen.

use super::*;
use crate::event_loop::UiRuntime;

fn setback(attempt: usize, max_attempts: usize) -> crate::app::ProviderRetryState {
    crate::app::ProviderRetryState {
        attempt,
        max_attempts,
        retry_at: std::time::Instant::now() + std::time::Duration::from_secs(4),
        failure: "Anthropic HTTP 529: Overloaded".to_string(),
    }
}

/// The producer half of `RoundEvent::RetryScheduled` in the response
/// translator, routing included: the primary session's clause goes to the App
/// mirror, a live aside's to its own chrome entry.
fn publish_setback(
    app: &mut App,
    runtime: &UiRuntime,
    session_id: &str,
    side: bool,
    attempt: usize,
) {
    let state = setback(attempt, 16);
    if side {
        crate::event_loop::apply::apply(
            app,
            runtime,
            crate::event_loop::AppMutation::ChromeEdit {
                session_id: session_id.to_string(),
                edit: crate::event_loop::mutations::ChromeEdit::TransportSetback(Box::new(state)),
            },
        );
    } else {
        // RetryScheduled also parks the round in the transport-wait phase.
        crate::event_loop::apply::apply(
            app,
            runtime,
            crate::event_loop::AppMutation::SetProviderRetry(state),
        );
        crate::event_loop::apply::apply(
            app,
            runtime,
            crate::event_loop::AppMutation::SetPhase(Some(crate::phase::Phase::AwaitingModel)),
        );
    }
}

fn set_phase(app: &mut App, runtime: &UiRuntime, phase: Option<crate::phase::Phase>) {
    crate::event_loop::apply::apply(
        app,
        runtime,
        crate::event_loop::AppMutation::SetPhase(phase),
    );
}

fn chrome_edit(
    app: &mut App,
    runtime: &UiRuntime,
    session_id: &str,
    edit: crate::event_loop::mutations::ChromeEdit,
) {
    crate::event_loop::apply::apply(
        app,
        runtime,
        crate::event_loop::AppMutation::ChromeEdit {
            session_id: session_id.to_string(),
            edit,
        },
    );
}

#[test]
fn the_transport_wait_phase_hosts_the_clause() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    publish_setback(&mut app, &runtime, "s1", false, 2);

    // The viewed chrome carries the countdown, and the breathing dot keeps
    // animating for it.
    let viewed = app.viewed_chrome();
    let clause = viewed
        .transport_setback
        .as_ref()
        .expect("the clause is live while the phase waits on the model");
    assert_eq!(
        clause.summary(std::time::Instant::now()),
        "retry 1/15 (next in 4.0s)"
    );
    assert!(app.has_live_transport_setback());
}

#[test]
fn a_second_retry_replaces_the_clause_and_keeps_it_alive() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    publish_setback(&mut app, &runtime, "s1", false, 2);
    publish_setback(&mut app, &runtime, "s1", false, 3);

    let viewed = app.viewed_chrome();
    let clause = viewed.transport_setback.expect("still backing off");
    assert_eq!(clause.attempt, 3, "the newest setback wins");
    assert_eq!(viewed.phase, Some(crate::phase::Phase::AwaitingModel));
}

/// The reported bug, at its narrowest: the stream started (the retried request
/// landed), the bar moved to `answering` — and the clause stayed up for the
/// whole response.
#[test]
fn a_started_stream_retires_the_clause() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    publish_setback(&mut app, &runtime, "s1", false, 2);
    assert!(app.has_live_transport_setback());

    // `RoundEvent::StreamStart`: the primary mirror's phase fact, plus the
    // session-scoped chrome edit.
    set_phase(&mut app, &runtime, Some(crate::phase::Phase::Answering));
    chrome_edit(
        &mut app,
        &runtime,
        "s1",
        crate::event_loop::mutations::ChromeEdit::StreamStarted,
    );

    let viewed = app.viewed_chrome();
    assert_eq!(viewed.phase, Some(crate::phase::Phase::Answering));
    assert!(
        viewed.transport_setback.is_none(),
        "a successful stream is evidence the retried request landed"
    );
    assert!(
        !app.has_live_transport_setback(),
        "and the dot stops paying for it"
    );
}

/// Every other in-flight progress fact retires it too — text deltas, reasoning,
/// tool work, a one-shot `Text` payload, and the round's end.
#[test]
fn every_progress_fact_retires_the_clause() {
    let cases: Vec<(&str, Option<crate::phase::Phase>)> = vec![
        ("answering (delta)", Some(crate::phase::Phase::Answering)),
        ("thinking", Some(crate::phase::Phase::Reasoning)),
        (
            "tool work",
            Some(crate::phase::Phase::Tool(crate::phase::ToolVerb::Running)),
        ),
        ("finalizing", Some(crate::phase::Phase::Finalizing)),
        ("awaiting the user", Some(crate::phase::Phase::AwaitingUser)),
        ("idle", None),
    ];

    for (label, phase) in cases {
        let (mut app, _tmp) = app_in_tempdir(&[], &[]);
        let runtime = crate::event_loop::UiRuntime::minimal_for_test();
        publish_setback(&mut app, &runtime, "s1", false, 2);
        assert!(app.has_live_transport_setback(), "{label}: precondition");

        set_phase(&mut app, &runtime, phase);
        assert!(
            !app.has_live_transport_setback(),
            "{label} is progress: the clause must be gone"
        );
    }
}

/// `RoundEvent::Text` — the one-shot payload a non-streaming route delivers —
/// is an answer like any other, so it moves the phase and takes the clause with
/// it.
#[test]
fn a_one_shot_text_payload_retires_the_clause() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    publish_setback(&mut app, &runtime, "s1", false, 2);

    // The translator's `Text` arm: chrome phase fact first, then the primary
    // mirror (the round is still running, so it does not idle the bar).
    chrome_edit(
        &mut app,
        &runtime,
        "s1",
        crate::event_loop::mutations::ChromeEdit::PhaseOnly(Some(crate::phase::Phase::Answering)),
    );
    set_phase(&mut app, &runtime, Some(crate::phase::Phase::Answering));

    assert!(!app.has_live_transport_setback());
    assert_eq!(
        app.viewed_chrome().phase,
        Some(crate::phase::Phase::Answering)
    );
}

/// An interrupted round ends the clause: the translator's interrupt/error arms
/// write the primary mirror *and* the session chrome, and both must end up
/// clean.
#[test]
fn an_ended_round_retires_the_clause() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    app.live_session_id = "s1".to_string();

    publish_setback(&mut app, &runtime, "s1", false, 2);
    set_phase(&mut app, &runtime, None);
    chrome_edit(
        &mut app,
        &runtime,
        "s1",
        crate::event_loop::mutations::ChromeEdit::RoundEnded,
    );

    assert!(app.viewed_chrome().phase.is_none());
    assert!(!app.has_live_transport_setback());
    assert!(
        app.session_chrome
            .get("s1")
            .is_some_and(|chrome| chrome.transport_setback.is_none() && chrome.phase.is_none()),
        "the session-scoped store is clean too"
    );
}

/// A background `/btw` aside backing off against a rate-limited upstream is its
/// own session's business: the primary view's bar stays clean (the bug the
/// session-scoped chrome exists for), while the countdown keeps ticking for the
/// aside itself.
#[test]
fn an_asides_setback_never_reaches_the_primary_bar() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    app.current_session_id = "primary".to_string();

    publish_setback(&mut app, &runtime, "side-1", true, 2);

    assert!(
        app.viewed_chrome().transport_setback.is_none(),
        "the primary view must not inherit a background aside's countdown"
    );
    assert!(
        app.has_live_transport_setback(),
        "…but the aside's countdown still needs frames to tick"
    );

    // Inside the aside, the clause is the aside's own.
    app.enter_side_view("side-1".to_string());
    assert!(app.viewed_chrome().transport_setback.is_some());

    // Leaving restores the primary's clean bar — with nothing parked to
    // resurrect, because the clause is not a displayed-mirror slot.
    app.exit_side_view();
    assert!(app.viewed_chrome().transport_setback.is_none());
}

/// The aside's own phase progress retires its clause through the chrome path.
#[test]
fn an_asides_phase_progress_retires_its_own_clause() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    app.current_session_id = "primary".to_string();

    publish_setback(&mut app, &runtime, "side-1", true, 2);
    assert!(app.has_live_transport_setback());

    chrome_edit(
        &mut app,
        &runtime,
        "side-1",
        crate::event_loop::mutations::ChromeEdit::StreamStarted,
    );

    assert!(
        !app.has_live_transport_setback(),
        "the aside's stream started, so its setback is over"
    );
}
