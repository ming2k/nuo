//! The stored context measurement (ADR-0220 §4).
//!
//! Two facts live on one value: the plan measured while the request was
//! assembled, and the token shape measured at the attempt that sent it. These
//! tests pin the ordering contract between them — the reason they are stored
//! together instead of being derived from one another.

use super::*;

/// A plan measured for a request whose attempt has not run yet carries no token
/// shape.
#[test]
fn a_fresh_plan_clears_the_previous_estimate() {
    let agent = agent();
    let messages: Vec<Message> = Vec::new();
    // First assembly measures a plan; no attempt has measured its request yet.
    let _ = agent.model_request(&messages);
    let measurement = agent.context_measurement().expect("a plan was measured");
    assert!(
        measurement.estimate.is_none(),
        "a plan that no attempt has sent has no token shape: {measurement:?}"
    );
    assert!(
        !measurement.plan.blocks.is_empty(),
        "the default policy declares sources, so the plan names them"
    );
}

/// The attempt boundary fills the estimate of the plan it is sending, and a
/// later assembly does not inherit it.
#[test]
fn an_estimate_belongs_to_the_request_that_carried_it() {
    let agent = agent();
    let messages: Vec<Message> = Vec::new();
    let _ = agent.model_request(&messages);

    let estimate = agent.estimate_next_request_tokens(&messages);
    let measurement = agent.context_measurement().expect("measurement");
    assert_eq!(
        measurement.estimate,
        Some(estimate),
        "the estimate recorded is the one the attempt measured"
    );
    assert!(
        measurement.estimate.expect("estimate").instructions_tokens > 0,
        "the instruction region is measured, not merely summed"
    );

    // A new assembly starts a new measurement: the old numbers describe a
    // request that is no longer the next one.
    let _ = agent.model_request(&messages);
    assert!(
        agent
            .context_measurement()
            .expect("measurement")
            .estimate
            .is_none(),
        "carrying the previous attempt's numbers forward would describe a request \
         that no longer exists"
    );
}

/// The operator view joins the plan to its rendered bodies, and every body costs
/// what the row reports (`[INV-CTXPLAN-04]`).
#[test]
fn the_operator_view_is_the_plan_plus_its_own_bytes() {
    let agent = agent();
    let messages: Vec<Message> = Vec::new();
    let _ = agent.model_request(&messages);
    let view = agent.context_plan_view().expect("view");

    assert_eq!(
        view.blocks.len(),
        agent.context_plan().expect("plan").blocks.len()
    );
    for block in view.blocks.iter().filter(|block| block.active) {
        let body = block
            .body
            .as_deref()
            .unwrap_or_else(|| panic!("an active block must carry its bytes: {block:?}"));
        assert_eq!(
            tool::count_tokens(body),
            block.tokens,
            "a body that does not cost its reported tokens is a lying inspector: {}",
            block.id
        );
    }
    for block in view.blocks.iter().filter(|block| !block.active) {
        assert!(
            block.body.is_none(),
            "an inactive block must not claim bytes: {block:?}"
        );
    }
}

/// A measurement is a snapshot: answering a question about it must not advance
/// the prefix-drift witness, or the next real request would report drift against
/// a prompt nobody sent (`[INV-CTXPLAN-07]`).
#[test]
fn reading_the_measurement_does_not_disturb_the_drift_witness() {
    let agent = agent();
    let messages: Vec<Message> = Vec::new();
    let _ = agent.model_request(&messages);
    let first = agent.context_plan().expect("plan").drift;

    // Read it a few times through every surface.
    for _ in 0..3 {
        let _ = agent.context_plan();
        let _ = agent.context_measurement();
        let _ = agent.context_plan_view();
        let _ = agent.render_context_table();
    }

    let _ = agent.model_request(&messages);
    assert_eq!(
        agent.context_plan().expect("plan").drift,
        first,
        "reading the plan is a pure read; only a real request may move the witness"
    );
}
