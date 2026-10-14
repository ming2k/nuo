//! Model catalog discovery interfaces, query types, and update envelopes (ADR-0015).

use async_trait::async_trait;
use nuo_model_codec::{CatalogShape, SecretString};

pub use nuo_model_codec::catalog::{DiscoveredModel, ModelListError};

/// Everything a live catalog request needs.
#[derive(Clone)]
pub struct RemoteCatalogRequest<'a> {
    pub protocol: CatalogShape,
    pub base_url: &'a str,
    pub api_key: &'a SecretString,
    pub account_id: Option<&'a str>,
    pub org_id: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    pub extra_headers: &'a [(&'a str, &'a str)],
    pub catalog_signing: Option<&'a dyn CatalogSigning>,
    pub dimensions: &'a [(&'a str, &'a str)],
}

/// Conditional revalidation inputs for remote-catalog revalidation (RFC 7232).
#[derive(Debug, Clone, Copy, Default)]
pub struct RemoteCatalogOptions<'a> {
    /// Previously observed response ETag, used for conditional revalidation.
    pub etag: Option<&'a str>,
}

/// A catalog request signature.
#[derive(Debug, Clone)]
pub struct CatalogSignature {
    pub authorization: String,
    pub date: String,
    pub key: String,
}

/// Dialect-signed catalog transport signer port.
pub trait CatalogSigning: Send + Sync {
    fn identity_headers(&self) -> Vec<(String, String)>;
    fn identity_subject_headers(&self) -> Vec<(String, String)>;
    fn sign(&self, signed_path: &str) -> Result<CatalogSignature, String>;
}

/// Result of a cache-aware remote-catalog request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCatalogUpdate {
    /// The endpoint returned a new catalog payload.
    Modified {
        models: Vec<DiscoveredModel>,
        etag: Option<String>,
    },
    /// The endpoint confirmed that the cached payload is still current.
    NotModified { etag: Option<String> },
}

/// Port for fetching remote model catalogs.
#[async_trait]
pub trait CatalogDiscovery: Send + Sync {
    /// Fetch remote catalog with conditional revalidation options.
    async fn fetch_remote_catalog(
        &self,
        request: RemoteCatalogRequest<'_>,
        options: RemoteCatalogOptions<'_>,
    ) -> Result<RemoteCatalogUpdate, ModelListError>;
}

static CATALOG_DISCOVERY: std::sync::OnceLock<Box<dyn CatalogDiscovery>> =
    std::sync::OnceLock::new();

/// Register the global catalog discovery implementation.
pub fn register_catalog_discovery(discovery: Box<dyn CatalogDiscovery>) {
    let _ = CATALOG_DISCOVERY.set(discovery);
}

/// Global helper dispatching to the registered catalog discovery engine.
pub async fn fetch_remote_catalog(
    request: RemoteCatalogRequest<'_>,
    options: RemoteCatalogOptions<'_>,
) -> Result<RemoteCatalogUpdate, ModelListError> {
    if let Some(discovery) = CATALOG_DISCOVERY.get() {
        discovery.fetch_remote_catalog(request, options).await
    } else {
        Err(ModelListError::BadEndpoint(
            "no CatalogDiscovery registered in nuo-provider".to_string(),
        ))
    }
}

use crate::credentials::CredentialStore;

pub type CatalogSignerBuilder =
    Box<dyn Fn(&dyn CredentialStore, &str, &str) -> Option<Box<dyn CatalogSigning>> + Send + Sync>;

static CATALOG_SIGNER_BUILDER: std::sync::RwLock<Option<CatalogSignerBuilder>> =
    std::sync::RwLock::new(None);

pub fn register_catalog_signer_builder(builder: CatalogSignerBuilder) {
    *CATALOG_SIGNER_BUILDER.write().unwrap() = Some(builder);
}

pub fn build_catalog_signer(
    store: &dyn CredentialStore,
    connection_id: &str,
    bearer: &str,
) -> Option<Box<dyn CatalogSigning>> {
    if let Some(builder) = CATALOG_SIGNER_BUILDER.read().unwrap().as_ref() {
        builder(store, connection_id, bearer)
    } else {
        None
    }
}

pub fn catalog_root_for_connection(
    store: &dyn CredentialStore,
    connection_id: &str,
) -> Option<String> {
    let tokens = store.read(connection_id).ok()??;
    let val: serde_json::Value = tokens.get_json_attr("qoder")?;
    val.get("elected_endpoint")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            val.get("endpoint")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
}
