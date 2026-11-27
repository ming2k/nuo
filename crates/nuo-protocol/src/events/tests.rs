//! Wire-type tests.

use super::*;

#[test]
fn turn_context_is_part_of_the_wire_shape() {
    // ADR-0168: the turn identity is not an optional side channel. Pin the
    // externally tagged shape so a future field rename or a scope
    // regression is a deliberate protocol move, not an accident.
    let encoded = serde_json::to_value(AgentResponse::Round {
        thread_id: "s".into(),
        scope: RoundScope::Turn {
            context: TurnContext::new(3, 7, 1_700_000_000_000),
            event: RoundEvent::StreamDiscard,
        },
    })
    .unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({
            "Round": {
                "thread_id": "s",
                "scope": {
                    "Turn": {
                        "context": {
                            "round": 3,
                            "turn": 7,
                            "sent_at_ms": 1_700_000_000_000u64
                        },
                        "event": "StreamDiscard"
                    }
                }
            }
        })
    );
    let decoded: AgentResponse = serde_json::from_value(encoded).unwrap();
    assert!(matches!(
        decoded,
        AgentResponse::Round {
            scope: RoundScope::Turn {
                context: TurnContext {
                    round: 3,
                    turn: 7,
                    ..
                },
                event: RoundEvent::StreamDiscard,
            },
            ..
        }
    ));
}

#[test]
fn round_summary_tps_excludes_human_pause() {
    // Fallback path (no generation_ms recorded): 500 output tokens over
    // 10s wall-clock, 8s of which the user spent deliberating on a
    // permission prompt → only 2s of active time. TPS = 250.
    let summary = RoundSummary {
        round: 3,
        output_tokens: 500,
        duration_ms: 10_000,
        paused_ms: 8_000,
        generation_ms: 0,
        ..Default::default()
    };
    assert_eq!(summary.active_ms(), 2_000);
    assert!(
        (summary.tps() - 250.0).abs() < 0.01,
        "got {}",
        summary.tps()
    );
}

#[test]
fn round_summary_tps_uses_generation_time_excluding_tools() {
    // A round that streamed 500 output tokens in 2s of real generation,
    // then spent 30s executing tools, then 8s parked on a permission.
    // Only the 2s of generation counts: TPS = 250, NOT ~12.5 (round
    // active_ms) and NOT ~3.1 (wall-clock).
    let summary = RoundSummary {
        round: 3,
        output_tokens: 500,
        duration_ms: 40_000,
        paused_ms: 8_000,
        generation_ms: 2_000,
        ..Default::default()
    };
    assert_eq!(summary.active_ms(), 32_000);
    assert!(
        (summary.tps() - 250.0).abs() < 0.01,
        "got {}",
        summary.tps()
    );
}

#[test]
fn round_summary_tps_is_zero_when_round_had_no_active_time() {
    let summary = RoundSummary {
        round: 1,
        output_tokens: 0,
        duration_ms: 0,
        paused_ms: 0,
        generation_ms: 0,
        ..Default::default()
    };
    assert_eq!(summary.active_ms(), 0);
    assert_eq!(summary.tps(), 0.0);
}

#[test]
fn round_summary_active_time_saturates_when_pause_exceeds_duration() {
    // Defensive: a round whose recorded pause exceeds its wall-clock
    // (shouldn't happen, but must never panic on subtraction) yields zero
    // active time rather than a negative.
    let summary = RoundSummary {
        round: 1,
        output_tokens: 100,
        duration_ms: 1_000,
        paused_ms: 2_000,
        generation_ms: 0,
        ..Default::default()
    };
    assert_eq!(summary.active_ms(), 0);
    assert_eq!(summary.tps(), 0.0);
}

#[test]
fn command_ack_notice_is_toast_surfaced_info_from_harness() {
    // A slash-command reply (the *acknowledgment*, not the invocation) is
    // stamped uniformly so frontends can branch on kind + surface:
    //   - severity Info (it is a status confirmation, not an error),
    //   - the kind that `nuo_view::model::layer` routes to the toast layer
    //     (a transient bubble, never appended to the transcript),
    //   - source Harness (not the agent / a tool).
    let notice = AgentNotice::command_ack("Delegated mode ON: …");
    assert_eq!(notice.kind, NoticeKind::CommandAck);
    assert_eq!(notice.severity, NoticeSeverity::Info);
    assert_eq!(notice.source, NoticeSource::Harness);
    assert_eq!(notice.title, "Delegated mode ON: …");
    assert!(notice.body.is_none());
}

#[test]
fn command_ack_kind_serialises_as_snake_case() {
    // The closed NoticeKind classifier must serialise the new variant
    // distinctly so frontends (and persisted/forwarded notices) cannot
    // confuse it with the existing kinds.
    let notice = AgentNotice::command_ack("x");
    let json = serde_json::to_string(&notice.kind).expect("serialise");
    assert_eq!(json, "\"command_ack\"");
}

#[test]
fn trust_changed_kind_serialises_as_snake_case_and_round_trips() {
    // TrustChanged is first-class, stamped uniformly by
    // `trust_changed()` — Warning severity, Harness source — and must stay
    // distinct from the generic ReviewAlert on the wire so frontends can
    // branch on it exactly.
    let notice = AgentNotice::trust_changed("Workspace configurations changed");
    assert_eq!(notice.kind, NoticeKind::TrustChanged);
    assert_eq!(notice.severity, NoticeSeverity::Warning);
    assert_eq!(notice.source, NoticeSource::Harness);
    let json = serde_json::to_string(&notice.kind).expect("serialise");
    assert_eq!(json, "\"trust_changed\"");
    let back: AgentNotice =
        serde_json::from_str(&serde_json::to_string(&notice).unwrap()).expect("round-trip");
    assert_eq!(back.kind, NoticeKind::TrustChanged);
}

#[test]
fn a_trust_decision_is_carried_explicitly_not_inferred_from_the_domains() {
    // The inversion this makes unrepresentable: `domains` alone does not say
    // which direction it points, so a caller holding only a list of refused
    // domains would have *granted* them. The decision is its own field.
    let grant = AgentRequest::TrustWorkspace {
        domains: vec![TrustDomain::Instructions],
        decision: TrustDecision::Grant,
    };
    let deny = AgentRequest::TrustWorkspace {
        domains: vec![TrustDomain::Instructions],
        decision: TrustDecision::Deny,
    };
    let grant_json = serde_json::to_string(&grant).expect("serialise");
    let deny_json = serde_json::to_string(&deny).expect("serialise");
    assert_ne!(
        grant_json, deny_json,
        "identical domain lists with opposite decisions must not serialise identically"
    );
    assert!(deny_json.contains("\"deny\""), "got {deny_json}");
    assert!(grant_json.contains("\"grant\""), "got {grant_json}");

    // The decision is mandatory: a frame that omits it is rejected rather
    // than silently read as a grant. Defaulting would mean a malformed or
    // truncated frame *authorizes* project content, which is the one
    // direction the default must never point.
    let missing =
        serde_json::from_str::<AgentRequest>(r#"{"TrustWorkspace":{"domains":["instructions"]}}"#);
    assert!(
        missing.is_err(),
        "a TrustWorkspace without a decision must not deserialise"
    );
}

#[test]
fn command_ack_kind_round_trips() {
    let notice = AgentNotice::command_ack("x");
    let json = serde_json::to_string(&notice).expect("serialise");
    let back: AgentNotice = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back.kind, NoticeKind::CommandAck);
}

/// The context-plan reply round-trips with its bodies (ADR-0220 §1).
///
/// The reply is the one place a rendered prompt crosses the wire, so the shape
/// must survive serde intact: an active block that lost its `body` in transit
/// would render a blank inspector, and a dropped `thread_id` would let a reply
/// that raced a switch populate the wrong thread.
#[test]
fn a_context_plan_reply_round_trips_with_its_bodies() {
    use crate::context::{ContextPlanView, PlannedBlockView, Position, Stability, Topic};

    let plan = ContextPlanView {
        blocks: vec![PlannedBlockView {
            id: "project.rules".to_owned(),
            topic: Topic::ProjectAuthority,
            rank: Topic::ProjectAuthority.rank(),
            position: Position::Body,
            stability: Stability::Workspace,
            tokens: 12,
            elision: None,
            active: true,
            inactive_reason: None,
            body: Some("<policy source=\"AGENTS.md\">\nFollow the rules.\n</policy>".to_owned()),
        }],
        fixed_tokens: 12,
        window_tokens: 200_000,
        instructions_ceiling_tokens: 16_000,
        ..ContextPlanView::default()
    };
    let expected = plan.clone();
    let reply = AgentResponse::ContextPlan {
        thread_id: "s1".to_owned(),
        plan: Some(plan),
    };
    let json = serde_json::to_string(&reply).expect("serialise");
    let back: AgentResponse = serde_json::from_str(&json).expect("deserialise");
    match back {
        AgentResponse::ContextPlan { thread_id, plan } => {
            assert_eq!(thread_id, "s1");
            assert_eq!(plan, Some(expected), "the plan must survive intact");
            let body = plan
                .expect("plan")
                .blocks
                .first()
                .and_then(|block| block.body.clone())
                .expect("the inactive filter must not have dropped an active body");
            assert!(body.contains("Follow the rules."), "{body}");
        }
        other => panic!("expected a ContextPlan reply, got {other:?}"),
    }

    // `None` is a real answer (no request assembled yet), not a malformed frame.
    let empty = serde_json::to_string(&AgentResponse::ContextPlan {
        thread_id: "s1".to_owned(),
        plan: None,
    })
    .expect("serialise");
    let back: AgentResponse = serde_json::from_str(&empty).expect("deserialise");
    assert!(matches!(
        back,
        AgentResponse::ContextPlan { plan: None, .. }
    ));
}

/// The request is a unit verb: it names no thread, because the driver that owns
/// the sender answers for its own session (ADR-0220 §3).
#[test]
fn the_context_plan_query_carries_no_thread() {
    let json = serde_json::to_string(&AgentRequest::QueryContextPlan).expect("serialise");
    assert_eq!(json, "\"QueryContextPlan\"", "got {json}");
    let back: AgentRequest = serde_json::from_str(&json).expect("deserialise");
    assert!(matches!(back, AgentRequest::QueryContextPlan));
}
