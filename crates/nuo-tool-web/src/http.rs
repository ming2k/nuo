//! The web tools' HTTP handles, on the owned transport.
//!
//! Two distinct client types mirror the two real trust boundaries:
//! - [`WebHttp`]: unrestricted outbound transport for configured service
//!   endpoints (search backends, Jina Reader, self-hosted localhost/LAN
//!   SearXNG, and hosted APIs behind transparent FakeIP proxies).
//! - [`GuardedWebHttp`]: SSRF-guarded outbound transport for untrusted,
//!   model-supplied URLs ([`crate::client::guarded_get`] / `take_snapshot`),
//!   enforcing [`crate::ssrf::SsrfGuardedResolver`] on every dial and every
//!   redirect hop without DNS-rebinding TOCTOU.
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use netune::{Client, ClientConfig, RequestHead, Response, Target};
use nuo_protocol::WebConfig;

/// A response whose body has been read.
#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// Failure returned by [`WebHttp::open_until_classified`] with explicit
/// transient-retry eligibility derived from `netune` (ADR-0224).
#[derive(Debug)]
pub(crate) struct OpenError {
    pub message: String,
    pub retryable: bool,
}

/// Outbound HTTP client for configured web service endpoints.
///
/// The transport itself stays unrestricted — matching the provider, MCP, and
/// remote-skill clients — so configured backends (hosted APIs behind FakeIP
/// transparent proxies as well as self-hosted endpoints such as localhost
/// SearXNG) work without false blocks. Untrusted model-supplied URLs must use
/// [`GuardedWebHttp`] instead.
pub struct WebHttp {
    client: Box<Client>,
    timeout: Duration,
}

/// SSRF-guarded outbound HTTP client for untrusted, model-supplied URLs.
///
/// Installs [`crate::ssrf::SsrfGuardedResolver`] directly on the `netune`
/// client so every initial dial and every redirect hop validates the exact
/// resolved socket addresses immediately before TCP connect (eliminating
/// redirect-hop bypasses and DNS-rebinding TOCTOU).
pub struct GuardedWebHttp {
    inner: WebHttp,
}

impl GuardedWebHttp {
    /// Build an SSRF-guarded handle from resolved `[web]` behavior.
    pub fn new(config: &WebConfig) -> Self {
        let timeout = Duration::from_secs(config.timeout_secs.max(1));
        Self::with_resolver(crate::ssrf::SsrfGuardedResolver, timeout)
    }

    fn with_resolver<R: netune::DnsResolver + 'static>(resolver: R, timeout: Duration) -> Self {
        let client_config = ClientConfig::default()
            .with_user_agent(crate::client::MOZILLA_UA)
            .with_max_redirects(crate::client::MAX_REDIRECTS);
        let socket = netune::SocketConfig::default();
        #[cfg(unix)]
        let socket = socket.with_keepalive(Duration::from_secs(30));
        let client = Client::builder()
            .config(client_config)
            .socket_config(socket)
            .dns_resolver(resolver)
            .build();
        Self {
            inner: WebHttp {
                client: Box::new(client),
                timeout,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn with_resolver_for_test<R: netune::DnsResolver + 'static>(
        resolver: R,
        timeout: Duration,
    ) -> Self {
        Self::with_resolver(resolver, timeout)
    }

    pub(crate) fn clear_pool(&self) {
        self.inner.clear_pool();
    }

    pub(crate) async fn open_until_classified(
        &self,
        request: WebRequest,
        deadline: tokio::time::Instant,
    ) -> Result<Response, OpenError> {
        self.inner.open_until_classified(request, deadline).await
    }

    pub fn default_timeout(&self) -> Duration {
        self.inner.default_timeout()
    }
}

impl WebHttp {
    /// Build the handle from resolved `[web]` behavior.
    pub fn new(config: &WebConfig) -> Self {
        let timeout = Duration::from_secs(config.timeout_secs.max(1));
        Self::build(timeout)
    }

    fn build(timeout: Duration) -> Self {
        let client_config = ClientConfig::default()
            .with_user_agent(crate::client::MOZILLA_UA)
            .with_max_redirects(crate::client::MAX_REDIRECTS);
        let socket = netune::SocketConfig::default();
        #[cfg(unix)]
        let socket = socket.with_keepalive(Duration::from_secs(30));
        let client = Client::builder()
            .config(client_config)
            .socket_config(socket)
            .build();
        Self {
            client: Box::new(client),
            timeout,
        }
    }

    /// Evict pooled sockets before a retry backoff so a fresh attempt dials a
    /// clean connection instead of inheriting a stranded keep-alive socket.
    pub(crate) fn clear_pool(&self) {
        self.client.clear_pool();
    }

    #[cfg(test)]
    fn with_timeout_for_test(timeout: Duration) -> Self {
        Self::build(timeout)
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Open a request bounded by `deadline`, retaining whether the transport
    /// failure is eligible for a transient retry (excludes invalid URLs, TLS
    /// certificate errors, and offline unreachability).
    pub(crate) async fn open_until_classified(
        &self,
        request: WebRequest,
        deadline: tokio::time::Instant,
    ) -> Result<Response, OpenError> {
        let (target, path) = Target::from_url(&request.url).map_err(|error| OpenError {
            message: format!("Invalid url: {error}"),
            retryable: false,
        })?;
        let mut head = RequestHead::new(request.method, path);
        for (name, value) in request.headers.iter() {
            if let Ok(value) = value.to_str() {
                head = head.with_header(name.as_str(), value);
            }
        }
        let send = async {
            self.client
                .request(&target, head, request.body)
                .await
                .map_err(|error| {
                    let retryable = error.error().is_retryable() && !error.error().is_unreachable();
                    // Keep netune's stable classification and the trace id the
                    // error belongs to; the enclosing timeout only knows that
                    // the wall clock expired.
                    OpenError {
                        message: format!(
                            "{} [{} trace {}]",
                            error,
                            error.diagnostic().code,
                            error.trace().meta.id
                        ),
                        retryable,
                    }
                })
        };
        tokio::time::timeout_at(deadline, send)
            .await
            .map_err(|_| OpenError {
                message: format!("Request to {} timed out", request.url),
                retryable: true,
            })?
    }

    /// Open a request and hand back the streaming response, bounded by `deadline`.
    pub async fn open_until(
        &self,
        request: WebRequest,
        deadline: tokio::time::Instant,
    ) -> Result<Response, String> {
        self.open_until_classified(request, deadline)
            .await
            .map_err(|err| err.message)
    }

    /// Open a request and hand back the streaming response.
    ///
    /// `timeout` bounds connect + head: callers that stream the body themselves
    /// should pass a shared [`tokio::time::Instant`] to [`Self::open_until`] so
    /// head and body share one deadline (ADR-0224).
    pub async fn open(&self, request: WebRequest, timeout: Duration) -> Result<Response, String> {
        self.open_until(request, tokio::time::Instant::now() + timeout)
            .await
    }

    /// GET a URL and read the whole body as text.
    pub async fn get(&self, url: &str, headers: HeaderMap) -> Result<Reply, String> {
        self.read(WebRequest::get(url).headers(headers).timeout(self.timeout))
            .await
    }

    /// POST a JSON body and read the whole body as text.
    pub async fn post_json(
        &self,
        url: &str,
        headers: HeaderMap,
        body: &serde_json::Value,
    ) -> Result<Reply, String> {
        self.read(WebRequest::post_json(url, headers, body).timeout(self.timeout))
            .await
    }

    /// POST a form body and read the whole body as text.
    pub async fn post_form(
        &self,
        url: &str,
        headers: HeaderMap,
        form: &[(&str, &str)],
    ) -> Result<Reply, String> {
        self.read(WebRequest::post_form(url, headers, form).timeout(self.timeout))
            .await
    }

    async fn read(&self, request: WebRequest) -> Result<Reply, String> {
        let url = request.url.clone();
        let deadline = tokio::time::Instant::now() + request.timeout;
        let mut response = self.open_until(request, deadline).await?;
        let status = response.head.status;
        let headers = response.head.headers.clone();
        let mut body = Vec::new();
        loop {
            let chunk = tokio::time::timeout_at(deadline, response.body.next_chunk())
                .await
                .map_err(|_| format!("Request to {url} timed out while reading body"))?
                .map_err(|error| {
                    format!("Failed to read body: {error} [{}]", error.diagnostic().code)
                })?;
            let Some(chunk) = chunk else {
                break;
            };
            body.extend_from_slice(&chunk);
        }
        Ok(Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }

    /// Convenience for the deadline the web tools use.
    pub fn default_timeout(&self) -> Duration {
        self.timeout()
    }
}

/// A web-tool request under construction.
pub struct WebRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    pub timeout: Duration,
}

impl WebRequest {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: None,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::GET, url)
    }

    pub fn headers(mut self, headers: HeaderMap) -> Self {
        for (name, value) in headers.iter() {
            self.headers.insert(name.clone(), value.clone());
        }
        self
    }

    pub fn post_json(url: impl Into<String>, headers: HeaderMap, body: &serde_json::Value) -> Self {
        let mut request = Self::new(Method::POST, url).headers(headers);
        request.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        request.body = serde_json::to_vec(body).ok().map(Bytes::from);
        request
    }

    pub fn post_form(url: impl Into<String>, headers: HeaderMap, form: &[(&str, &str)]) -> Self {
        let mut request = Self::new(Method::POST, url).headers(headers);
        request.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        let mut encoded = String::new();
        for (index, (name, value)) in form.iter().enumerate() {
            if index > 0 {
                encoded.push('&');
            }
            encoded.push_str(&percent_encode(name));
            encoded.push('=');
            encoded.push_str(&percent_encode(value));
        }
        request.body = Some(Bytes::from(encoded));
        request
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Append percent-encoded query parameters to a URL.
pub fn with_query(url: &str, params: &[(&str, &str)]) -> String {
    let mut out = String::from(url);
    out.push(if out.contains('?') { '&' } else { '?' });
    for (index, (name, value)) in params.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(name));
        out.push('=');
        out.push_str(&percent_encode(value));
    }
    out
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parameters_are_encoded() {
        assert_eq!(
            with_query(
                "https://example.invalid/s",
                &[("q", "a b+c"), ("kl", "us-en")]
            ),
            "https://example.invalid/s?q=a+b%2Bc&kl=us-en"
        );
        assert_eq!(
            with_query("https://example.invalid/s?x=1", &[("q", "z")]),
            "https://example.invalid/s?x=1&q=z"
        );
    }

    #[test]
    fn a_form_request_sets_its_content_type_and_body() {
        let request = WebRequest::post_form(
            "https://example.invalid/s",
            HeaderMap::new(),
            &[("q", "hello"), ("kl", "us-en")],
        );
        assert_eq!(
            request.headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/x-www-form-urlencoded"
        );
        assert_eq!(&request.body.unwrap()[..], b"q=hello&kl=us-en");
    }

    #[test]
    fn web_http_honors_configured_timeout_secs() {
        let cfg = WebConfig {
            timeout_secs: 17,
            ..WebConfig::default()
        };
        let http = WebHttp::new(&cfg);
        assert_eq!(http.default_timeout(), Duration::from_secs(17));
    }

    #[tokio::test]
    async fn web_http_read_enforces_deadline_across_stalled_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial")
                .await;
            std::future::pending::<()>().await;
        });

        let http = WebHttp::with_timeout_for_test(Duration::from_millis(120));
        let started = tokio::time::Instant::now();
        let err = http
            .get(&format!("http://{addr}/slow"), HeaderMap::new())
            .await
            .expect_err("stalled body must expire at the shared request deadline");
        server.abort();

        assert!(err.contains("timed out while reading body"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must not fall back to netune's 60s body idle timeout, elapsed={:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn web_http_reaches_configured_localhost_endpoint() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        });

        let http = WebHttp::new(&WebConfig::default());
        let reply = http
            .get(&format!("http://{addr}/search"), HeaderMap::new())
            .await
            .expect("configured local backend must not be blocked by egress confinement");
        server.await.expect("server task");

        assert!(reply.is_success());
        assert_eq!(reply.body, "ok");
    }

    #[tokio::test]
    async fn open_until_classified_marks_invalid_url_non_retryable() {
        let http = WebHttp::with_timeout_for_test(Duration::from_secs(5));
        let err = http
            .open_until_classified(
                WebRequest::get("not-a-valid-url"),
                tokio::time::Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect_err("invalid url must fail");
        assert!(!err.retryable, "invalid URL must never trigger a retry");
    }
}
