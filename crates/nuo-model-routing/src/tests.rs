//! Unit tests for the catalog modules: runtime derivation of routes from
//! connections + presets + catalog cache, credential resolution, per-route
//! reasoning, the fitted-model overlay, and live catalog sync.

use super::derive::{
    DerivationInputs, derive_channel, derive_entries, resolve_credential, route_models,
};
use super::picker::channel_model_info;
use super::sync::source_identity_for_connection;
use super::{
    build_picker_state, build_routes, refresh_connection_models_for_etag, sync_connection_routes,
    sync_fitted_model_registry, sync_remote_routes,
};
use nuo_persistence::config::{
    Config, Credentials, FittedModelInfo, ModelListCacheState, RemoteCatalogCache,
};
use nuo_persistence::connections::{Connection, Connections};
use nuo_persistence::route_settings::RouteSettingsStore;
use nuo_provider::catalog::Transport;
use nuo_provider::{CredentialStore, TokenSet};
use nuo_provider::{DEEPSEEK_BUILTIN_MODELS, route_for_model};
use nuo_wire::{
    ConnectionAuth, ConnectionFilterPolicy, Effort, NamedFilterPolicy, OpenAiResponsesDialect,
    ReasoningMode, WireProtocol,
};

use std::sync::Mutex;

/// Tests that mutate process-wide env vars or the paths override must
/// serialize against each other so the parallel subagent never observes a
/// half-set environment or a foreign `Dirs`.
static ENV_GUARD: Mutex<()> = Mutex::new(());
static TEST_INIT: std::sync::Once = std::sync::Once::new();

fn ensure_test_init() {
    TEST_INIT.call_once(|| {
        nuo_server::provider_registry::init();
    });
}

/// Owned derivation inputs for a test.
///
/// A derivation takes one borrowed bundle. Tests vary one or two stores and keep
/// the rest at their defaults; this owns the bundle, and `inputs()` hands out the
/// borrow. Because a temporary in an expression lives to the end of its
/// statement, `&TestInputs::new(..).inputs()` is a valid call argument.
struct TestInputs {
    cache: RemoteCatalogCache,
    routes: RouteSettingsStore,
    creds: Credentials,
    providers: nuo_persistence::model_providers::ModelProviders,
    credentials: nuo_provider::CredentialHost,
}

impl TestInputs {
    fn new(cache: &RemoteCatalogCache, routes: &RouteSettingsStore, creds: &Credentials) -> Self {
        ensure_test_init();
        Self {
            cache: cache.clone(),
            routes: routes.clone(),
            creds: creds.clone(),
            providers: nuo_persistence::model_providers::ModelProviders::load(),
            credentials: nuo_provider::CredentialHost::file_backed(
                nuo_persistence::paths::get().auth_file(),
                nuo_persistence::paths::get().state_dir.join("machine_id"),
            ),
        }
    }

    fn inputs(&self) -> DerivationInputs<'_> {
        DerivationInputs {
            cache: &self.cache,
            routes: &self.routes,
            creds: &self.creds,
            providers: &self.providers,
            credentials: &self.credentials,
        }
    }
}

/// The sandboxed credential store a test writes through.
///
/// Reads the paths override, so a test that stores a token cannot touch a
/// developer's real `auth.toml`.
fn test_credential_store() -> std::sync::Arc<dyn CredentialStore> {
    std::sync::Arc::new(nuo_provider::FileCredentialStore::new(
        nuo_persistence::paths::get().auth_file(),
    ))
}

/// The host's provider declarations, as a value (ADR-0081 §1).
///
/// Leaked for the same reason `inputs_of` is: the call site stays an expression.
fn declarations() -> &'static nuo_persistence::model_providers::ModelProviders {
    ensure_test_init();
    Box::leak(Box::new(
        nuo_persistence::model_providers::ModelProviders::load(),
    ))
}

/// One-expression derivation inputs.
///
/// Leaks the owned bundle deliberately: a test bundle is tiny and the test
/// process is short-lived, and keeping the call site an expression means the
/// rewrite works in argument position, in a `let`, and inside an assertion
/// alike.
fn inputs_of(
    cache: &RemoteCatalogCache,
    routes: &RouteSettingsStore,
    creds: &Credentials,
) -> DerivationInputs<'static> {
    let owned: &'static TestInputs = Box::leak(Box::new(TestInputs::new(cache, routes, creds)));
    owned.inputs()
}

/// The common shape: defaults for everything except the cache.
#[allow(dead_code)]
fn inputs_with_cache(cache: &RemoteCatalogCache) -> DerivationInputs<'static> {
    // Leaked deliberately: a test bundle is tiny and the process is short-lived,
    // and this keeps every call site a single expression.
    let owned: &'static TestInputs = Box::leak(Box::new(TestInputs::new(
        cache,
        &RouteSettingsStore::default(),
        &Credentials::default(),
    )));
    owned.inputs()
}

/// Sandbox the process-wide XDG roots for the duration of a test. Tests that
/// write the instance store / credentials / catalog cache must bind the
/// returned guard (and its `TempDir`) to `_sandbox` for the whole body;
/// [`nuo_persistence::paths::TestDirsGuard`] holds the shared override lock and
/// clears the roots on drop.
fn sandboxed_paths() -> (nuo_persistence::paths::TestDirsGuard, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: None,
    };
    (nuo_persistence::paths::TestDirsGuard::install(dirs), tmp)
}

fn instance(name: &str, provider: Option<&str>) -> Connection {
    ensure_test_init();
    Connection {
        name: name.to_string(),
        provider: provider.unwrap_or("deepseek").to_string(),
        ..Default::default()
    }
}

fn register_mock_provider(
    id: &'static str,
    base_url: &str,
    protocol: WireProtocol,
    models: &'static [&'static str],
    catalog_protocol: nuo_provider::CatalogShape,
) {
    let mut store = nuo_persistence::model_providers::ModelProviders::load();
    store.set_provider(
        id,
        nuo_persistence::model_providers::UserDeclaredProvider {
            label: Some(id.to_string()),
            root_url: base_url.to_string(),
            default_protocol: Some(protocol),
            client_profile: None,
            user_agent: None,
            catalog: Some(nuo_provider::RemoteCatalogSource::Endpoint(
                catalog_protocol,
            )),
            dialect: None,
            protocol_roots: vec![],
            catalog_root_url: None,
            prompt_cache: None,
            client_profile_sensitive: false,
        },
    );
    store.get_or_create_mut(id).include = models
        .iter()
        .map(|id| nuo_wire::DeclaredModel {
            id: id.to_string(),
            ..Default::default()
        })
        .collect();
    nuo_persistence::model_providers::ModelProviders::save(&store).unwrap();
    nuo_provider::sync_user_declared_providers(
        &nuo_persistence::model_providers::ModelProviders::load(),
    )
    .unwrap();
}

#[test]
fn provider_dialect_is_inherited_independently_of_auth_and_remote_protocol() {
    use nuo_wire::{GoogleGenerateContentDialect, ProviderDialect};
    let _sandbox = sandboxed_paths();
    for auth in [
        ConnectionAuth::Static,
        ConnectionAuth::negotiated("google-antigravity"),
    ] {
        for remote_protocol in [None, Some(WireProtocol::GoogleGenerateContent)] {
            let mut conn = instance("dialect-inheritance", Some("google-antigravity"));
            conn.auth = auth.clone();
            // An unknown generation must inherit without a baseline entry.
            let model = "gemini-future-flash-tiered";
            let mut cache = RemoteCatalogCache::default();
            cache
                .remote_metadata
                .entry(conn.name.clone())
                .or_default()
                .insert(
                    model.into(),
                    nuo_wire::RemoteModelMetadata {
                        protocol: remote_protocol,
                        ..Default::default()
                    },
                );
            let channel = derive_channel(
                &conn,
                model,
                &inputs_of(
                    &cache,
                    &RouteSettingsStore::default(),
                    &Credentials::default(),
                ),
            )
            .unwrap();
            assert!(matches!(
                channel.transport,
                Transport::GoogleGenerateContent {
                    dialect: GoogleGenerateContentDialect::Antigravity,
                    ..
                }
            ));
            assert_eq!(channel.model, model);
        }
    }
    // Credential type cannot turn an ordinary Google service into Antigravity.
    let mut conn = instance("native-google", Some("google"));
    conn.auth = ConnectionAuth::negotiated("google-antigravity");
    let channel = derive_channel(
        &conn,
        "gemini-3.8-flash",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(matches!(
        channel.transport,
        Transport::GoogleGenerateContent {
            dialect: GoogleGenerateContentDialect::GenerativeLanguage,
            ..
        }
    ));
    assert_eq!(
        nuo_provider::model_provider_spec("google-antigravity")
            .unwrap()
            .dialect,
        ProviderDialect::Antigravity
    );
}

#[test]
fn provider_dialect_follows_model_protocol_across_service_families() {
    let _sandbox = sandboxed_paths();
    for (provider, wire, expected) in [
        ("github-copilot", WireProtocol::OpenAiResponses, "Copilot"),
        ("github-copilot", WireProtocol::AnthropicMessages, "Copilot"),
        (
            "github-copilot",
            WireProtocol::OpenAiChatCompletions,
            "Copilot",
        ),
        (
            "openai-subscription",
            WireProtocol::OpenAiResponses,
            "ChatGpt",
        ),
        ("qoder", WireProtocol::OpenAiChatCompletions, "Qoder"),
        ("deepseek", WireProtocol::OpenAiResponses, "DeepSeek"),
        (
            "openrouter",
            WireProtocol::OpenAiChatCompletions,
            "OpenRouter",
        ),
    ] {
        // Deliberately use API-key auth to prove dialect is service-owned.
        let conn = instance("dialect-family", Some(provider));
        let mut cache = RemoteCatalogCache::default();
        cache
            .remote_metadata
            .entry(conn.name.clone())
            .or_default()
            .insert(
                "remote-model".into(),
                nuo_wire::RemoteModelMetadata {
                    protocol: Some(wire),
                    ..Default::default()
                },
            );
        let channel = derive_channel(
            &conn,
            "remote-model",
            &inputs_of(
                &cache,
                &RouteSettingsStore::default(),
                &Credentials::default(),
            ),
        )
        .unwrap();
        let (dialect, endpoint) = match channel.transport {
            Transport::OpenAiChatCompletions {
                dialect, base_url, ..
            } => (format!("{dialect:?}"), base_url),
            Transport::OpenAiResponses {
                dialect, base_url, ..
            } => (format!("{dialect:?}"), base_url),
            Transport::AnthropicMessages {
                dialect, base_url, ..
            } => (format!("{dialect:?}"), base_url),
            other => panic!("unexpected transport {other:?}"),
        };
        assert_eq!(dialect, expected, "{provider}/{wire:?}");
        if provider == "github-copilot" {
            let suffix = match wire {
                WireProtocol::OpenAiResponses => "/responses",
                WireProtocol::AnthropicMessages => "/v1/messages",
                _ => "/chat/completions",
            };
            assert_eq!(endpoint, format!("https://api.githubcopilot.com{suffix}"));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn declared_antigravity_provider_sends_internal_requests_from_derived_channel() {
    use nuo_wire::{ModelRequest, ProviderDialect};
    let _sandbox = sandboxed_paths();
    let mut server = mockito::Server::new_async().await;
    let model = "gemini-3.8-flash";
    // Exercise persisted provider definition, OAuth resolution, catalog derivation,
    // factory and HTTP adapter together. No manually constructed Transport.
    let mut providers = nuo_persistence::model_providers::ModelProviders::default();
    providers.set_provider(
        "test-antigravity-wire",
        nuo_persistence::model_providers::UserDeclaredProvider {
            label: None,
            root_url: format!("{}/proxy/team", server.url()),
            default_protocol: Some(WireProtocol::GoogleGenerateContent),
            dialect: Some(ProviderDialect::Antigravity),
            client_profile: None,
            user_agent: None,
            catalog: Some(nuo_wire::RemoteCatalogSource::None),
            protocol_roots: vec![],
            catalog_root_url: None,
            prompt_cache: None,
            client_profile_sensitive: false,
        },
    );
    nuo_persistence::model_providers::ModelProviders::save(&providers).unwrap();
    nuo_provider::sync_user_declared_providers(
        &nuo_persistence::model_providers::ModelProviders::load(),
    )
    .unwrap();
    let mut conn = instance("test-agy-wire", Some("test-antigravity-wire"));
    conn.auth = ConnectionAuth::negotiated("google-antigravity");
    {
        let store = test_credential_store();
        let mut auth = store.lock().await.unwrap();
        let mut t = TokenSet {
            access: "test-token".into(),
            refresh: "test-refresh".into(),
            expires_ms: i64::MAX,
            id_token: None,
            token_type: Some("Bearer".into()),
            scope: None,
            user_email: None,
            attributes: serde_json::Map::new(),
        };
        t.set_attr("account_id", "test-project");
        t.set_attr("project_id", "test-project");
        auth.set(&conn.name, t);
        auth.commit().await.unwrap();
    }
    let channel = derive_channel(
        &conn,
        model,
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    let provider = nuo_provider::build_provider_for_channel(&channel, &conn.name, None)
        .expect("provider factory registered by the composition root");
    for streaming in [false, true] {
        let action = if streaming {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let path = format!("/proxy/team/v1internal:{action}");
        let mut mock = server
            .mock("POST", path.as_str())
            .match_header("authorization", "Bearer test-token")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "project": "test-project", "model": "gemini-3.8-flash-tiered", "request": {"contents": []}
            })));
        mock = if streaming {
            mock.match_query(mockito::Matcher::UrlEncoded("alt".into(), "sse".into()))
                .with_header("content-type", "text/event-stream")
                .with_body("data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}}\n\n")
        } else {
            mock.match_query(mockito::Matcher::Missing)
                .with_header("content-type", "application/json")
                .with_body(r#"{"response":{"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}}"#)
        };
        let mock = mock.create_async().await;
        if streaming {
            use futures::StreamExt;
            let mut stream = provider
                .stream_chat(ModelRequest::new(vec![]))
                .await
                .unwrap();
            let mut output = String::new();
            while let Some(chunk) = stream.next().await {
                output.push_str(&chunk.unwrap());
            }
            assert_eq!(output, "ok");
        } else {
            provider.chat(ModelRequest::new(vec![])).await.unwrap();
        }
        mock.assert_async().await;
    }
}

// derivation

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
            dialect: nuo_wire::OpenAiChatDialect::OpenRouter,
            ref base_url,
            ..
        } if base_url == "https://openrouter.ai/api/v1/chat/completions"
    ));
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
    assert!(matches!(
        channel_responses.transport,
        Transport::OpenAiResponses {
            ref base_url,
            dialect: nuo_wire::OpenAiResponsesDialect::DeepSeek,
            ..
        } if base_url == "https://api.commandcode.ai/provider/v1/responses"
    ));
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
        nuo_provider::CatalogShape::OpenAi,
    );
    let mut custom = instance("relay", Some("test-relay-declared"));
    custom.models.include = vec![
        nuo_wire::model::DeclaredModel {
            id: "a".to_string(),
            ..Default::default()
        },
        nuo_wire::model::DeclaredModel {
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
    use nuo_wire::{ConnectionFilterPolicy, NamedFilterPolicy};
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
fn catalog_cache_identity_tracks_client_emulation() {
    let cache = RemoteCatalogCache::default();
    let mut connection = instance("chatgpt", Some("openai-subscription"));
    let codex_identity = source_identity_for_connection(&connection, &cache).unwrap();

    connection.client_identity = nuo_wire::ClientProfile::custom(
        "codex_cli_rs/future",
        vec![(
            "openai-intent".to_string(),
            "conversation-edits".to_string(),
        )],
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
fn opencode_plan_routes_models_by_wire_format() {
    let go = instance("opencode-plan", Some("opencode-plan"));
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
        matches!(&glm.transport, Transport::OpenAiChatCompletions { base_url, client_profile, .. } if base_url == "https://opencode.ai/zen/go/v1/chat/completions" && *client_profile == nuo_wire::ClientProfile::OpenCode),
        "glm-5.2 must route to OpenAI chat-completions on zen/go relay with OpenCode client profile"
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
        route_for_model("opencode-plan", "qwen3.6-plus").map(|(p, b, _)| (p, b)),
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
        .effort = Some("max".to_string());
    routes
        .settings_for_mut("anthropic", "claude-opus-4-8")
        .thinking = Some(false);

    let anthropic = instance("anthropic", Some("anthropic"));
    let channel = derive_channel(
        &anthropic,
        "claude-opus-4-8",
        &inputs_of(&cache, &routes, &Credentials::default()),
    )
    .unwrap();
    match &channel.transport {
        Transport::AnthropicMessages {
            effort, thinking, ..
        } => {
            assert_eq!(*effort, Some(Effort::Max));
            assert_eq!(*thinking, Some(ReasoningMode::Off), "explicit off wins");
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
    // A sibling model with no entry stays at the opt-in default (off).
    let sonnet = derive_channel(
        &anthropic,
        "claude-sonnet-4-6",
        &inputs_of(&cache, &routes, &Credentials::default()),
    )
    .unwrap();
    match &sonnet.transport {
        Transport::AnthropicMessages {
            effort, thinking, ..
        } => {
            assert!(effort.is_none());
            assert!(thinking.is_none());
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
}

#[test]
fn copilot_route_uses_remote_endpoint_metadata() {
    let mut cache = RemoteCatalogCache::default();
    cache.remote_metadata.insert("copilot".to_string(), {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "gpt-5".to_string(),
            nuo_wire::RemoteModelMetadata {
                protocol: Some(WireProtocol::OpenAiResponses),
                ..Default::default()
            },
        );
        m
    });
    let copilot = Connection {
        name: "copilot".to_string(),
        provider: "github-copilot".to_string(),
        auth: nuo_wire::ConnectionAuth::negotiated("copilot"),
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
            nuo_wire::RemoteModelMetadata {
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
        nuo_provider::CatalogShape::OpenAi,
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

// picker

#[test]
fn build_picker_state_reflects_instances() {
    let _sandbox = sandboxed_paths();
    let instances = Connections {
        connections: vec![instance("deepseek", Some("deepseek"))],
    };
    instances.save().unwrap();
    let config = Config {
        default_connection: "deepseek".to_string(),
        default_model: Some("deepseek-flash".to_string()),
        ..Default::default()
    };
    let snapshot = build_picker_state(
        &config,
        &nuo_persistence::connection_usage::ConnectionUsage::default(),
    );
    assert_eq!(snapshot.default_id, "deepseek");
    let row = snapshot
        .rows
        .iter()
        .find(|r| r.id == "deepseek")
        .expect("deepseek row");
    assert_eq!(row.name, "deepseek");
    assert_eq!(row.provider, "deepseek");
    assert!(row.models.contains(&"deepseek-flash".to_string()));
}

#[test]
fn channel_model_info_effort_ladders_survive() {
    // A Gemini model advertises an effort ladder, so its picker row exposes an
    // effort defaulting to `high` (the ladder's top rung) when unset.
    let gemini37 = nuo_provider::catalog::Channel {
        id: "default".to_string(),
        transport: Transport::GoogleGenerateContent {
            base_url: "https://cloudcode-pa.googleapis.com".to_string(),
            client_profile: nuo_wire::ClientProfile::Antigravity,
            effort: None,
            dialect: Default::default(),
        },
        credentials: nuo_wire::static_credential(""),
        model: "gemini-3.7-flash".to_string(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_wire::PromptCachePreference::default(),
        prompt_cache: nuo_wire::PromptCacheCapabilities::unsupported(),
    };
    let info = channel_model_info(&gemini37);
    assert_eq!(info.protocol, WireProtocol::GoogleGenerateContent.as_str());
    assert_eq!(info.effort.as_deref(), Some("high"));
    assert_eq!(info.thinking, None);
}

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
        nuo_provider::CatalogShape::OpenAi,
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
        nuo_provider::CatalogShape::GoogleCloudCode,
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
        nuo_provider::CatalogShape::OpenAi,
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
        nuo_provider::CatalogShape::OpenAi,
    );
    register_mock_provider(
        unrel_id,
        &format!("{}/v1", unrelated_server.url()),
        WireProtocol::OpenAiResponses,
        &["deepseek-v4-pro"],
        nuo_provider::CatalogShape::OpenAi,
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
        nuo_provider::CatalogShape::OpenAi,
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
        nuo_provider::CatalogShape::OpenAi,
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
    connection.models.filter = Some(nuo_wire::ConnectionFilterPolicy::Named(
        nuo_wire::NamedFilterPolicy::All,
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
    let resolved = nuo_wire::model::resolve("kimi-for-coding");
    assert_eq!(resolved.context_window, 262_144);
    assert!(resolved.reasoning());
}

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
    conn.auth = nuo_wire::ConnectionAuth::negotiated("google-antigravity");
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
        nuo_wire::model::DeclaredModel {
            id: "model-a".to_string(),
            ..Default::default()
        },
        nuo_wire::model::DeclaredModel {
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
    conn1.models.include = vec![nuo_wire::model::DeclaredModel {
        id: "shared-model".to_string(),
        ..Default::default()
    }];

    let mut conn2 = instance("conn-2", Some("test-open-relay-recency"));
    conn2.models.include = vec![nuo_wire::model::DeclaredModel {
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
    ds_preset.include.push(nuo_wire::model::DeclaredModel {
        id: "deepseek-preset-preview".to_string(),
        ..Default::default()
    });
    // Preset excludes deepseek-chat
    ds_preset.exclude.push("deepseek-chat".to_string());

    let mut conn1 = instance("ds-work", Some("deepseek"));
    // Connection instance excludes deepseek-coder
    conn1.models.exclude.push("deepseek-coder".to_string());
    // Connection instance includes an instance-specific model
    conn1.models.include.push(nuo_wire::model::DeclaredModel {
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
                nuo_wire::RemoteModelMetadata {
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
            nuo_wire::RemoteModelMetadata {
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
            nuo_wire::RemoteModelMetadata {
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
        auth: nuo_wire::ConnectionAuth::negotiated("qoder"),
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
            nuo_wire::RemoteModelMetadata {
                availability: Some(nuo_wire::Availability::usable()),
                ..Default::default()
            },
        );
        m.insert(
            "gmodel".to_string(),
            nuo_wire::RemoteModelMetadata {
                availability: Some(nuo_wire::Availability::locked(None)),
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
        Some(nuo_wire::Availability::locked(None)),
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
    injected.models.include = vec![nuo_wire::DeclaredModel {
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
        Some(nuo_wire::Availability::usable()),
        "the effective verdict is usable after the override"
    );
}

#[test]
fn relay_surfaces_default_to_curated_noise_hygiene() {
    // ADR-0081 [INV-CATALOG-06]: a surface that publishes bare ids publishes a
    // noisy catalog too, so its default admission gate is `Curated`. The
    // predicate is the spec's declared display policy, never a hardcoded
    // provider-id list.
    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "my-go".to_string(),
        vec![
            "deepseek-v4.1-flash".to_string(),
            "glm-5.3".to_string(),
            "beam-free".to_string(),
            "hy4-preview".to_string(),
            "gemini-4-argon-contributor".to_string(),
            "omen-alpha".to_string(),
        ],
    );
    let connection = instance("my-go", Some("opencode-plan"));

    let models = route_models(&connection, &cache, declarations());
    assert!(models.contains(&"deepseek-v4.1-flash".to_string()));
    assert!(models.contains(&"glm-5.3".to_string()));
    assert!(
        !models.contains(&"beam-free".to_string()),
        "promotional tier pruned"
    );
    assert!(
        !models.contains(&"hy4-preview".to_string()),
        "preview snapshot pruned"
    );
    assert!(
        !models.contains(&"gemini-4-argon-contributor".to_string()),
        "contributor node pruned"
    );
    assert!(!models.contains(&"omen-alpha".to_string()), "alpha pruned");

    // Sovereign injection bypasses the hygiene filter unconditionally.
    let mut sovereign = connection.clone();
    sovereign.models.include = vec![nuo_persistence::connections::DeclaredModel {
        id: "beam-free".into(),
        ..Default::default()
    }];
    assert!(
        route_models(&sovereign, &cache, declarations()).contains(&"beam-free".to_string()),
        "an explicit include is never filtered"
    );

    // A curated native surface keeps its open admission: DeepSeek's own
    // `/models` is trustworthy and its ids are not relay noise.
    let mut native_cache = RemoteCatalogCache::default();
    native_cache.connection_models.insert(
        "my-deepseek".to_string(),
        vec!["deepseek-v4.1-flash".to_string()],
    );
    let native = instance("my-deepseek", Some("deepseek"));
    assert_eq!(
        route_models(&native, &native_cache, declarations()),
        vec!["deepseek-v4.1-flash"]
    );
}

#[test]
fn relay_labels_and_seeds_survive_the_hygiene_valve() {
    ensure_test_init();
    // An ids-only relay advertises no metadata at all, so the structural label
    // must be produced from the id itself (ADR-0081 [INV-CATALOG-05]) — gating
    // the mapping on an existing remote record is how it silently never runs.
    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "my-go".to_string(),
        vec!["deepseek-v4.1-flash".to_string(), "beam-free".to_string()],
    );
    let connection = instance("my-go", Some("opencode-plan"));
    let inputs = inputs_with_cache(&cache);

    let ch = derive_channel(&connection, "deepseek-v4.1-flash", &inputs).expect("channel");
    assert_eq!(
        ch.remote.as_ref().and_then(|r| r.name.as_deref()),
        Some("DeepSeek V4.1 Flash"),
        "a bare relay id is lifted into typography"
    );
    assert_eq!(
        channel_model_info(&ch).name.as_deref(),
        Some("DeepSeek V4.1 Flash"),
        "the picker snapshot carries the mapped label"
    );
    assert_eq!(
        ch.model, "deepseek-v4.1-flash",
        "identity stays the wire id"
    );

    // A promotional id discovered from the relay is pruned from the listing.
    let models = route_models(&connection, &cache, declarations());
    assert!(models.contains(&"deepseek-v4.1-flash".to_string()));
    assert!(!models.contains(&"beam-free".to_string()));

    // ...but an id the surface's own baseline curates is never filtered by that
    // same valve: `opencode-plan` seeds `*-free` tiers deliberately.
    let mut seeded_cache = RemoteCatalogCache::default();
    seeded_cache.connection_models.insert(
        "my-go".to_string(),
        vec![
            "deepseek-v4.1-flash".to_string(),
            "deepseek-v4.1-flash-free".to_string(),
            "glm-5.3-flash-free".to_string(),
        ],
    );
    let seeded = route_models(&connection, &seeded_cache, declarations());
    assert!(
        seeded.contains(&"deepseek-v4.1-flash-free".to_string()),
        "a seeded free tier survives its own hygiene valve: {seeded:?}"
    );
    assert!(
        seeded.contains(&"glm-5.3-flash-free".to_string()),
        "compiled baseline and surface must agree: {seeded:?}"
    );
}
