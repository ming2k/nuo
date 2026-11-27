//! Route derivation from presets, declared models, and live catalogs.

use super::*;

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
                nuo_protocol::RemoteModelMetadata {
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
    use nuo_protocol::{ModelRequest, ProviderDialect};
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
            user_agent: None,
            headers: None,
            catalog: Some(nuo_protocol::RemoteCatalogSource::None),
            protocol_roots: vec![],
            catalog_root_url: None,
            prompt_cache: None,
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
                .stream_chat(
                    &ModelRequest::new(vec![]),
                    nuo_protocol::TransportTelemetry::default(),
                )
                .await
                .unwrap();
            let mut output = String::new();
            while let Some(chunk) = stream.next().await {
                output.push_str(&chunk.unwrap());
            }
            assert_eq!(output, "ok");
        } else {
            provider.chat(&ModelRequest::new(vec![])).await.unwrap();
        }
        mock.assert_async().await;
    }
}

// derivation
