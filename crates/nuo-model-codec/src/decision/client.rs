//! Decision client executing System One evaluations.

use crate::client::WireClient;
use crate::decision::endpoint::DecisionEndpoint;
use crate::decision::types::{DecisionRequest, DecisionResponse};
use crate::error::Result;

/// Specialized client for executing typed System One decision evaluations.
#[derive(Clone, Default)]
pub struct DecisionClient {
    wire: WireClient,
}

impl DecisionClient {
    /// Creates a new decision client with standard connection pooling.
    pub fn new() -> Self {
        Self {
            wire: WireClient::new(),
        }
    }

    /// Creates a decision client sharing an existing [`WireClient`] connection pool.
    pub fn with_wire(wire: WireClient) -> Self {
        Self { wire }
    }

    /// Evaluates a [`DecisionRequest`] against target [`DecisionEndpoint`].
    pub async fn decide(
        &self,
        endpoint: &DecisionEndpoint,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse> {
        self.wire.execute_decision(endpoint, request).await
    }
}
