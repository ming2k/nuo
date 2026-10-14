//! Endpoint definition for System One decision models.

use crate::credentials::{CredentialsProvider, StaticApiKey};
use crate::decision::protocol::DecisionProtocol;
use crate::error::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Endpoint configuration for System One decision models (e.g. TypeSafe Jev).
#[derive(Clone)]
pub struct DecisionEndpoint {
    pub protocol: DecisionProtocol,
    pub base_url: String,
    pub credentials: Arc<dyn CredentialsProvider>,
    pub model: String,
    pub timeout: Duration,
    pub custom_headers: HashMap<String, String>,
}

impl std::fmt::Debug for DecisionEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionEndpoint")
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("custom_headers", &self.custom_headers)
            .finish()
    }
}

impl DecisionEndpoint {
    pub fn new(
        protocol: DecisionProtocol,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let mut url = base_url.into();
        if url.ends_with('/') {
            url.pop();
        }
        Self {
            protocol,
            base_url: url,
            credentials: Arc::new(StaticApiKey::new(api_key)),
            model: model.into(),
            timeout: DEFAULT_TIMEOUT,
            custom_headers: HashMap::new(),
        }
    }

    /// Creates an endpoint pre-configured for TypeSafe AI / Jev.
    pub fn typesafe(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            DecisionProtocol::TypeSafeSystemOne,
            "https://api.typesafe.ai/v1",
            api_key,
            model,
        )
    }

    /// Convenience constructor targeting the latest Jev model (`jev-latest`).
    pub fn jev(api_key: impl Into<String>) -> Self {
        Self::typesafe(api_key, "jev-latest")
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let mut url = base_url.into();
        if url.ends_with('/') {
            url.pop();
        }
        self.base_url = url;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.custom_headers.insert(key.into(), value.into());
        self
    }

    pub fn with_credentials_provider(mut self, provider: Arc<dyn CredentialsProvider>) -> Self {
        self.credentials = provider;
        self
    }

    pub async fn resolve_api_key(&self) -> Result<String> {
        self.credentials.get_token().await
    }
}
