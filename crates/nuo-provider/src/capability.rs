//! Orthogonal capability contracts for model providers (ADR-0015).

use async_trait::async_trait;

pub use nuo_model_codec::Provider;
pub use nuo_model_codec::catalog::{DiscoveredModel, ModelListError};

/// Optional orthogonal capability: remote model catalog discovery (`/models` endpoint).
#[async_trait]
pub trait CatalogDiscovery: Send + Sync {
    /// Discover available models from the provider endpoint.
    async fn list_models(&self) -> Result<Vec<DiscoveredModel>, ModelListError>;
}

/// Optional orthogonal capability: token quota, credit balance, and usage querying.
#[async_trait]
pub trait QuotaTracker: Send + Sync {
    /// Fetch token usage or account balance summary.
    async fn fetch_quota(&self) -> Result<nuo_model_codec::TokenUsage, String>;
}
