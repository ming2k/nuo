//! Endpoint configuration and vendor target definitions.

use crate::credentials::CredentialsProvider;
use crate::error::Result;
use crate::client_identity::ClientProfile;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Supported AI model vendor wire protocol targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderVendor {
    OpenAi,
    Anthropic,
    Google,
    DeepSeek,
    Ollama,
}

use std::sync::Mutex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TransportObservation {
    #[default]
    Unreported,
    PooledConnection,
    ColdConnection,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportTimings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_sent_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_ready_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u64>,
    #[serde(default)]
    pub retransmits: u32,
    #[serde(default)]
    pub observation: TransportObservation,
    #[serde(skip)]
    pub dispatch_at: Option<std::time::Instant>,
}

#[derive(Clone, Default, Debug)]
pub struct TransportTelemetry(Arc<Mutex<Option<TransportTimings>>>);

impl TransportTelemetry {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
    pub fn publish(&self, timings: TransportTimings) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(timings);
    }
    pub fn snapshot(&self) -> Option<TransportTimings> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn read(&self) -> Option<TransportTimings> {
        self.snapshot()
    }
}

/// Target API endpoint configuration.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub vendor: ProviderVendor,
    pub base_url: String,
    pub credentials: Arc<dyn CredentialsProvider>,
    pub model: String,
    pub timeout: Duration,
    pub client_profile: ClientProfile,
    pub custom_headers: HashMap<String, String>,
}

impl Endpoint {
    pub fn new(
        vendor: ProviderVendor,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let mut url = base_url.into();
        if url.ends_with('/') {
            url.pop();
        }
        Self {
            vendor,
            base_url: url,
            credentials: Arc::new(crate::credentials::StaticApiKey::new(api_key)),
            model: model.into(),
            timeout: DEFAULT_TIMEOUT,
            client_profile: ClientProfile::Native,
            custom_headers: HashMap::new(),
        }
    }

    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            ProviderVendor::OpenAi,
            "https://api.openai.com/v1",
            api_key,
            model,
        )
    }

    pub fn anthropic(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            ProviderVendor::Anthropic,
            "https://api.anthropic.com/v1",
            api_key,
            model,
        )
    }

    pub fn deepseek(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            ProviderVendor::DeepSeek,
            "https://api.deepseek.com/v1",
            api_key,
            model,
        )
    }

    pub fn google(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            ProviderVendor::Google,
            "https://generativelanguage.googleapis.com/v1beta",
            api_key,
            model,
        )
    }

    pub fn ollama(model: impl Into<String>) -> Self {
        Self::new(
            ProviderVendor::Ollama,
            "http://localhost:11434/v1",
            "ollama",
            model,
        )
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

    /// Attaches a client fingerprint profile (e.g. Copilot, ClaudeCode, Cursor).
    pub fn with_client_profile(mut self, profile: ClientProfile) -> Self {
        self.client_profile = profile;
        self
    }

    /// Attaches a dynamic credentials provider.
    pub fn with_credentials_provider(mut self, provider: Arc<dyn CredentialsProvider>) -> Self {
        self.credentials = provider;
        self
    }

    /// Resolves the effective API credential token from the credentials provider.
    pub async fn resolve_api_key(&self) -> Result<String> {
        self.credentials.get_token().await
    }
}
