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
