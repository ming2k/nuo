//! The provider-side HTTP client, on the owned transport (ADR-0200).
//!
//! OAuth, usage, and endpoint catalog use this bounded client.
//! Inference and provider services share direct transport construction and
//! platform trust; streaming inference owns its separate deadline policy.
//!
//! The surface is deliberately small and *bounded*: a request has an overall
//! deadline (these are all short control-plane calls), the body is returned as
//! text, and content-encoding is already decoded by the transport.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{HeaderMap, Method, StatusCode};
use netune::{Client, ClientConfig, Target, TcpConnector, TlsConnector};

/// Request body shapes these call sites use.
#[derive(Debug, Clone)]
pub enum Body {
    Json(serde_json::Value),
    Form(Vec<(String, String)>),
    /// An already-encoded body (the OAuth module's form serializer predates
    /// this helper and is exercised by its own tests).
    Raw(String),
}

/// A control-plane request.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Body>,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn json(mut self, value: &serde_json::Value) -> Self {
        self.body = Some(Body::Json(value.clone()));
        self
    }

    pub fn form(mut self, form: Vec<(String, String)>) -> Self {
        self.body = Some(Body::Form(form));
        self
    }

    pub fn raw_body(mut self, body: String) -> Self {
        self.body = Some(Body::Raw(body));
        self
    }
}

/// A control-plane reply with the body already read.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// Parse the body as JSON, with a diagnostic that names the endpoint.
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(&self.body)
            .map_err(|error| format!("invalid JSON response: {error} (body: {})", self.body))
    }
}

/// A pooled HTTP handle over the owned transport.
#[derive(Clone)]
pub struct Http {
    client: Arc<Client<TlsConnector<TcpConnector>>>,
    timeout: Duration,
}

static SHARED_CONTROL_PLANE: std::sync::OnceLock<Http> = std::sync::OnceLock::new();

impl Http {
    /// Build a handle with an overall per-request deadline.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        Ok(Self {
            client: Arc::new(crate::network::direct_client(
                ClientConfig::default(),
            )?),
            timeout,
        })
    }

    /// Build a handle with this crate's control-plane deadline (10 s).
    pub fn control_plane() -> Result<Self, String> {
        Self::new(Duration::from_secs(10))
    }

    /// Return a shared control-plane client instance (10 s deadline) reusing
    /// connection pools and TLS sessions across control-plane calls.
    pub fn shared_control_plane() -> Result<Self, String> {
        if let Some(client) = SHARED_CONTROL_PLANE.get() {
            return Ok(client.clone());
        }
        let client = Self::control_plane()?;
        let _ = SHARED_CONTROL_PLANE.set(client.clone());
        Ok(client)
    }

    pub async fn send(&self, request: Request) -> Result<Reply, String> {
        let (target, path) = Target::from_url(&request.url).map_err(|error| error.to_string())?;
        let mut head = netune::RequestHead::new(request.method.clone(), path);
        let mut content_type = None;
        for (name, value) in &request.headers {
            if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.clone());
            }
            head = head.with_header(name.as_str(), value.as_str());
        }
        let body = match &request.body {
            None => None,
            Some(Body::Json(value)) => {
                if content_type.is_none() {
                    head = head.with_header("content-type", "application/json");
                }
                Some(
                    serde_json::to_vec(value)
                        .map_err(|error| format!("could not encode request: {error}"))?
                        .into(),
                )
            }
            Some(Body::Form(fields)) => {
                if content_type.is_none() {
                    head = head.with_header("content-type", "application/x-www-form-urlencoded");
                }
                Some(form_encode(fields).into())
            }
            Some(Body::Raw(body)) => Some(body.clone().into()),
        };

        let recorder = Arc::new(Mutex::new(netune_trace::Recorder::start(256)));
        let exchange = async {
            let mut response = self
                .client
                .send(&target, Arc::clone(&recorder), head, body)
                .await?;
            let status = response.head.status;
            let headers = response.head.headers.clone();
            let bytes = response.body.read_to_end().await?;
            Ok::<_, netune::NetError>((status, headers, bytes))
        };
        let (status, headers, bytes) = tokio::time::timeout(self.timeout, exchange)
            .await
            .map_err(|_| {
                let recorder = recorder.lock().unwrap_or_else(|error| error.into_inner());
                format!(
                    "request to {} timed out after {:.1}s during {}",
                    request.url.split(['?', '#']).next().unwrap_or(&request.url),
                    self.timeout.as_secs_f64(),
                    timeout_phase(recorder.log())
                )
            })?
            .map_err(|error| error.to_string())?;
        Ok(Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    pub async fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Reply, String> {
        let mut request = Request::new(Method::GET, url);
        for (name, value) in headers {
            request = request.header(name, *value);
        }
        self.send(request).await
    }
}

/// Ignore sampling/read events and report the latest protocol phase. Iteration
/// also handles redirects, whose DNS/TLS phases restart within the same trace.
fn timeout_phase(log: &netune_trace::EventLog) -> &'static str {
    use netune_trace::EventKind;
    let mut phase = "request setup";
    for event in log.iter() {
        phase = match event.kind {
            EventKind::DnsStart => "DNS resolution",
            EventKind::DnsEnd | EventKind::TcpStart => "TCP connection",
            EventKind::TcpEnd | EventKind::TlsStart => "TLS handshake",
            EventKind::TlsEnd | EventKind::ConnectReused | EventKind::RequestWriteStart => {
                "request write"
            }
            EventKind::RequestWriteEnd => "response headers",
            EventKind::HeadComplete
            | EventKind::BodyStart
            | EventKind::ChunkBoundary
            | EventKind::Trailers => "response body",
            _ => phase,
        };
    }
    phase
}

/// Percent-encode one URL component (query names and values, form fields).
pub fn encode_component(input: &str) -> String {
    percent_encode(input)
}

/// `application/x-www-form-urlencoded` encoding, per WHATWG URL.
fn form_encode(fields: &[(String, String)]) -> String {
    let mut out = String::new();
    for (index, (name, value)) in fields.iter().enumerate() {
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

    #[tokio::test]
    async fn timeout_reports_headers_or_body_and_redacts_query() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for send_headers in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 4096];
                let _bytes_read = socket.read(&mut buffer).await.unwrap();
                if send_headers {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nx")
                        .await
                        .unwrap();
                }
                futures::future::pending::<()>().await;
            });
            let error = Http::new(Duration::from_millis(100))
                .unwrap()
                .get(&format!("http://{addr}/models?key=secret"), &[])
                .await
                .unwrap_err();
            server.abort();
            assert!(
                error.contains(if send_headers {
                    "response body"
                } else {
                    "response headers"
                }),
                "{error}"
            );
            assert!(error.contains("after 0.1s"), "{error}");
            assert!(!error.contains("secret"), "{error}");
        }
    }

    #[test]
    fn timeout_phase_tracks_redirects_and_ignores_sampling() {
        use netune_trace::EventKind;
        let mut recorder = netune_trace::Recorder::start(64);
        for (event, expected) in [
            (EventKind::DnsStart, "DNS resolution"),
            (EventKind::TcpStart, "TCP connection"),
            (EventKind::TlsStart, "TLS handshake"),
            (EventKind::RequestWriteStart, "request write"),
            (EventKind::RequestWriteEnd, "response headers"),
            (EventKind::TcpInfo, "response headers"),
            (EventKind::HeadComplete, "response body"),
            (EventKind::Read, "response body"),
            (EventKind::DnsStart, "DNS resolution"),
        ] {
            recorder.mark(event, 0, 0);
            assert_eq!(timeout_phase(recorder.log()), expected);
        }
    }

    #[test]
    fn form_encoding_matches_the_url_standard() {
        assert_eq!(
            form_encode(&[
                ("grant_type".into(), "authorization_code".into()),
                ("code".into(), "a b+c/d".into()),
            ]),
            "grant_type=authorization_code&code=a+b%2Bc%2Fd"
        );
    }

    #[test]
    fn a_reply_parses_json_and_reports_failures() {
        let reply = Reply {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: "{\"ok\":true}".into(),
        };
        assert!(reply.is_success());
        assert_eq!(reply.json().expect("json")["ok"], serde_json::json!(true));

        let broken = Reply {
            status: StatusCode::BAD_GATEWAY,
            headers: HeaderMap::new(),
            body: "not json".into(),
        };
        assert!(!broken.is_success());
        assert!(broken.json().is_err());
    }
}
