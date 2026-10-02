//! Shared resolution of route defaults and per-request prompt-cache intent.

use nuo_contracts::{
    ModelRequest, PromptCacheCapabilities, PromptCacheModePreference, PromptCachePreference,
    ResolvedCachePolicy,
};

/// Immutable prompt-cache policy attached to one concrete provider route.
#[derive(Debug, Clone)]
pub struct PromptCacheConfig {
    capabilities: PromptCacheCapabilities,
    route_preference: PromptCachePreference,
    routing_key: Option<String>,
}

impl Default for PromptCacheConfig {
    fn default() -> Self {
        Self {
            capabilities: PromptCacheCapabilities::unsupported(),
            route_preference: PromptCachePreference::default(),
            routing_key: None,
        }
    }
}

impl PromptCacheConfig {
    pub fn new(
        capabilities: PromptCacheCapabilities,
        route_preference: PromptCachePreference,
        routing_key: Option<String>,
    ) -> Self {
        Self {
            capabilities,
            route_preference,
            routing_key,
        }
    }

    /// The routing key (session identifier) attached to this prompt-cache configuration, if any.
    pub fn routing_key(&self) -> Option<&str> {
        self.routing_key.as_deref()
    }

    /// Merge a request override onto the route default, then validate the
    /// exact result against this route's declared capabilities.
    pub fn resolve(&self, request: &ModelRequest) -> Result<ResolvedCachePolicy, String> {
        let request_preference = request.prompt_cache_preference;
        let preference = PromptCachePreference {
            mode: if request_preference.mode == PromptCacheModePreference::ProviderDefault {
                self.route_preference.mode
            } else {
                request_preference.mode
            },
            retention: request_preference
                .retention
                .or(self.route_preference.retention),
        };
        self.capabilities
            .resolve(preference, self.routing_key.clone())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_contracts::{CacheRetention, PromptCacheMode};

    #[test]
    fn request_retention_overrides_route_retention_without_losing_route_mode() {
        let config = PromptCacheConfig::new(
            PromptCacheCapabilities {
                modes: vec![PromptCacheMode::Automatic],
                default_mode: Some(PromptCacheMode::Automatic),
                supported_retentions: vec![CacheRetention::FiveMinutes, CacheRetention::OneHour],
                default_retention: Some(CacheRetention::FiveMinutes),
                disable_supported: true,
                routing_key_supported: false,
                max_breakpoints: None,
                min_cacheable_tokens: None,
                reports_reads: true,
                reports_writes: true,
                reports_misses: false,
            },
            PromptCachePreference {
                mode: PromptCacheModePreference::Automatic,
                retention: Some(CacheRetention::FiveMinutes),
            },
            None,
        );
        let request =
            ModelRequest::new(Vec::new()).with_prompt_cache_preference(PromptCachePreference {
                mode: PromptCacheModePreference::ProviderDefault,
                retention: Some(CacheRetention::OneHour),
            });
        assert_eq!(
            config.resolve(&request).unwrap(),
            ResolvedCachePolicy::Enabled {
                mode: PromptCacheMode::Automatic,
                retention: Some(CacheRetention::OneHour),
                routing_key: None,
                max_breakpoints: None,
            }
        );
    }
}
