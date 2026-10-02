//! Wire client executing model requests over HTTP transport.

use crate::endpoint::Endpoint;
use crate::error::{Result, WireError};
use crate::protocol::build_request;
use crate::stream::SseDecoder;
use crate::types::{WireChunk, WireRequest, WireResponse};
use futures::{SinkExt, Stream};
use std::pin::Pin;
use std::task::{Context, Poll};

#[cfg(feature = "netune")]
use netune::{Client, ClientConfig, Pool, RequestHead, Target, TcpConnector, TlsConnector};

/// Unified stream of model wire chunks.
pub struct WireStream {
    receiver: futures::channel::mpsc::Receiver<Result<WireChunk>>,
}

impl WireStream {
    pub fn new(receiver: futures::channel::mpsc::Receiver<Result<WireChunk>>) -> Self {
        Self { receiver }
    }

    /// Wraps this stream with a [`StreamLoopDetector`] to intercept degenerative token loops.
    pub fn with_loop_detector(self, mut detector: crate::loop_detector::StreamLoopDetector) -> Self {
        let (mut tx, rx) = futures::channel::mpsc::channel(64);
        let mut source = self.receiver;

        tokio::spawn(async move {
            use futures::StreamExt;
            while let Some(item) = source.next().await {
                match item {
                    Ok(chunk) => {
                        let text_to_check = chunk.delta_content.as_deref().or(chunk.delta_thinking.as_deref());
                        if let Some(text) = text_to_check
                            && let Some(pat) = detector.push_and_check(text)
                        {
                            let _ = tx.send(Err(WireError::DegenerativeLoop(pat.description()))).await;
                            return;
                        }
                        if tx.send(Ok(chunk)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
        });

        Self { receiver: rx }
    }
}

impl Stream for WireStream {
    type Item = Result<WireChunk>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_next(cx)
    }
}

/// High-performance HTTP client for dispatching model wire requests.
#[derive(Clone)]
pub struct WireClient {
    #[cfg(feature = "netune")]
    netune_client: std::sync::Arc<Client<TlsConnector<TcpConnector>>>,
    #[cfg(all(feature = "reqwest-oracle", not(feature = "netune")))]
    reqwest_client: reqwest::Client,
}

impl Default for WireClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WireClient {
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        #[cfg(feature = "netune")]
        {
            let connector = TlsConnector::platform(TcpConnector::new())
                .expect("Failed to initialize Netune platform TLS connector");
            let netune_client = std::sync::Arc::new(Client::new(
                connector,
                Pool::default(),
                ClientConfig::default(),
            ));
            Self { netune_client }
        }

        #[cfg(all(feature = "reqwest-oracle", not(feature = "netune")))]
        {
            Self {
                reqwest_client: reqwest::Client::builder().build().unwrap_or_default(),
            }
        }

        #[cfg(not(any(feature = "netune", feature = "reqwest-oracle")))]
        {
            Self {}
        }
    }

    /// Fallible constructor returning [`WireError`] if platform TLS fails to initialize.
    pub fn try_new() -> Result<Self> {
        #[cfg(feature = "netune")]
        {
            let connector = TlsConnector::platform(TcpConnector::new()).map_err(|err| {
                WireError::Http(format!("Failed to initialize Netune platform TLS: {err}"))
            })?;
            let netune_client = std::sync::Arc::new(Client::new(
                connector,
                Pool::default(),
                ClientConfig::default(),
            ));
            Ok(Self { netune_client })
        }

        #[cfg(all(feature = "reqwest-oracle", not(feature = "netune")))]
        {
            Ok(Self {
                reqwest_client: reqwest::Client::builder()
                    .build()
                    .map_err(|err| WireError::Http(err.to_string()))?,
            })
        }

        #[cfg(not(any(feature = "netune", feature = "reqwest-oracle")))]
        {
            Ok(Self {})
        }
    }

    /// Executes a [`WireRequest`] against `endpoint`, collecting stream into unified [`WireResponse`].
    pub async fn execute(
        &self,
        endpoint: &Endpoint,
        request: &WireRequest,
    ) -> Result<WireResponse> {
        use futures::StreamExt;
        let mut stream = self.execute_stream(endpoint, request).await?;
        let mut accumulator = crate::types::StreamAccumulator::new();

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res?;
            accumulator.feed(&chunk);
        }

        Ok(accumulator.finish())
    }

    /// Executes a streaming request, returning a [`WireStream`] yielding [`WireChunk`] tokens.
    pub async fn execute_stream(
        &self,
        endpoint: &Endpoint,
        request: &WireRequest,
    ) -> Result<WireStream> {
        let token = endpoint.resolve_api_key().await?;
        let (url, headers, body) = build_request(endpoint, &token, request, true);

        let (mut tx, rx) = futures::channel::mpsc::channel(64);

        #[cfg(feature = "netune")]
        {
            let (target, path) = Target::from_url(&url)
                .map_err(|err| WireError::Http(format!("invalid target URL `{url}`: {err}")))?;

            let mut head = RequestHead::new(http::Method::POST, path);
            for (name, value) in headers.iter() {
                if let Ok(value_str) = value.to_str() {
                    head = head.with_header(name.as_str(), value_str);
                }
            }

            let body_bytes = serde_json::to_vec(&body)
                .map_err(|err| WireError::Protocol(format!("failed to serialize body: {err}")))?;

            let recorder =
                std::sync::Arc::new(std::sync::Mutex::new(netune_trace::Recorder::start(1024)));

            let response = self
                .netune_client
                .send(
                    &target,
                    recorder,
                    head,
                    Some(bytes::Bytes::from(body_bytes)),
                )
                .await
                .map_err(|err| WireError::Http(format!("Netune transport error: {err}")))?;

            let status = response.head.status;
            let mut body_reader = response.body;

            if !status.is_success() {
                let mut full_body = Vec::new();
                while let Ok(Some(chunk)) = body_reader.next_chunk().await {
                    full_body.extend_from_slice(&chunk);
                }
                let error_text = String::from_utf8_lossy(&full_body).to_string();
                return Err(WireError::ApiError {
                    status: status.as_u16(),
                    message: error_text,
                });
            }

            let ep = endpoint.clone();
            tokio::spawn(async move {
                let mut decoder = SseDecoder::new();
                while let Ok(Some(chunk)) = body_reader.next_chunk().await {
                    match decoder.feed(&ep, &chunk) {
                        Ok(chunks) => {
                            for c in chunks {
                                if tx.send(Ok(c)).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    }
                }

                if let Ok(chunks) = decoder.finish(&ep) {
                    for c in chunks {
                        let _ = tx.send(Ok(c)).await;
                    }
                }
            });

            Ok(WireStream::new(rx))
        }

        #[cfg(all(feature = "reqwest-oracle", not(feature = "netune")))]
        {
            let resp = self
                .reqwest_client
                .post(&url)
                .headers(headers)
                .timeout(endpoint.timeout)
                .json(&body)
                .send()
                .await
                .map_err(|err| WireError::Http(err.to_string()))?;

            let status = resp.status();
            if !status.is_success() {
                let error_text = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "unable to read error response".into());
                return Err(WireError::ApiError {
                    status: status.as_u16(),
                    message: error_text,
                });
            }

            let ep = endpoint.clone();
            let mut byte_stream = resp.bytes_stream();
            tokio::spawn(async move {
                let mut decoder = SseDecoder::new();
                use futures::StreamExt;
                while let Some(item) = byte_stream.next().await {
                    match item {
                        Ok(bytes) => match decoder.feed(&ep, &bytes) {
                            Ok(chunks) => {
                                for c in chunks {
                                    if tx.send(Ok(c)).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        },
                        Err(e) => {
                            let _ = tx.send(Err(WireError::Http(e.to_string()))).await;
                            return;
                        }
                    }
                }
                if let Ok(chunks) = decoder.finish(&ep) {
                    for c in chunks {
                        let _ = tx.send(Ok(c)).await;
                    }
                }
            });

            Ok(WireStream::new(rx))
        }

        #[cfg(not(any(feature = "netune", feature = "reqwest-oracle")))]
        {
            Err(WireError::Http(
                "No HTTP transport feature enabled for WireClient (enable `netune` or `reqwest-oracle`)".into(),
            ))
        }
    }
}
