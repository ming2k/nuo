//! Bounded control-plane HTTP handle over the owned netune transport.
//!
//! OAuth, usage, and catalog queries use this bounded client.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{HeaderMap, Method, StatusCode};
use netune::{Client, Target, TcpConnector, TlsConnector};

pub use super::encoding::encode_component;
use super::encoding::form_encode;

/// Request body shapes control-plane call sites use.
#[derive(Debug, Clone)]
pub enum Body {
    Json(serde_json::Value),
    Form(Vec<(String, String)>),
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

/// A control-plane reply with the body buffered in memory.
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

    /// Parse the body as JSON, naming the failure in diagnostics.
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(&self.body)
            .map_err(|error| format!("invalid JSON response: {error} (body: {})", self.body))
    }
}

/// A pooled control-plane HTTP handle.
#[derive(Clone)]
pub struct Http {
    client: Arc<Client<TlsConnector<TcpConnector>>>,
    timeout: Duration,
}

impl Http {
    /// Build a handle with an explicit per-request deadline.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        Ok(Self {
            client: Arc::clone(super::network::shared_client()),
            timeout,
        })
    }

    /// Build a handle with the default control-plane deadline (10 s).
    pub fn control_plane() -> Result<Self, String> {
        Self::new(Duration::from_secs(10))
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
