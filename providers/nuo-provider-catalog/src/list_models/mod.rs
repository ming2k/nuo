//! Live remote-catalog fetch from each provider's API.
//!
//! A connection created from a preset can either mirror the
//! provider's *compiled-in* model list ([`nuo_provider::ModelProviderSpec`])
//! or fetch the list *live* from the provider's own `GET /models` endpoint.
//! This module owns the live path: it speaks the three wire protocols
//! (`openai` / `anthropic` / `google`), authenticates the same way a chat
//! request would, and parses the returned model entries. Beyond the id,
//! endpoints may advertise per-model capability hints (context length,
//! reasoning, image input, effort tiers — the Kimi Code platform is the rich
//! case); these ride along on [`DiscoveredModel`] as `Option`s, and the
//! catalog decides per template whether to trust and persist them.
//!
//! ## Source selection
//!
//! The preset's `RemoteCatalogSource` or the connection override selects one
//! source. An endpoint catalog returns advertised metadata for reconciliation;
//! transport, status and schema failures retain the previous connection list.
//! A valid empty endpoint catalog is authoritative. Sources set to `None`
//! use the compiled baseline without a network fetch.
//!
//! ## Protocol details
//!
//! - **OpenAI-compatible** (OpenAI, DeepSeek, xAI, Kimi, Z.AI, sub2api
//!   relays): `GET {base}/v1/models`, `Authorization: Bearer <key>`, body
//!   `{data: [{id}, …]}`. Auth matches the chat path: a keyless relay sends
//!   no bearer header at all.
//! - **Anthropic**: `GET {base}/v1/models`, `x-api-key` + `anthropic-version`,
//!   body `{data: [{id}, …]}`.
//! - **Google native**: `GET {base}/v1beta/models`, `x-goog-api-key: <key>`,
//!   body `{models: [{name: "models/<id>", supportedGenerationMethods: […]}, …]}`
//!   — only `generateContent`-capable text models are kept.
//! - **Google Antigravity (cloudcode)**: `POST {base}/v1internal:fetchAvailableModels`,
//!   bearer `Authorization` when a key is set — a distinct scheme from the
//!   Google native surface (see [`CatalogShape::GoogleCloudCode`]).
//! - **ChatGPT Codex**: `GET {base}/backend-api/codex/models` with
//!   `client_version` + `originator` headers.
//! - **OpenCode Console**: `GET {root}/api/config` with bearer session token +
//!   `x-org-id` workspace header; body `{config: {provider: {opencode:
//!   {npm, models}}}}`, where each model may override its wire (`provider.npm`)
//!   and inference root (`provider.api`).
//!
//! ## Endpoint derivation
//!
//! The chat endpoint a channel already carries is the source of truth; this
//! module strips the path suffix (`/chat/completions`, `/messages`, or the
//! bare `/v1beta` root) and re-appends the models path. A caller that already
//! has a bare API root can pass it directly.
pub use nuo_protocol::CatalogShape;

/// Everything a live catalog request needs, borrowed from the instance's
/// first channel. Fields mirror what a chat request would use so the auth
/// matches exactly.
mod fetch;
mod parsers;

#[cfg(test)]
mod tests;

use nuo_provider::catalog::{
    DiscoveredModel, ModelListError, RemoteCatalogOptions, RemoteCatalogRequest,
    RemoteCatalogUpdate,
};

pub use self::fetch::{fetch_remote_catalog, list_models};
#[allow(unused_imports)]
pub use self::parsers::{CatalogParser, parser_for, parser_for_with_dimensions};

#[cfg(test)]
pub(crate) use self::parsers::{clean_display_label, deepseek_accepted_efforts, parse_models};
pub(crate) use self::parsers::{parse_models_with_dimensions, validate_catalog_shape};

fn append_query(url: &str, params: &[(&str, &str)]) -> String {
    let mut out = String::from(url);
    out.push(if out.contains('?') { '&' } else { '?' });
    for (index, (name, value)) in params.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&nuo_provider::transport::encode_component(name));
        out.push('=');
        out.push_str(&nuo_provider::transport::encode_component(value));
    }
    out
}

/// Derive the `GET /models` endpoint from an API root URL.
///
/// (Deterministic Root URL Algebra), catalog paths are derived
/// directly from the API root: `models_url = root_url + relative_catalog_path`.
/// Callers pass the provider's API root (never a full inference path); suffix
/// stripping is forbidden by `[INV-ROUTE-01]`.
///
/// Returns [`ModelListError::BadEndpoint`] only for an invalid base.
pub fn models_endpoint_for(
    protocol: CatalogShape,
    base_url: &str,
) -> Result<String, ModelListError> {
    let root = nuo_protocol::ApiRoot::parse(base_url).map_err(ModelListError::BadEndpoint)?;
    Ok(root.append(protocol.path()))
}
