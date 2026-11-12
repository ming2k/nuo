use super::*;

#[tokio::test]
async fn record_provider_ack_appends_durable_ack_to_command_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let session = SessionStore::for_path(tmp.path().join("session.json"));
    record_provider_ack(
        &session,
        "111xianyu",
        "k3",
        "Connection switched to 111xianyu (k3)".to_string(),
    )
    .await;

    let commands = session.commands().await;
    assert_eq!(commands.len(), 1);
    let record = &commands[0];
    assert_eq!(record.name, "models");
    assert_eq!(record.args, "111xianyu k3");
    assert_eq!(record.status, nuo_protocol::CommandStatus::Success);
    match &record.result {
        Some(nuo_protocol::CommandResult::Ack { title, .. }) => {
            assert_eq!(title, "Connection switched to 111xianyu (k3)");
        }
        other => panic!("expected a durable Ack result, got {other:?}"),
    }
}

#[test]
fn wire_protocol_parser_is_exact_and_rejects_unknown_labels() {
    assert_eq!(
        "anthropic-messages".parse::<WireProtocol>(),
        Ok(WireProtocol::AnthropicMessages)
    );
    assert_eq!(
        "openai-responses".parse::<WireProtocol>(),
        Ok(WireProtocol::OpenAiResponses)
    );
    assert_eq!(
        "openai-chat-completions".parse::<WireProtocol>(),
        Ok(WireProtocol::OpenAiChatCompletions)
    );
    assert_eq!(
        "google-generate-content".parse::<WireProtocol>(),
        Ok(WireProtocol::GoogleGenerateContent)
    );
    assert_eq!(
        "google-interactions".parse::<WireProtocol>(),
        Ok(WireProtocol::GoogleInteractions)
    );
    assert!("responses".parse::<WireProtocol>().is_err());
    assert!("chat-completions".parse::<WireProtocol>().is_err());
    assert!("google-gemini".parse::<WireProtocol>().is_err());
    assert!("openai".parse::<WireProtocol>().is_err());
    assert!("future".parse::<WireProtocol>().is_err());
}

#[test]
fn connection_names_are_unique_and_suggest_an_alternative() {
    let mut connections = Connections::default();
    connections.connections.push(Connection {
        name: "my-relay".to_string(),
        ..Default::default()
    });
    // A different name is accepted (trimmed), a duplicate is rejected with
    // a suggestion, and an empty name is rejected (INV-3).
    assert_eq!(
        connections.check_new_name(" My Relay ").unwrap(),
        "My Relay"
    );
    let err = connections.check_new_name("my-relay").unwrap_err();
    assert!(err.contains("my-relay-2"), "{err}");
    assert!(connections.check_new_name("  ").is_err());
}

#[tokio::test]
async fn add_provider_oauth_auth_mismatch_never_writes_connection_or_token() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        state_dir: dir.path().join("state"),
        cache_dir: dir.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);

    let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();
    let session = SessionStore::for_path(dir.path().join("session.json"));
    let mut config = Config::default();
    let mut usage = ConnectionUsage::default();
    let agent = nuo_harness::Agent::builder(
        Arc::new(nuo_harness::NoProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    )
    .build();
    let provider_for_task = Arc::new(std::sync::RwLock::new(agent.provider.clone()));

    let env = ProviderEnv {
        config: &mut config,
        agent: &agent,
        provider_for_task: &provider_for_task,
        session: &session,
        resp_tx: &resp_tx,
        provider_usage: &mut usage,
    };

    // Pending auth is for Antigravity, but params requests ChatGPT
    let pending = PendingOAuthAuthorization {
        auth: nuo_protocol::ConnectionAuth::negotiated("google-antigravity"),
        tokens: nuo_provider::TokenSet {
            access: "tok".into(),
            refresh: "ref".into(),
            expires_ms: 1000,
            id_token: None,
            token_type: None,
            scope: None,
            user_email: None,
            attributes: serde_json::Map::new(),
        },
    };

    let params = AddConnectionParams {
        name: "Mismatched Provider".to_string(),
        provider: "openai-subscription".to_string(),
        api_key: "".into(),
        models: vec!["gpt-5.6".to_string()],
        auth: nuo_protocol::ConnectionAuth::negotiated("chatgpt"),
        headers: None,
        budget: Default::default(),
        entitlement: None,
    };

    add(env, params, Some(pending)).await;

    // Verify response was ConnectStatus::Failed
    let resp = resp_rx.recv().await.unwrap();
    assert!(matches!(
        resp,
        AgentResponse::ConnectStatus(nuo_protocol::ConnectStatus::Failed { .. })
    ));

    // Verify no connection was written
    let conns = Connections::load();
    assert!(conns.connections.is_empty());

    // Verify no token was written
    let store = crate::credentials_host::store();
    assert!(store.read("anything").unwrap().is_none());
    assert!(
        store.read("probe").unwrap().is_none(),
        "no credential may be written for a rejected connection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn query_connection_detail_sends_initial_and_background_detail() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        state_dir: dir.path().join("state"),
        cache_dir: dir.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);

    let mut store = nuo_persistence::model_providers::ModelProviders::default();
    store.set_provider(
        "test-prov",
        nuo_persistence::model_providers::UserDeclaredProvider {
            label: Some("Test Relay".to_string()),
            root_url: "https://example.com".to_string(),
            default_protocol: Some(WireProtocol::OpenAiChatCompletions),
            user_agent: None,
            headers: None,
            catalog: None,
            dialect: None,
            protocol_roots: vec![],
            catalog_root_url: None,
            prompt_cache: None,
        },
    );
    ModelProviders::save(&store).unwrap();
    crate::provider_registry::sync_user_declared_providers(&store).unwrap();

    let mut conns = Connections::default();
    conns.connections.push(Connection {
        name: "test-relay".to_string(),
        provider: "test-prov".to_string(),
        ..Default::default()
    });
    conns.save().unwrap();

    let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();
    query_connection_detail(&resp_tx, "test-relay".to_string(), false).await;

    // Phase 1: immediate detail with Fetching usage
    let initial = resp_rx.recv().await.expect("initial response");
    match initial {
        AgentResponse::ConnectionDetail(detail) => {
            assert_eq!(detail.name, "test-relay");
            assert_eq!(detail.usage, nuo_protocol::ConnectionUsageState::Fetching);
        }
        other => panic!("expected ConnectionDetail, got {other:?}"),
    }

    // Phase 2: background resolution
    let final_resp = resp_rx.recv().await.expect("final response");
    match final_resp {
        AgentResponse::ConnectionDetail(detail) => {
            assert_eq!(detail.name, "test-relay");
            // Unsupported since base_url is example.com and no API key is set
            assert!(matches!(
                detail.usage,
                nuo_protocol::ConnectionUsageState::Error(_)
                    | nuo_protocol::ConnectionUsageState::Unsupported
            ));
        }
        other => panic!("expected final ConnectionDetail, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_update_surfaces_warning_and_picker_then_final_keys() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        state_dir: dir.path().join("state"),
        cache_dir: dir.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);

    let mut config = Config::default();
    let mut usage = ConnectionUsage::default();
    let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

    apply_connection_update(
        &mut config,
        &resp_tx,
        &mut usage,
        nuo_model_routing::ConnectionUpdate {
            connection: "gmain".to_string(),
            changed: false,
            error: Some("network error".to_string()),
            refused: false,
        },
    );

    let warning = resp_rx.recv().await.expect("warning expected");
    match warning {
        AgentResponse::ConnectStatus(nuo_protocol::ConnectStatus::CatalogSyncWarning {
            provider,
            message,
            kind,
        }) => {
            assert_eq!(provider, "gmain");
            assert_eq!(message, "network error");
            assert_eq!(kind, nuo_protocol::CatalogSyncFailure::Transient);
        }
        other => panic!("expected CatalogSyncWarning, got {other:?}"),
    }
    let picker = resp_rx.recv().await.expect("picker expected");
    assert!(matches!(picker, AgentResponse::ProviderPicker(_)));

    apply_catalog_sync_outcome(
        &mut config,
        &resp_tx,
        &mut usage,
        nuo_model_routing::RoutingSyncOutcome::default(),
        None,
    );

    let keys = resp_rx.recv().await.expect("keys expected");
    assert!(matches!(keys, AgentResponse::ProviderKeys(_)));
    let picker = resp_rx.recv().await.expect("picker expected");
    assert!(matches!(picker, AgentResponse::ProviderPicker(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0199_handlers_mutate_preset_and_connection_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        state_dir: dir.path().join("state"),
        cache_dir: dir.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);

    let mut config = Config::default();
    let mut usage = ConnectionUsage::default();
    let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

    // 1. Include on provider scope
    include_model(
        &mut config,
        &resp_tx,
        &mut usage,
        ModelTargetScope::Provider("deepseek".into()),
        nuo_protocol::model::DeclaredModel {
            id: "deepseek-v4-preview".into(),
            context_window: Some(1_000_000),
            ..Default::default()
        },
    )
    .await;

    let picker = resp_rx.recv().await.expect("picker after include");
    assert!(matches!(picker, AgentResponse::ProviderPicker(_)));

    let providers = ModelProviders::load();
    let ds = providers.get("deepseek").expect("provider scope saved");
    assert_eq!(ds.include.len(), 1);
    assert_eq!(ds.include[0].id, "deepseek-v4-preview");

    // 2. Exclude on provider scope
    exclude_model(
        &mut config,
        &resp_tx,
        &mut usage,
        ModelTargetScope::Provider("deepseek".into()),
        "deepseek-chat".into(),
    )
    .await;
    let _ = resp_rx.recv().await;

    let providers = ModelProviders::load();
    let ds = providers.get("deepseek").unwrap();
    assert_eq!(ds.exclude, vec!["deepseek-chat"]);

    // 3. Clear rule on provider scope
    clear_model_rule(
        &mut config,
        &resp_tx,
        &mut usage,
        ModelTargetScope::Provider("deepseek".into()),
        "deepseek-chat".into(),
    )
    .await;
    let _ = resp_rx.recv().await;

    let providers = ModelProviders::load();
    let ds = providers.get("deepseek").unwrap();
    assert!(!ds.exclude.contains(&"deepseek-chat".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn add_provider_snapshots_rules_without_materializing_model_ids() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        state_dir: dir.path().join("state"),
        cache_dir: dir.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);

    let mut config = Config::default();
    let session = SessionStore::for_path(dir.path().join("session.json"));
    let agent = nuo_harness::Agent::builder(
        Arc::new(nuo_harness::NoProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    )
    .build();
    let provider_for_task = Arc::new(std::sync::RwLock::new(agent.provider.clone()));
    let (resp_tx, _resp_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut usage = ConnectionUsage::default();

    let env = ProviderEnv {
        config: &mut config,
        agent: &agent,
        provider_for_task: &provider_for_task,
        session: &session,
        resp_tx: &resp_tx,
        provider_usage: &mut usage,
    };

    let params = AddConnectionParams {
        name: "openai-test".to_string(),
        provider: "openai".to_string(),
        api_key: SecretString::from("sk-test"),
        models: vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()],
        auth: nuo_protocol::ConnectionAuth::Static,
        headers: None,
        budget: Default::default(),
        entitlement: None,
    };

    add(env, params, None).await;

    let conns = Connections::load();
    let conn = conns.get("openai-test").expect("connection was saved");
    // [INV-CATALOG-05]: filter rule is persisted, not materialized model ID list!
    assert!(
        conn.models.include.is_empty(),
        "curated connection must not persist materialized model list"
    );
    assert_eq!(
        conn.models.filter,
        Some(nuo_protocol::ConnectionFilterPolicy::Named(
            nuo_protocol::NamedFilterPolicy::All
        ))
    );
}

#[tokio::test]
async fn add_connection_persists_commandcode_and_opencode() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        state_dir: dir.path().join("state"),
        cache_dir: dir.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);
    crate::provider_registry::init();

    let mut config = Config::default();
    let session = SessionStore::for_path(dir.path().join("session.json"));
    let agent = nuo_harness::Agent::builder(
        Arc::new(nuo_harness::NoProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    )
    .build();
    let provider_for_task = Arc::new(std::sync::RwLock::new(agent.provider.clone()));
    let (resp_tx, _resp_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut usage = ConnectionUsage::default();

    let env = ProviderEnv {
        config: &mut config,
        agent: &agent,
        provider_for_task: &provider_for_task,
        session: &session,
        resp_tx: &resp_tx,
        provider_usage: &mut usage,
    };

    // Add CommandCode GOAT connection
    let params = AddConnectionParams {
        name: "cc-goat".to_string(),
        provider: "commandcode-goat".to_string(),
        api_key: SecretString::from("key-cc"),
        models: vec![],
        auth: nuo_protocol::ConnectionAuth::Static,
        headers: None,
        budget: Default::default(),
        entitlement: None,
    };
    add(env, params, None).await;

    let env = ProviderEnv {
        config: &mut config,
        agent: &agent,
        provider_for_task: &provider_for_task,
        session: &session,
        resp_tx: &resp_tx,
        provider_usage: &mut usage,
    };

    // Add OpenCode Go connection
    let params = AddConnectionParams {
        name: "oc-go".to_string(),
        provider: "opencode-go".to_string(),
        api_key: SecretString::from("key-oc"),
        models: vec![],
        auth: nuo_protocol::ConnectionAuth::Static,
        headers: None,
        budget: Default::default(),
        entitlement: None,
    };
    add(env, params, None).await;

    let conns = Connections::load();
    assert_eq!(
        conns.get("cc-goat").expect("cc-goat saved").provider,
        "commandcode-goat"
    );
    assert_eq!(
        conns.get("oc-go").expect("oc-go saved").provider,
        "opencode-go"
    );
}

/// The model `e` editor's effort choice must be persisted on **every**
/// transport, Google included. The handler used to carry an explicit
/// no-op arm for the two Google transports, so a `gemini-3.8-flash`
/// ladder change was validated and then dropped on the floor: the route
/// kept deriving `effort: None` and the provider fell back to
/// `Effort::channel_default` (`high`), which is why the slider appeared
/// to do nothing and the picker snapped back to the old rung.
#[tokio::test]
async fn edit_model_persists_effort_on_google_routes() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        state_dir: dir.path().join("state"),
        cache_dir: dir.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);
    crate::provider_registry::init();

    let mut conns = Connections::default();
    conns.connections.push(Connection {
        name: "gemini".to_string(),
        provider: "google".to_string(),
        ..Default::default()
    });
    conns.save().unwrap();

    // `default_connection` stays empty, so this is not the live route and
    // the handler takes its picker-refresh branch instead of activating.
    let mut config = Config::default();
    let mut usage = ConnectionUsage::default();
    let (resp_tx, _resp_rx) = mpsc::unbounded_channel();
    let agent = nuo_harness::Agent::builder(
        Arc::new(nuo_harness::NoProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    )
    .build();
    let provider_for_task = Arc::new(RwLock::new(agent.provider.clone()));
    let session = SessionStore::for_path(dir.path().join("session.json"));

    edit_model(
        ProviderEnv {
            config: &mut config,
            agent: &agent,
            provider_for_task: &provider_for_task,
            session: &session,
            resp_tx: &resp_tx,
            provider_usage: &mut usage,
        },
        "gemini".to_string(),
        "gemini-3.8-flash".to_string(),
        Some("low".to_string()),
        None,
        None,
    )
    .await;

    assert_eq!(
        RouteSettingsStore::load()
            .settings_for("gemini", "gemini-3.8-flash")
            .and_then(|settings| settings.tunings.effort())
            .map(|e| e.as_str().to_string())
            .as_deref(),
        Some("low"),
        "a Google route must persist the chosen effort instead of dropping it"
    );

    // The persisted rung must also reach the derived route, which is what
    // the provider factory reads to stamp `thinkingConfig.thinkingLevel`.
    let stores = nuo_model_routing::Stores::load();
    let conn = stores.connections.get("gemini").unwrap();
    let channel =
        nuo_model_routing::derive_channel(conn, "gemini-3.8-flash", &stores.inputs()).unwrap();
    // The stored rung travels on the channel's tunings, and the resolved
    // capabilities clamp it onto the route's ladder. The transport carries
    // no user choice at all (ADR-0126).
    let caps = channel.capabilities();
    assert_eq!(
        channel.effective_effort(&caps),
        Some(nuo_provider::Effort::Low),
        "the derived route must expose the stored rung"
    );
}

/// A session that already runs a model keeps it when a new connection is
/// added via `/connections`: the add handler must not touch the live
/// provider holder, the global default, or the session pin. Activation is
/// a model-switch concern (`/models`), not a connection-management one.
#[tokio::test(flavor = "multi_thread")]
async fn add_connection_never_hijacks_an_active_session_model() {
    use nuo_protocol::ModelRequest;

    struct LiveStubProvider;
    #[async_trait::async_trait]
    impl nuo_protocol::Provider for LiveStubProvider {
        async fn chat(
            &self,
            _request: &ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("stub is never invoked")
        }
        async fn stream_chat(
            &self,
            _request: &ModelRequest,
            _telemetry: nuo_provider::TransportTelemetry,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
        > {
            unreachable!("stub is never invoked")
        }
        fn provider_id(&self) -> String {
            "existing-conn".to_string()
        }
        fn model(&self) -> String {
            "existing-model".to_string()
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        state_dir: dir.path().join("state"),
        cache_dir: dir.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);

    // A session pinned to an existing connection/model (as after /models).
    let mut config = Config {
        default_connection: "existing-conn".to_string(),
        default_model: Some("existing-model".to_string()),
        ..Default::default()
    };
    let session = SessionStore::for_path(dir.path().join("session.json"));
    session
        .set_provider_selection(Some(ProviderSelection {
            connection: "existing-conn".to_string(),
            model: Some("existing-model".to_string()),
        }))
        .await
        .ok();

    // A live, non-sentinel provider holder (NOT NoProvider).
    let agent = nuo_harness::Agent::builder(
        Arc::new(LiveStubProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    )
    .build();
    let provider_for_task = Arc::new(std::sync::RwLock::new(agent.provider.clone()));
    let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut usage = ConnectionUsage::default();

    let env = ProviderEnv {
        config: &mut config,
        agent: &agent,
        provider_for_task: &provider_for_task,
        session: &session,
        resp_tx: &resp_tx,
        provider_usage: &mut usage,
    };
    let params = AddConnectionParams {
        name: "second-conn".to_string(),
        provider: "openai".to_string(),
        api_key: SecretString::from("sk-test"),
        models: vec!["gpt-4o".to_string()],
        auth: nuo_protocol::ConnectionAuth::Static,
        headers: None,
        budget: Default::default(),
        entitlement: None,
    };

    add(env, params, None).await;

    // The connection itself was still persisted — connection management happened.
    assert!(
        Connections::load().get("second-conn").is_some(),
        "the new connection must be created"
    );

    // ...but the live provider, global default, and session pin are untouched.
    assert_eq!(
        provider_for_task
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .provider_id(),
        "existing-conn",
        "live provider must not be swapped by /connections"
    );
    assert_eq!(
        config.default_connection, "existing-conn",
        "global default must not follow the newly added connection"
    );
    assert_eq!(
        config.default_model.as_deref(),
        Some("existing-model"),
        "global default model must not follow the newly added connection"
    );
    let pin = session.provider_selection().await;
    assert_eq!(
        pin.as_ref().map(|s| s.connection.as_str()),
        Some("existing-conn"),
        "session pin must keep its original connection"
    );
    assert_eq!(
        pin.as_ref().and_then(|s| s.model.as_deref()),
        Some("existing-model"),
        "session pin must keep its original model"
    );

    // No ProviderSwitched broadcast may fire — that would flip the TUI.
    while let Ok(resp) = resp_rx.try_recv() {
        assert!(
            !matches!(resp, AgentResponse::ProviderSwitched { .. }),
            "add must never broadcast ProviderSwitched"
        );
    }
}

#[tokio::test]
async fn query_provider_quotas_snapshot_aggregates_connections() {
    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
    query_provider_quotas(&resp_tx, Some("google-antigravity".to_string()), false).await;
    let resp = resp_rx
        .try_recv()
        .expect("must receive ProviderQuotas response");
    match resp {
        AgentResponse::ProviderQuotas(snapshot) => {
            assert_eq!(
                snapshot.provider_filter.as_deref(),
                Some("google-antigravity")
            );
            // Connections from default store or empty test environment
            assert_eq!(snapshot.total_accounts, snapshot.entries.len());
        }
        other => panic!("expected ProviderQuotas, got {other:?}"),
    }
}

#[tokio::test]
async fn query_provider_quotas_emits_immediate_initial_and_streamed_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_persistence::paths::TestDirsGuard::install(dirs);

    // Add a dummy quota-capable connection
    let mut conns = Connections::default();
    conns
        .connections
        .push(nuo_persistence::connections::Connection {
            name: "test-antigravity".into(),
            provider: "google-antigravity".into(),
            auth: nuo_protocol::ConnectionAuth::Static,
            ..Default::default()
        });
    conns.save().unwrap();

    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
    // Force refresh ensures it performs phase 1 (Fetching) then phase 2
    query_provider_quotas(&resp_tx, Some("google-antigravity".to_string()), true).await;

    // Phase 1 response must be received first
    let first = resp_rx
        .try_recv()
        .expect("must receive phase 1 skeleton snapshot");
    if let AgentResponse::ProviderQuotas(snap1) = first {
        assert_eq!(snap1.entries.len(), 1);
        assert_eq!(snap1.entries[0].name, "test-antigravity");
        assert!(matches!(
            snap1.entries[0].state,
            nuo_protocol::ConnectionUsageState::Fetching
        ));
    } else {
        panic!("expected ProviderQuotas, got {first:?}");
    }

    // Phase 2 updated response must follow
    let second = resp_rx
        .try_recv()
        .expect("must receive phase 2 update snapshot");
    if let AgentResponse::ProviderQuotas(snap2) = second {
        assert_eq!(snap2.entries.len(), 1);
        assert_eq!(snap2.entries[0].name, "test-antigravity");
        // Since it's a dummy connection with no real credentials, state will transition away from Fetching
        assert!(!matches!(
            snap2.entries[0].state,
            nuo_protocol::ConnectionUsageState::Fetching
        ));
    } else {
        panic!("expected ProviderQuotas, got {second:?}");
    }
}
