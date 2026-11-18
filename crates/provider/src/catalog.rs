//! Model catalog discovery interfaces, query types, and update envelopes (ADR-0015).

use async_trait::async_trait;
use host::SecretString;
use protocol::surface::RemoteCatalogSource;

pub use protocol::dialects::{
    AnthropicMessagesDialect, Channel, DiscoveredModel, GoogleInteractionsDialect, ModelListError,
    ProviderEntry, Transport, clean_model_display_name, format_model_display_name,
};

/// Everything a live catalog request needs.
#[derive(Clone)]
pub struct RemoteCatalogRequest<'a> {
    pub source: RemoteCatalogSource,
    pub base_url: &'a str,
    pub api_key: &'a SecretString,
    pub account_id: Option<&'a str>,
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
            "no CatalogDiscovery registered in provider".to_string(),
        ))
    }
}

use crate::credentials::CredentialStore;

/// Builds the dialect signer a custom catalog source needs, if any.
///
/// The composition root owns provider knowledge: it receives the source and
/// the connection identity, and returns a signer only for a source that
/// actually signs. A `None` means the request is unsigned.
pub type CatalogSignerBuilder = Box<
    dyn Fn(&dyn CredentialStore, &str, &str, RemoteCatalogSource) -> Option<Box<dyn CatalogSigning>>
        + Send
        + Sync,
>;

static CATALOG_SIGNER_BUILDER: std::sync::RwLock<Option<CatalogSignerBuilder>> =
    std::sync::RwLock::new(None);

pub fn register_catalog_signer_builder(builder: CatalogSignerBuilder) {
    *CATALOG_SIGNER_BUILDER
        .write()
        .unwrap_or_else(|error| error.into_inner()) = Some(builder);
}

pub fn build_catalog_signer(
    store: &dyn CredentialStore,
    connection_id: &str,
    bearer: &str,
    source: RemoteCatalogSource,
) -> Option<Box<dyn CatalogSigning>> {
    if let Some(builder) = CATALOG_SIGNER_BUILDER
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
    {
        builder(store, connection_id, bearer, source)
    } else {
        None
    }
}

/// Append URL-encoded query parameters to an endpoint URL.
pub fn append_query(url: &str, params: &[(&str, &str)]) -> String {
    if params.is_empty() {
        return url.to_string();
    }
    let mut out = String::from(url);
    out.push(if out.contains('?') { '&' } else { '?' });
    for (index, (name, value)) in params.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&crate::transport::encode_component(name));
        out.push('=');
        out.push_str(&crate::transport::encode_component(value));
    }
    out
}

/// Shared `GET {root}/models` bearer fetch used by OpenAI-compatible catalog fetchers.
pub async fn fetch_bearer_models_response(
    req: &RemoteCatalogRequest<'_>,
    options: RemoteCatalogOptions<'_>,
) -> Result<crate::transport::Reply, ModelListError> {
    let root = protocol::ApiRoot::parse(req.base_url).map_err(ModelListError::BadEndpoint)?;
    let endpoint = root.append("models");
    let user_agent = req.user_agent.unwrap_or(crate::NUO_USER_AGENT);
    let mut request = crate::transport::Request::new(netune::Method::GET, &endpoint)
        .header("user-agent", user_agent);
    if !req.api_key.expose_secret().trim().is_empty() {
        request = request.header(
            "authorization",
            format!("Bearer {}", req.api_key.expose_secret()),
        );
    }
    if let Some(etag) = options.etag {
        request = request.header("if-none-match", etag);
    }
    for (name, value) in req.extra_headers {
        request = request.header(name, *value);
    }
    crate::transport::Http::control_plane()
        .map_err(ModelListError::Http)?
        .send(request)
        .await
        .map_err(ModelListError::Http)
}

/// Shared HTTP 304 / status / JSON / deduplication tail for catalog fetchers.
pub fn finalize_catalog_response(
    response: crate::transport::Reply,
    options: RemoteCatalogOptions<'_>,
    sort_by_id: bool,
    parse: impl FnOnce(&serde_json::Value) -> Result<Vec<DiscoveredModel>, ModelListError>,
) -> Result<RemoteCatalogUpdate, ModelListError> {
    let status = response.status;
    let response_etag = response
        .headers
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if status == http::StatusCode::NOT_MODIFIED {
        return Ok(RemoteCatalogUpdate::NotModified {
            etag: response_etag.or_else(|| options.etag.map(str::to_string)),
        });
    }
    if !status.is_success() {
        return Err(ModelListError::Status(status.as_u16(), response.body));
    }

    let json: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|e| ModelListError::Parse(format!("response is not valid JSON: {e}")))?;

    let mut models = parse(&json)?;
    if sort_by_id {
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
    } else {
        let mut seen = std::collections::HashSet::new();
        models.retain(|model| seen.insert(model.id.clone()));
    }
    Ok(RemoteCatalogUpdate::Modified {
        models,
        etag: response_etag,
    })
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
