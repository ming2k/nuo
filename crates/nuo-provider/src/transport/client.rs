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

/// Bound on the advisory pre-warm.
///
/// A pre-warm races credential resolution and is joined before dispatch, so a
/// peer that accepts TCP/TLS and then stalls before HTTP/2 SETTINGS must not
/// hold the real request behind it. Netune bounds each establishment phase at
/// 10 s; this tighter caller-side budget also caps the advisory wait itself,
/// because a pre-warm that cannot finish is worth less than the request it
/// would have accelerated.
const PREWARM_TIMEOUT: Duration = Duration::from_secs(5);

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

use super::diagnostic::TransportDiagnostic;

fn diagnostic_from_snapshot(
    snapshot: &netune_trace::TraceSnapshot,
) -> Option<Box<TransportDiagnostic>> {
    TransportDiagnostic::from_snapshot(snapshot).map(Box::new)
}

fn attempt_error(label: &'static str, error: &netune::AttemptError) -> TransportError {
    let retryable = error.error().is_retryable();
    TransportError::Transport {
        label,
        message: error.to_string(),
        retryable,
        diagnostic: diagnostic_from_snapshot(error.trace()),
    }
}

fn net_error(label: &'static str, error: netune::NetError) -> TransportError {
    let retryable = error.is_retryable();
    TransportError::Transport {
        label,
        message: error.to_string(),
        retryable,
        diagnostic: None,
    }
}

mod owned {
    use std::sync::Arc;

    use bytes::Bytes;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use netune::{Connector, TcpConnector, TlsRouterConnector};
    use netune_trace::{
        AttemptRef, ConnectionIdentity, ConnectionInfo, DiagnosticCode, DiagnosticRecord,
        EndpointRef, EventKind, Fidelity, RequestTrace, TRACE_SCHEMA_VERSION, TraceCollector,
        TraceSnapshot, derive,
    };

    use super::super::diagnostic::{last_phase, phase_label};
    use super::super::error::TransportError;
    use super::super::request::RequestParts;
    use super::super::telemetry::{TransportObservation, TransportTelemetry, TransportTimings};
    use super::{
        HttpResponse, Transport, TransportDiagnostic, attempt_error, diagnostic_from_snapshot,
        net_error,
    };

    const TRACE_CAPACITY: usize = 16_384;

    /// Rebuild an attempt envelope from the collector's v2 snapshot.
    ///
    /// The snapshot owns the real generated [`TraceMeta`](netune_trace::TraceMeta)
    /// and the diagnostic side table, so a failed attempt no longer fabricates
    /// a placeholder id or drops its evidence. Connection identity stays
    /// unknown here because only a completed response body can name the socket
    /// it actually rode.
    fn trace_from_snapshot(snapshot: TraceSnapshot, endpoint: EndpointRef) -> RequestTrace {
        RequestTrace {
            meta: snapshot.meta,
            attempt: AttemptRef {
                round: 0,
                turn: 0,
                attempt: 0,
            },
            endpoint,
            connection: ConnectionInfo::default(),
            identity: ConnectionIdentity::default(),
            schema_version: TRACE_SCHEMA_VERSION,
            fidelity: snapshot.fidelity,
            log: snapshot.log,
            diagnostics: snapshot.diagnostics,
        }
    }

    /// Publish a terminal attempt to the sink, including failures.
    ///
    /// A sink that only saw successful bodies could never explain a failed
    /// attempt. The collector records diagnostics even under `Observation::Off`,
    /// so this path is safe for every observation tier Nuo selects.
    fn publish_terminal(
        sink: &Option<TraceSink>,
        collector: &TraceCollector,
        endpoint: &EndpointRef,
    ) -> Option<Box<TransportDiagnostic>> {
        let snapshot = collector.snapshot(Fidelity::l1());
        let diagnostic = diagnostic_from_snapshot(&snapshot);
        if let Some(sink) = sink {
            sink(trace_from_snapshot(snapshot, endpoint.clone()));
        }
        diagnostic
    }

    /// Turn a body/stream failure into a typed transport failure.
    ///
    /// netune's `send_traced` boundary owns the head; a failure while reading
    /// the body surfaces as a bare `NetError`. The collector still knows the
    /// phase it reached, so the diagnostic is added there and the whole
    /// evidence record is published before the error is returned.
    fn stream_error(
        label: &'static str,
        error: netune::NetError,
        collector: &TraceCollector,
        endpoint: &EndpointRef,
        sink: &Option<TraceSink>,
    ) -> TransportError {
        if collector.diagnostics().last_error().is_none() {
            collector.record_error(error.diagnostic());
        }
        let diagnostic = publish_terminal(sink, collector, endpoint);
        TransportError::Stream {
            label,
            message: error.to_string(),
            retryable: error.is_retryable(),
            diagnostic,
        }
    }

    fn timeout_error(
        label: &'static str,
        collector: &TraceCollector,
        endpoint: &EndpointRef,
        sink: &Option<TraceSink>,
    ) -> TransportError {
        let phase = last_phase(collector);
        let record = DiagnosticRecord::new(DiagnosticCode::EGRESS_TIMEOUT)
            .with_phase(phase)
            .with_message(format!("request timed out during {}", phase_label(phase)));
        collector.record_error(record);
        let diagnostic = publish_terminal(sink, collector, endpoint);
        TransportError::Timeout { label, diagnostic }
    }

    struct TimingsWriter {
        telemetry: TransportTelemetry,
        collector: TraceCollector,
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
            let trace = trace_from_snapshot(
                self.collector.snapshot(Fidelity::l1()),
                self.endpoint.clone(),
            );
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

    /// Attempt-level cold-path sink.
    ///
    /// It receives one trace per attempt, on success and on failure. A failed
    /// trace carries the diagnostics recorded before the error was returned;
    /// its `meta.id` matches the id on the `TransportDiagnostic`. The netune
    /// event ring remains the record of truth; this sink is a view.
    pub type TraceSink = Arc<dyn Fn(RequestTrace) + Send + Sync>;

    /// The owned netune transport implementation.
    pub struct NetTransport<C: Connector = TlsRouterConnector<TcpConnector>> {
        client: std::sync::Arc<netune::Client<C>>,
        sink: Option<TraceSink>,
    }

    impl NetTransport<TlsRouterConnector<TcpConnector>> {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                client: std::sync::Arc::clone(super::super::network::shared_client()),
                sink: None,
            })
        }

        pub fn from_shared_client(
            client: std::sync::Arc<netune::Client<TlsRouterConnector<TcpConnector>>>,
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
            let (mut target, path) = netune::Target::from_url(&parts.url).map_err(|error| {
                TransportError::InvalidRequest {
                    label: parts.label,
                    message: format!("invalid url: {error}"),
                }
            })?;
            if let Some(profile) = parts.tls_profile {
                target = target.with_tls_profile(profile);
            }
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
            // Model attempts always need the protocol timeline: `dns_us`,
            // `tcp_us`, `tls_us` and stream readiness are pure derivations of
            // the collector's events. The byte-path tap stays owned by netune
            // (`Observation::Traced`), never by this crate.
            let collector = TraceCollector::start(TRACE_CAPACITY);
            let endpoint = EndpointRef {
                provider: parts.label.to_string(),
                model: String::new(),
                authority,
            };
            let writer = TimingsWriter {
                telemetry: parts.telemetry.clone(),
                collector: collector.clone(),
                dispatch_at,
                endpoint: endpoint.clone(),
                sealed: false,
            };
            let send =
                self.client
                    .send_traced(&target, collector.clone(), head, parts.body.clone());
            let send_result = match deadline {
                Some(deadline) => match tokio::time::timeout_at(deadline, send).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(timeout_error(
                            parts.label,
                            &collector,
                            &endpoint,
                            &self.sink,
                        ));
                    }
                },
                None => send.await,
            };
            let response = match send_result {
                Ok(response) => response,
                Err(error) => {
                    // The failed attempt is evidence, not a discarded record:
                    // publish its snapshot before converting the error.
                    let snapshot = error.trace().clone();
                    if let Some(sink) = &self.sink {
                        sink(trace_from_snapshot(snapshot, endpoint.clone()));
                    }
                    return Err(attempt_error(parts.label, &error));
                }
            };
            let status = response.head.status;
            let headers = response.head.headers.clone();

            let sink = self.sink.clone();
            let label = parts.label;
            let body: BoxStream<'static, Result<Bytes, TransportError>> = futures::stream::unfold(
                (
                    response.body,
                    collector,
                    sink,
                    endpoint,
                    deadline,
                    label,
                    writer,
                    false,
                ),
                |(
                    mut body,
                    collector,
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
                            (body, collector, sink, endpoint, deadline, label, writer, false),
                        )),
                        Ok(None) => {
                            if let Some(sink) = sink
                                && let Some(mut trace) = body.trace()
                            {
                                // The body owns the honest envelope: the
                                // negotiated connection identity, the schema
                                // version, and the fidelity its tap actually
                                // produced (L1 for HTTP/1.1, L0 for HTTP/2).
                                trace.endpoint = endpoint;
                                sink(trace);
                            }
                            writer.seal();
                            None
                        }
                        Err(error) => Some((
                            Err(stream_error(
                                label, error, &collector, &endpoint, &sink,
                            )),
                            (body, collector, sink, endpoint, deadline, label, writer, true),
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
    ///
    /// The caller treats the outcome as advisory, so the wait is bounded: a
    /// pre-warm that cannot finish within the transport's pre-warm budget is
    /// abandoned instead of delaying the request that would have used it.
    pub async fn prewarm(&self, url: &str, _label: &'static str) -> Result<bool, TransportError> {
        self.prewarm_within(url, PREWARM_TIMEOUT).await
    }

    async fn prewarm_within(&self, url: &str, timeout: Duration) -> Result<bool, TransportError> {
        match tokio::time::timeout(timeout, self.transport.prewarm(url)).await {
            Ok(result) => result,
            Err(_) => Err(TransportError::Timeout {
                label: "prewarm",
                diagnostic: None,
            }),
        }
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
    use super::super::telemetry::{TransportObservation, TransportTelemetry};
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
    async fn the_trace_sink_gets_the_honest_envelope_and_timings_are_published() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let seen: Arc<Mutex<Option<netune_trace::RequestTrace>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&seen);
        let transport = NetTransport::plain().with_trace_sink(Arc::new(move |trace| {
            *slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(trace);
        }));

        let telemetry = TransportTelemetry::new();
        let parts = RequestBuilder::new(http::Method::GET, format!("http://{addr}/v1"))
            .with_telemetry(telemetry.clone())
            .build("Test")
            .unwrap();
        let response = transport.send(parts).await.unwrap();
        assert_eq!(response.status, http::StatusCode::OK);
        assert_eq!(&response.into_bytes().await.unwrap()[..], b"ok");

        let trace = seen
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("the sink must receive the completed attempt");
        assert_eq!(trace.endpoint.provider, "Test");
        assert_eq!(trace.schema_version, netune_trace::TRACE_SCHEMA_VERSION);
        assert!(
            trace.fidelity.syscall,
            "an HTTP/1.1 attempt owns a syscall tap: {:?}",
            trace.fidelity
        );
        assert!(
            trace
                .log
                .first_of(netune_trace::EventKind::TcpStart)
                .is_some()
        );

        let timings = telemetry.snapshot().expect("timings published");
        assert_eq!(timings.observation, TransportObservation::ColdConnection);
        assert!(timings.tcp_us.is_some(), "the connect phase is recorded");
    }

    struct StallingTransport;

    #[async_trait::async_trait]
    impl Transport for StallingTransport {
        async fn send(&self, _parts: RequestParts) -> Result<HttpResponse, TransportError> {
            std::future::pending::<Result<HttpResponse, TransportError>>().await
        }

        async fn prewarm(&self, _url: &str) -> Result<bool, TransportError> {
            std::future::pending::<Result<bool, TransportError>>().await
        }
    }

    #[tokio::test]
    async fn advisory_prewarm_is_bounded() {
        let client = Client {
            transport: Arc::new(StallingTransport),
            request_timeout: Duration::from_secs(1),
        };
        let started = std::time::Instant::now();
        let error = client
            .prewarm_within("http://stall.invalid", Duration::from_millis(50))
            .await
            .expect_err("a stalled pre-warm must not wait forever");
        assert!(matches!(
            error,
            TransportError::Timeout {
                label: "prewarm",
                ..
            }
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the bound must be the configured one, not the transport's: {:?}",
            started.elapsed()
        );
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
        let diagnostic = error
            .diagnostic()
            .expect("a cancelled attempt still carries a diagnostic");
        assert_eq!(
            diagnostic.code(),
            netune_trace::DiagnosticCode::EGRESS_TIMEOUT
        );
        assert!(
            diagnostic.is_retryable(),
            "the producer's retry verdict must survive: {diagnostic:?}"
        );
        assert!(
            !diagnostic.trace_id().as_str().is_empty(),
            "the timeout must carry the trace id it belongs to"
        );
    }

    #[tokio::test]
    async fn a_failed_attempt_carries_its_diagnostic_and_publishes_its_trace() {
        use netune_trace::{DiagnosticCode, EventKind, TRACE_SCHEMA_VERSION};

        // Bind then release a port: connecting to it fails immediately with a
        // refused TCP connection instead of waiting on a live blackhole.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let seen: Arc<Mutex<Vec<netune_trace::RequestTrace>>> = Arc::new(Mutex::new(Vec::new()));
        let slot = Arc::clone(&seen);
        let transport = NetTransport::plain().with_trace_sink(Arc::new(move |trace| {
            slot.lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(trace);
        }));

        let parts = RequestBuilder::new(http::Method::GET, format!("http://127.0.0.1:{port}/v1"))
            .build("Test")
            .unwrap();
        let error = transport.send(parts).await.unwrap_err();

        let diagnostic = error
            .diagnostic()
            .expect("a failed attempt must carry netune's diagnosis");
        assert_eq!(diagnostic.code(), DiagnosticCode::EGRESS_TCP_FAILED);
        assert!(
            error.is_retryable(),
            "a refused connect is retryable: {error}"
        );
        assert_eq!(
            diagnostic.trace_id().as_str().len(),
            36,
            "the trace id must be a real generated id, not a placeholder"
        );

        let trace = seen
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop()
            .expect("the sink must receive the failed attempt too");
        assert_eq!(trace.schema_version, TRACE_SCHEMA_VERSION);
        assert_eq!(&trace.meta.id, diagnostic.trace_id());
        assert_eq!(trace.diagnostic_code(), Some(diagnostic.code()));
        assert!(
            trace.log.first_of(EventKind::TcpStart).is_some(),
            "the failed attempt retains the phase it reached"
        );
    }

    #[tokio::test]
    async fn a_body_failure_carries_the_phase_and_publishes_its_trace() {
        use netune_trace::{DiagnosticCode, EventKind};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await.unwrap();
            // Promise ten bytes, send one, then close: the body is provably
            // incomplete rather than merely empty.
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nx")
                .await
                .unwrap();
        });

        let seen: Arc<Mutex<Vec<netune_trace::RequestTrace>>> = Arc::new(Mutex::new(Vec::new()));
        let slot = Arc::clone(&seen);
        let transport = NetTransport::plain().with_trace_sink(Arc::new(move |trace| {
            slot.lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(trace);
        }));
        let parts = RequestBuilder::new(http::Method::GET, format!("http://{addr}/v1"))
            .build("Test")
            .unwrap();
        let response = transport.send(parts).await.unwrap();
        let error = response
            .into_bytes()
            .await
            .expect_err("a truncated body must fail");

        let diagnostic = error
            .diagnostic()
            .expect("a body failure must carry netune's diagnosis");
        assert_eq!(diagnostic.code(), DiagnosticCode::EGRESS_HTTP1_INCOMPLETE);
        assert!(error.is_retryable());

        let trace = seen
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop()
            .expect("the sink must receive the failed body attempt");
        assert_eq!(&trace.meta.id, diagnostic.trace_id());
        assert_eq!(trace.diagnostic_code(), Some(diagnostic.code()));
        assert!(
            trace.log.first_of(EventKind::HeadComplete).is_some(),
            "the body failure retains the head phase it reached"
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
