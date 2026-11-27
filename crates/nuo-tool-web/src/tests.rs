use super::*;
use crate::html::extract_html_title;
use nuo_tool::Tool;
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
    use std::future::Future;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    use crate::client::guarded_get;
    use crate::http::GuardedWebHttp;
    use crate::ssrf::SsrfGuardedResolver;

    /// Test resolver that allows ONLY the ephemeral local fixture server's
    /// exact `SocketAddr` while delegating every other host/port (including
    /// every redirect hop) to the production [`SsrfGuardedResolver`].
    #[derive(Debug)]
    struct AllowFixtureResolver {
        allowed: SocketAddr,
    }

    impl netune::DnsResolver for AllowFixtureResolver {
        fn resolve<'a>(
            &'a self,
            host: &'a str,
            port: u16,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, netune::NetError>> + Send + 'a>>
        {
            Box::pin(async move {
                if let Ok(ip) = host.parse::<std::net::IpAddr>()
                    && SocketAddr::new(ip, port) == self.allowed
                {
                    return Ok(vec![self.allowed]);
                }
                SsrfGuardedResolver.resolve(host, port).await
            })
        }
    }

    async fn redirect_server(target: &'static str) -> (String, SocketAddr, Arc<AtomicBool>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Arc::new(AtomicBool::new(false));
        let served_flag = Arc::clone(&served);
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut socket, _)) = listener.accept().await {
                served_flag.store(true, Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{addr}/hop"), addr, served)
    }

    fn test_guarded_client() -> GuardedWebHttp {
        GuardedWebHttp::new(&nuo_protocol::WebConfig::default())
    }

    fn fixture_guarded_client(allowed: SocketAddr) -> GuardedWebHttp {
        GuardedWebHttp::with_resolver_for_test(
            AllowFixtureResolver { allowed },
            Duration::from_secs(5),
        )
    }

    #[tokio::test]
    async fn redirect_to_metadata_endpoint_is_refused() {
        let (url, addr, served) = redirect_server("http://169.254.169.254/latest/meta-data/").await;
        let err = guarded_get(&fixture_guarded_client(addr), &url, Default::default())
            .await
            .expect_err("redirect into the metadata endpoint must be refused");
        assert!(
            served.load(Ordering::SeqCst),
            "hop 0 must actually reach the redirect server so hop 1 is exercised"
        );
        assert!(
            err.contains("SSRF guard") && err.contains("169.254.169.254"),
            "expected an SSRF-guard error for the metadata hop, got: {err}"
        );
    }

    #[tokio::test]
    async fn redirect_to_loopback_is_refused() {
        let (url, addr, served) = redirect_server("http://127.0.0.1:9/secret").await;
        let err = guarded_get(&fixture_guarded_client(addr), &url, Default::default())
            .await
            .expect_err("redirect into loopback must be refused");
        assert!(
            served.load(Ordering::SeqCst),
            "hop 0 must actually reach the redirect server so hop 1 is exercised"
        );
        assert!(
            err.contains("SSRF guard") && err.contains("127.0.0.1"),
            "expected an SSRF-guard error for the loopback hop, got: {err}"
        );
    }

    #[tokio::test]
    async fn redirect_to_fake_ip_literal_is_refused() {
        let (url, addr, served) = redirect_server("http://198.18.0.203/secret").await;
        let err = guarded_get(&fixture_guarded_client(addr), &url, Default::default())
            .await
            .expect_err("redirect into a literal FakeIP address must be refused");
        assert!(
            served.load(Ordering::SeqCst),
            "hop 0 must actually reach the redirect server so hop 1 is exercised"
        );
        assert!(
            err.contains("SSRF guard") && err.contains("198.18.0.203"),
            "expected an SSRF-guard error for the FakeIP literal hop, got: {err}"
        );
    }

    #[tokio::test]
    async fn legitimate_redirect_is_followed_end_to_end() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // Hop 0: 302 redirect to /final on the same allowed fixture origin.
            let (mut s1, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = s1.read(&mut buf).await;
            let _ = s1
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            // Hop 1: 200 OK body.
            let (mut s2, _) = listener.accept().await.unwrap();
            let _ = s2.read(&mut buf).await;
            let _ = s2
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\nredirected-ok",
                )
                .await;
        });

        let response = guarded_get(
            &fixture_guarded_client(addr),
            &format!("http://{addr}/start"),
            Default::default(),
        )
        .await
        .expect("legitimate redirect must succeed");
        server.await.unwrap();

        assert_eq!(response.body, b"redirected-ok");
    }

    #[tokio::test]
    async fn direct_private_url_is_refused_before_any_connection() {
        let err = guarded_get(
            &test_guarded_client(),
            "http://10.255.255.1/x",
            Default::default(),
        )
        .await
        .expect_err("private IP must be refused by the resolver");
        assert!(err.contains("SSRF guard"), "got: {err}");
    }

    #[tokio::test]
    async fn direct_fake_ip_literal_is_refused_before_any_connection() {
        let err = guarded_get(
            &test_guarded_client(),
            "http://198.18.0.203/x",
            Default::default(),
        )
        .await
        .expect_err("literal FakeIP must be refused by the resolver");
        assert!(
            err.contains("SSRF guard") && err.contains("198.18.0.203"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn searxng_localhost_endpoint_succeeds_through_web_search_tool() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let body = r#"{"results":[{"title":"ShellCheck","url":"https://github.com/koalaman/shellcheck","content":"Static analysis tool for shell scripts"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let config = nuo_protocol::WebRuntimeConfig {
            behavior: nuo_protocol::WebConfig {
                provider: nuo_protocol::WebSearchProvider::Searxng,
                searxng_url: Some(format!("http://{addr}/search")),
                ..nuo_protocol::WebConfig::default()
            },
            ..nuo_protocol::WebRuntimeConfig::default()
        };
        let tool = crate::WebSearchTool::with_config(config);
        let (provider, client) = tool.current_provider();
        let output = provider
            .search(&client, "shellcheck")
            .await
            .expect("self-hosted localhost SearXNG must succeed");
        server.await.unwrap();

        let crate::search::ProviderOutput::Results(hits) = output else {
            panic!("expected structured SearXNG hits");
        };
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "ShellCheck");
        assert_eq!(hits[0].url, "https://github.com/koalaman/shellcheck");
    }
}

mod shared_config_tests {
    use super::*;
    use nuo_protocol::{
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
        let (primary, _) = tool.current_provider();
        assert_eq!(primary.name(), "Exa");

        let mut next = runtime(WebConfig {
            provider: WebSearchProvider::Tavily,
            ..WebConfig::default()
        });
        next.search_credential = Some(SecretString::new("tvly-x"));
        shared.replace(next);

        let (primary, _) = tool.current_provider();
        assert_eq!(primary.name(), "Tavily");

        let (again, _) = tool.current_provider();
        assert_eq!(again.name(), "Tavily");
    }

    #[test]
    fn webreader_client_cache_rebuilds_on_revision_change() {
        let shared = SharedWebConfig::new(WebRuntimeConfig::default());
        let reader = WebReaderTool::with_shared_config(shared.clone());
        let first = reader.client();
        let first_guarded = reader.guarded_client();

        shared.replace(shared.get());
        let second = reader.client();
        let second_guarded = reader.guarded_client();

        assert!(
            !std::sync::Arc::ptr_eq(&first, &second),
            "a new authoritative revision must invalidate the reader client cache"
        );
        assert!(
            !std::sync::Arc::ptr_eq(&first_guarded, &second_guarded),
            "a new authoritative revision must invalidate the guarded client cache"
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
