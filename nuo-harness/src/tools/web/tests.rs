use super::*;
use crate::tools::web::html::extract_html_title;
use nuo_contracts::Tool;
use sha2::{Digest, Sha256};

#[test]
fn html_title_is_normalized_for_watch_summaries() {
    let html = "<html><head><title>  Market &amp; Risk </title></head><body>x</body></html>";
    assert_eq!(extract_html_title(html), "Market & Risk");
    assert_eq!(extract_html_title("<html>untitled</html>"), "");
}

#[test]
fn snapshot_shape_round_trips_through_json() {
    let snapshot = WebPageSnapshot {
        requested_url: "https://example.com/a".to_string(),
        final_url: "https://example.com/a".to_string(),
        title: "A".to_string(),
        text_preview: "preview".to_string(),
        content_hash: format!("{:x}", Sha256::digest(b"body")),
        content_type: "text/html".to_string(),
        etag: Some("v1".to_string()),
        last_modified: None,
        body_bytes: 4,
        checked_at_ms: 1,
    };
    let encoded = serde_json::to_string(&snapshot).expect("snapshot JSON");
    let decoded: WebPageSnapshot = serde_json::from_str(&encoded).expect("snapshot round trip");
    assert_eq!(decoded, snapshot);
}

mod guarded_get_tests {
    use crate::tools::web::client::guarded_get;

    async fn redirect_server(target: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        format!("http://{addr}/hop")
    }

    fn test_client() -> crate::tools::web::http::WebHttp {
        crate::tools::web::http::WebHttp::new(&nuo_contracts::WebConfig::default())
            .expect("test client")
    }

    #[tokio::test]
    async fn redirect_to_metadata_endpoint_is_refused() {
        let url = redirect_server("http://169.254.169.254/latest/meta-data/").await;
        let err = guarded_get(&test_client(), &url, Default::default())
            .await
            .expect_err("redirect into the metadata endpoint must be refused");
        assert!(
            err.contains("SSRF guard"),
            "expected an SSRF-guard error, got: {err}"
        );
    }

    #[tokio::test]
    async fn redirect_to_loopback_is_refused() {
        let url = redirect_server("http://127.0.0.1:9/secret").await;
        let err = guarded_get(&test_client(), &url, Default::default())
            .await
            .expect_err("redirect into loopback must be refused");
        assert!(
            err.contains("SSRF guard"),
            "expected an SSRF-guard error, got: {err}"
        );
    }

    #[tokio::test]
    async fn direct_private_url_is_refused_before_any_connection() {
        let err = guarded_get(&test_client(), "http://10.255.255.1/x", Default::default())
            .await
            .expect_err("private IP must be refused by the pre-flight");
        assert!(err.contains("SSRF guard"));
    }
}

mod shared_config_tests {
    use super::*;
    use nuo_contracts::{
        SecretString, SharedWebConfig, WebConfig, WebReaderProvider, WebRuntimeConfig,
        WebSearchProvider,
    };

    fn runtime(behavior: WebConfig) -> WebRuntimeConfig {
        WebRuntimeConfig {
            behavior,
            search_credential: None,
            reader_credential: None,
        }
    }

    #[test]
    fn websearch_chain_rebuilds_when_shared_config_changes() {
        let shared = SharedWebConfig::new(WebRuntimeConfig::default());
        let tool = WebSearchTool::with_shared_config(shared.clone());
        let (primary, _) = tool.current_provider().expect("default provider builds");
        assert_eq!(primary.name(), "Exa");

        let mut next = runtime(WebConfig {
            provider: WebSearchProvider::Tavily,
            ..WebConfig::default()
        });
        next.search_credential = Some(SecretString::new("tvly-x"));
        shared.replace(next);

        let (primary, _) = tool.current_provider().expect("rebuilt provider builds");
        assert_eq!(primary.name(), "Tavily");

        let (again, _) = tool.current_provider().expect("cached provider builds");
        assert_eq!(again.name(), "Tavily");
    }

    #[test]
    fn webreader_client_cache_rebuilds_on_revision_change() {
        let shared = SharedWebConfig::new(WebRuntimeConfig::default());
        let reader = WebReaderTool::with_shared_config(shared.clone());
        let first = reader.client().expect("initial client builds");

        shared.replace(shared.get());
        let second = reader.client().expect("replacement client builds");

        assert!(
            !std::sync::Arc::ptr_eq(&first, &second),
            "a new authoritative revision must invalidate the reader client cache"
        );
    }

    #[test]
    fn runtime_debug_hides_secrets_and_revision_is_the_cache_identity() {
        let mut with_secret = runtime(WebConfig::default());
        with_secret.search_credential = Some(SecretString::new("sk-secret-value"));
        assert!(!format!("{with_secret:?}").contains("sk-secret-value"));

        let shared = SharedWebConfig::new(WebRuntimeConfig::default());
        assert_eq!(shared.snapshot().0, 0);
        assert_eq!(shared.replace(with_secret), 1);
        assert_eq!(shared.snapshot().0, 1);
    }

    #[test]
    fn websearch_and_webreader_is_available_reflects_configuration() {
        let shared = SharedWebConfig::new(WebRuntimeConfig::default());
        let search = WebSearchTool::with_shared_config(shared.clone());
        let reader = WebReaderTool::with_shared_config(shared.clone());

        // Default search (exa) is available; the default reader is disabled.
        assert!(search.is_available());
        assert!(!reader.is_available());

        // Selecting Jina makes the reader available.
        shared.replace(runtime(WebConfig {
            reader: WebReaderProvider::Jina,
            ..WebConfig::default()
        }));
        assert!(reader.is_available());

        // Disabling search leaves the reader intact.
        shared.replace(runtime(WebConfig {
            provider: WebSearchProvider::Disabled,
            reader: WebReaderProvider::Jina,
            ..WebConfig::default()
        }));
        assert!(!search.is_available());
        assert!(reader.is_available());

        // A credentialed provider without a credential is unavailable.
        shared.replace(runtime(WebConfig {
            provider: WebSearchProvider::Tavily,
            ..WebConfig::default()
        }));
        assert!(!search.is_available());
        let mut with_key = runtime(WebConfig {
            provider: WebSearchProvider::Tavily,
            ..WebConfig::default()
        });
        with_key.search_credential = Some(SecretString::new("tvly-xxx"));
        shared.replace(with_key);
        assert!(search.is_available());

        // SearXNG needs an endpoint.
        shared.replace(runtime(WebConfig {
            provider: WebSearchProvider::Searxng,
            searxng_url: None,
            ..WebConfig::default()
        }));
        assert!(!search.is_available());
        shared.replace(runtime(WebConfig {
            provider: WebSearchProvider::Searxng,
            searxng_url: Some("http://localhost:8080".to_string()),
            ..WebConfig::default()
        }));
        assert!(search.is_available());

        // Disabling the reader leaves search intact.
        shared.replace(runtime(WebConfig {
            provider: WebSearchProvider::Searxng,
            reader: WebReaderProvider::Disabled,
            searxng_url: Some("http://localhost:8080".to_string()),
            ..WebConfig::default()
        }));
        assert!(search.is_available());
        assert!(!reader.is_available());
    }
}
