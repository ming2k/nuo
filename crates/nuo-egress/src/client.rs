//! Pooled HTTP client executing requests through netune.

use std::sync::Arc;
use std::time::Duration;

use crate::egress::{Egress, HttpResponse};
use crate::error::EgressError;

/// Overall bound for a single non-streaming chat request, applied per request
/// by [`Client::send_json`] — never client-wide and never to streaming.
const CHAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

fn egress_from_env() -> Arc<dyn Egress> {
    owned_egress()
}

/// Build the direct owned egress on netune.
fn owned_egress() -> Arc<dyn Egress> {
    match crate::NuoNetEgress::new() {
        Ok(egress) => Arc::new(egress),
        Err(error) => {
            let egress = crate::NuoNetEgress::from_connector(netune::TcpConnector::new());
            tracing::error!(
                fallback = "plaintext connector; TLS requests will fail at dispatch",
                %error,
                "shared netune client init failed; degrading egress"
            );
            Arc::new(egress)
        }
    }
}

/// Pooled HTTP client executing requests through netune.
pub struct Client {
    egress: Arc<dyn Egress>,
    request_timeout: Duration,
}

impl Client {
    pub fn new() -> Self {
        Self {
            egress: egress_from_env(),
            request_timeout: CHAT_REQUEST_TIMEOUT,
        }
    }

    pub fn with_egress(egress: Arc<dyn Egress>) -> Self {
        Self {
            egress,
            request_timeout: CHAT_REQUEST_TIMEOUT,
        }
    }

    /// The overall timeout non-streaming call sites stamp per request.
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    #[cfg(test)]
    fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Send a fully-built request **without** validating the status.
    pub async fn send_raw(
        &self,
        request: crate::request::RequestBuilder,
        label: &'static str,
    ) -> Result<HttpResponse, EgressError> {
        let parts = request.build(label)?;
        self.egress.send(parts).await
    }

    /// Prime a connection to `url`'s origin into the idle pool.
    pub async fn prewarm(&self, url: &str, _label: &'static str) -> Result<bool, EgressError> {
        self.egress.prewarm(url).await
    }

    /// Send a fully-built request and enforce HTTP success.
    pub async fn send(
        &self,
        request: crate::request::RequestBuilder,
        label: &'static str,
    ) -> Result<HttpResponse, EgressError> {
        let response = self.send_raw(request, label).await?;
        if response.status.is_success() {
            Ok(response)
        } else {
            let status = response.status;
            let retry_after = crate::error::retry_after_ms(&response.headers);
            let message = response.into_text().await.unwrap_or_default();
            Err(EgressError::Http {
                status,
                message,
                retry_after,
            })
        }
    }

    /// Send a fully-built request, enforce success, and decode the body as JSON.
    pub async fn send_json(
        &self,
        request: crate::request::RequestBuilder,
        label: &'static str,
    ) -> Result<serde_json::Value, EgressError> {
        let response = self
            .send(request.timeout(self.request_timeout), label)
            .await?;
        let bytes = response.into_bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| EgressError::Decode {
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

    fn post_json(_client: &Client, url: &str) -> crate::request::RequestBuilder {
        crate::request::RequestBuilder::new(http::Method::POST, url).json(&serde_json::json!({}))
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
