// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use serde_json::json;
use tool::{
    BuiltinTool, RiskProfile, Tool, ToolContext, ToolError, ToolExposure, ToolOutput,
    prepare_tool_arguments,
};

struct EchoArgs;

#[async_trait]
impl Tool for EchoArgs {
    fn name(&self) -> &str {
        "echo_args"
    }

    fn description(&self) -> &str {
        "Echo a validated argument object"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {"type": "string"},
                "count": {"type": "integer"}
            },
            "required": ["text"],
            "additionalProperties": false
        })
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "text": {"type": "string"},
                "count": {"type": "integer"}
            },
            "required": ["text", "count"]
        }))
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let text = arguments["text"].as_str().unwrap_or_default().to_string();
        let count = arguments["count"].as_u64().unwrap_or(0);
        Ok(ToolOutput::Structured {
            content: format!("{text} x{count}"),
            value: json!({"text": text, "count": count}),
            failed: false,
        })
    }
}

#[tokio::test]
async fn raw_arguments_are_prepared_before_the_tool_runs() {
    let tool = EchoArgs;
    let output = tool
        .execute_json(
            &ToolContext::default(),
            r#"{"text":"hi","count":"2","extra":true}"#,
        )
        .await
        .expect("coercion and closed-object pruning must succeed");
    assert_eq!(output.to_text(), "hi x2");
    assert!(matches!(output, ToolOutput::Structured { .. }));
}

#[tokio::test]
async fn malformed_and_wrong_typed_arguments_never_reach_the_tool() {
    let tool = EchoArgs;
    let malformed = tool
        .execute_json(&ToolContext::default(), "{not json")
        .await
        .expect_err("malformed JSON must be rejected");
    assert!(malformed.to_string().contains("not valid JSON"));

    let wrong_type = tool
        .execute_json(&ToolContext::default(), r#"{"text":42}"#)
        .await
        .expect_err("a number is not a string");
    assert!(wrong_type.to_string().contains("string"), "{wrong_type}");
}

#[test]
fn argument_preparation_reports_repairs() {
    let prepared = prepare_tool_arguments(
        &json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer"},
                "flags": {"type": "array", "items": {"type": "boolean"}}
            }
        }),
        r#"{"limit":"20","flags":"true"}"#,
    )
    .expect("coercible");
    assert_eq!(prepared.value["limit"], json!(20));
    assert_eq!(prepared.value["flags"], json!([true]));
    assert_eq!(prepared.repairs.len(), 3);
}

#[test]
fn builtin_aliases_are_unique_and_resolve() {
    let mut seen = std::collections::HashSet::new();
    for builtin in BuiltinTool::ALL {
        assert!(
            seen.insert(builtin.as_str()),
            "duplicate canonical name {}",
            builtin.as_str()
        );
        assert_eq!(BuiltinTool::from_name(builtin.as_str()), Some(*builtin));
        for alias in builtin.aliases() {
            assert!(
                seen.insert(*alias),
                "alias {alias} is claimed more than once"
            );
            assert_eq!(
                BuiltinTool::from_name(alias),
                Some(*builtin),
                "alias {alias} must resolve to {}",
                builtin.as_str()
            );
        }
    }
}

#[test]
fn tool_exposure_defaults_to_direct_and_is_explicit() {
    assert!(ToolExposure::Direct.is_direct());
    assert!(!ToolExposure::Deferred.is_direct());
    assert!(ToolExposure::Deferred.needs_discovery());
    assert!(!ToolExposure::Hidden.is_model_reachable());
}
