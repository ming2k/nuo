//! Route endpoints, credentials, reasoning settings, and protocol cascades.

use super::*;

#[test]
fn preset_connection_derives_models_from_the_preset() {
    let deepseek = instance("deepseek", Some("deepseek"));
    let models = route_models(&deepseek, &RemoteCatalogCache::default(), declarations());
    assert_eq!(models, DEEPSEEK_BUILTIN_MODELS);
    // Catalog-enabled presets without a cache fall back to the snapshot.
    let openai = instance("openai", Some("openai"));
    assert!(!route_models(&openai, &RemoteCatalogCache::default(), declarations()).is_empty());
}

#[test]
fn openrouter_connection_derives_gateway_dialect_and_nex_seed() {
    let connection = instance("openrouter", Some("openrouter"));
    assert_eq!(
        route_models(&connection, &RemoteCatalogCache::default(), declarations()),
        vec!["nex-agi/nex-n2.5-pro:free"]
    );
    let channel = derive_channel(
        &connection,
        "nex-agi/nex-n2.5-pro:free",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(matches!(
        channel.transport,
        Transport::OpenAiChatCompletions {
            dialect: nuo_protocol::OpenAiChatDialect::OpenRouter,
            ref base_url,
            ..
        } if base_url == "https://openrouter.ai/api/v1/chat/completions"
    ));
}

#[test]
fn commandcode_flash_fast_effort_ladder_survives_a_live_catalog() {
    // Both `deepseek/deepseek-v4-flash-fast` and `deepseek/deepseek-v4.1-flash-fast`
    // reach the picker through the live `/models` payload. Commandcode's shaped
    // catalog advertises no effort tiers (only name/context_length/supported_endpoints),
    // so its discovered remote_metadata carries `effort_levels: None`. The channel
    // must fall back to the compiled baseline's `LOW_HIGH_MAX` ladder so the effort
    // (reasoning-degree) knob stays available after a sync — exactly like its
    // siblings. Regression guard for the reasoning-effort knob on these models.
    ensure_test_init();
    for id in [
        "deepseek/deepseek-v4-flash-fast",
        "deepseek/deepseek-v4.1-flash-fast",
    ] {
        let connection = instance("cmd", Some("commandcode"));
        let mut cache = RemoteCatalogCache::default();
        cache
            .connection_models
            .insert("cmd".to_string(), vec![id.to_string()]);
        // A faithful live sync: commandcode's shaped catalog advertises only
        // name/context_length/supported_endpoints — no effort tiers — so the
        // discovered remote_metadata carries `effort_levels: None`. The channel
        // must fall back to the compiled baseline's `LOW_HIGH_MAX` ladder.
        cache.remote_metadata.insert(
            "cmd".to_string(),
            std::collections::BTreeMap::from([(
                id.to_string(),
                nuo_protocol::RemoteModelMetadata {
                    context_window: Some(1_000_000),
                    effort_levels: None,
                    ..Default::default()
                },
            )]),
        );
        let channel = derive_channel(
            &connection,
            id,
            &inputs_of(
                &cache,
                &RouteSettingsStore::default(),
                &Credentials::default(),
            ),
        )
        .expect("flash-fast routes");
        let caps = channel.capabilities();
        assert!(
            !caps.effort_levels.is_empty(),
            "{id} must resolve an effort ladder from the baseline after a \
             live sync that advertises none: {:?}",
            caps.effort_levels
        );
        let info = channel_model_info(&channel);
        assert!(
            !info.effort_levels.is_empty(),
            "the picker snapshot must expose an effort ladder for {id}: {:?}",
            info.effort_levels
        );
    }
}

#[test]
fn commandcode_connection_derives_routes() {
    let connection = instance("cmd", Some("commandcode"));
    let channel = derive_channel(
        &connection,
        "claude-sonnet-5-5",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(matches!(
        channel.transport,
        Transport::AnthropicMessages {
            ref base_url,
            ..
        } if base_url == "https://api.commandcode.ai/provider/v1/messages"
    ));

    let channel_responses = derive_channel(
        &connection,
        "deepseek/deepseek-v4.1-flash",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    // The CommandCode gateway declares `ProviderDialect::Standard`; its
    // DeepSeek models speak that OpenAI-shaped surface, not DeepSeek's native
    // Responses dialect (ADR-0113).
    assert!(matches!(
        channel_responses.transport,
        Transport::OpenAiResponses {
            ref base_url,
            dialect: nuo_protocol::OpenAiResponsesDialect::Standard,
            ..
        } if base_url == "https://api.commandcode.ai/provider/v1/responses"
    ));
}

/// The Responses dialect belongs to the *endpoint*, never to the model id.
///
/// A DeepSeek model on an OpenAI-compatible relay speaks the relay's Standard
/// surface; only the native DeepSeek platform (`ProviderDialect::DeepSeek`)
/// gets the DeepSeek dialect. Guessing from the id sent DeepSeek's nested tool
/// definitions to CommandCode's gateway, whose Zod schema expects the
/// flattened OpenAI shape and answered `400 expected string, received
/// undefined` (ADR-0113).
#[test]
fn responses_dialect_is_provider_owned_not_model_derived() {
    let inputs = inputs_of(
        &RemoteCatalogCache::default(),
        &RouteSettingsStore::default(),
        &Credentials::default(),
    );

    let relay = instance("cmd", Some("commandcode"));
    let channel = derive_channel(&relay, "deepseek/deepseek-v4.1-flash", &inputs).unwrap();
    assert!(
        matches!(
            channel.transport,
            Transport::OpenAiResponses {
                dialect: OpenAiResponsesDialect::Standard,
                ..
            }
        ),
        "a namespaced DeepSeek id on an OpenAI-compatible relay must keep the relay's dialect"
    );

    let native = instance("deepseek", Some("deepseek"));
    let channel = derive_channel(&native, "deepseek-flash", &inputs).unwrap();
    assert!(
        matches!(
            channel.transport,
            Transport::OpenAiResponses {
                dialect: OpenAiResponsesDialect::DeepSeek,
                ..
            }
        ),
        "the native DeepSeek platform declares the DeepSeek dialect"
    );
}

/// Response statefulness is declared by the surface, never assumed from the
/// wire dialect (ADR-0114).
///
/// CommandCode's gateway accepts `store: true` and returns a response id, but
/// keeps no state: a turn anchored on `previous_response_id` reached DeepSeek as
/// a lone `tool` message with no preceding `tool_calls` and was rejected with
/// `400 Messages with role 'tool' must be a response to a preceding message
/// with 'tool_calls'`. Both relay and native DeepSeek surfaces therefore declare
/// `Stateless`, which makes every request self-contained.
#[test]
fn responses_state_is_provider_declared_not_assumed() {
    let inputs = inputs_of(
        &RemoteCatalogCache::default(),
        &RouteSettingsStore::default(),
        &Credentials::default(),
    );

    let relay = instance("cmd", Some("commandcode"));
    let channel = derive_channel(&relay, "deepseek/deepseek-v4.1-flash", &inputs).unwrap();
    assert!(
        matches!(
            channel.transport,
            Transport::OpenAiResponses {
                state: nuo_protocol::ResponsesState::Stateless,
                ..
            }
        ),
        "the CommandCode relay keeps no response state"
    );

    let native = instance("deepseek", Some("deepseek"));
    let channel = derive_channel(&native, "deepseek-flash", &inputs).unwrap();
    assert!(
        matches!(
            channel.transport,
            Transport::OpenAiResponses {
                state: nuo_protocol::ResponsesState::Stateless,
                ..
            }
        ),
        "the native DeepSeek platform keeps no response state"
    );
}

#[test]
fn discovered_model_list_prefers_the_cache() {
    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "deepseek".to_string(),
        vec!["deepseek-v4-flash".to_string()],
    );
    let deepseek = instance("deepseek", Some("deepseek"));
    assert_eq!(
        route_models(&deepseek, &cache, declarations()),
        vec!["deepseek-v4-flash"]
    );
}

#[test]
fn declared_extra_models_union_after_the_derived_set() {
    let mut deepseek = instance("ds-personal", Some("deepseek"));
    deepseek.models.include = vec![nuo_persistence::connections::DeclaredModel {
        id: "deepseek-v4-pro-preview-0912".into(),
        context_window: Some(500_000),
        ..Default::default()
    }];
    // Snapshot floor (no cache): extras append after the preset ids.
    let models = route_models(&deepseek, &RemoteCatalogCache::default(), declarations());
    assert_eq!(models.first().map(String::as_str), Some("deepseek-flash"));
    assert!(models.contains(&"deepseek-v4-pro-preview-0912".to_string()));

    // A catalog sync that omits the hidden id can never evict it —
    // the union happens after the sync, per connection (ADR-0068).
    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "ds-personal".to_string(),
        vec!["deepseek-v4-flash".to_string()],
    );
    assert_eq!(
        route_models(&deepseek, &cache, declarations()),
        vec![
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro-preview-0912".to_string()
        ]
    );

    // Scoping: a second deepseek connection without extras derives nothing —
    // its snapshot floor is untouched and no extra id appears.
    let other = instance("ds-other", Some("deepseek"));
    assert_eq!(
        route_models(&other, &cache, declarations()),
        DEEPSEEK_BUILTIN_MODELS
    );
    assert!(
        !route_models(&other, &cache, declarations())
            .contains(&"deepseek-v4-pro-preview-0912".to_string())
    );

    // A declared id the catalog list already carries is deduped, not doubled.
    let mut cache_hit = RemoteCatalogCache::default();
    cache_hit.connection_models.insert(
        "ds-personal".to_string(),
        vec![
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro-preview-0912".to_string(),
        ],
    );
    assert_eq!(route_models(&deepseek, &cache_hit, declarations()).len(), 2);
}

#[test]
fn declared_extra_model_rides_the_preset_route_with_declared_capabilities() {
    let mut deepseek = instance("ds-personal", Some("deepseek"));
    deepseek.models.include = vec![nuo_persistence::connections::DeclaredModel {
        id: "deepseek-v4-pro-preview-0912".into(),
        context_window: Some(500_000),
        vision: Some(true),
        ..Default::default()
    }];
    let channel = derive_channel(
        &deepseek,
        "deepseek-v4-pro-preview-0912",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    // Routing falls through to the preset's derived route (DeepSeek Responses).
    match &channel.transport {
        Transport::OpenAiResponses {
            base_url, dialect, ..
        } => {
            assert_eq!(base_url, "https://api.deepseek.com/v1/responses");
            assert_eq!(*dialect, OpenAiResponsesDialect::DeepSeek);
        }
        other => panic!("expected Responses transport, got {other:?}"),
    }
    // Declared capability facts overlay the registry default for an
    // unregistered id (ADR-0068: user declaration is the remote layer).
    let capabilities = channel.capabilities();
    assert_eq!(capabilities.context_window, 500_000);
    assert_eq!(capabilities.vision, Some(true));
    // Undeclared fields fall through (no max_output_tokens declared).
    assert_eq!(capabilities.max_output_tokens, None);
}

#[test]
fn custom_instance_serves_its_declared_models() {
    let _sandbox = sandboxed_paths();
    register_mock_provider(
        "test-relay-declared",
        "https://relay.example.com/v1",
        WireProtocol::OpenAiChatCompletions,
        &[],
        nuo_protocol::CatalogShape::OpenAi,
    );
    let mut custom = instance("relay", Some("test-relay-declared"));
    custom.models.include = vec![
        nuo_protocol::model::DeclaredModel {
            id: "a".to_string(),
            ..Default::default()
        },
        nuo_protocol::model::DeclaredModel {
            id: "b".to_string(),
            ..Default::default()
        },
    ];
    assert_eq!(
        route_models(&custom, &RemoteCatalogCache::default(), declarations()),
        vec!["a", "b"]
    );
    let entry = derive_entries(
        &Connections {
            connections: vec![custom],
        },
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .pop()
    .expect("one entry");
    assert_eq!(entry.name, "relay");
    assert_eq!(entry.channels.len(), 2);
    assert!(matches!(
        entry.channels[0].transport,
        Transport::OpenAiChatCompletions { .. }
    ));
}

#[test]
fn adr0203_connection_pipe_valve_algebra() {
    use nuo_protocol::{ConnectionFilterPolicy, NamedFilterPolicy};
    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "my-openai".to_string(),
        vec![
            "gpt-5.6-sol".to_string(),
            "gpt-5.6-luna".to_string(),
            "gpt-4o-mini".to_string(),
            "gpt-6-unannotated".to_string(),
        ],
    );

    // 1. Explicit safe filter ("baseline"): only models in baseline pass through.
    // gpt-6-unannotated is remote-discovered but NOT in baseline, so it is filtered out.
    let mut conn = instance("my-openai", Some("openai"));
    conn.models.filter = Some(ConnectionFilterPolicy::Named(NamedFilterPolicy::Baseline));
    let models = route_models(&conn, &cache, declarations());
    assert!(models.contains(&"gpt-5.6-sol".to_string()));
    assert!(models.contains(&"gpt-5.6-luna".to_string()));
    assert!(
        !models.contains(&"gpt-6-unannotated".to_string()),
        "baseline filter blocks unannotated astra"
    );

    // 2. Open filter ("all"): all remote models pass through pipe!
    conn.models.filter = Some(ConnectionFilterPolicy::Named(NamedFilterPolicy::All));
    let models = route_models(&conn, &cache, declarations());
    assert!(
        models.contains(&"gpt-6-unannotated".to_string()),
        "open filter admits remote astra"
    );

    // 3. Glob pattern filter: admits matching pattern only.
    conn.models.filter = Some(ConnectionFilterPolicy::Glob(vec!["gpt-5.6*".to_string()]));
    let models = route_models(&conn, &cache, declarations());
    assert!(models.contains(&"gpt-5.6-sol".to_string()));
    assert!(models.contains(&"gpt-5.6-luna".to_string()));
    assert!(!models.contains(&"gpt-4o-mini".to_string()));
    assert!(!models.contains(&"gpt-6-unannotated".to_string()));

    // 4. Sovereign injection (inject): bypasses filter unconditionally!
    conn.models.include = vec![nuo_persistence::connections::DeclaredModel {
        id: "gpt-6-unannotated".into(),
        context_window: Some(1_050_000),
        ..Default::default()
    }];
    let models = route_models(&conn, &cache, declarations());
    assert!(
        models.contains(&"gpt-6-unannotated".to_string()),
        "inject bypasses glob filter"
    );

    // 5. Absolute block: prunes model unconditionally!
    conn.models.exclude = vec!["gpt-5.6-luna".to_string()];
    let models = route_models(&conn, &cache, declarations());
    assert!(
        !models.contains(&"gpt-5.6-luna".to_string()),
        "block prunes unconditionally"
    );
}

#[test]
fn remote_catalog_provider_defaults_to_open_admission() {
    let mut cache = RemoteCatalogCache::default();
    cache
        .connection_models
        .insert("chatgpt".to_string(), vec!["gpt-6-astra".to_string()]);
    let connection = instance("chatgpt", Some("openai-subscription"));

    assert_eq!(
        route_models(&connection, &cache, declarations()),
        vec!["gpt-6-astra"]
    );

    cache.connection_models.remove("chatgpt");
    cache
        .model_lists
        .insert("chatgpt".to_string(), ModelListCacheState::default());
    assert!(route_models(&connection, &cache, declarations()).is_empty());
}

#[test]
fn catalog_cache_identity_tracks_custom_headers() {
    let cache = RemoteCatalogCache::default();
    let mut connection = instance("chatgpt", Some("openai-subscription"));
    let codex_identity = source_identity_for_connection(&connection, &cache).unwrap();

    connection
        .headers
        .insert("User-Agent".to_string(), "codex_cli_rs/future".to_string());
    connection.headers.insert(
        "openai-intent".to_string(),
        "conversation-edits".to_string(),
    );
    let future_identity = source_identity_for_connection(&connection, &cache).unwrap();

    assert_ne!(codex_identity, future_identity);
}

#[test]
fn deepseek_route_is_the_responses_transport() {
    let deepseek = instance("deepseek", Some("deepseek"));
    let channel = derive_channel(
        &deepseek,
        "deepseek-v4-flash",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    match &channel.transport {
        Transport::OpenAiResponses {
            base_url, dialect, ..
        } => {
            assert_eq!(base_url, "https://api.deepseek.com/v1/responses");
            assert_eq!(*dialect, OpenAiResponsesDialect::DeepSeek);
        }
        other => panic!("expected Responses transport, got {other:?}"),
    }
}

#[test]
fn opencode_go_routes_models_by_wire_format() {
    let go = instance("opencode-go", Some("opencode-go"));
    let glm = derive_channel(
        &go,
        "glm-5.2",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(
        matches!(&glm.transport, Transport::OpenAiResponses { base_url, user_agent, .. } if base_url == "https://opencode.ai/zen/go/v1/responses" && user_agent == "opencode/2.0.32"),
        "glm-5.2 must route to the portable Responses surface on the zen/go relay with the OpenCode user agent (ADR-0142)"
    );
    let minimax = derive_channel(
        &go,
        "minimax-m3",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(
        matches!(&minimax.transport, Transport::AnthropicMessages { base_url, .. } if base_url == "https://opencode.ai/inference/go/anthropic/v1/messages"),
        "minimax-m3 must route to the Anthropic inference/go surface"
    );
    let qwen = derive_channel(
        &go,
        "qwen3.6-plus",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(
        matches!(&qwen.transport, Transport::AnthropicMessages { base_url, .. } if base_url == "https://opencode.ai/inference/go/anthropic/v1/messages"),
        "qwen3.6-plus must route to the Anthropic /messages surface on zen/go relay"
    );
    assert_eq!(
        route_for_model("opencode-go", "qwen3.6-plus").map(|(p, b, _)| (p, b)),
        Some((
            WireProtocol::AnthropicMessages,
            "https://opencode.ai/inference/go/anthropic/v1/messages".to_string()
        ))
    );
}

#[test]
fn openai_route_uses_official_api_not_opencode_go_relay() {
    ensure_test_init();
    // OpenAI uses its official endpoint as its remote catalog source, and
    // its inference routes MUST target https://api.openai.com, never
    // the opencode.ai relay endpoint.
    let (protocol, base_url, _) =
        route_for_model("openai", "gpt-4o").expect("openai gpt-4o route must exist");
    assert_eq!(protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(base_url, "https://api.openai.com/v1/chat/completions");

    let openai_conn = instance("openai-main", Some("openai"));
    let channel = derive_channel(
        &openai_conn,
        "gpt-4o",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    match &channel.transport {
        Transport::OpenAiChatCompletions { base_url, .. } => {
            assert_eq!(base_url, "https://api.openai.com/v1/chat/completions");
        }
        other => panic!("expected OpenAi transport, got {other:?}"),
    }
}

#[test]
fn connection_uses_provider_transport_endpoint() {
    let relay_conn = instance("corp-relay", Some("openai"));
    let channel = derive_channel(
        &relay_conn,
        "gpt-4o",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    match &channel.transport {
        Transport::OpenAiChatCompletions { base_url, .. } => {
            assert_eq!(base_url, "https://api.openai.com/v1/chat/completions");
        }
        other => panic!("expected OpenAi transport, got {other:?}"),
    }
}

#[test]
fn preset_instance_always_uses_the_hardcoded_template_endpoint() {
    let deepseek = instance("deepseek", Some("deepseek"));
    let channel = derive_channel(
        &deepseek,
        "deepseek-v4-flash",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    match &channel.transport {
        Transport::OpenAiResponses { base_url, .. } => {
            assert_eq!(base_url, "https://api.deepseek.com/v1/responses");
        }
        other => panic!("expected Responses transport, got {other:?}"),
    }
}

#[test]
fn credential_resolves_env_then_credentials_then_empty() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let _sandbox = sandboxed_paths();
    let mut creds = Credentials::default();
    creds.set_api_key("deepseek", Some("from-file".into()));
    creds.save().unwrap();

    // No env → the stored credential.
    unsafe {
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    let deepseek = instance("deepseek", Some("deepseek"));
    assert_eq!(
        resolve_credential(&deepseek, &creds).expose_secret(),
        "from-file"
    );

    // `api_key_env` set and populated → env wins.
    let mut env_instance = instance("deepseek", Some("deepseek"));
    env_instance.api_key_env = Some("DEEPSEEK_API_KEY".to_string());
    unsafe {
        std::env::set_var("DEEPSEEK_API_KEY", "from-env");
    }
    assert_eq!(
        resolve_credential(&env_instance, &creds).expose_secret(),
        "from-env"
    );

    // No credential anywhere → empty (a keyless relay sends no bearer).
    let bare = instance("relay", None);
    assert!(
        resolve_credential(&bare, &Credentials::default())
            .expose_secret()
            .is_empty()
    );
}

#[test]
fn reasoning_route_settings_apply_to_anthropic_routes() {
    let cache = RemoteCatalogCache::default();
    let mut routes = RouteSettingsStore::default();
    routes
        .settings_for_mut("anthropic", "claude-opus-4-8")
        .tunings
        .set_effort(Some(Effort::Max));
    routes
        .settings_for_mut("anthropic", "claude-opus-4-8")
        .tunings
        .set_thinking(Some(false));

    let anthropic = instance("anthropic", Some("anthropic"));
    let channel = derive_channel(
        &anthropic,
        "claude-opus-4-8",
        &inputs_of(&cache, &routes, &Credentials::default()),
    )
    .unwrap();
    // Depth and the switch live on the channel's tunings, not on the
    // transport: the transport is the pipe, the tuning is the user's choice
    // (ADR-0126).
    assert!(matches!(
        channel.transport,
        Transport::AnthropicMessages { .. }
    ));
    assert_eq!(
        channel.tunings.effort(),
        Some(Effort::Max),
        "the stored depth reaches the channel"
    );
    assert_eq!(channel.tunings.thinking(), Some(false), "explicit off wins");
    // A sibling model with no entry stays at the opt-in default (off).
    let sonnet = derive_channel(
        &anthropic,
        "claude-sonnet-4-6",
        &inputs_of(&cache, &routes, &Credentials::default()),
    )
    .unwrap();
    assert!(matches!(
        sonnet.transport,
        Transport::AnthropicMessages { .. }
    ));
    assert_eq!(
        sonnet.effective_effort(&sonnet.capabilities()),
        Some(Effort::High),
        "a sibling with no stored choice takes the provider's declared default"
    );
    assert_eq!(
        sonnet.thinking_state(&sonnet.capabilities()),
        Some(false),
        "the thinking switch has no user choice, so it is off"
    );
    assert!(sonnet.tunings.thinking().is_none(), "nothing was stored");
}

#[test]
fn copilot_route_uses_remote_endpoint_metadata() {
    let mut cache = RemoteCatalogCache::default();
    cache.remote_metadata.insert("copilot".to_string(), {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "gpt-5".to_string(),
            nuo_protocol::RemoteModelMetadata {
                protocol: Some(WireProtocol::OpenAiResponses),
                ..Default::default()
            },
        );
        m
    });
    let copilot = Connection {
        name: "copilot".to_string(),
        provider: "github-copilot".to_string(),
        auth: nuo_protocol::ConnectionAuth::negotiated("copilot"),
        ..Default::default()
    };
    let channel = derive_channel(
        &copilot,
        "gpt-5",
        &inputs_of(
            &cache,
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(
        matches!(
            &channel.transport,
            Transport::OpenAiResponses {
                dialect: OpenAiResponsesDialect::Copilot,
                ..
            }
        ),
        "advertised Responses endpoint routes to the Responses transport"
    );
}

#[test]
fn model_level_protocol_cascade_resolution() {
    let _sandbox = sandboxed_paths();
    let mut cache = RemoteCatalogCache::default();
    // Advertise that a model on an OpenAI-compatible connection wants Anthropic wire format
    cache.remote_metadata.insert("corp-relay".to_string(), {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "claude-custom".to_string(),
            nuo_protocol::RemoteModelMetadata {
                protocol: Some(WireProtocol::AnthropicMessages),
                ..Default::default()
            },
        );
        m
    });
    register_mock_provider(
        "corp-relay-test-prov",
        "https://relay.example.com/v1",
        WireProtocol::OpenAiChatCompletions,
        &[],
        nuo_protocol::CatalogShape::OpenAi,
    );
    let conn = Connection {
        name: "corp-relay".to_string(),
        provider: "corp-relay-test-prov".to_string(),
        ..Default::default()
    };
    let channel = derive_channel(
        &conn,
        "claude-custom",
        &inputs_of(
            &cache,
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(
        matches!(
            &channel.transport,
            Transport::AnthropicMessages {
                dialect: nuo_provider::catalog::AnthropicMessagesDialect::Standard,
                ..
            }
        ),
        "ADR-0015: model-level protocol override is honored over provider default"
    );
}
