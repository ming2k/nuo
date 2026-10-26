//! Unified HTTP transport client, connection pooling, and request dispatch.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use http::{HeaderMap, StatusCode};

use super::error::TransportError;
use super::request::{RequestBuilder, RequestParts};

/// Overall bound for a single non-streaming request.
const CHAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// A response: status, headers, and an unconsumed byte stream.
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: BoxStream<'static, Result<Bytes, TransportError>>,
}

impl HttpResponse {
    /// The body as a byte stream.
    pub fn into_byte_stream(self) -> BoxStream<'static, Result<Bytes, TransportError>> {
        self.body
    }

    /// Read the whole body into memory.
    pub async fn into_bytes(self) -> Result<Bytes, TransportError> {
        let mut stream = self.into_byte_stream();
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(Bytes::from(out))
    }

    /// Read the whole body as text (lossy for non-UTF-8, as diagnostics expect).
    pub async fn into_text(self) -> Result<String, TransportError> {
        let bytes = self.into_bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HttpResponse({})", self.status)
    }
}

/// The transport that executes a request.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, parts: RequestParts) -> Result<HttpResponse, TransportError>;

    /// Optional pre-warm: prime a connection to `url` into the idle pool.
    async fn prewarm(&self, _url: &str) -> Result<bool, TransportError> {
        Ok(false)
    }
}

fn net_error(label: &'static str, error: netune::NetError) -> TransportError {
    let retryable = error.is_retryable();
    TransportError::Transport {
        label,
        message: error.to_string(),
        retryable,
    }
}

fn timeout_error(label: &'static str) -> TransportError {
    TransportError::Timeout { label }
}

mod owned {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use netune::{Connector, TcpConnector, TlsConnector};
    use netune_trace::{
        AttemptRef, ConnectionInfo, EndpointRef, EventKind, Fidelity, Recorder, RequestTrace,
        TraceId, derive,
    };

    use super::super::error::TransportError;
    use super::super::request::RequestParts;
    use super::super::telemetry::{TransportObservation, TransportTelemetry, TransportTimings};
    use super::{HttpResponse, Transport, net_error, timeout_error};

    const TRACE_CAPACITY: usize = 16_384;

    struct TimingsWriter {
        telemetry: TransportTelemetry,
        recorder: Arc<Mutex<Recorder>>,
        dispatch_at: std::time::Instant,
        endpoint: EndpointRef,
        sealed: bool,
    }

    impl TimingsWriter {
        fn seal(&mut self) {
            if self.sealed {
                return;
            }
            self.sealed = true;
            let log = self
                .recorder
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .log()
                .clone();
            let trace = RequestTrace {
                id: TraceId::new("transport"),
                attempt: AttemptRef {
                    round: 0,
                    turn: 0,
                    attempt: 0,
                },
                endpoint: self.endpoint.clone(),
                connection: ConnectionInfo::default(),
                fidelity: Fidelity::l1(),
                log,
            };
            let derived = derive(&trace);

            let measured = |reading: netune_trace::Reading<u64>| reading.value();
            let phases = measured(derived.dns_us).is_some()
                || measured(derived.tcp_us).is_some()
                || measured(derived.tls_us).is_some();
            let reuse_recorded = trace.log.first_of(EventKind::ConnectReused).is_some();
            let observation = if phases {
                TransportObservation::ColdConnection
            } else if reuse_recorded {
                TransportObservation::PooledConnection
            } else {
                TransportObservation::Unreported
            };

            let connected_ns = trace
                .log
                .first_of(EventKind::TlsEnd)
                .or_else(|| trace.log.first_of(EventKind::TcpEnd))
                .or_else(|| trace.log.first_of(EventKind::ConnectReused))
                .map(|event| event.at_ns);
            let connected_us = connected_ns
                .filter(|_| observation != TransportObservation::Unreported)
                .map(|ns| ns / 1_000);

            let timings = TransportTimings {
                dns_us: measured(derived.dns_us),
                tcp_us: measured(derived.tcp_us),
                tls_us: measured(derived.tls_us),
                connected_us,
                request_sent_us: measured(derived.request_sent_us),
                stream_ready_us: measured(derived.ttfb_us),
                rtt_us: measured(derived.rtt_us),
                retransmits: derived.retransmits.value().unwrap_or(0) as u32,
                observation,
                dispatch_at: Some(self.dispatch_at),
            };

            self.telemetry.publish(timings);
        }
    }

    impl Drop for TimingsWriter {
        fn drop(&mut self) {
            self.seal();
        }
    }

    pub type TraceSink = Arc<dyn Fn(RequestTrace) + Send + Sync>;

    /// The owned netune transport implementation.
    pub struct NetTransport<C: Connector = TlsConnector<TcpConnector>> {
        client: std::sync::Arc<netune::Client<C>>,
        sink: Option<TraceSink>,
    }

    impl NetTransport<TlsConnector<TcpConnector>> {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                client: std::sync::Arc::clone(super::super::network::shared_client()),
                sink: None,
            })
        }

        pub fn from_shared_client(
            client: std::sync::Arc<netune::Client<TlsConnector<TcpConnector>>>,
        ) -> Self {
            Self { client, sink: None }
        }
    }

    impl NetTransport<TcpConnector> {
        pub fn plain() -> Self {
            Self::from_connector(TcpConnector::new())
        }
    }

    impl<C: Connector> NetTransport<C> {
        pub fn from_connector(connector: C) -> Self {
            Self {
                client: std::sync::Arc::new(netune::Client::new(
                    connector,
                    netune::Pool::default(),
                    netune::ClientConfig::default(),
                )),
                sink: None,
            }
        }

        pub fn with_trace_sink(mut self, sink: TraceSink) -> Self {
            self.sink = Some(sink);
            self
        }
    }

    #[async_trait::async_trait]
    impl<C: Connector> Transport for NetTransport<C> {
        async fn send(&self, parts: RequestParts) -> Result<HttpResponse, TransportError> {
            let (target, path) = netune::Target::from_url(&parts.url).map_err(|error| {
                TransportError::InvalidRequest {
                    label: parts.label,
                    message: format!("invalid url: {error}"),
                }
            })?;
            let authority = target.authority.clone();

            let mut head = netune::RequestHead::new(parts.method.clone(), path);
            for (name, value) in parts.headers.iter() {
                if let Ok(value) = value.to_str() {
                    head = head.with_header(name.as_str(), value);
                }
            }

            let deadline = parts
                .timeout
                .map(|timeout| tokio::time::Instant::now() + timeout);
            let dispatch_at = std::time::Instant::now();
            let recorder = Arc::new(Mutex::new(Recorder::start(TRACE_CAPACITY)));
            let endpoint = EndpointRef {
                provider: parts.label.to_string(),
                model: String::new(),
                authority,
            };
            let writer = TimingsWriter {
                telemetry: parts.telemetry.clone(),
                recorder: Arc::clone(&recorder),
                dispatch_at,
                endpoint: endpoint.clone(),
                sealed: false,
            };
            let send = self
                .client
                .send(&target, Arc::clone(&recorder), head, parts.body.clone());
            let response = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, send)
                    .await
                    .map_err(|_| timeout_error(parts.label))?
                    .map_err(|error| net_error(parts.label, error))?,
                None => send.await.map_err(|error| net_error(parts.label, error))?,
            };
            let status = response.head.status;
            let headers = response.head.headers.clone();

            let sink = self.sink.clone();
            let label = parts.label;
            let body: BoxStream<'static, Result<Bytes, TransportError>> = futures::stream::unfold(
                (
                    response.body,
                    recorder,
                    sink,
                    endpoint,
                    deadline,
                    label,
                    writer,
                    false,
                ),
                |(
                    mut body,
                    recorder,
                    sink,
                    endpoint,
                    deadline,
                    label,
                    mut writer,
                    failed,
                )| async move {
                    if failed {
                        return None;
                    }
                    let chunk = match deadline {
                        Some(deadline) => {
                            match tokio::time::timeout_at(deadline, body.next_chunk()).await {
                                Ok(result) => result,
                                Err(_) => Err(netune::NetError::Io(std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    "request timed out while reading the body",
                                ))),
                            }
                        }
                        None => body.next_chunk().await,
                    };
                    match chunk {
                        Ok(Some(chunk)) => Some((
                            Ok(chunk),
                            (body, recorder, sink, endpoint, deadline, label, writer, false),
                        )),
                        Ok(None) => {
                            if let Some(sink) = sink {
                                let log = recorder
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .log()
                                    .clone();
                                sink(RequestTrace {
                                    id: TraceId::new("egress"),
                                    attempt: AttemptRef {
                                        round: 0,
                                        turn: 0,
                                        attempt: 0,
                                    },
                                    endpoint,
                                    connection: ConnectionInfo::default(),
                                    fidelity: Fidelity::l1(),
                                    log,
                                });
                            }
                            writer.seal();
                            None
                        }
                        Err(error) => Some((
                            Err(net_error(label, error)),
                            (body, recorder, sink, endpoint, deadline, label, writer, true),
                        )),
                    }
                },
            )
            .boxed();

            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        }

        async fn prewarm(&self, url: &str) -> Result<bool, TransportError> {
            let (target, _) =
                netune::Target::from_url(url).map_err(|error| TransportError::InvalidRequest {
                    label: "netune",
                    message: format!("invalid prewarm url: {error}"),
                })?;
            self.client
                .prewarm(&target)
                .await
                .map_err(|error| net_error("netune", error))
        }
    }
}

pub use owned::{NetTransport, TraceSink};

/// Pooled HTTP client executing requests through the owned netune transport.
#[derive(Clone)]
pub struct Client {
    transport: Arc<dyn Transport>,
    request_timeout: Duration,
}

impl Client {
    pub fn new() -> Self {
        let transport: Arc<dyn Transport> = match NetTransport::new() {
            Ok(transport) => Arc::new(transport),
            Err(error) => {
                let transport = NetTransport::from_connector(netune::TcpConnector::new());
                tracing::error!(
                    fallback = "plaintext connector; TLS requests will fail at dispatch",
                    %error,
                    "shared netune client init failed; degrading transport"
                );
                Arc::new(transport)
            }
        };
        Self {
            transport,
            request_timeout: CHAT_REQUEST_TIMEOUT,
        }
    }

    pub fn with_transport(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            request_timeout: CHAT_REQUEST_TIMEOUT,
        }
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    #[cfg(test)]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Send a fully-built request **without** enforcing HTTP 2xx status.
    pub async fn send_raw(
        &self,
        request: RequestBuilder,
        label: &'static str,
    ) -> Result<HttpResponse, TransportError> {
        let parts = request.build(label)?;
        self.transport.send(parts).await
    }

    /// Prime a connection to `url`'s origin into the idle pool.
    pub async fn prewarm(&self, url: &str, _label: &'static str) -> Result<bool, TransportError> {
        self.transport.prewarm(url).await
    }

    /// Send a fully-built request and enforce HTTP 2xx success.
    pub async fn send(
        &self,
        request: RequestBuilder,
        label: &'static str,
    ) -> Result<HttpResponse, TransportError> {
        let response = self.send_raw(request, label).await?;
        if response.status.is_success() {
            Ok(response)
        } else {
            let status = response.status;
            let retry_after = super::error::retry_after_ms(&response.headers);
            let message = response.into_text().await.unwrap_or_default();
            Err(TransportError::Http {
                status,
                message,
                retry_after,
            })
        }
    }

    /// Send a request, enforce success, and decode JSON.
    pub async fn send_json(
        &self,
        request: RequestBuilder,
        label: &'static str,
    ) -> Result<serde_json::Value, TransportError> {
        let response = self
            .send(request.timeout(self.request_timeout), label)
            .await?;
        let bytes = response.into_bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| TransportError::Decode {
            label,
            message: e.to_string(),
        })
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    struct Blackhole {
        addr: std::net::SocketAddr,
        held: Arc<Mutex<Vec<TcpStream>>>,
    }

    impl Blackhole {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let held: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
            let held_in_thread = Arc::clone(&held);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => held_in_thread
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .push(stream),
                        Err(_) => break,
                    }
                }
            });
            Self { addr, held }
        }

        fn url(&self) -> String {
            format!("http://{}/chat/completions", self.addr)
        }

        fn drop_connections(&self) {
            self.held
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
    }

    fn post_json(_client: &Client, url: &str) -> RequestBuilder {
        RequestBuilder::new(http::Method::POST, url).json(&serde_json::json!({}))
    }

    #[tokio::test]
    async fn non_streaming_timeout_fires_and_is_retryable() {
        let server = Blackhole::start();
        let client = Client::new().with_request_timeout(Duration::from_millis(300));
        let started = std::time::Instant::now();
        let error = client
            .send_json(post_json(&client, &server.url()), "Test")
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(300),
            "the error must be the 300ms request timeout, not an instant failure: \
             {elapsed:?} / {error}"
        );
        assert!(
            error.is_retryable(),
            "a timeout must classify as retryable: {error}"
        );
    }

    #[tokio::test]
    async fn connect_phase_failure_is_retryable() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new();
        let error = client
            .send(
                post_json(&client, &format!("http://127.0.0.1:{port}/v1/messages")),
                "Test",
            )
            .await
            .unwrap_err();
        assert!(
            error.is_retryable(),
            "connect-phase errors must be retryable: {error}"
        );
    }

    #[test]
    fn streaming_send_carries_no_overall_timeout() {
        let server = Blackhole::start();
        let url = server.url();
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let client = Client::new().with_request_timeout(Duration::from_millis(300));
                let request = post_json(&client, &url);
                client.send(request, "Test").await
            })
        });
        std::thread::sleep(Duration::from_millis(1_000));
        assert!(
            !handle.is_finished(),
            "send must outlast the 300ms non-streaming timeout"
        );
        server.drop_connections();
        let result = handle.join().unwrap();
        assert!(result.is_err(), "EOF before response headers is an error");
    }
}
