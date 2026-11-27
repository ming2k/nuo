#![cfg(test)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nuo_persistence::config::{Config, Credentials, resolve_web_config};
use nuo_protocol::{SecretString, WebCredentialStatus, WebProviderAxis, WebSearchProvider};

#[test]
fn config_is_behavior_only_and_uses_web_table() {
    let mut config = Config::default();
    config.web.provider = WebSearchProvider::Tavily;
    let encoded = toml::to_string_pretty(&config).unwrap();
    assert!(encoded.contains("[web]"));
    assert!(encoded.contains("provider = \"tavily\""));
    assert!(!encoded.contains("api_key"));
}

#[test]
fn provider_credentials_round_trip_by_axis_and_id() {
    let mut credentials = Credentials::default();
    credentials.set_web_credential(
        WebProviderAxis::Search,
        "tavily",
        Some(SecretString::new("tvly-1")),
    );
    credentials.set_web_credential(
        WebProviderAxis::Reader,
        "jina",
        Some(SecretString::new("jina-1")),
    );
    let encoded = toml::to_string_pretty(&credentials).unwrap();
    let decoded: Credentials = toml::from_str(&encoded).unwrap();
    assert_eq!(
        decoded
            .web_credential(WebProviderAxis::Search, "tavily")
            .map(SecretString::expose_secret),
        Some("tvly-1")
    );
    assert_eq!(
        decoded
            .web_credential(WebProviderAxis::Reader, "jina")
            .map(SecretString::expose_secret),
        Some("jina-1")
    );
}

#[test]
fn required_credential_readiness_is_explicit() {
    let mut config = Config::default();
    config.web.provider = WebSearchProvider::Tavily;
    let resolved = resolve_web_config(&config.web, &Credentials::default());
    assert_eq!(
        resolved.search_credential,
        WebCredentialStatus::RequiredMissing
    );
    assert!(resolved.runtime.search_credential.is_none());
}
