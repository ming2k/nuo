//! Endpoint configuration and vendor target definitions.

use crate::credentials::CredentialsProvider;
use crate::error::Result;
use crate::identity::ClientProfile;
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
