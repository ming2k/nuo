//! Integration and wire fidelity tests for System One discriminative models (TypeSafe Jev).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_inference_wire::discriminative::{
    Answer, DiscriminativeEndpoint, DiscriminativeRequest, DiscriminativeResponse,
    DiscriminativeUsage, NoulAnswer,
};
use serde_json::json;

#[test]
fn test_discriminative_request_builder_and_serialization() {
    let req = DiscriminativeRequest::with_state_text(
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
    let endpoint = DiscriminativeEndpoint::jev("ts_test_key_12345")
        .with_header("X-Custom-Trace", "trace-abc-123");

    let req = DiscriminativeRequest::with_state_text("jev-1.13.0", "State text")
        .add_noul("flag", "Is this valid?");

    let (url, headers, body) = nuo_inference_wire::discriminative::protocol::build_request(
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

    let endpoint = DiscriminativeEndpoint::jev("test_key");
    let resp =
        nuo_inference_wire::discriminative::protocol::parse_response(&endpoint, &raw_response)
            .expect("Failed to parse response");

    assert_eq!(resp.model, "jev-1.13.0");
    assert_eq!(resp.usage.unwrap().input_tokens, 58);

    // Noul assertions
    let urgent = resp.noul("urgent").expect("urgent answer missing");
    assert!((urgent.noul - 0.984).abs() < f64::EPSILON);

    // Choice assertions
    let dept = resp
        .choice("department")
        .expect("department answer missing");
    assert_eq!(dept.choice, "billing");
    assert!((dept.confidence - 0.845).abs() < f64::EPSILON);
    assert_eq!(dept.probabilities.get("billing"), Some(&0.892));

    let ranked = dept.ranked();
    assert_eq!(ranked[0].0, "billing");
    assert_eq!(ranked[1].0, "support");
    assert_eq!(ranked[2].0, "retention");
    assert!((dept.margin() - (0.892 - 0.088)).abs() < 1e-6);

    // Score assertions
    let frustration = resp
        .score("frustration")
        .expect("frustration answer missing");
    assert!((frustration.score - 2.75).abs() < f64::EPSILON);
    assert_eq!(frustration.modal_level(), Some(3));
    assert_eq!(frustration.level_probability(3), Some(0.80));
    assert_eq!(frustration.level_probability(0), Some(0.0));
}

#[test]
fn test_typesafe_parse_error_payload() {
    let error_response = json!({
        "error": {
            "message": "Model 'jev-invalid' not found",
            "type": "invalid_request_error"
        }
    });

    let endpoint = DiscriminativeEndpoint::jev("test_key");
    let res =
        nuo_inference_wire::discriminative::protocol::parse_response(&endpoint, &error_response);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(
        err.to_string().contains("Model 'jev-invalid' not found"),
        "error text: {err}"
    );
}

#[test]
fn test_typesafe_rejects_out_of_bounds_probability() {
    let invalid_prob_response = json!({
        "model": "jev-latest",
        "answers": {
            "urgent": {
                "type": "noul",
                "noul": 1.45 // invalid probability > 1.0
            }
        }
    });

    let endpoint = DiscriminativeEndpoint::jev("test_key");
    let res = nuo_inference_wire::discriminative::protocol::parse_response(
        &endpoint,
        &invalid_prob_response,
    );
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(err.to_string().contains("out of [0, 1] range"));
}

#[test]
fn test_resilient_deserialization_without_explicit_type_tag() {
    let raw = json!({
        "noul": 0.75
    });

    let ans: Answer = serde_json::from_value(raw).expect("resilient parse");
    assert_eq!(ans.as_noul().unwrap().noul, 0.75);

    let raw_choice = json!({
        "choice": "opt_b",
        "confidence": 0.82,
        "probabilities": { "opt_a": 0.18, "opt_b": 0.82 }
    });
    let ans_choice: Answer = serde_json::from_value(raw_choice).expect("resilient parse");
    assert_eq!(ans_choice.as_choice().unwrap().choice, "opt_b");

    let raw_score = json!({
        "score": 3.2,
        "confidence": 0.9,
        "probabilities": { "3": 0.8, "4": 0.2 }
    });
    let ans_score: Answer = serde_json::from_value(raw_score).expect("resilient parse");
    assert_eq!(ans_score.as_score().unwrap().score, 3.2);
}

#[test]
fn test_discriminative_roundtrip_serde() {
    #[derive(serde::Serialize)]
    struct Context {
        user_id: u64,
        role: &'static str,
    }

    let ctx = Context {
        user_id: 42,
        role: "admin",
    };
    let req = DiscriminativeRequest::with_state_value("jev-latest", &ctx)
        .expect("serialize ctx")
        .add_noul("is_authorized", "Verify authorization level");

    let req_json = serde_json::to_string(&req).expect("to string");
    let req_deser: DiscriminativeRequest = serde_json::from_str(&req_json).expect("from string");
    assert_eq!(req_deser.model, "jev-latest");
    assert_eq!(req_deser.questions.len(), 1);

    let mut answers = std::collections::BTreeMap::new();
    answers.insert(
        "is_authorized".to_string(),
        Answer::Noul(NoulAnswer { noul: 0.95 }),
    );
    let resp = DiscriminativeResponse {
        model: "jev-latest".to_string(),
        answers,
        usage: Some(DiscriminativeUsage {
            input_tokens: 12,
            output_tokens: 1,
        }),
    };

    let resp_json = serde_json::to_string(&resp).expect("resp to json");
    let resp_deser: DiscriminativeResponse =
        serde_json::from_str(&resp_json).expect("resp from json");
    assert_eq!(resp_deser.model, "jev-latest");
    assert_eq!(resp_deser.noul("is_authorized").unwrap().noul, 0.95);
}
