//! Model catalog construction, pruning, recency, and hygiene valves.

use super::*;

// helpers

#[test]
fn catalog_builds_from_the_state_store_only() {
    let _sandbox = sandboxed_paths();
    let instances = Connections {
        connections: vec![instance("deepseek", Some("deepseek"))],
    };
    instances.save().unwrap();
    let entries = build_routes();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "deepseek");
    // A config without any provider info still derives from the store.
    let _empty = build_routes();
}

#[test]
fn antigravity_models_derivation_and_hidden_filter() {
    let _sandbox = sandboxed_paths();
    let mut conn = instance("g11", Some("google-antigravity"));
    conn.auth = nuo_protocol::ConnectionAuth::negotiated("google-antigravity");
    let connections = Connections {
        connections: vec![conn],
    };
    connections.save().unwrap();

    let config = Config {
        default_connection: "g11".to_string(),
        default_model: Some("gemini-3.8-flash".to_string()),
        hidden_models: vec![
            "gemini-3.6-flash*".to_string(),
            "gemini-3-flash*".to_string(),
        ],
        ..Config::default()
    };

    let usage = nuo_persistence::connection_usage::ConnectionUsage::default();
    let picker = build_picker_state(&config, &usage);
    let g11_row = picker
        .rows
        .iter()
        .find(|r| r.id == "g11")
        .expect("g11 in picker");

    assert!(g11_row.models.contains(&"gemini-3.8-flash".to_string()));
    assert!(g11_row.models.contains(&"gemini-4-argon".to_string()));
    assert!(
        !g11_row
            .models
            .iter()
            .any(|m| m.starts_with("gemini-3.6-flash"))
    );
    assert!(
        !g11_row
            .models
            .iter()
            .any(|m| m.starts_with("gemini-3-flash"))
    );
}

#[test]
fn prune_stale_models_prunes_favorites_and_usage_and_default_model() {
    let _sandbox = sandboxed_paths();
    let mut conn = instance("my-custom", None);
    conn.models.filter = Some(nuo_persistence::connections::ConnectionFilterPolicy::Named(
        nuo_persistence::connections::NamedFilterPolicy::All,
    ));
    conn.models.include = vec![
        nuo_protocol::model::DeclaredModel {
            id: "model-a".to_string(),
            ..Default::default()
        },
        nuo_protocol::model::DeclaredModel {
            id: "model-b".to_string(),
            ..Default::default()
        },
    ];
    let connections = Connections {
        connections: vec![conn],
    };
    connections.save().unwrap();

    let mut config = Config {
        default_connection: "my-custom".to_string(),
        default_model: Some("model-deleted".to_string()),
        favorites: vec!["model-a".to_string(), "model-deleted".to_string()],
        ..Default::default()
    };

    let mut usage = nuo_persistence::connection_usage::ConnectionUsage::default();
    usage.record("my-custom");
    usage.record("deleted-connection");
    usage.record_model("my-custom", "model-a");
    usage.record_model("my-custom", "model-deleted");
    usage.record_model("deleted-connection", "model-deleted");

    let changed = super::prune_stale_models(&mut config, &mut usage);
    assert!(changed);

    // Favorites pruned of non-existent model
    assert_eq!(config.favorites, vec!["model-a"]);
    // default_model reset since it was deleted
    assert_eq!(config.default_model, None);

    // Usage pruned of deleted connection and deleted models
    assert!(usage.recency_of("my-custom") > 0);
    assert_eq!(usage.recency_of("deleted-connection"), 0);
    assert!(usage.model_recency("my-custom", "model-a") > 0);
    assert_eq!(usage.model_recency("my-custom", "model-deleted"), 0);
    assert_eq!(
        usage.model_recency("deleted-connection", "model-deleted"),
        0
    );
    assert_eq!(usage.last_model_for("my-custom"), None);
    assert_eq!(usage.last_model_for("deleted-connection"), None);
}

#[test]
fn model_recency_isolation_across_same_preset_connections() {
    let _sandbox = sandboxed_paths();
    register_mock_provider(
        "test-open-relay-recency",
        "https://relay.example.com",
        WireProtocol::OpenAiChatCompletions,
        &[],
        nuo_provider::CatalogShape::OpenAi,
    );
    let mut conn1 = instance("conn-1", Some("test-open-relay-recency"));
    conn1.models.include = vec![nuo_protocol::model::DeclaredModel {
        id: "shared-model".to_string(),
        ..Default::default()
    }];

    let mut conn2 = instance("conn-2", Some("test-open-relay-recency"));
    conn2.models.include = vec![nuo_protocol::model::DeclaredModel {
        id: "shared-model".to_string(),
        ..Default::default()
    }];

    let connections = Connections {
        connections: vec![conn1, conn2],
    };
    connections.save().unwrap();

    let config = Config {
        default_connection: "conn-1".to_string(),
        ..Default::default()
    };

    let mut usage = nuo_persistence::connection_usage::ConnectionUsage::default();
    // User activates shared-model on conn-1 only
    usage.record("conn-1");
    usage.record_model("conn-1", "shared-model");

    let snapshot = super::build_picker_state(&config, &usage);
    let row1 = snapshot
        .rows
        .iter()
        .find(|r| r.id == "conn-1")
        .expect("row1");
    let row2 = snapshot
        .rows
        .iter()
        .find(|r| r.id == "conn-2")
        .expect("row2");

    let model1 = row1
        .model_info
        .iter()
        .find(|m| m.model == "shared-model")
        .expect("conn-1 shared-model");
    let model2 = row2
        .model_info
        .iter()
        .find(|m| m.model == "shared-model")
        .expect("conn-2 shared-model");

    // conn-1 has last_used_ms recorded
    assert!(
        model1.last_used_ms.is_some(),
        "conn-1 shared-model must have recency"
    );
    // conn-2 must NOT have last_used_ms set!
    assert_eq!(
        model2.last_used_ms, None,
        "conn-2 shared-model must NOT inherit conn-1 recency"
    );
}

#[tokio::test]
async fn etag_matching_stale_renews_timestamp() {
    let _sandbox = sandboxed_paths();
    let conn = instance("test-etag-conn", Some("openai"));
    let connections = Connections {
        connections: vec![conn],
    };
    connections.save().unwrap();

    let mut cache = RemoteCatalogCache::default();
    let source_identity =
        source_identity_for_connection(connections.get("test-etag-conn").unwrap(), &cache).unwrap();
    cache.model_lists.insert(
        "test-etag-conn".to_string(),
        ModelListCacheState {
            etag: Some("etag-abc".to_string()),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            source_identity,
            refreshed_at_ms: 1000, // very stale
            refresh_failed: false,
        },
    );
    cache.save().unwrap();

    let outcome = refresh_connection_models_for_etag("test-etag-conn", "etag-abc").await;
    assert!(!outcome.changed);
    assert!(outcome.failures.is_empty());

    let reloaded = RemoteCatalogCache::load();
    let state = reloaded.model_lists.get("test-etag-conn").unwrap();
    assert_eq!(state.etag.as_deref(), Some("etag-abc"));
    assert!(state.refreshed_at_ms > 1000);
}

#[tokio::test]
async fn catalog_sync_never_resurrects_deleted_connection() {
    let _sandbox = sandboxed_paths();
    // Do NOT add "deleted-conn" to Connections
    let connections = Connections {
        connections: Vec::new(),
    };
    connections.save().unwrap();

    let mut locked_cache = RemoteCatalogCache::lock().await.unwrap();
    locked_cache.remove_connection("deleted-conn");
    locked_cache.save().unwrap();

    let outcome = sync_connection_routes("deleted-conn").await;
    assert!(!outcome.changed);

    let final_cache = RemoteCatalogCache::load();
    assert!(!final_cache.connection_models.contains_key("deleted-conn"));
    assert!(!final_cache.model_lists.contains_key("deleted-conn"));
}

#[test]
fn adr0199_preset_scope_and_instance_scope_cascade() {
    use nuo_persistence::model_providers::ModelProviders;
    ensure_test_init();

    let mut providers = ModelProviders::default();
    let ds_preset = providers.get_or_create_mut("deepseek");
    ds_preset.include.push(nuo_protocol::model::DeclaredModel {
        id: "deepseek-preset-preview".to_string(),
        ..Default::default()
    });
    // Preset excludes deepseek-chat
    ds_preset.exclude.push("deepseek-chat".to_string());

    let mut conn1 = instance("ds-work", Some("deepseek"));
    // Connection instance excludes deepseek-coder
    conn1.models.exclude.push("deepseek-coder".to_string());
    // Connection instance includes an instance-specific model
    conn1
        .models
        .include
        .push(nuo_protocol::model::DeclaredModel {
            id: "deepseek-instance-private".to_string(),
            ..Default::default()
        });

    let models1 = route_models(&conn1, &RemoteCatalogCache::default(), &providers);
    assert!(
        models1.contains(&"deepseek-preset-preview".to_string()),
        "preset include present"
    );
    assert!(
        models1.contains(&"deepseek-instance-private".to_string()),
        "instance include present"
    );
    assert!(
        !models1.contains(&"deepseek-chat".to_string()),
        "preset exclude applied"
    );
    assert!(
        !models1.contains(&"deepseek-coder".to_string()),
        "instance exclude applied"
    );

    // Second connection with same preset inherits preset include/exclude, but not instance1's deltas
    let conn2 = instance("ds-personal", Some("deepseek"));
    let models2 = route_models(&conn2, &RemoteCatalogCache::default(), &providers);
    assert!(models2.contains(&"deepseek-preset-preview".to_string()));
    assert!(!models2.contains(&"deepseek-chat".to_string()));
    assert!(!models2.contains(&"deepseek-instance-private".to_string()));
}

#[test]
fn provider_dialect_rejects_incompatible_remote_protocol_without_panicking() {
    let _sandbox = sandboxed_paths();
    for (provider, protocol) in [
        ("google-antigravity", WireProtocol::OpenAiChatCompletions),
        ("github-copilot", WireProtocol::GoogleGenerateContent),
        ("openai-subscription", WireProtocol::OpenAiChatCompletions),
        ("qoder", WireProtocol::OpenAiResponses),
    ] {
        let conn = instance("invalid-wire", Some(provider));
        let mut cache = RemoteCatalogCache::default();
        cache
            .remote_metadata
            .entry(conn.name.clone())
            .or_default()
            .insert(
                "remote-model".into(),
                nuo_protocol::RemoteModelMetadata {
                    protocol: Some(protocol),
                    ..Default::default()
                },
            );
        let error = derive_channel(
            &conn,
            "remote-model",
            &inputs_of(
                &cache,
                &RouteSettingsStore::default(),
                &Credentials::default(),
            ),
        )
        .unwrap_err();
        assert!(
            error
                .message()
                .contains("incompatible with provider dialect")
        );
    }
}

#[test]
fn provider_dialect_selects_endpoint_after_remote_protocol_override() {
    let _sandbox = sandboxed_paths();
    assert_eq!(
        nuo_provider::model_provider_spec("opencode-zen")
            .unwrap()
            .model_protocol("glm-5.2"),
        WireProtocol::OpenAiChatCompletions
    );
    let conn = instance("endpoint-order", Some("opencode-zen"));
    let mut cache = RemoteCatalogCache::default();
    cache
        .remote_metadata
        .entry(conn.name.clone())
        .or_default()
        .insert(
            "glm-5.2".into(),
            nuo_protocol::RemoteModelMetadata {
                protocol: Some(WireProtocol::AnthropicMessages),
                ..Default::default()
            },
        );
    let channel = derive_channel(
        &conn,
        "glm-5.2",
        &inputs_of(
            &cache,
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(
        matches!(channel.transport, Transport::AnthropicMessages { ref base_url, .. }
        if base_url == "https://opencode.ai/inference/anthropic/v1/messages")
    );
}

#[test]
fn catalog_advertised_root_replaces_the_compiled_spec_route() {
    // ADR-0079: a per-model `endpoint` root from the remote catalog overrides
    // the spec's route, with the suffix still appended by the ADR-0015 algebra.
    let _sandbox = sandboxed_paths();
    let conn = instance("root-override", Some("opencode-zen"));
    let mut cache = RemoteCatalogCache::default();
    cache
        .remote_metadata
        .entry(conn.name.clone())
        .or_default()
        .insert(
            "glm-5.3".into(),
            nuo_protocol::RemoteModelMetadata {
                protocol: Some(WireProtocol::GoogleGenerateContent),
                endpoint: Some("https://opencode.ai/inference/google/v1beta".to_string()),
                ..Default::default()
            },
        );
    let channel = derive_channel(
        &conn,
        "glm-5.3",
        &inputs_of(
            &cache,
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    // The Google protocol carries no suffix: the advertised root is verbatim.
    assert!(
        matches!(channel.transport, Transport::GoogleGenerateContent { ref base_url, .. }
        if base_url == "https://opencode.ai/inference/google/v1beta"),
        "advertised root must ride the shared suffix algebra, got {:?}",
        channel.transport
    );

    // An invalid advertised root is a readable routing error, not a panic or a
    // silent fall-through to the spec route.
    cache
        .remote_metadata
        .get_mut(conn.name.as_str())
        .unwrap()
        .get_mut("glm-5.3")
        .unwrap()
        .endpoint = Some("not a url".to_string());
    let error = derive_channel(
        &conn,
        "glm-5.3",
        &inputs_of(
            &cache,
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap_err();
    assert!(error.message().contains("catalog-advertised root"));
}

/// ADR-0068 end-to-end: a model the provider declared unavailable for this
/// account stays *visible* in the picker (so the account can see what an
/// upgrade would unlock, with the provider's own reason when it gave one) while
/// the server refuses to build a route for it. A user injection overrides the
/// verdict but the upstream declaration is never rewritten.
#[test]
fn declared_unavailable_model_is_listed_but_refused_by_the_server() {
    let _sandbox = sandboxed_paths();
    let qoder = Connection {
        name: "qoder".to_string(),
        provider: "qoder".to_string(),
        auth: nuo_protocol::ConnectionAuth::negotiated("qoder"),
        ..Default::default()
    };
    Connections {
        connections: vec![qoder.clone()],
    }
    .save()
    .unwrap();

    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "qoder".to_string(),
        vec!["qfmodel".to_string(), "gmodel".to_string()],
    );
    cache.remote_metadata.insert("qoder".to_string(), {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "qfmodel".to_string(),
            nuo_protocol::RemoteModelMetadata {
                availability: Some(nuo_protocol::Availability::usable()),
                ..Default::default()
            },
        );
        m.insert(
            "gmodel".to_string(),
            nuo_protocol::RemoteModelMetadata {
                availability: Some(nuo_protocol::Availability::locked(None)),
                ..Default::default()
            },
        );
        m
    });
    cache.save().unwrap();

    let config = Config {
        default_connection: "qoder".to_string(),
        default_model: Some("gmodel".to_string()),
        ..Default::default()
    };
    let usage = nuo_persistence::connection_usage::ConnectionUsage::default();
    let snapshot = build_picker_state(&config, &usage);
    let row = snapshot
        .rows
        .iter()
        .find(|r| r.id == "qoder")
        .expect("qoder row");
    let locked = row
        .model_info
        .iter()
        .find(|info| info.model == "gmodel")
        .expect("the locked model is still listed, not dropped");
    assert_eq!(
        locked.availability,
        Some(nuo_protocol::Availability::locked(None)),
        "the declaration reaches the picker so it can dim the row"
    );
    assert!(
        row.models.contains(&"gmodel".to_string()),
        "membership is untouched by availability"
    );

    // The server refuses the explicitly requested locked model...
    assert!(
        super::build_provider_for_model(&config, "qoder", Some("gmodel"), None).is_none(),
        "a provider-declared-unavailable model must not be routable"
    );
    // ...while a usable sibling still routes.
    assert!(
        super::build_provider_for_model(&config, "qoder", Some("qfmodel"), None).is_some(),
        "an available sibling must still route"
    );
    // A user injection overrides the verdict ([INV-CATALOG-04]):
    // the route is built, and the override is disclosed rather than silent.
    let mut injected = qoder;
    // An explicit filter keeps the connection out of the legacy-policy
    // migration path, which folds only `extra_models` back into the scope.
    injected.models.filter = Some(ConnectionFilterPolicy::Named(NamedFilterPolicy::All));
    injected.models.include = vec![nuo_protocol::DeclaredModel {
        id: "gmodel".to_string(),
        ..Default::default()
    }];
    Connections {
        connections: vec![injected],
    }
    .save()
    .unwrap();
    assert!(
        super::build_provider_for_model(&config, "qoder", Some("gmodel"), None).is_some(),
        "a sovereign injection overrides the provider's unavailable verdict"
    );
    let snapshot = build_picker_state(&config, &usage);
    let row = snapshot
        .rows
        .iter()
        .find(|r| r.id == "qoder")
        .expect("qoder row");
    let overridden = row
        .model_info
        .iter()
        .find(|info| info.model == "gmodel")
        .expect("model listed");
    assert!(overridden.availability_overridden);
    assert_eq!(
        overridden.availability,
        Some(nuo_protocol::Availability::usable()),
        "the effective verdict is usable after the override"
    );
}
