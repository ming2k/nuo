//! Contract tests for the GitHub Copilot provider surface.
//!
//! Copilot is the subscription-lane sibling of OpenAI chat completions: the
//! spec owns the endpoint and client identity, while the OAuth preset owns the
//! device grant. These tests pin the data the composition root relies on; the
//! shared registry-wide suite in `crates/nuo/tests/it/provider_contract.rs`
//! covers the cross-provider invariants.

use nuo_inference_wire::WireProtocol;
use nuo_provider::LoginMethod;
use nuo_provider::auth::{DeviceFlowMode, PkceMode};

use crate::spec::{COPILOT_CLIENT_HEADERS, MODEL_PROVIDER_SPEC, MODELS};
use crate::{COPILOT_SEED_MODELS, oauth};

#[test]
fn spec_identifies_the_copilot_chat_surface() {
    let spec = &MODEL_PROVIDER_SPEC;
    spec.validate()
        .expect("Copilot spec must be structurally valid");

    assert_eq!(spec.id.as_ref(), "github-copilot");
    assert_eq!(spec.root_url.as_ref(), "https://api.githubcopilot.com");
    assert_eq!(spec.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(spec.models, COPILOT_SEED_MODELS);
    assert_eq!(spec.models, &["gpt-4o-mini"]);
    assert_eq!(spec.baselines.len(), MODELS.len());
    for (baseline, declared) in spec.baselines.iter().zip(MODELS) {
        assert_eq!(baseline.id, declared.id);
    }
    assert_eq!(spec.default_headers.as_ref(), COPILOT_CLIENT_HEADERS);

    assert!(
        spec.default_headers
            .iter()
            .any(|(name, value)| { *name == "Copilot-Integration-Id" && *value == "vscode-chat" })
    );
    assert!(
        spec.default_headers
            .iter()
            .any(|(name, _)| *name == "Editor-Version"),
        "Copilot ingress requires an editor identity"
    );
}

#[test]
fn seed_model_is_available_on_every_plan_and_speaks_chat_completions() {
    let baseline = MODEL_PROVIDER_SPEC
        .baselines
        .iter()
        .find(|model| model.id == "gpt-4o-mini")
        .expect("the universal seed must carry baseline capabilities");

    assert_eq!(baseline.protocol, WireProtocol::OpenAiChatCompletions);
    assert!(baseline.tool_call);
    assert!(baseline.vision);
    assert_eq!(baseline.context_window, 128_000);
    assert!(
        MODEL_PROVIDER_SPEC.dialect.supports(baseline.protocol),
        "the Copilot dialect must accept the seed's wire protocol"
    );
}

#[test]
fn oauth_preset_is_a_public_pkce_device_grant() {
    let preset = oauth::preset();
    assert_eq!(preset.provider_id.as_ref(), "copilot");
    assert_eq!(preset.client_id.as_ref(), oauth::COPILOT_CLIENT_ID);
    assert!(
        preset.client_secret.is_none(),
        "a public client must not carry a client secret"
    );
    assert_eq!(
        preset.client_auth_method,
        nuo_provider::auth::ClientAuthMethod::None
    );
    assert_eq!(preset.default_login_method, LoginMethod::Device);
    assert_eq!(preset.device_flow, DeviceFlowMode::rfc8628());
    assert_eq!(preset.pkce_mode, PkceMode::S256);
    assert_eq!(
        preset.authorize_url.as_ref(),
        "https://github.com/login/oauth/authorize"
    );
    assert_eq!(
        preset.device_authorization_url.as_ref(),
        "https://github.com/login/device/code"
    );
    assert_eq!(
        preset.token_url.as_ref(),
        "https://github.com/login/oauth/access_token"
    );
    assert_eq!(preset.scope.as_ref(), "read:user");
    assert!(
        !preset.browser_login,
        "Copilot defaults to the device grant"
    );
}

#[test]
fn oauth_surface_registers_both_public_aliases() {
    let surfaces = oauth::providers();
    assert_eq!(surfaces.len(), 1, "one crate owns one Copilot surface");

    let mut ids = surfaces[0].ids().to_vec();
    ids.sort_unstable();
    assert_eq!(ids, ["copilot", "github-copilot"]);
    assert_eq!(surfaces[0].config().provider_id.as_ref(), "copilot");
}
