//! Provider usage and quota querying.
//!
//! Providers differ in their quota policies, billing models, and balance API endpoints:
//! - DeepSeek exposes `GET /user/balance` with currency, total, granted, and topped-up balances.
//! - Kimi (Moonshot) exposes `GET /v1/users/me/balance` with available, cash, and voucher balances.
//! - OpenRouter exposes `GET /api/v1/auth/key` with usage, credit limit, and rate limits.
//! - SiliconFlow exposes `GET /v1/user/info` with total and charge balances.
//!
//! This module defines an extensible trait [`ProviderUsageFetcher`] and a unified dispatcher
//! [`fetch_provider_usage`] that translates provider-specific responses into the generic
//! [`nuo_wire::ProviderUsage`] model.

use nuo_wire::async_trait;
use nuo_wire::{ConnectionUsageState, ProviderUsage};

mod antigravity;
mod commandcode;
mod deepseek;
mod kimi;
mod openrouter;
pub use nuo_provider_qoder::usage::QoderUsageFetcher;
mod siliconflow;

pub use antigravity::{
    AntigravityQuotaBucket, AntigravityQuotaGroup, AntigravityQuotaSummaryResponse,
    AntigravityUsageFetcher,
};
pub use commandcode::CommandCodeUsageFetcher;
pub use deepseek::DeepSeekUsageFetcher;
pub use kimi::KimiUsageFetcher;
pub use openrouter::OpenRouterUsageFetcher;
pub use siliconflow::SiliconFlowUsageFetcher;

#[async_trait]
impl ProviderUsageFetcher for QoderUsageFetcher {
    fn matches(&self, provider: &str, base_url: &str) -> bool {
        self.matches(provider, base_url)
    }

    async fn fetch_usage(
        &self,
        client: &crate::http::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        self.fetch_usage(client, base_url, api_key).await
    }
}

/// Trait implemented by provider-specific usage / quota fetchers.
#[async_trait]
pub trait ProviderUsageFetcher: Send + Sync {
    /// Whether this fetcher handles the given model provider or base URL.
    fn matches(&self, provider: &str, base_url: &str) -> bool;

    /// Fetch and normalize usage/quota data from the provider endpoint.
    async fn fetch_usage(
        &self,
        client: &crate::http::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String>;
}

/// Registry of built-in provider usage fetchers.
pub fn registered_fetchers() -> &'static [&'static dyn ProviderUsageFetcher] {
    &[
        &AntigravityUsageFetcher,
        &CommandCodeUsageFetcher,
        &DeepSeekUsageFetcher,
        &KimiUsageFetcher,
        &OpenRouterUsageFetcher,
        &QoderUsageFetcher,
        &SiliconFlowUsageFetcher,
    ]
}

/// Query provider usage for a connection based on its model provider or endpoint URL.
pub async fn fetch_provider_usage(
    provider: &str,
    base_url: &str,
    api_key: &str,
) -> ConnectionUsageState {
    let Ok(client) = crate::http::Http::control_plane() else {
        return ConnectionUsageState::Error("could not build the HTTP client".to_string());
    };
    fetch_provider_usage_with_client(&client, provider, base_url, api_key).await
}

/// Query provider usage using an explicit client handle.
pub async fn fetch_provider_usage_with_client(
    client: &crate::http::Http,
    provider: &str,
    base_url: &str,
    api_key: &str,
) -> ConnectionUsageState {
    let key = api_key.trim();
    if key.is_empty() {
        return ConnectionUsageState::Error("API key is not configured".to_string());
    }

    for fetcher in registered_fetchers() {
        if fetcher.matches(provider, base_url) {
            return match fetcher.fetch_usage(client, base_url, key).await {
                Ok(usage) => ConnectionUsageState::Available(Box::new(usage)),
                Err(err) => ConnectionUsageState::Error(err),
            };
        }
    }

    ConnectionUsageState::Unsupported
}
