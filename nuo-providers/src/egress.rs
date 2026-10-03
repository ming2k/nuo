//! The egress seam: one request, one transport, one trace.
//!
//! ADR-0200 replaces `reqwest` with an owned transport, and the switch must not
//! reach the protocol adapters. The seam is drawn at "given a fully formed
//! request, give me a response": [`Egress`] is implemented by [`MutaNetEgress`]
//! (production) and, behind the `reqwest-oracle` feature, by `ReqwestEgress`
//! — which exists only so the differential and shadow comparisons have a
//! reference implementation. `reqwest` is therefore a *dev/test* dependency;
//! it is not in the production graph.
//!
//! [`HttpResponse`] is deliberately small — status, headers, and a byte stream —
//! because that is all a protocol adapter may depend on. Everything richer
//! (retry classification, JSON diagnostics) is layered on top in
//! [`crate::transport`], so every transport gets identical behaviour by
//! construction rather than by duplicated effort.

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use http::{HeaderMap, Method, StatusCode};
use nuo_model_codec::{ProviderError, ProviderErrorKind};

/// A request in transport-neutral form.
#[derive(Debug, Clone)]
pub struct RequestParts {
    /// Provider label used for error attribution and retry classification.
    pub label: &'static str,
    pub method: Method,
    /// Absolute URL.
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    /// Whole-request timeout, applied only to non-streaming requests by the
    /// caller (a streaming request must never carry one).
    pub timeout: Option<Duration>,
    /// Where this attempt's transport telemetry goes (ADR-0232).
    ///
    /// The handle belongs to the attempt that issued the request: the transport
    /// publishes into it and never needs to know which attempt it was serving.
    /// A default handle absorbs the timings harmlessly, so a caller that does
    /// not care about telemetry (a test, a probe) needs no ceremony.
    pub telemetry: nuo_model_codec::TransportTelemetry,
}

/// A response: status, headers, and a body that has not been read yet.
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: BoxStream<'static, Result<Bytes, ProviderError>>,
}

impl HttpResponse {
    /// The body as a byte stream. The errors are already provider errors: the
    /// transport classified them.
    pub fn into_byte_stream(self) -> BoxStream<'static, Result<Bytes, ProviderError>> {
        self.body
    }

    /// Read the whole body.
    pub async fn into_bytes(self) -> Result<Bytes, ProviderError> {
        let mut stream = self.into_byte_stream();
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(Bytes::from(out))
    }

    /// Read the whole body as text (lossy for non-UTF-8, as the diagnostics
    /// expect).
    pub async fn into_text(self) -> Result<String, ProviderError> {
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
pub trait Egress: Send + Sync {
    async fn send(&self, parts: RequestParts) -> Result<HttpResponse, ProviderError>;

    /// Optional pre-warm: prime a connection to `url` into the idle pool in advance.
    async fn prewarm(&self, _url: &str) -> Result<bool, ProviderError> {
        Ok(false)
    }
}

pub use owned::{NuoNetEgress, TraceSink};
pub type MutaNetEgress = NuoNetEgress;

/// Map an owned-transport failure onto the provider error the retry classifier
/// reads, so both transports classify identically.
fn net_error(label: &'static str, error: netune::NetError) -> ProviderError {
    let retryable = error.is_retryable();
    let kind = match error.class() {
        "resolve" | "connect" | "io" => ProviderErrorKind::Transport,
        "encoding" | "decode" => ProviderErrorKind::Decode,
        _ => ProviderErrorKind::Protocol,
    };
    let mapped = ProviderError::new(label, kind, error.to_string());
    if retryable {
        mapped.retryable(None)
    } else {
        mapped
    }
}

/// A retryable overall-timeout error, matching the reqwest transport's framing.
fn timeout_error(label: &'static str) -> ProviderError {
    ProviderError::new(
        label,
        ProviderErrorKind::Timeout,
        "transport error: request timed out".to_string(),
    )
    .retryable(None)
}

mod owned {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use nuo_model_codec::{ProviderError, TransportObservation, TransportTimings};
    use netune::{Connector, TcpConnector, TlsConnector};
    use netune_trace::{
        AttemptRef, ConnectionInfo, EndpointRef, EventKind, Fidelity, Recorder, RequestTrace,
        TraceId, derive,
    };

    use super::{Egress, HttpResponse, RequestParts, net_error, timeout_error};

    /// Capacity of one request's trace ring on the owned path.
    const TRACE_CAPACITY: usize = 16_384;

    /// The attempt's trace, sealed into the attempt's own telemetry handle when
    /// the body ends — or when the caller drops the body early, which is the
    /// common case for a streaming response.
    ///
    /// Deriving in `Drop` rather than at EOF is the whole point: a protocol
    /// adapter stops reading as soon as it sees the provider's completion
    /// marker, so an unfold that only recorded on `Ok(None)` would report
    /// nothing for every streamed turn. Derivation is a pure function over the
    /// events already in memory, so a partial trace yields partial timings —
    /// each with its own verdict, never a fabricated number.
    ///
    /// `dispatch_at` is captured before the request is handed to the transport,
    /// which makes every offset in [`TransportTimings`] a duration from a real
    /// instant the caller can re-anchor against its own clock.
    struct TimingsWriter {
        /// The handle belonging to the attempt this request represents. The
        /// writer never consults which attempt that was: it publishes into the
        /// handle it was handed, which is what keeps concurrent attempts on one
        /// shared transport from aliasing (ADR-0232).
        telemetry: nuo_model_codec::TransportTelemetry,
        recorder: Arc<Mutex<Recorder>>,
        dispatch_at: std::time::Instant,
        endpoint: EndpointRef,
        sealed: bool,
    }

    impl TimingsWriter {
        /// Derive and store the attempt's timings. Idempotent, because it runs
        /// both when the body ends and again when the stream is dropped.
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
                id: TraceId::new("egress"),
                attempt: AttemptRef {
                    round: 0,
                    turn: 0,
                    attempt: 0,
                },
                endpoint: self.endpoint.clone(),
                // `derive` reads the reuse fact from the `ConnectReused` event,
                // not from this struct, so the default costs the timings
                // nothing. The sink path is the one that needs it filled.
                connection: ConnectionInfo::default(),
                fidelity: Fidelity::l1(),
                log,
            };
            let derived = derive(&trace);

            // A reading carries its own verdict; only a measured one may become
            // a number. `observation` then separates "the transport watched and
            // paid nothing" from "nobody was watching".
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
                // No connection event of either shape: either the attempt never
                // reached the transport's dispatch, or it died before the
                // connection was settled. Claiming a regime here would be
                // exactly the fabrication this field exists to prevent.
                TransportObservation::Unreported
            };

            // Connection ready: the end of the last phase actually paid, or the
            // pool handing the socket over. Both are events in the log, so this
            // is the trace's own statement rather than a sum of durations, which
            // would silently absorb any gap between the phases and the write.
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
                retransmits: derived.retransmits.value().unwrap_or(0),
                observation,
                dispatch_at: Some(self.dispatch_at),
            };

            self.telemetry.publish(timings);
        }
    }

    impl Drop for TimingsWriter {
        /// The abandonment path: a protocol adapter that stops reading at the
        /// provider's completion marker never reaches `Ok(None)`, so this is the
        /// only place the common case can be sealed from.
        fn drop(&mut self) {
            self.seal();
        }
    }

    /// Called once per completed request with its trace.
    pub type TraceSink = Arc<dyn Fn(RequestTrace) + Send + Sync>;

    /// The owned transport (ADR-0200).
    ///
    /// Every request produces a [`RequestTrace`]; when a [`TraceSink`] is
    /// installed it receives it as soon as the body ends.
    pub struct NuoNetEgress<C: Connector = TlsConnector<TcpConnector>> {
        client: netune::Client<C>,
        sink: Option<TraceSink>,
    }

    impl NuoNetEgress<TlsConnector<TcpConnector>> {
        /// The production configuration: platform trust store, direct.
        pub fn new() -> Result<Self, String> {
            crate::network::direct_client(netune::ClientConfig::default())
                .map(|client| Self { client, sink: None })
        }
    }

    impl NuoNetEgress<TcpConnector> {
        /// Plaintext, no proxy: tests and local servers only.
        pub fn plain() -> Self {
            Self::from_connector(TcpConnector::new())
        }
    }

    impl<C: Connector> NuoNetEgress<C> {
        pub fn from_connector(connector: C) -> Self {
            Self {
                client: netune::Client::new(
                    connector,
                    netune::Pool::default(),
                    netune::ClientConfig::default(),
                ),
                sink: None,
            }
        }

        /// Install a sink for completed traces.
        pub fn with_trace_sink(mut self, sink: TraceSink) -> Self {
            self.sink = Some(sink);
            self
        }
    }
    #[async_trait::async_trait]
    impl<C: Connector> Egress for NuoNetEgress<C> {
        async fn send(&self, parts: RequestParts) -> Result<HttpResponse, ProviderError> {
            let (target, path) = netune::Target::from_url(&parts.url).map_err(|error| {
                ProviderError::invalid_request(parts.label, format!("invalid url: {error}"))
            })?;
            let authority = target.authority.clone();

            let mut head = netune::RequestHead::new(parts.method.clone(), path);
            for (name, value) in parts.headers.iter() {
                if let Ok(value) = value.to_str() {
                    head = head.with_header(name.as_str(), value);
                }
            }

            // A non-streaming request carries an overall deadline; it bounds
            // the body read too, exactly as the reqwest transport's per-request
            // timeout does.
            let deadline = parts
                .timeout
                .map(|timeout| tokio::time::Instant::now() + timeout);
            // The dispatch origin for every offset this attempt reports. Taken
            // before the request enters the transport so the trace's offsets and
            // the caller's own stamps share one anchor.
            let dispatch_at = std::time::Instant::now();
            let recorder = Arc::new(Mutex::new(Recorder::start(TRACE_CAPACITY)));
            let endpoint = EndpointRef {
                provider: parts.label.to_string(),
                model: String::new(),
                authority,
            };
            // Built before the request enters the transport, so every exit path
            // seals what was observed: a connection that failed mid-handshake is
            // exactly where the phases matter, and returning early on an error
            // would discard the only record of how far it got.
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
            let body: BoxStream<'static, Result<Bytes, ProviderError>> = futures::stream::unfold(
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
                            // Seal at EOF as well as on drop: the body ending is
                            // the one moment this attempt's timings are known to
                            // be complete, and it does not depend on how long the
                            // caller keeps the stream alive afterwards.
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

        async fn prewarm(&self, url: &str) -> Result<bool, ProviderError> {
            let (target, _) = netune::Target::from_url(url).map_err(|error| {
                ProviderError::invalid_request("netune", format!("invalid prewarm url: {error}"))
            })?;
            self.client
                .prewarm(&target)
                .await
                .map_err(|error| net_error("netune", error))
        }
    }
}
