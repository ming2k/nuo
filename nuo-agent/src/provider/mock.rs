use super::{ModelRequest, ModelResponse, Provider, ProviderDelta};
use crate::error::{AgentError, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Mock provider for testing and deterministic multi-agent simulation.
#[derive(Clone)]
pub struct MockProvider {
    responses: Arc<Mutex<Vec<ModelResponse>>>,
    streaming_queues: Arc<Mutex<Vec<Vec<ProviderDelta>>>>,
    recorded_requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockProvider {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(Vec::new())),
            streaming_queues: Arc::new(Mutex::new(Vec::new())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Queues a response to be returned on subsequent invocations.
    pub async fn push_response(&self, response: ModelResponse) {
        let mut guard = self.responses.lock().await;
        guard.push(response);
    }

    /// Pushes a plain text response.
    pub async fn push_text(&self, text: impl Into<String>) {
        self.push_response(ModelResponse::text(text, 10, 10)).await;
    }

    /// Queues a sequence of streaming deltas for the next `stream` invocation.
    pub async fn push_stream_deltas(&self, deltas: Vec<ProviderDelta>) {
        let mut guard = self.streaming_queues.lock().await;
        guard.push(deltas);
    }

    /// Retrieves all recorded requests sent to this mock provider.
    pub async fn requests(&self) -> Vec<ModelRequest> {
        self.recorded_requests.lock().await.clone()
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<futures::channel::mpsc::Receiver<Result<ProviderDelta>>> {
        {
            let mut req_guard = self.recorded_requests.lock().await;
            req_guard.push(request);
        }

        let mut guard = self.streaming_queues.lock().await;
        if !guard.is_empty() {
            let deltas = guard.remove(0);
            let (mut tx, rx) = futures::channel::mpsc::channel(64);
            use futures::SinkExt;
            tokio::spawn(async move {
                for d in deltas {
                    let _ = tx.send(Ok(d)).await;
                }
            });
            Ok(rx)
        } else {
            let mut resp_guard = self.responses.lock().await;
            if resp_guard.is_empty() {
                Err(AgentError::Provider(
                    "mock provider has no queued responses or streams".into(),
                ))
            } else {
                let resp = resp_guard.remove(0);
                let (mut tx, rx) = futures::channel::mpsc::channel(1);
                use futures::SinkExt;
                tokio::spawn(async move {
                    let _ = tx.send(Ok(ProviderDelta::from_response(resp))).await;
                });
                Ok(rx)
            }
        }
    }
}
