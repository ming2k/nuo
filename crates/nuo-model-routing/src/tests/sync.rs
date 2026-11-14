//! Catalog sync, fitted-model overlay, and identity tracking.

use super::*;

// catalog sync + fitted overlay

#[tokio::test]
async fn live_catalog_sync_writes_the_per_instance_cache() {
    let _sandbox = sandboxed_paths();
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"data":[{"id":"deepseek-v4-flash"},{"id":"deepseek-v4-pro"},{"id":"deepseek-v4-flash-vision-exp"}]}"#,
        )
        .create_async()
        .await;

    let provider_id = "test-mock-deepseek";
    register_mock_provider(
        provider_id,
        &format!("{}/v1", server.url()),
        WireProtocol::OpenAiResponses,
        &["deepseek-v4-flash", "deepseek-v4-pro", "deepseek-v4-lite"],
        nuo_protocol::CatalogShape::OpenAi,
    );
    let instances = Connections {
        connections: vec![Connection {
            name: "deepseek".to_string(),
            provider: provider_id.to_string(),
            ..Default::default()
        }],
    };
    instances.save().unwrap();
    let mut creds = Credentials::default();
    creds.set_api_key("deepseek", Some("sk-test".into()));
    creds.save().unwrap();

    let outcome = sync_remote_routes().await;
    assert!(outcome.changed, "catalog sync must record a change");
    assert!(outcome.failures.is_empty());

    let cache = RemoteCatalogCache::load();
    assert_eq!(
        cache.connection_models.get("deepseek").map(|m| m.len()),
        Some(3),
        "the discovered list lands in the cache"
    );
    assert!(cache.connection_models["deepseek"].contains(&"deepseek-v4-flash".to_string()));
    assert!(
        cache.connection_models["deepseek"].contains(&"deepseek-v4-flash-vision-exp".to_string())
    );
}

#[tokio::test]
async fn antigravity_oauth_live_catalog_sync_materializes_tiered_generations() {
    // The antigravity-oauth preset declares its first-party catalog
    // (`fetchAvailableModels`). An OAuth connection must actually run that
    // sync — not silently keep the compiled snapshot — so a generation
    // shipped upstream (gemini-3.8-flash) materializes for the signed-in
    // account with no client edit. Regression guard: the sync used to skip
    // every AntigravityOAuth connection, leaving the cache empty forever.
    let _sandbox = sandboxed_paths();
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/v1internal:fetchAvailableModels")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
                "defaultAgentModelId": "gemini-3.8-flash",
                "agentModelSorts": [
                  {
                    "displayName": "Recommended",
                    "groups": [
                      {
                        "modelIds": [
                          "gemini-4-argon",
                          "gemini-3.8-flash-high",
                          "gemini-pro-agent",
                          "claude-opus-4-6-thinking"
                        ]
                      }
                    ]
                  }
                ],
                "models": {
                  "gemini-3.8-flash-high": { "maxTokens": 1048576, "maxOutputTokens": 65536, "supportsThinking": true, "supportsImages": true },
                  "gemini-4-argon": { "maxTokens": 1048576, "maxOutputTokens": 65536, "supportsThinking": true, "supportsImages": true },
                  "gemini-3.6-flash-high": { "maxTokens": 1048576, "supportsThinking": true },
                  "gemini-pro-agent": { "maxTokens": 1048576, "supportsThinking": true },
                  "gemini-3.1-pro-high": { "maxTokens": 1048576, "supportsThinking": true },
                  "gemini-2.5-flash": { "maxTokens": 1048576 },
                  "chat_20706": { "maxTokens": 16000 },
                  "claude-opus-4-6-thinking": { "maxTokens": 1048576, "supportsThinking": true }
                },
                "deprecatedModelIds": { "gemini-3.1-pro-high": { "newModelId": "gemini-pro-agent" } }
            }"#,
        )
        .create_async()
        .await;

    let provider_id = "test-mock-agy";
    register_mock_provider(
        provider_id,
        &server.url().to_string(),
        WireProtocol::GoogleGenerateContent,
        &[],
        nuo_protocol::CatalogShape::GoogleCloudCode,
    );
    let mut conn = instance("agy-live", Some(provider_id));
    conn.auth = ConnectionAuth::negotiated("google-antigravity");
    let connections = Connections {
        connections: vec![conn],
    };
    connections.save().unwrap();

    // Seed an unexpired OAuth access token for the connection. The sandboxed
    // state dir makes the auth-store write test-local.
    let expires_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis() as i64
        + 3_600_000;
    {
        let store = test_credential_store();
        let mut store = store.lock().await.expect("credential store lock");
        let mut t = TokenSet {
            access: "ya29.antigravity-test".into(),
            refresh: "refresh-test".into(),
            expires_ms,
            id_token: None,
            token_type: Some("Bearer".into()),
            scope: None,
            user_email: None,
            attributes: serde_json::Map::new(),
        };
        t.set_attr("project_id", "projects/antigravity-test");
        store.set("agy-live", t);
        store.commit().await.expect("credential store commit");
    }

    let outcome = sync_connection_routes("agy-live").await;
    assert!(
        outcome.changed,
        "antigravity live catalog sync must record a change"
    );
    assert!(
        outcome.failures.is_empty(),
        "failures: {:?}",
        outcome.failures
    );

    let cache = RemoteCatalogCache::load();
    let models = cache
        .connection_models
        .get("agy-live")
        .expect("catalog lands in the cache");
    assert!(models.contains(&"gemini-3.8-flash".to_string()));
    assert!(!models.contains(&"gemini-3.8-flash-high".to_string()));
    assert!(models.contains(&"gemini-4-argon".to_string()));
    assert!(models.contains(&"gemini-pro-agent".to_string()));
    assert!(models.contains(&"claude-opus-4-6".to_string()));
    assert!(!models.contains(&"claude-opus-4-6-thinking".to_string()));
    // Internal helpers, pseudo-effort variants, the legacy 3.6 generation, and deprecated ids stay out.
    assert!(!models.iter().any(|m| m.starts_with("chat_")));
    assert!(
        !models.iter().any(|m| m.starts_with("gemini-3.6-flash")),
        "legacy 3.6 flash generation must stay suppressed"
    );
    assert!(!models.contains(&"gemini-3.1-pro-high".to_string()));
    assert!(
        !models.contains(&"gemini-2.5-flash".to_string()),
        "legacy 2.5 flash generation must stay suppressed"
    );
}

#[tokio::test]
async fn single_source_endpoint_failure_records_failure_and_preserves_determinism() {
    // Under ADR-0081 INV-CATALOG-02 / INV-CATALOG-03: single-source determinism.
    // When the configured endpoint returns 401, it reports failure without flapping
    // to an unrelated fallback source.
    let _sandbox = sandboxed_paths();
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body("Authentication Fails")
        .create_async()
        .await;

    let provider_id = "test-mock-glm-cn";
    register_mock_provider(
        provider_id,
        &server.url(),
        WireProtocol::OpenAiChatCompletions,
        &["glm-4-plus"],
        nuo_protocol::CatalogShape::OpenAi,
    );
    let instances = Connections {
        connections: vec![Connection {
            name: "glm-cn".to_string(),
            provider: provider_id.to_string(),
            ..Default::default()
        }],
    };
    instances.save().unwrap();

    let outcome = sync_remote_routes().await;
    assert!(
        !outcome.changed,
        "failed catalog sync must not record a change"
    );
    assert!(
        outcome
            .failures
            .iter()
            .any(|failure| failure.connection == "glm-cn"),
        "the failed endpoint must be reported as a failure: {:?}",
        outcome.failures
    );
}

#[tokio::test]
async fn connection_catalog_sync_never_touches_unrelated_connections() {
    ensure_test_init();
    let _sandbox = sandboxed_paths();
    let mut selected_server = mockito::Server::new_async().await;
    let selected_mock = selected_server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"id":"deepseek-v4-flash"}]}"#)
        .expect(1)
        .create_async()
        .await;
    let mut unrelated_server = mockito::Server::new_async().await;
    let unrelated_mock = unrelated_server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"id":"deepseek-v4-pro"}]}"#)
        .expect(0)
        .create_async()
        .await;

    let sel_id = "test-mock-selected";
    let unrel_id = "test-mock-unrelated";
    register_mock_provider(
        sel_id,
        &format!("{}/v1", selected_server.url()),
        WireProtocol::OpenAiResponses,
        &["deepseek-v4-flash"],
        nuo_protocol::CatalogShape::OpenAi,
    );
    register_mock_provider(
        unrel_id,
        &format!("{}/v1", unrelated_server.url()),
        WireProtocol::OpenAiResponses,
        &["deepseek-v4-pro"],
        nuo_protocol::CatalogShape::OpenAi,
    );
    Connections {
        connections: vec![
            Connection {
                name: "selected".to_string(),
                provider: sel_id.to_string(),
                ..Default::default()
            },
            Connection {
                name: "unrelated".to_string(),
                provider: unrel_id.to_string(),
                ..Default::default()
            },
        ],
    }
    .save()
    .unwrap();
    let mut creds = Credentials::default();
    creds.set_api_key("selected", Some("sk-selected".into()));
    creds.set_api_key("unrelated", Some("sk-unrelated".into()));
    creds.save().unwrap();

    let outcome = sync_connection_routes("selected").await;
    assert!(
        outcome.changed,
        "unexpected catalog sync result: {outcome:?}"
    );
    assert!(
        outcome.failures.is_empty(),
        "unexpected catalog sync failures: {:?}",
        outcome.failures
    );
    selected_mock.assert_async().await;
    unrelated_mock.assert_async().await;

    let cache = RemoteCatalogCache::load();
    assert_eq!(
        cache.connection_models.get("selected"),
        Some(&vec!["deepseek-v4-flash".to_string()])
    );
    assert!(!cache.connection_models.contains_key("unrelated"));
}

#[tokio::test]
async fn catalog_sync_failure_keeps_the_previous_subset_and_reports() {
    ensure_test_init();
    let _sandbox = sandboxed_paths();
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body("Authentication Fails")
        .create_async()
        .await;

    let provider_id = "test-mock-deepseek-fail";
    register_mock_provider(
        provider_id,
        &format!("{}/v1", server.url()),
        WireProtocol::OpenAiResponses,
        &["deepseek-v4-flash"],
        nuo_protocol::CatalogShape::OpenAi,
    );
    let instances = Connections {
        connections: vec![Connection {
            name: "deepseek".to_string(),
            provider: provider_id.to_string(),
            ..Default::default()
        }],
    };
    instances.save().unwrap();

    let outcome = sync_remote_routes().await;
    assert!(!outcome.changed);
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].connection, "deepseek");
    // The previous subset is untouched (there was none → snapshot still wins).
    let cache = RemoteCatalogCache::load();
    assert!(cache.connection_models.is_empty());
}

#[tokio::test]
async fn successful_empty_remote_catalog_clears_previous_models() {
    ensure_test_init();
    let _sandbox = sandboxed_paths();
    let mut server = mockito::Server::new_async().await;
    let model_list = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[]}"#)
        .create_async()
        .await;
    let provider_id = "test-mock-deepseek-empty";
    register_mock_provider(
        provider_id,
        &format!("{}/v1", server.url()),
        WireProtocol::OpenAiResponses,
        &["deepseek-v4-flash"],
        nuo_protocol::CatalogShape::OpenAi,
    );
    // This connection is populated only by the remote catalog; drop the
    // helper's declared seed so an authoritative empty catalog is observable.
    let mut providers = nuo_persistence::model_providers::ModelProviders::load();
    providers.get_or_create_mut(provider_id).include.clear();
    nuo_persistence::model_providers::ModelProviders::save(&providers).unwrap();
    nuo_provider::sync_user_declared_providers(
        &nuo_persistence::model_providers::ModelProviders::load(),
    )
    .unwrap();
    let mut connection = Connection {
        name: "deepseek".to_string(),
        provider: provider_id.to_string(),
        ..Default::default()
    };
    connection.models.filter = Some(nuo_protocol::ConnectionFilterPolicy::Named(
        nuo_protocol::NamedFilterPolicy::All,
    ));
    Connections {
        connections: vec![connection],
    }
    .save()
    .unwrap();

    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "deepseek".to_string(),
        vec!["deepseek-v4-flash".to_string()],
    );
    cache
        .model_lists
        .insert("deepseek".to_string(), ModelListCacheState::default());
    cache.save().unwrap();

    let outcome = sync_connection_routes("deepseek").await;
    assert!(
        outcome.changed,
        "unexpected catalog sync result: {outcome:?}"
    );
    assert!(outcome.failures.is_empty());
    model_list.assert_async().await;

    let cache = RemoteCatalogCache::load();
    assert_eq!(cache.connection_models.get("deepseek"), Some(&Vec::new()));
    let connections = Connections::load();
    assert!(route_models(connections.get("deepseek").unwrap(), &cache, declarations()).is_empty());
}

#[tokio::test]
async fn orphaned_response_etag_does_not_renew_catalog_state() {
    let _sandbox = sandboxed_paths();
    let mut cache = RemoteCatalogCache::default();
    cache.model_lists.insert(
        "chatgpt".to_string(),
        ModelListCacheState {
            etag: Some("catalog-v2".to_string()),
            client_version: "stale-client".to_string(),
            source_identity: "stale-source".to_string(),
            refreshed_at_ms: 0,
            refresh_failed: false,
        },
    );
    cache.save().unwrap();

    let outcome = refresh_connection_models_for_etag("chatgpt", "catalog-v2").await;
    assert!(!outcome.changed);
    assert!(outcome.failures.is_empty());

    let renewed = RemoteCatalogCache::load();
    let state = renewed.model_lists.get("chatgpt").expect("cache state");
    assert_eq!(state.etag.as_deref(), Some("catalog-v2"));
    assert_eq!(state.client_version, "stale-client");
    assert_eq!(state.refreshed_at_ms, 0);
}

#[test]
fn sync_fitted_model_registry_overlays_fitted_ids() {
    let _sandbox = sandboxed_paths();
    let instances = Connections {
        connections: vec![instance("kimi", Some("kimi-code"))],
    };
    instances.save().unwrap();
    let mut cache = RemoteCatalogCache::default();
    cache.fitted_models.insert("kimi".to_string(), {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "kimi-for-coding".to_string(),
            FittedModelInfo {
                context_window: 262_144,
                reasoning: true,
                vision: Some(true),
                efforts: vec!["max".to_string()],
            },
        );
        m
    });
    cache.save().unwrap();

    sync_fitted_model_registry();
    let resolved = nuo_protocol::model::resolve("kimi-for-coding");
    assert_eq!(resolved.context_window, 262_144);
    assert!(resolved.reasoning());
}
