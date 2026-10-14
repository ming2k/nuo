//! Integration and wire fidelity tests for System One decision models (TypeSafe Jev).

use nuo_model_codec::decision::{
    DecisionAnswer, DecisionEndpoint, DecisionRequest,
    DecisionResponse, DecisionUsage, NoulAnswer,
};
use serde_json::json;

#[test]
fn test_decision_request_builder_and_serialization() {
    let req = DecisionRequest::with_state_text(
        "jev-latest",
        "Customer: I was charged twice and nobody has replied for 3 days.",
    )
    .add_noul("urgent", "Escalate to a human now?")
    .add_choice(
        "department",
        "Route ticket to owner",
        [
            ("billing", "money, charge, invoice issues"),
            ("support", "technical bugs and product questions"),
            ("retention", "cancellation and churn threats"),
        ],
    )
    .add_score(
        "frustration",
        "How frustrated is the customer?",
        ["calm", "annoyed", "frustrated", "furious"],
    );

    let serialized = serde_json::to_value(&req).expect("Failed to serialize request");

    assert_eq!(serialized["model"], "jev-latest");
    assert_eq!(
        serialized["state"],
        "Customer: I was charged twice and nobody has replied for 3 days."
    );

    let questions = serialized["questions"]
        .as_object()
        .expect("questions must be an object");
    assert_eq!(questions.len(), 3);

    assert_eq!(questions["urgent"]["type"], "noul");
    assert_eq!(
        questions["urgent"]["instructions"],
        "Escalate to a human now?"
    );

    assert_eq!(questions["department"]["type"], "choice");
    assert_eq!(
        questions["department"]["instructions"],
        "Route ticket to owner"
    );
    assert_eq!(
        questions["department"]["criteria"]["billing"],
        "money, charge, invoice issues"
    );

    assert_eq!(questions["frustration"]["type"], "score");
    let criteria = questions["frustration"]["criteria"]
        .as_array()
        .expect("criteria must be array");
    assert_eq!(criteria.len(), 4);
    assert_eq!(criteria[0], "calm");
    assert_eq!(criteria[3], "furious");
}

#[test]
fn test_typesafe_build_request_wire_framing() {
    let endpoint = DecisionEndpoint::jev("ts_test_key_12345")
        .with_header("X-Custom-Trace", "trace-abc-123");

    let req = DecisionRequest::with_state_text("jev-1.13.0", "State text")
        .add_noul("flag", "Is this valid?");

    let (url, headers, body) = nuo_model_codec::decision::protocol::build_request(
        &endpoint,
        "ts_test_key_12345",
        &req,
    );

    assert_eq!(url, "https://api.typesafe.ai/v1/systemone");
    assert_eq!(headers.get("content-type").unwrap(), "application/json");
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer ts_test_key_12345"
    );
    assert_eq!(headers.get("x-custom-trace").unwrap(), "trace-abc-123");
    assert_eq!(body["model"], "jev-1.13.0");
}

#[test]
fn test_typesafe_parse_response_with_calibrated_answers() {
    let raw_response = json!({
        "model": "jev-1.13.0",
        "answers": {
            "urgent": {
                "type": "noul",
                "noul": 0.984
            },
            "department": {
                "type": "choice",
                "choice": "billing",
                "probabilities": {
                    "billing": 0.892,
                    "support": 0.088,
                    "retention": 0.020
                },
                "confidence": 0.845
            },
            "frustration": {
                "type": "score",
                "score": 2.75,
                "legend": {
                    "0": "calm",
                    "1": "annoyed",
                    "2": "frustrated",
                    "3": "furious"
                },
                "probabilities": {
                    "0": 0.0,
                    "1": 0.05,
                    "2": 0.15,
                    "3": 0.80
                },
                "confidence": 0.792
            }
        },
        "usage": {
            "input_tokens": 58,
            "output_tokens": 3
        }
    });

    let endpoint = DecisionEndpoint::jev("test_key");
    let resp = nuo_model_codec::decision::protocol::parse_response(&endpoint, &raw_response)
        .expect("Failed to parse response");

    assert_eq!(resp.model, "jev-1.13.0");

    // Noul getter assertions
    assert_eq!(resp.noul_prob("urgent"), Some(0.984));
    let noul_ans = resp.noul("urgent").expect("urgent answer missing");
    assert_eq!(noul_ans.noul, 0.984);

    // Choice getter assertions
    assert_eq!(resp.choice_value("department"), Some("billing"));
    let choice_ans = resp.choice("department").expect("department answer missing");
    assert_eq!(choice_ans.choice, "billing");
    assert_eq!(choice_ans.confidence, 0.845);
    assert_eq!(choice_ans.probabilities.get("billing"), Some(&0.892));
    let ranked = choice_ans.ranked();
    assert_eq!(ranked[0].0, "billing");
    assert_eq!(ranked[1].0, "support");
    assert!((choice_ans.margin() - (0.892 - 0.088)).abs() < 1e-6);

    // Score getter assertions
    assert_eq!(resp.score_val("frustration"), Some(2.75));
    let score_ans = resp.score("frustration").expect("frustration answer missing");
    assert_eq!(score_ans.score, 2.75);
    assert_eq!(score_ans.confidence, 0.792);
    assert_eq!(score_ans.legend.get("3"), Some(&"furious".to_string()));
    assert_eq!(score_ans.modal_level(), Some(3));
    assert_eq!(score_ans.level_probability(3), Some(0.80));

    // Usage assertions
    let usage = resp.usage.expect("usage missing");
    assert_eq!(usage.input_tokens, 58);
    assert_eq!(usage.output_tokens, 3);
}

#[test]
fn test_decision_protocol_error_handling() {
    let endpoint = DecisionEndpoint::jev("test_key");
    let error_response = json!({
        "error": {
            "message": "Invalid API key provided",
            "type": "authentication_error"
        }
    });

    let res = nuo_model_codec::decision::protocol::parse_response(&endpoint, &error_response);
    assert!(res.is_err());
    let err_str = res.unwrap_err().to_string();
    assert!(err_str.contains("Invalid API key provided"));
}

#[test]
fn test_decision_probability_out_of_bounds_rejection() {
    let endpoint = DecisionEndpoint::jev("test_key");
    let invalid_prob_response = json!({
        "model": "jev-latest",
        "answers": {
            "invalid_noul": {
                "type": "noul",
                "noul": 1.55 // Invalid probability > 1.0
            }
        }
    });

    let res = nuo_model_codec::decision::protocol::parse_response(&endpoint, &invalid_prob_response);
    assert!(res.is_err());
    let err_str = res.unwrap_err().to_string();
    assert!(err_str.contains("out of [0, 1] range"));
}

#[test]
fn test_decision_answer_resilient_untagged_deserialization() {
    // Test that even without explicit "type": "noul", deserializer can recover
    let untagged_noul = json!({
        "noul": 0.42
    });
    let ans: DecisionAnswer = serde_json::from_value(untagged_noul).expect("Failed to deserialize untagged noul");
    assert_eq!(ans.as_noul().unwrap().noul, 0.42);

    let untagged_choice = json!({
        "choice": "opt_a",
        "probabilities": {"opt_a": 0.8, "opt_b": 0.2},
        "confidence": 0.8
    });
    let ans_choice: DecisionAnswer = serde_json::from_value(untagged_choice).expect("Failed to deserialize untagged choice");
    assert_eq!(ans_choice.as_choice().unwrap().choice, "opt_a");
}

#[test]
fn test_decision_request_with_structured_state() {
    #[derive(serde::Serialize)]
    struct TicketContext {
        user_id: u64,
        invoice_status: &'static str,
        retry_count: u32,
    }

    let ctx = TicketContext {
        user_id: 9812,
        invoice_status: "FAILED_INSUFFICIENT_FUNDS",
        retry_count: 3,
    };

    let req = DecisionRequest::with_state_value("jev-latest", &ctx)
        .expect("serialization should succeed")
        .add_noul("should_block_account", "Should this account be suspended?");

    assert_eq!(req.state["user_id"], 9812);
    assert_eq!(req.state["invoice_status"], "FAILED_INSUFFICIENT_FUNDS");
    assert_eq!(req.state["retry_count"], 3);
}

#[test]
fn test_decision_response_roundtrip_fidelity() {
    let original = DecisionResponse {
        model: "jev-latest".to_string(),
        answers: [
            (
                "q1".to_string(),
                DecisionAnswer::Noul(NoulAnswer { noul: 0.95 }),
            ),
        ]
        .into_iter()
        .collect(),
        usage: Some(DecisionUsage {
            input_tokens: 10,
            output_tokens: 1,
        }),
    };

    let serialized = serde_json::to_value(&original).expect("Failed to serialize");
    let deserialized: DecisionResponse =
        serde_json::from_value(serialized).expect("Failed to deserialize");

    assert_eq!(original, deserialized);
}
