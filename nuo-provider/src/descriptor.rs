//! Static metadata and declared capabilities of a model provider (ADR-0015).

use serde::{Deserialize, Serialize};

/// Immutable descriptor defining a provider's identity, default route, and capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    /// Distinct identifier of the provider (e.g. `"openai"`, `"anthropic"`, `"deepseek"`, `"ollama"`).
    pub id: String,
    /// Human-readable label for UI pickers.
    pub name: String,
    /// Default API root URL.
    pub default_root_url: String,
    /// Default wire transport protocol.
    pub default_protocol: nuo_model_codec::WireProtocol,
    /// Known baseline models supported by this provider.
    pub baseline_models: Vec<String>,
    /// Whether this provider supports prompt caching.
    pub supports_prompt_cache: bool,
    /// Whether this provider supports native reasoning / thinking blocks.
    pub supports_reasoning: bool,
    /// Whether this provider supports multimodal vision inputs.
    pub supports_vision: bool,
    /// Whether this provider surfaces authoritative token usage metrics.
    pub supports_usage: bool,
}

impl ProviderDescriptor {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        default_root_url: impl Into<String>,
        default_protocol: nuo_model_codec::WireProtocol,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            default_root_url: default_root_url.into(),
            default_protocol,
            baseline_models: Vec::new(),
            supports_prompt_cache: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_usage: false,
        }
    }

    pub fn with_models(mut self, models: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.baseline_models = models.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_capabilities(
        mut self,
        prompt_cache: bool,
        reasoning: bool,
        vision: bool,
        usage: bool,
    ) -> Self {
        self.supports_prompt_cache = prompt_cache;
        self.supports_reasoning = reasoning;
        self.supports_vision = vision;
        self.supports_usage = usage;
        self
    }
}
