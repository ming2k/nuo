#![cfg(test)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nuo_contracts::{
    SecretString, WebCredentialStatus, WebProviderAxis, WebReaderProvider, WebSearchProvider,
};
use nuo_persistence::config::{Config, Credentials, resolve_web_config};

fn sandbox() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    nuo_persistence::paths::set_test_default(Some(nuo_persistence::paths::Dirs {
        config_dir: root.path().join("config"),
        data_dir: root.path().join("data"),
        state_dir: root.path().join("state"),
        cache_dir: root.path().join("cache"),
        runtime_dir: None,
    }));
    (root, guard)
}

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
fn legacy_builtin_reader_canonicalizes_to_disabled() {
    let (_root, _guard) = sandbox();
    std::fs::create_dir_all(&nuo_persistence::paths::get().config_dir).unwrap();
    std::fs::write(
        Config::config_file_path(),
        "[websearch]\nreader = 'builtin'\n",
    )
    .unwrap();
    let config = Config::load();
    assert_eq!(config.web.reader, WebReaderProvider::Disabled);
    nuo_persistence::paths::set_test_default(None);
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

#[test]
fn legacy_connection_selection_and_token_migrate_without_deleting_source() {
    let (_root, _guard) = sandbox();
    let dirs = nuo_persistence::paths::get();
    std::fs::create_dir_all(&dirs.config_dir).unwrap();
    std::fs::create_dir_all(&dirs.state_dir).unwrap();
    std::fs::write(
        Config::config_file_path(),
        "[websearch]\nprovider = 'team-search'\nreader = 'builtin'\n",
    )
    .unwrap();
    std::fs::write(
        dirs.web_connections_file(),
        "[[search_connections]]\nid = 'team-search'\npreset_id = 'tavily'\nenabled = true\n",
    )
    .unwrap();
    std::fs::write(
        dirs.credentials_file(),
        "[connections]\nteam-search = 'legacy-secret'\n",
    )
    .unwrap();

    let config = Config::load();
    let credentials = Credentials::load();
    assert_eq!(config.web.provider, WebSearchProvider::Tavily);
    assert_eq!(config.web.reader, WebReaderProvider::Disabled);
    assert_eq!(
        credentials
            .web_credential(WebProviderAxis::Search, "tavily")
            .map(SecretString::expose_secret),
        Some("legacy-secret")
    );
    assert!(
        dirs.web_connections_file().exists(),
        "legacy source is archival, not deleted"
    );

    let mut credentials = credentials;
    credentials.set_web_credential(WebProviderAxis::Search, "tavily", None);
    credentials.save().unwrap();
    let reloaded = Credentials::load();
    assert!(
        reloaded
            .web_credential(WebProviderAxis::Search, "tavily")
            .is_none(),
        "the one-shot migration marker must prevent a cleared token from being resurrected"
    );
    assert!(
        reloaded.connections.contains_key("team-search"),
        "the preserved source credential remains available for manual recovery"
    );

    nuo_persistence::paths::set_test_default(None);
}
