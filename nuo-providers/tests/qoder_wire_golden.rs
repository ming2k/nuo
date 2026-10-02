//! Golden-wire integration tests for Qoder's shaped pipeline ([INV-WIRE-01],
//! ADR-0271).
//!
//! These observe the *planned* outbound request exactly as the executor would
//! send it — URL, header set, and body bytes. They exist because the
//! 0.50.5→0.50.7 pipeline refactor left the phases unwired and every
//! phase-level test stayed green while production 404'd. This crate hosts the
//! tests because it is the only one that can reference both the executor
//! (`muta-llm-client`) and the Qoder wire implementation without inverting the
//! dependency graph.

// Golden-wire tests assert on parsed JSON; an `expect` that names the missing
// field is the most useful failure mode here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nuo_contracts::{
    ClientPreset, ResolvedAuth, SecretString,
    TransportTelemetry,
};
use nuo_providers::protocol::openai::chat_completions::OpenAiChatCompletionsProvider;
use nuo_providers::qoder::{build_qoder_pipeline, QoderRequestIdentity};

/// A static OAuth-like credential source: token + typed Qoder identity.
#[derive(Debug)]
struct StaticOAuthSource {
    token: &'static str,
    identity: QoderRequestIdentity,
}

impl nuo_contracts::CredentialSource for StaticOAuthSource {
    fn resolve_auth(&self) -> futures::future::BoxFuture<'_, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            Ok(ResolvedAuth::new(self.token).with_extension(self.identity.clone()))
        })
    }
    fn force_refresh(&self) -> futures::future::BoxFuture<'_, Result<ResolvedAuth, String>> {
        unimplemented!("golden-wire tests never trigger a refresh")
    }
    fn is_oauth(&self) -> bool {
        true
    }
}

fn qoder_test_identity() -> QoderRequestIdentity {
    QoderRequestIdentity {
        uid: "u_test_uid".to_string(),
        machine_key_hex: SecretString::new("0123456789abcdef0123456789abcdef".to_string()),
        data_policy_agreed: true,
        organization_id: None,
        organization_tags: Vec::new(),
        infer_endpoint: None,
    }
}

fn qoder_wire_provider() -> OpenAiChatCompletionsProvider {
    OpenAiChatCompletionsProvider::with_credentials(
        std::sync::Arc::new(StaticOAuthSource {
            token: "exchange-token-1",
            identity: qoder_test_identity(),
        }),
        "qfmodel".to_string(),
        "https://api2.qoder.sh",
        ClientPreset::Native,
    )
    .with_dialect(nuo_contracts::OpenAiChatDialect::Qoder)
    .with_pipeline(build_qoder_pipeline())
}

fn qoder_body() -> serde_json::Value {
    serde_json::json!({
        "model": "qfmodel",
        "messages": [{"role": "user", "content": "Say OK only."}],
        "stream": true,
    })
}

fn planned_request(
    provider: &OpenAiChatCompletionsProvider,
    auth: &ResolvedAuth,
) -> nuo_providers::request::RequestBuilder {
    provider
        .execute_plan(&qoder_body(), auth, &TransportTelemetry::default())
        .expect("plan")
}

#[test]
fn qoder_golden_wire_url_is_the_surface_inference_url() {
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(qoder_test_identity());
    let req = planned_request(&qoder_wire_provider(), &auth).build("QoderGoldenWire").expect("build");

    // The surface's inference path + fixed query — NOT the bare root (the
    // regression: a flat JSON POST to the root → HTTP 404). The base URL is
    // an arbitrary stand-in here: the real pin lives in MODEL_PROVIDER_SPEC
    // (currently https://api3.qoder.sh — see docs §3.1a).
    assert_eq!(
        req.url,
        "https://api2.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation\
         ?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1"
    );
}

/// The identity's server-elected endpoint (center region map, §3.1a)
/// overrides the executor's pinned base URL — the signature binds the
/// request to whichever host serves it.
#[test]
fn elected_endpoint_overrides_the_pinned_base_url() {
    let elected = "https://api3.qoder.sh".to_string();
    let identity = QoderRequestIdentity {
        infer_endpoint: Some(elected.clone()),
        ..qoder_test_identity()
    };
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(identity);
    let req = planned_request(&qoder_wire_provider(), &auth).build("QoderGoldenWire").expect("build");
    assert!(req.url.starts_with(&format!("{elected}/algo/api/v2/service/pro/sse/agent_chat_generation")), "url: {}", req.url);
    // The COSY signature still validates against the same identity — the
    // body and header set are unchanged by the host override.
    let authorization = req.headers.get("authorization").unwrap().to_str().unwrap();
    assert!(authorization.starts_with("Bearer COSY."), "{authorization}");
}

/// No election synced (`infer_endpoint: None`) keeps the executor's base
/// URL — the pinned `MODEL_PROVIDER_SPEC.root_url` stays authoritative.
#[test]
fn missing_election_falls_back_to_the_pinned_base() {
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(qoder_test_identity());
    let req = planned_request(&qoder_wire_provider(), &auth).build("QoderGoldenWire").expect("build");
    assert!(req.url.starts_with("https://api2.qoder.sh/algo/api/v2/service/pro/sse"), "url: {}", req.url);
}

#[test]
fn qoder_golden_wire_headers_carry_the_cosy_signature_set() {
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(qoder_test_identity());
    let req = planned_request(&qoder_wire_provider(), &auth).build("QoderGoldenWire").expect("build");
    let h = &req.headers;

    let authorization = h.get("authorization").unwrap().to_str().unwrap();
    assert!(
        authorization.starts_with("Bearer COSY."),
        "COSY-signed authorization required, got: {authorization}"
    );
    assert!(h.get("cosy-date").is_some(), "Cosy-Date header required");
    assert!(h.get("cosy-key").is_some(), "Cosy-Key header required");
    assert_eq!(h.get("cosy-user").unwrap(), "u_test_uid");
    assert_eq!(h.get("x-model-key").unwrap(), "qfmodel");
    assert_eq!(h.get("x-model-source").unwrap(), "system");
    assert_eq!(h.get("cosy-version").unwrap(), "1.1.58");
    assert_eq!(h.get("cosy-business-product").unwrap(), "cli");
    assert_eq!(h.get("cosy-business-type").unwrap(), "agent");
    assert!(h.get("content-type").is_some());
    // No plain bearer leaks alongside the COSY signature.
    assert!(!authorization.contains("exchange-token-1"));
}

#[test]
fn qoder_golden_wire_body_is_the_qoderencoded_agent_chat_envelope() {
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(qoder_test_identity());
    let req = planned_request(&qoder_wire_provider(), &auth)
        .build("QoderGoldenWire")
        .expect("build");

    let body = req.body.expect("planned body");
    let body_text = std::str::from_utf8(&body).unwrap();
    assert!(
        !body_text.trim_start().starts_with('{'),
        "body must be QoderEncoding-encoded, not flat JSON: {body_text}"
    );

    // Round-trip through the codec, then assert the envelope structurally.
    let decoded = nuo_providers::qoder::wire_decode(body_text)
        .expect("body decodes through the Qoder codec");
    let envelope: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(envelope["chat_task"], "FREE_INPUT");
    assert_eq!(envelope["agent_id"], "agent_common");
    assert_eq!(envelope["session_type"], "qodercli");
    assert_eq!(envelope["model_config"]["key"], "qfmodel");
    assert_eq!(envelope["model_config"]["display_name"], "qfmodel");
    assert_eq!(envelope["model_config"]["source"], "system");
    assert_eq!(envelope["business"]["product"], "cli");
    assert_eq!(envelope["business"]["version"], "1.1.58");
    assert_eq!(envelope["messages"][0]["content"], "Say OK only.");
    for slot in ["request_id", "request_set_id", "chat_record_id"] {
        assert!(
            !envelope[slot].as_str().unwrap_or_default().is_empty(),
            "fresh-UUID slot `{slot}` must be populated"
        );
    }

    // Fields the reference client states on every turn. `model_format` is
    // deliberately absent: the vendor's envelope has no root-level field by
    // that name — `format` lives inside `model_config`.
    assert_eq!(envelope["stream"], true);
    assert_eq!(envelope["is_reply"], true);
    assert_eq!(envelope["is_retry"], false);
    assert_eq!(envelope["aliyun_user_type"], "");
    assert!(
        envelope.get("model_format").is_none(),
        "`model_format` is not a vendor envelope field; `format` belongs in model_config"
    );
    assert_eq!(envelope["model_config"]["format"], "openai");

    // `parameters` is always present. With no effort in the body it carries the
    // surface's declared output cap and nothing else — no synthesized reasoning
    // control, since a turn that asked for none must not get one.
    assert_eq!(envelope["parameters"]["max_tokens"], 32_000);
    assert!(
        envelope["parameters"].get("reasoning_effort").is_none(),
        "no effort was requested, so none may be projected"
    );
    assert!(envelope["parameters"].get("enable_thinking").is_none());
}

/// The channel capability view a live Qoder catalog produces.
///
/// Qoder's compiled baselines declare no effort ladder (`effort_levels: &[]`)
/// because the platform's tiers are account- and model-specific; the ladder
/// arrives with the catalog (`thinking_config.enabled.efforts`, e.g.
/// `low`/`medium`/`xhigh` on `qfmodel`). This mirrors that so the tests below
/// exercise the *production* chain — catalog metadata → flat body →
/// envelope — rather than a hand-written body that skips the gate.
fn catalog_capabilities(model: &str) -> nuo_contracts::ModelCapabilities {
    let remote = nuo_contracts::RemoteModelMetadata {
        effort_levels: Some(
            ["low", "medium", "xhigh"]
                .iter()
                .map(|level| nuo_contracts::EffortLevel::parse(level))
                .collect(),
        ),
        ..Default::default()
    };
    nuo_contracts::ModelCapabilities::for_channel(model, Some(&remote))
}

/// Build the flat chat-completions body the way production does, then plan it
/// through the Qoder pipeline and return the decoded envelope.
fn envelope_for(body: &serde_json::Value) -> serde_json::Value {
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(qoder_test_identity());
    let req = qoder_wire_provider()
        .execute_plan(body, &auth, &TransportTelemetry::default())
        .expect("plan")
        .build("QoderGoldenWire")
        .expect("build");
    let decoded =
        nuo_providers::qoder::wire_decode(std::str::from_utf8(&req.body.expect("body")).unwrap())
            .expect("body decodes through the Qoder codec");
    serde_json::from_slice(&decoded).expect("envelope is JSON")
}

/// The user's effort selection must reach the wire. The reference client puts
/// reasoning controls under `parameters`, not at the envelope root, so the flat
/// body's `reasoning_effort` must land in `parameters.reasoning_effort` plus
/// the derived `enable_thinking`. Dropping it is silent: the service runs its
/// own default and every downstream signal describes a turn the user did not
/// ask for.
#[test]
fn qoder_golden_wire_projects_the_requested_effort_into_parameters() {
    let body = nuo_providers::protocol::openai::chat_completions::request::body_with_capabilities(
        vec![nuo_contracts::Message::new(nuo_contracts::Role::User, "Say OK only.")],
        nuo_providers::protocol::openai::chat_completions::request::BodyInput {
            model: "qfmodel",
            stream: true,
            instructions: None,
            tool_specs: None,
            reasoning_effort: Some(nuo_contracts::Effort::Xhigh),
            dialect: nuo_contracts::OpenAiChatDialect::Qoder,
            cache_plan: &nuo_contracts::ResolvedCachePolicy::Unsupported,
        },
        &catalog_capabilities("qfmodel"),
    );
    // The intermediate step must itself carry the effort, or the test would be
    // asserting a projection of a body production never builds.
    assert_eq!(body["reasoning_effort"], "xhigh", "flat body gate");

    let envelope = envelope_for(&body);
    assert_eq!(envelope["parameters"]["reasoning_effort"], "xhigh");
    // The vendor's rule: any effort other than `none` means reasoning is on.
    assert_eq!(envelope["parameters"]["enable_thinking"], true);
}

/// `reasoning_effort: "none"` is the vendor's explicit "reasoning off", not an
/// absent value — it must reach the wire as `none` with `enable_thinking`
/// false, rather than being dropped as if unrequested.
#[test]
fn qoder_golden_wire_projects_effort_none_as_reasoning_off() {
    let body = serde_json::json!({
        "model": "qfmodel",
        "messages": [{"role": "user", "content": "Say OK only."}],
        "stream": true,
        "reasoning_effort": "none",
    });
    let envelope = envelope_for(&body);
    assert_eq!(envelope["parameters"]["reasoning_effort"], "none");
    assert_eq!(envelope["parameters"]["enable_thinking"], false);
}

/// The offline case: with no catalog metadata the baseline ladder is empty, so
/// the flat body emits no effort and the envelope must not synthesize one. A
/// turn that asked for no reasoning control must not get one — `parameters`
/// still carries the declared output cap, because the reference client always
/// states `max_tokens`.
#[test]
fn qoder_golden_wire_without_a_catalog_ladder_invents_no_effort() {
    let offline = nuo_contracts::ModelCapabilities::for_channel("qfmodel", None);
    assert!(
        offline.effort_levels.is_empty(),
        "the baseline declares no ladder; this test is about the offline path"
    );
    let body = nuo_providers::protocol::openai::chat_completions::request::body_with_capabilities(
        vec![nuo_contracts::Message::new(nuo_contracts::Role::User, "Say OK only.")],
        nuo_providers::protocol::openai::chat_completions::request::BodyInput {
            model: "qfmodel",
            stream: true,
            instructions: None,
            tool_specs: None,
            reasoning_effort: Some(nuo_contracts::Effort::Xhigh),
            dialect: nuo_contracts::OpenAiChatDialect::Qoder,
            cache_plan: &nuo_contracts::ResolvedCachePolicy::Unsupported,
        },
        &offline,
    );
    assert!(
        body.get("reasoning_effort").is_none(),
        "an empty ladder gates the effort off upstream"
    );

    let envelope = envelope_for(&body);
    assert!(envelope["parameters"].get("reasoning_effort").is_none());
    assert!(envelope["parameters"].get("enable_thinking").is_none());
    assert_eq!(envelope["parameters"]["max_tokens"], 32_000);
}

/// A body with no model id is a construction bug, not a wire condition. The
/// envelope must refuse it rather than stamp a placeholder into
/// `X-Model-Key` / `model_config.key`, which the service routes on — the
/// resulting refusal would otherwise arrive as an opaque upstream error with no
/// local cause.
#[test]
fn qoder_envelope_refuses_a_body_with_no_model() {
    use nuo_providers::pipeline::EnvelopePhase;
    let result = nuo_providers::qoder::QoderAgentEnvelope
        .reshape_body(&serde_json::json!({"messages": [], "stream": true}));
    let error = result.expect_err("a modelless body must not build an envelope");
    assert!(
        error.message().contains("model"),
        "the refusal must name the missing model id: {}",
        error.message()
    );

    // A blank model id is the same bug wearing a different shape.
    let blank = nuo_providers::qoder::QoderAgentEnvelope
        .reshape_body(&serde_json::json!({"model": "  ", "messages": []}));
    assert!(blank.is_err(), "a blank model id must also be refused");
}

#[test]
fn standard_dialect_golden_wire_is_the_plain_chat_completions_wire() {
    let provider = OpenAiChatCompletionsProvider::with_base_url(
        "test-key".to_string(),
        "glm-5.2".to_string(),
        "https://api.example.com/v1/chat/completions",
    );
    let auth = ResolvedAuth::new("test-key");
    let body = serde_json::json!({"model": "glm-5.2", "messages": [], "stream": true});

    let req = provider
        .execute_plan(&body, &auth, &TransportTelemetry::default())
        .expect("plan")
        .build("StandardGoldenWire")
        .expect("build");

    // Pass-through plan: URL unchanged, flat JSON body, bearer header.
    assert_eq!(req.url, "https://api.example.com/v1/chat/completions");
    assert_eq!(req.headers.get("authorization").unwrap(), "Bearer test-key");
    let body_bytes = req.body.expect("body");
    let body_text = std::str::from_utf8(&body_bytes).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(body_text).unwrap();
    assert_eq!(parsed["model"], "glm-5.2");
}
