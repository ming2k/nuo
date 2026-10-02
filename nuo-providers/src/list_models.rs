//! Live remote-catalog fetch from each provider's API.
//!
//! A connection created from a preset can either mirror the
//! provider's *compiled-in* model list ([`crate::registry::ModelProviderSpec`])
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
//! - **Google native**: `GET {base}/v1beta/models?key=<key>`,
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

use std::collections::HashSet;

use nuo_contracts::{
    Availability, ReasoningSupport, RemoteModelMetadata, SecretString, WireProtocol,
};
use serde_json::Value;

pub use nuo_contracts::CatalogShape;

/// Everything a live catalog request needs, borrowed from the instance's
/// first channel. Fields mirror what a chat request would use so the auth
/// matches exactly.
#[derive(Clone)]
pub struct RemoteCatalogRequest<'a> {
    pub protocol: CatalogShape,
    /// The channel's chat endpoint base URL (e.g.
    /// `https://api.openai.com/v1/chat/completions`). The models path is
    /// derived from it via [`models_endpoint_for`].
    pub base_url: &'a str,
    pub api_key: &'a SecretString,
    /// Optional account ID associated with OAuth tokens (e.g. ChatGPT-Account-Id).
    pub account_id: Option<&'a str>,
    /// Optional workspace/org ID for org-scoped catalogs (OpenCode Console's
    /// `x-org-id`, ADR-0269). `None` sends no org header.
    pub org_id: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    /// Extra request headers a provider requires beyond standard auth —
    /// e.g. GitHub Copilot's `x-initiator` / `Openai-Intent` /
    /// `X-GitHub-Api-Version`. Empty for stock OpenAI/Anthropic/Google.
    /// Applied to every protocol; a provider that needs per-header logic can
    /// still set them here since the fetch is read-only (GET).
    pub extra_headers: &'a [(&'a str, &'a str)],
    /// The dialect's catalog signing, when the shape's auth is `Dialect`. The
    /// catalog sync layer supplies it; this fetcher never names a provider.
    pub catalog_signing: Option<&'a dyn CatalogSigning>,
    /// The request dimensions that select *which* catalog the server returns
    /// ([`CatalogShape::dimensions`]), already resolved with any
    /// connection-level override. Empty for dimension-free shapes.
    pub dimensions: &'a [(&'a str, &'a str)],
}

/// Conditional revalidation inputs for remote-catalog revalidation (RFC 7232).
#[derive(Debug, Clone, Copy, Default)]
pub struct RemoteCatalogOptions<'a> {
    /// Previously observed response ETag, used for conditional revalidation.
    pub etag: Option<&'a str>,
}

/// A catalog request signature (the COSY bundle: authorization, date, key).
#[derive(Debug, Clone)]
pub struct CatalogSignature {
    pub authorization: String,
    pub date: String,
    pub key: String,
}

/// Dialect-signed catalog transport, supplied by the caller.
///
/// A shape whose [`CatalogAuth`](nuo_contracts::provider_surface::CatalogAuth)
/// is `Dialect` authenticates with the dialect's own request signing. The
/// signer and the identity-headers table are supplied as data by the catalog
/// sync layer (which legitimately knows the provider) so this generic fetcher
/// contains no provider-name branch — the ADR-0260 invariant.
pub trait CatalogSigning: Send + Sync {
    /// The dialect's identity headers (`Cosy-*`, `Login-Version`, …), including
    /// the version header. Read from the owning dialect's declared surface.
    fn identity_headers(&self) -> Vec<(String, String)>;

    /// Headers carrying the connection's identity (e.g. `Cosy-User`), beyond
    /// the static identity table.
    fn identity_subject_headers(&self) -> Vec<(String, String)>;

    /// Sign a signed-path request (empty body — the catalog is a GET).
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

/// Why a live model list could not be obtained. The catalog layer treats every
/// variant the same way — fall back to the compiled-in snapshot — but the
/// distinction is no longer diagnostic-only: [`Self::is_refusal`] separates a
/// durable upstream *refusal* from a transient failure so the connection's
/// status can say which happened (ADR-0273).
#[derive(Debug)]
pub enum ModelListError {
    /// The chat base URL could not be turned into a models URL (e.g. it was
    /// empty or had an unexpected shape).
    BadEndpoint(String),
    /// The HTTP request failed (network/DNS/TLS). Carries the underlying
    /// Transport failure, for logging.
    Http(String),
    /// The API returned a non-2xx status. Carries the status code and body
    /// snippet so a misconfigured key surfaces a readable reason.
    Status(u16, String),
    /// The response body could not be parsed into a model list (missing
    /// `data`/`models`, wrong types). Carries a short description.
    Parse(String),
}

impl ModelListError {
    /// Whether the upstream **refused** the request, as opposed to failing to
    /// serve it (ADR-0273).
    ///
    /// Only an explicit `401`/`403` counts: those are the server saying the
    /// account may not do this. Everything else — including `404`, which for a
    /// catalog fetch usually means a misconfigured path rather than a
    /// revocation, and `429`/`5xx`, which are transient by definition — is
    /// **not** a refusal. Guessing otherwise would be the same inference this
    /// axis exists to forbid (`[INV-AVAIL-02]`).
    pub const fn is_refusal(&self) -> bool {
        matches!(self, Self::Status(401 | 403, _))
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::ModelListError;

    #[test]
    fn only_explicit_unauthorized_and_forbidden_are_refusals() {
        // The server saying "you may not" is the only durable refusal.
        assert!(ModelListError::Status(401, String::new()).is_refusal());
        assert!(ModelListError::Status(403, String::new()).is_refusal());
        // Everything else is transient, and must never be reported as the
        // account being refused: a 404 is a wrong path, a 429/5xx is the
        // server having a bad moment, and a transport error never reached it.
        for code in [400, 404, 408, 429, 500, 502, 503] {
            assert!(
                !ModelListError::Status(code, String::new()).is_refusal(),
                "HTTP {code} must not be reported as a refusal"
            );
        }
        assert!(!ModelListError::Http("dns failure".to_string()).is_refusal());
        assert!(!ModelListError::BadEndpoint("nope".to_string()).is_refusal());
        assert!(!ModelListError::Parse("bad json".to_string()).is_refusal());
    }
}

impl std::fmt::Display for ModelListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadEndpoint(msg) => write!(f, "bad model-list endpoint: {msg}"),
            Self::Http(e) => write!(f, "model-list HTTP request failed: {e}"),
            Self::Status(code, body) => {
                let snippet = body.chars().take(200).collect::<String>();
                write!(f, "model-list request returned HTTP {code}: {snippet}")
            }
            Self::Parse(msg) => write!(f, "could not parse model list: {msg}"),
        }
    }
}

impl std::error::Error for ModelListError {
    // The transport failure is a string now (ADR-0200); there is no inner
    // error to hand out.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/// A model entry discovered from a provider's live `GET /models` list. The
/// `id` is always present; every capability field is `None` when the endpoint
/// does not advertise it. The stock OpenAI/Anthropic/Google shapes carry no
/// capability data. Two rich shapes are recognized: the Kimi Code platform
/// (`api.kimi.com/coding`), advertising flat `context_length` /
/// `supports_reasoning` / `supports_image_in` / `think_efforts` fields per
/// entry, and GitHub Copilot (`api.githubcopilot.com`), advertising the same
/// information nested under `capabilities.{limits,supports}` (see
/// `discovered_model_from_entry`). The catalog decides per preset — via
/// `RemoteCatalogSource` (ADR-0203) — whether these hints are trusted and
/// overlaid onto the baseline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub id: String,
    /// The provider's declared availability for this model (ADR-0273): whether
    /// the account behind the connection may run it, and the provider's own
    /// reason when it gives one. `None` means the endpoint declared no verdict
    /// — never a synthesized one.
    pub availability: Option<Availability>,
    /// The provider's listing intent: whether this model is meant to appear in
    /// a model picker listing. `None` means the endpoint expressed none.
    pub advertised: Option<bool>,
    /// Exact API surface advertised for the model. This is provider-scoped: a
    /// Copilot model can use Messages, Responses, or Chat Completions while the
    /// same id elsewhere uses another route.
    pub protocol: Option<WireProtocol>,
    /// Provider-advertised **API root** override for this model (OpenCode
    /// Console's per-model `provider.api`). Route building appends the wire
    /// suffix through the shared ADR-0259 algebra, so this is a root, never a
    /// full endpoint (ADR-0269).
    pub endpoint: Option<String>,
    /// Provider model family, when advertised.
    pub family: Option<String>,
    /// Human-readable label the endpoint publishes for this model, when it
    /// advertises one (`name`/`display_name`/`displayName`). Presentation
    /// only — never identity, and never required: the surfaces fall back to
    /// the wire id when this is `None`.
    pub name: Option<String>,
    /// Advertised context window in tokens (Kimi's `context_length`, or
    /// Copilot's `capabilities.limits.max_context_window_tokens`).
    pub context_window: Option<usize>,
    /// Maximum generated tokens, when advertised.
    pub max_output_tokens: Option<u32>,
    /// Reasoning support. For Kimi, an explicit `supports_thinking_type`
    /// (`"only"`/`"both"` → true, `"no"` → false) wins over the legacy
    /// `supports_reasoning` boolean. For Copilot, a non-empty
    /// `capabilities.supports.reasoning_effort` list means true.
    pub reasoning: Option<bool>,
    /// The precise reasoning wire representation when advertised. This is
    /// stronger than the coarse [`Self::reasoning`] display flag.
    pub thinking: Option<ReasoningSupport>,
    /// Native tool/function calling support, when advertised.
    pub tool_call: Option<bool>,
    /// Image-input support (Kimi's `supports_image_in`, or Copilot's
    /// `capabilities.supports.vision`).
    pub vision: Option<bool>,
    /// Advertised reasoning-effort tiers (Kimi's
    /// `think_efforts.valid_efforts`, or Copilot's
    /// `capabilities.supports.reasoning_effort`).
    pub effort_levels: Option<Vec<String>>,
    /// The catalog the model came from, as the provider names it (Qoder's
    /// `source`: `"system"` / `"custom"`). Round-tripped rather than derived,
    /// because a signed surface may carry it on the wire as part of the
    /// request's model identity.
    pub catalog_source: Option<String>,
}

impl DiscoveredModel {
    /// Convert live provider facts into the persisted channel-scoped snapshot.
    /// `None` fields intentionally remain absent so the static baseline may
    /// provide a conservative fallback for fields the endpoint does not expose.
    pub fn remote_metadata(&self) -> RemoteModelMetadata {
        RemoteModelMetadata {
            protocol: self.protocol,
            endpoint: self.endpoint.clone(),
            family: self.family.clone(),
            name: self
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty() && *name != self.id.as_str())
                .map(str::to_string),
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            thinking: self.thinking.or_else(|| {
                self.reasoning.map(|reasoning| {
                    if reasoning {
                        ReasoningSupport::ReasoningContent
                    } else {
                        ReasoningSupport::None
                    }
                })
            }),
            tool_call: self.tool_call,
            vision: self.vision,
            effort_levels: self.effort_levels.as_ref().map(|levels| {
                // Non-dropping parse: a known rung becomes Known, anything else
                // becomes Other carrying the raw wire string — a provider tier
                // outside the vocabulary is preserved verbatim (ADR-0065)
                // rather than silently dropped.
                levels
                    .iter()
                    .map(|level| nuo_contracts::EffortLevel::parse(level))
                    .collect()
            }),
            catalog_source: self.catalog_source.clone(),
            availability: self.availability.clone(),
            advertised: self.advertised,
        }
    }
}

/// Append `key=value` pairs to a URL, percent-encoding as needed.
fn append_query(url: &str, params: &[(&str, &str)]) -> String {
    let mut out = String::from(url);
    out.push(if out.contains('?') { '&' } else { '?' });
    for (index, (name, value)) in params.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&crate::http::encode_component(name));
        out.push('=');
        out.push_str(&crate::http::encode_component(value));
    }
    out
}

/// Derive the `GET /models` endpoint from an API root URL.
///
/// Under ADR-0259 (Deterministic Root URL Algebra), catalog paths are derived
/// directly from the API root: `models_url = root_url + relative_catalog_path`.
/// Callers pass the provider's API root (never a full inference path); suffix
/// stripping is forbidden by `[INV-ROUTE-01]`.
///
/// Returns [`ModelListError::BadEndpoint`] only for an invalid base.
pub fn models_endpoint_for(
    protocol: CatalogShape,
    base_url: &str,
) -> Result<String, ModelListError> {
    let root = nuo_contracts::ApiRoot::parse(base_url).map_err(ModelListError::BadEndpoint)?;
    Ok(root.append(protocol.path()))
}

/// Fetch the live model list for `req`. Pure network + parse; the fallback
/// decision lives in the caller. Sorted + de-duplicated by id so the
/// resulting channel set is stable across runs regardless of API ordering.
///
/// A structurally valid empty catalog is returned as an empty `Ok`: the remote
/// source is authoritative and may legitimately report that an account has no
/// currently available models. Malformed shapes remain parse errors.
pub async fn list_models(
    req: RemoteCatalogRequest<'_>,
) -> Result<Vec<DiscoveredModel>, ModelListError> {
    match fetch_remote_catalog(req, RemoteCatalogOptions::default()).await? {
        RemoteCatalogUpdate::Modified { models, .. } => Ok(models),
        RemoteCatalogUpdate::NotModified { .. } => Err(ModelListError::Parse(
            "endpoint returned 304 without a conditional request".to_string(),
        )),
    }
}

/// Fetch or conditionally revalidate a live model catalog. Unlike
/// [`list_models`], this retains the response ETag and represents HTTP 304
/// without forcing callers to discard their cached catalog.
pub async fn fetch_remote_catalog(
    req: RemoteCatalogRequest<'_>,
    options: RemoteCatalogOptions<'_>,
) -> Result<RemoteCatalogUpdate, ModelListError> {
    let endpoint = models_endpoint_for(req.protocol, req.base_url)?;
    let user_agent = req.user_agent.unwrap_or(crate::NUO_USER_AGENT);

    let client = crate::http::Http::control_plane().map_err(ModelListError::Http)?;

    let response = match req.protocol {
        CatalogShape::SceneMap => {
            // The catalog rides the dialect's own request signing. The signer
            // and the identity headers are supplied by the catalog sync layer as
            // data, so this fetcher names no provider (ADR-0260).
            let signing = req.catalog_signing.ok_or_else(|| {
                ModelListError::Parse(
                    "the signed catalog shape requires a catalog signer".to_string(),
                )
            })?;
            let signed_path = req.protocol.signed_path().ok_or_else(|| {
                ModelListError::Parse("catalog shape declares no signed path".to_string())
            })?;
            let url = append_query(&endpoint, req.protocol.query());
            let signed = signing.sign(signed_path).map_err(ModelListError::Parse)?;
            let mut request = crate::http::Request::new(netune::Method::GET, &url)
                .header("user-agent", user_agent)
                .header("authorization", signed.authorization)
                .header("cosy-date", signed.date)
                .header("cosy-key", signed.key);
            for (name, value) in signing.identity_subject_headers() {
                request = request.header(name.as_str(), value);
            }
            for (name, value) in signing.identity_headers() {
                request = request.header(name.as_str(), value);
            }
            for (name, value) in req.extra_headers {
                request = request.header(*name, *value);
            }
            crate::http::Http::control_plane()
                .map_err(ModelListError::Http)?
                .send(request)
                .await
                .map_err(ModelListError::Http)?
        }
        CatalogShape::OpenAi => {
            // OpenAI auth: a bearer when a key is set, NO header when keyless
            // (some relays reject a malformed bearer). Mirrors the chat path.
            let mut request = crate::http::Request::new(netune::Method::GET, &endpoint)
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
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::Codex => {
            // ChatGPT Codex models catalog: requires client_version query param,
            // originator header, and optional ChatGPT-Account-Id header.
            let endpoint = append_query(
                &endpoint,
                &[(
                    "client_version",
                    nuo_contracts::client_identity::CODEX_VERSION,
                )],
            );
            let mut request = crate::http::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent)
                .header("originator", "codex_cli_rs");
            if !req.api_key.expose_secret().trim().is_empty() {
                request = request.header(
                    "authorization",
                    format!("Bearer {}", req.api_key.expose_secret()),
                );
            }
            if let Some(account_id) = req.account_id {
                request = request.header("chatgpt-account-id", account_id);
            }
            if let Some(etag) = options.etag {
                request = request.header("if-none-match", etag);
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::Anthropic => {
            // Anthropic auth: x-api-key + the pinned API version. The version
            // header is mandatory on every Anthropic request including the
            // models list endpoint.
            let mut request = crate::http::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent)
                .header("x-api-key", req.api_key.expose_secret())
                .header("anthropic-version", anthropic_version());
            if req.api_key.expose_secret().trim().is_empty() {
                // A keyless request still sends the headers (harmless) but
                // most Anthropic relays require a key; the snapshot fallback
                // covers the keyless-misconfigured case.
                request = request.header("x-api-key", "");
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::GoogleCloudCode => {
            let mut request = crate::http::Request::new(netune::Method::POST, &endpoint)
                .header("user-agent", user_agent)
                .header("x-goog-api-client", nuo_contracts::client_identity::ANTIGRAVITY_API_CLIENT_HEADER)
                .json(&serde_json::json!({ "project": "" }));
            if !req.api_key.expose_secret().trim().is_empty() {
                request = request.header(
                    "authorization",
                    format!("Bearer {}", req.api_key.expose_secret()),
                );
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::Google => {
            // Google auth: the key is a query param, never a header. A keyless
            // request omits it entirely (Google rejects keyless, but a relay
            // might not require it).
            let endpoint = if req.api_key.expose_secret().trim().is_empty() {
                endpoint.clone()
            } else {
                append_query(&endpoint, &[("key", req.api_key.expose_secret())])
            };
            let mut request = crate::http::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent);
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::OpencodeConsole => {
            // The Console catalog is account-scoped: a bearer session token
            // plus the `x-org-id` workspace header — the catalog's own spelling,
            // distinct from the inference surface's `x-opencode-org-id`. The
            // server answers 400 `OrgRequired` without an org selection.
            let mut request = crate::http::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent)
                .header("accept", "application/json");
            if !req.api_key.expose_secret().trim().is_empty() {
                request = request.header(
                    "authorization",
                    format!("Bearer {}", req.api_key.expose_secret()),
                );
            }
            if let Some(org_id) = req.org_id {
                request = request.header("x-org-id", org_id);
            }
            if let Some(etag) = options.etag {
                request = request.header("if-none-match", etag);
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
    };

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

    let body = response.body;
    let json: Value = serde_json::from_str(&body)
        .map_err(|e| ModelListError::Parse(format!("response is not valid JSON: {e}")))?;

    validate_catalog_shape(req.protocol, &json)?;
    let mut models = parse_models_with_dimensions(req.protocol, &json, req.dimensions);
    if req.protocol == CatalogShape::Codex {
        // Codex's order is semantic (the endpoint's `priority` order), so
        // preserve it while discarding duplicate slugs.
        let mut seen = HashSet::new();
        models.retain(|model| seen.insert(model.id.clone()));
    } else {
        // Stable order regardless of API ordering: sort by id then de-dup.
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
    }
    Ok(RemoteCatalogUpdate::Modified {
        models,
        etag: response_etag,
    })
}

/// The pinned Anthropic API version sent on every request. Mirrors the chat
/// request header so a relay that pins a version accepts the models call too.
fn anthropic_version() -> &'static str {
    crate::protocol::anthropic::request::ANTHROPIC_VERSION
}

/// Strongly-typed catalog parser trait for model list payloads (ADR-0259).
pub trait CatalogParser: Send + Sync {
    /// Parse raw JSON value into standardized discovered models.
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel>;
}

pub struct OpenAiCatalogParser;
impl CatalogParser for OpenAiCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        parse_data_models(json)
    }
}

pub struct AnthropicCatalogParser;
impl CatalogParser for AnthropicCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        parse_data_models(json)
    }
}

pub struct GoogleCatalogParser;
impl CatalogParser for GoogleCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        parse_google_models(json)
    }
}

pub struct CodexCatalogParser;
impl CatalogParser for CodexCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        parse_codex_models(json)
    }
}

pub struct OpencodeConsoleCatalogParser;
impl CatalogParser for OpencodeConsoleCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        crate::registry::opencode::parse_config_catalog(json)
    }
}

/// Parser for Qoder's scene-keyed catalog (`{"<scene>": [{"key": …}]}`).
///
/// The response is a map from scene name to that scene's model entries. The
/// requested scene's array is the authoritative list; the parser reads the
/// scene the connection declared, falling back to `assistant` (the CLI's
/// default) when the response does not carry it.
pub struct SceneMapCatalogParser {
    /// The scene the connection requested.
    pub scene: String,
}

impl CatalogParser for SceneMapCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        crate::registry::qoder::parse_scene_catalog(json, &self.scene)
    }
}

/// Return the typed catalog parser for the given catalog shape (ADR-0259).
///
/// The shape is a parser, not a provider: many providers share one shape and
/// therefore one parser. `dimensions` supplies the request dimensions a
/// shape needs to select *which* catalog to read (Qoder's `scene`).
pub fn parser_for(protocol: CatalogShape) -> Box<dyn CatalogParser> {
    parser_for_with_dimensions(protocol, &[])
}

/// Return the typed catalog parser for a shape, given the request dimensions
/// the connection declared. Shapes that select a sub-catalog by a dimension
/// read it here; dimension-free shapes ignore the argument.
pub fn parser_for_with_dimensions(
    protocol: CatalogShape,
    dimensions: &[(&str, String)],
) -> Box<dyn CatalogParser> {
    match protocol {
        CatalogShape::OpenAi => Box::new(OpenAiCatalogParser),
        CatalogShape::Anthropic => Box::new(AnthropicCatalogParser),
        CatalogShape::Google | CatalogShape::GoogleCloudCode => Box::new(GoogleCatalogParser),
        CatalogShape::Codex => Box::new(CodexCatalogParser),
        CatalogShape::OpencodeConsole => Box::new(OpencodeConsoleCatalogParser),
        CatalogShape::SceneMap => {
            let scene = dimensions
                .iter()
                .find(|(name, _)| *name == "scene")
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| "assistant".to_string());
            Box::new(SceneMapCatalogParser { scene })
        }
    }
}

/// Parse a `GET /models` response body into a list of model entries, per
/// protocol via the typed CatalogParser pipeline (ADR-0259).
///
/// Test-facing shorthand for the dimension-free shapes; the live path calls
/// [`parse_models_with_dimensions`] so a shape can read its request dimensions.
#[cfg(test)]
fn parse_models(protocol: CatalogShape, json: &Value) -> Vec<DiscoveredModel> {
    parser_for(protocol).parse_json(json)
}

/// Parse a catalog response, passing the request dimensions a shape may need
/// to select its sub-catalog (Qoder's `scene`).
fn parse_models_with_dimensions(
    protocol: CatalogShape,
    json: &Value,
    dimensions: &[(&str, &str)],
) -> Vec<DiscoveredModel> {
    let owned: Vec<(&str, String)> = dimensions
        .iter()
        .map(|(name, value)| (*name, (*value).to_string()))
        .collect();
    parser_for_with_dimensions(protocol, &owned).parse_json(json)
}

fn validate_catalog_shape(protocol: CatalogShape, json: &Value) -> Result<(), ModelListError> {
    let valid = match protocol {
        CatalogShape::OpenAi | CatalogShape::Anthropic => {
            json.get("data").is_some_and(Value::is_array)
        }
        CatalogShape::Google | CatalogShape::GoogleCloudCode => json
            .get("models")
            .is_some_and(|models| models.is_array() || models.is_object()),
        CatalogShape::Codex => json.get("models").is_some_and(Value::is_array),
        CatalogShape::OpencodeConsole => json
            .get("config")
            .and_then(|config| config.get("provider"))
            .and_then(|providers| providers.get("opencode"))
            .and_then(|provider| provider.get("models"))
            .is_some_and(Value::is_object),
        // A scene map is an object whose values are arrays. An empty object is
        // structurally valid (an account may have no models in any scene).
        CatalogShape::SceneMap => json
            .as_object()
            .is_some_and(|scenes| scenes.values().all(Value::is_array)),
    };
    valid.then_some(()).ok_or_else(|| {
        ModelListError::Parse(
            match protocol {
                CatalogShape::OpenAi | CatalogShape::Anthropic => {
                    "response is missing the required data array"
                }
                CatalogShape::Google | CatalogShape::Codex => {
                    "response is missing the required models collection"
                }
                CatalogShape::GoogleCloudCode => "response is missing the required models map",
                CatalogShape::OpencodeConsole => {
                    "response is missing the required config.provider.opencode models map"
                }
                CatalogShape::SceneMap => "response is not a scene-keyed map of model arrays",
            }
            .to_string(),
        )
    })
}

/// Extract the ChatGPT Codex `{models:[...]}` catalog. The endpoint assigns a
/// numeric priority (lower first), uses `slug` as the request model id, and
/// explicitly marks picker visibility and reasoning tiers.
fn parse_codex_models(json: &Value) -> Vec<DiscoveredModel> {
    let Some(entries) = json.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut models: Vec<(i64, usize, DiscoveredModel)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let id = entry.get("slug").and_then(Value::as_str)?.to_string();
            let effort_levels: Vec<String> = entry
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|level| level.get("effort").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            let reasoning = !effort_levels.is_empty();
            // Two independent declarations, never ANDed into one bit
            // (ADR-0273): `visibility` says whether the model is *listed*, and
            // `supported_in_api` says whether the API will *run* it. The
            // `hidden-helper` case — listed nowhere, yet API-supported — is
            // exactly why they must not be conflated.
            let advertised = entry
                .get("visibility")
                .and_then(Value::as_str)
                .map(|visibility| visibility == "list");
            let availability =
                entry
                    .get("supported_in_api")
                    .and_then(Value::as_bool)
                    .map(|usable| Availability {
                        usable,
                        reason: None,
                    });
            let vision = entry
                .get("input_modalities")
                .and_then(Value::as_array)
                // Codex treats an omitted legacy field as text + image.
                .is_none_or(|modalities| {
                    modalities
                        .iter()
                        .any(|modality| modality.as_str() == Some("image"))
                });
            let context_window = entry
                .get("max_context_window")
                .or_else(|| entry.get("context_window"))
                .and_then(Value::as_i64)
                .and_then(|window| usize::try_from(window).ok());
            Some((
                entry
                    .get("priority")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::MAX),
                index,
                DiscoveredModel {
                    id,
                    availability,
                    advertised,
                    protocol: Some(WireProtocol::Responses),
                    endpoint: None,
                    family: None,
                    name: entry
                        .get("display_name")
                        .or_else(|| entry.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    context_window,
                    max_output_tokens: None,
                    reasoning: Some(reasoning),
                    thinking: Some(if reasoning {
                        ReasoningSupport::ReasoningSummary
                    } else {
                        ReasoningSupport::None
                    }),
                    tool_call: Some(true),
                    vision: Some(vision),
                    effort_levels: Some(effort_levels),
                    catalog_source: None,
                },
            ))
        })
        .collect();
    models.sort_by_key(|(priority, index, _)| (*priority, *index));
    models.into_iter().map(|(_, _, model)| model).collect()
}

/// Extract `data[]` entries. Used by both OpenAI-compat and Anthropic, which
/// share the `{data: [{id}, …]}` shape on their models endpoints. Every
/// capability field is optional: the stock endpoints omit them (yielding
/// `None`), while the Kimi Code platform advertises the full set.
fn parse_data_models(json: &Value) -> Vec<DiscoveredModel> {
    let Some(data) = json.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(discovered_model_from_entry)
        .collect()
}

/// Read one `data[]` entry: the mandatory `id` plus any advertised capability
/// fields (absent fields stay `None` — the caller decides whether to trust
/// and persist them). Non-chat entries (Copilot also lists embedding models,
/// tagged `capabilities.type != "chat"`) are skipped entirely rather than
/// surfaced with empty capabilities.
fn discovered_model_from_entry(entry: &Value) -> Option<DiscoveredModel> {
    let id = entry.get("id").and_then(Value::as_str)?.to_string();
    if let Some(capabilities) = entry.get("capabilities") {
        return copilot_model_from_capabilities(id, entry, capabilities);
    }
    // OpenRouter's catalog also contains image generators and embedding
    // models. This route is Chat Completions, so only surface entries that
    // can return text when the catalog advertises output modalities.
    if let Some(output_modalities) = entry
        .get("architecture")
        .and_then(|architecture| architecture.get("output_modalities"))
        .and_then(Value::as_array)
        && !output_modalities
            .iter()
            .any(|value| value.as_str() == Some("text"))
    {
        return None;
    }
    // Thinking-type precedence mirrors the kimi-code client: the newer
    // three-state field wins over the legacy boolean when present.
    let openrouter_reasoning = entry.get("reasoning").filter(|value| value.is_object());
    let reasoning = match entry.get("supports_thinking_type").and_then(Value::as_str) {
        Some("only") | Some("both") => Some(true),
        Some("no") => Some(false),
        _ => entry
            .get("supports_reasoning")
            .and_then(Value::as_bool)
            .or_else(|| openrouter_reasoning.map(|_| true)),
    };
    let effort_levels = entry
        .get("think_efforts")
        .and_then(|efforts| efforts.get("valid_efforts"))
        .or_else(|| openrouter_reasoning.and_then(|value| value.get("supported_efforts")))
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        });
    Some(DiscoveredModel {
        id,
        availability: None,
        advertised: None,
        protocol: None,
        endpoint: None,
        family: None,
        // Anthropic publishes `display_name`, Kimi `display_name`, and
        // OpenRouter a plain `name`; the stock OpenAI `/models` shape carries
        // none of them. Any of the three is accepted — the first one present
        // wins, and absence is normal.
        name: entry
            .get("display_name")
            .or_else(|| entry.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        context_window: entry
            .get("context_length")
            .and_then(Value::as_u64)
            .map(|length| length as usize),
        max_output_tokens: entry
            .get("top_provider")
            .and_then(|provider| provider.get("max_completion_tokens"))
            .and_then(Value::as_u64)
            .and_then(|length| u32::try_from(length).ok()),
        reasoning,
        thinking: openrouter_reasoning.map(|_| ReasoningSupport::ReasoningContent),
        tool_call: entry
            .get("supported_parameters")
            .and_then(Value::as_array)
            .map(|parameters| {
                parameters
                    .iter()
                    .any(|value| value.as_str() == Some("tools"))
            }),
        vision: entry
            .get("supports_image_in")
            .and_then(Value::as_bool)
            .or_else(|| {
                entry
                    .get("architecture")
                    .and_then(|architecture| architecture.get("input_modalities"))
                    .and_then(Value::as_array)
                    .map(|modalities| {
                        modalities
                            .iter()
                            .any(|value| value.as_str() == Some("image"))
                    })
            }),
        effort_levels,
        catalog_source: None,
    })
}

/// Read a Copilot-shaped `data[]` entry, whose capability fields live nested
/// under `capabilities.{limits,supports}` rather than the flat Kimi layout
/// (schema per `@vscode/copilot-api`'s `CCAModel`/`CCAModelCapabilities`):
/// `capabilities.limits.max_context_window_tokens`,
/// `capabilities.supports.vision`, and `capabilities.supports.reasoning_effort`
/// (a non-empty tier list — `o1`/`o3`/GPT-5-thinking-style models — implies
/// reasoning support; its entries double as `effort_levels`). Router/tool
/// entries and non-`"chat"` capability types (embeddings, etc.) are filtered
/// out here rather than by the caller, since only Copilot's response carries
/// that distinction.
fn copilot_model_from_capabilities(
    id: String,
    entry: &Value,
    capabilities: &Value,
) -> Option<DiscoveredModel> {
    if let Some(kind) = capabilities.get("type").and_then(Value::as_str)
        && kind != "chat"
    {
        return None;
    }
    let limits = capabilities.get("limits");
    let supports = capabilities.get("supports");
    let effort_levels: Option<Vec<String>> = supports
        .and_then(|s| s.get("reasoning_effort"))
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        });
    let reasoning = effort_levels
        .as_ref()
        .map(|levels| !levels.is_empty())
        .or_else(|| {
            supports
                .and_then(|s| s.get("adaptive_thinking"))
                .and_then(Value::as_bool)
        });
    let thinking = if supports
        .and_then(|s| s.get("adaptive_thinking"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        Some(ReasoningSupport::AnthropicAdaptive)
    } else if supports
        .and_then(|s| s.get("max_thinking_budget"))
        .and_then(Value::as_u64)
        .is_some()
    {
        Some(ReasoningSupport::AnthropicManual)
    } else {
        reasoning.map(|enabled| {
            if enabled {
                ReasoningSupport::ReasoningContent
            } else {
                ReasoningSupport::None
            }
        })
    };
    let protocol = copilot_protocol(entry.get("supported_endpoints"));
    // `model_picker_enabled` is *picker/listing* metadata — it sits beside
    // `model_picker_category` and `model_picker_price_category` in the vendor's
    // `CCAModel` — so it maps to listing intent, never to availability.
    let advertised = entry.get("model_picker_enabled").and_then(Value::as_bool);
    // Availability is the vendor's `policy.state`, a closed vocabulary
    // (`enabled`/`disabled`/`unconfigured`/`unknown`). Only an explicit
    // `disabled` is a declaration; `unconfigured`, `unknown`, and an absent
    // policy mean *undeclared*, not "off". `terms` is the provider's own
    // free-text explanation and is carried verbatim as the reason.
    let availability = match entry
        .get("policy")
        .and_then(|policy| policy.get("state"))
        .and_then(Value::as_str)
    {
        Some("disabled") => Some(Availability {
            usable: false,
            reason: entry
                .get("policy")
                .and_then(|policy| policy.get("terms"))
                .and_then(Value::as_str)
                .filter(|terms| !terms.is_empty())
                .map(str::to_string),
        }),
        Some("enabled") => Some(Availability::usable()),
        _ => None,
    };
    Some(DiscoveredModel {
        id,
        availability,
        advertised,
        protocol,
        endpoint: None,
        family: capabilities
            .get("family")
            .and_then(Value::as_str)
            .map(str::to_string),
        name: entry
            .get("name")
            .or_else(|| entry.get("display_name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        context_window: limits
            .and_then(|l| l.get("max_context_window_tokens"))
            .and_then(Value::as_u64)
            .map(|length| length as usize),
        max_output_tokens: limits
            .and_then(|l| l.get("max_output_tokens"))
            .and_then(Value::as_u64)
            .and_then(|length| u32::try_from(length).ok()),
        reasoning,
        thinking,
        tool_call: supports
            .and_then(|s| s.get("tool_calls"))
            .and_then(Value::as_bool),
        vision: supports
            .and_then(|s| s.get("vision"))
            .and_then(Value::as_bool),
        effort_levels,
        catalog_source: None,
    })
}

/// Decode Copilot's advertised route in deterministic priority order. Messages
/// is checked first because it requires a distinct wire format; Responses is
/// next; Chat Completions is the explicit final route. Missing or unfamiliar
/// entries leave the channel's configured fallback untouched.
fn copilot_protocol(value: Option<&Value>) -> Option<WireProtocol> {
    let endpoints = value?.as_array()?;
    let has = |needle| endpoints.iter().any(|value| value.as_str() == Some(needle));
    if has("/v1/messages") {
        Some(WireProtocol::AnthropicMessages)
    } else if has("/responses") {
        Some(WireProtocol::Responses)
    } else if has("/chat/completions") {
        Some(WireProtocol::ChatCompletions)
    } else {
        None
    }
}

/// Extract Google `models[]`, keeping only `generateContent`-capable text
/// models and stripping the `models/` name prefix to a bare id. Also supports
/// the Google Antigravity `fetchAvailableModels` shape.
fn parse_google_models(json: &Value) -> Vec<DiscoveredModel> {
    if let Some(models) = json.get("models").and_then(Value::as_array) {
        return models
            .iter()
            .filter_map(|entry| {
                // Only keep text-generation models. A Google model entry advertises
                // its capabilities via `supportedGenerationMethods`; entries that
                // list `generateContent` are the chat/text models an agent uses.
                // Embeddings/embedding-only and image/video models are excluded.
                let methods = entry
                    .get("supportedGenerationMethods")
                    .and_then(Value::as_array);
                let is_text = methods
                    .is_none_or(|arr| arr.iter().any(|m| m.as_str() == Some("generateContent")));
                if !is_text {
                    return None;
                }
                entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| DiscoveredModel {
                        id: name.strip_prefix("models/").unwrap_or(name).to_string(),
                        // Gemini's model resource carries the label in
                        // `displayName` (`name` is the `models/<id>` path), so
                        // the display label must not be read from `name` here.
                        name: entry
                            .get("displayName")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        ..DiscoveredModel::default()
                    })
            })
            .collect();
    }

    if let Some(models_map) = json.get("models").and_then(Value::as_object) {
        return parse_antigravity_models_map(models_map, json);
    }

    Vec::new()
}

fn parse_antigravity_models_map(
    models_map: &serde_json::Map<String, Value>,
    root: &Value,
) -> Vec<DiscoveredModel> {
    let deprecated_map = root.get("deprecatedModelIds").and_then(Value::as_object);
    let mut out = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();

    for (model_id, mdata) in models_map {
        // Suppress 3.6 flash models, internal chat/tab helpers, embeddings, image models, deprecated models
        if model_id.starts_with("gemini-3.6-flash")
            || model_id.starts_with("chat_")
            || model_id.starts_with("tab_")
            || model_id.starts_with("models/")
            || model_id.contains("image")
            || deprecated_map.is_some_and(|dep| dep.contains_key(model_id))
        {
            continue;
        }

        let context_window = mdata
            .get("maxTokens")
            .and_then(Value::as_u64)
            .map(|v| v as usize);
        let max_output_tokens = mdata
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        let reasoning = mdata
            .get("supportsThinking")
            .and_then(Value::as_bool)
            .or(Some(true));
        let vision = mdata.get("supportsImages").and_then(Value::as_bool);

        let discovered = DiscoveredModel {
            id: model_id.clone(),
            // This parser applies the provider's admission filter itself (it
            // drops deprecated and non-chat entries), so nothing survives that
            // it needs to declare unavailable or unlisted.
            availability: None,
            advertised: None,
            protocol: None,
            endpoint: None,
            family: Some("google".to_string()),
            name: mdata
                .get("displayName")
                .or_else(|| mdata.get("display_name"))
                .and_then(Value::as_str)
                .map(str::to_string),
            context_window,
            max_output_tokens,
            reasoning,
            thinking: Some(ReasoningSupport::ReasoningContent),
            tool_call: Some(true),
            vision,
            effort_levels: None,
            catalog_source: None,
        };
        if emitted.insert(discovered.id.clone()) {
            out.push(discovered);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_pipeline_dispatches_cleanly() {
        let openai_json: serde_json::Value = serde_json::json!({
            "data": [{"id": "model-1"}]
        });
        let parser = parser_for(CatalogShape::OpenAi);
        let models = parser.parse_json(&openai_json);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "model-1");

        let anthropic_parser = parser_for(CatalogShape::Anthropic);
        let anthropic_models = anthropic_parser.parse_json(&openai_json);
        assert_eq!(anthropic_models.len(), 1);
        assert_eq!(anthropic_models[0].id, "model-1");
    }

    #[test]
    fn catalog_endpoints_append_to_explicit_roots() {
        for (format, root, expected) in [
            (
                CatalogShape::OpenAi,
                "https://relay.example/team/v1",
                "https://relay.example/team/v1/models",
            ),
            (
                CatalogShape::Anthropic,
                "https://relay.example/team/v1/",
                "https://relay.example/team/v1/models",
            ),
            (
                CatalogShape::Codex,
                "https://chatgpt.com/backend-api/codex",
                "https://chatgpt.com/backend-api/codex/models",
            ),
            (
                CatalogShape::Google,
                "https://relay.example/team/v1beta",
                "https://relay.example/team/v1beta/models",
            ),
            (
                CatalogShape::GoogleCloudCode,
                "https://relay.example/team",
                "https://relay.example/team/v1internal:fetchAvailableModels",
            ),
            (
                CatalogShape::OpencodeConsole,
                "https://opencode.ai/console",
                "https://opencode.ai/console/api/config",
            ),
            // A path that happens to resemble an inference endpoint is still a root.
            (
                CatalogShape::OpenAi,
                "https://relay.example/chat/completions",
                "https://relay.example/chat/completions/models",
            ),
        ] {
            assert_eq!(models_endpoint_for(format, root).unwrap(), expected);
        }
        for invalid in [
            "",
            "relative/path",
            "https://relay.example/v1?key=secret",
            "https://user:pass@relay.example/v1",
            "file:///tmp/models",
        ] {
            assert!(models_endpoint_for(CatalogShape::OpenAi, invalid).is_err());
        }
    }

    #[test]
    fn parses_antigravity_models_filtering_3_6_and_deprecated() {
        let json = serde_json::json!({
            "models": {
                "gemini-3.8-flash-tiered": { "maxTokens": 1048576, "supportsThinking": true },
                "gemini-3.7-flash-tiered": { "maxTokens": 1000000, "supportsThinking": true },
                "gemini-3.6-flash-high": { "maxTokens": 1000000, "supportsThinking": true },
                "gemini-pro-agent": { "maxTokens": 1000000, "supportsThinking": true },
                "gemini-3.1-pro-high": { "maxTokens": 1000000, "supportsThinking": true },
                "chat_20706": { "maxTokens": 16000 }
            },
            "deprecatedModelIds": {
                "gemini-3.1-pro-high": { "newModelId": "gemini-pro-agent" }
            }
        });
        let got: Vec<String> = parse_models(CatalogShape::Google, &json)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(got.contains(&"gemini-3.8-flash-tiered".to_string()));
        assert!(!got.contains(&"gemini-3.8-flash".to_string()));
        assert!(got.contains(&"gemini-3.7-flash-tiered".to_string()));
        assert!(!got.contains(&"gemini-3.7-flash".to_string()));
        assert!(got.contains(&"gemini-pro-agent".to_string()));
        assert!(
            !got.contains(&"gemini-3.6-flash-high".to_string()),
            "3.6 flash must be suppressed"
        );
        assert!(
            !got.contains(&"gemini-3.1-pro-high".to_string()),
            "deprecated model must be suppressed"
        );
        assert!(
            !got.contains(&"chat_20706".to_string()),
            "internal helper model must be suppressed"
        );
    }

    #[test]
    fn rejects_empty_base_url() {
        assert!(matches!(
            models_endpoint_for(CatalogShape::OpenAi, ""),
            Err(ModelListError::BadEndpoint(_))
        ));
        assert!(matches!(
            models_endpoint_for(CatalogShape::OpenAi, "   "),
            Err(ModelListError::BadEndpoint(_))
        ));
    }

    #[test]
    fn parses_openai_data_ids() {
        let json = serde_json::json!({
            "data": [
                { "id": "gpt-5.6-sol", "object": "model" },
                { "id": "gpt-5.5", "object": "model" },
                { "id": "gpt-5.4-mini", "object": "model" }
            ]
        });
        let mut got: Vec<String> = parse_models(CatalogShape::OpenAi, &json)
            .into_iter()
            .map(|model| model.id)
            .collect();
        got.sort();
        assert_eq!(got, vec!["gpt-5.4-mini", "gpt-5.5", "gpt-5.6-sol"]);
    }

    #[test]
    fn parses_codex_catalog_in_priority_order_with_capabilities() {
        let json = serde_json::json!({
            "models": [
                {
                    "slug": "hidden-helper",
                    "priority": 0,
                    "visibility": "hide",
                    "supported_in_api": true,
                    "supported_reasoning_levels": [],
                    "context_window": 64_000,
                    "input_modalities": ["text"]
                },
                {
                    "slug": "gpt-codex",
                    "priority": 1,
                    "visibility": "list",
                    "supported_in_api": true,
                    "supported_reasoning_levels": [
                        {"effort": "low"}, {"effort": "high"}
                    ],
                    "context_window": 272_000,
                    "input_modalities": ["text", "image"]
                }
            ]
        });
        let models = parse_models(CatalogShape::Codex, &json);
        assert_eq!(models[0].id, "hidden-helper");
        // `visibility:"hide"` is a *listing* declaration, not an availability
        // one. `hidden-helper` is `supported_in_api:true`, so it stays usable —
        // exactly the distinction the old single bit erased (ADR-0273).
        assert_eq!(models[0].advertised, Some(false));
        assert_eq!(
            models[0].availability,
            Some(Availability::usable()),
            "an API-supported model must not be marked unusable for being unlisted"
        );
        assert_eq!(models[1].id, "gpt-codex");
        assert_eq!(models[1].advertised, Some(true));
        assert_eq!(models[1].availability, Some(Availability::usable()));
        assert_eq!(models[1].protocol, Some(WireProtocol::Responses));
        assert_eq!(models[1].context_window, Some(272_000));
        assert_eq!(models[1].thinking, Some(ReasoningSupport::ReasoningSummary));
        assert_eq!(models[1].vision, Some(true));
        assert_eq!(
            models[1].effort_levels,
            Some(vec!["low".to_string(), "high".to_string()])
        );
    }

    #[test]
    fn parses_visible_astra_from_codex_catalog() {
        let json = serde_json::json!({
            "models": [{
                "slug": "gpt-6-astra",
                "priority": 1,
                "visibility": "list",
                "supported_in_api": true,
                "supported_reasoning_levels": [
                    {"effort": "low"},
                    {"effort": "medium"},
                    {"effort": "high"},
                    {"effort": "xhigh"},
                    {"effort": "max"},
                    {"effort": "ultra"}
                ],
                "context_window": 272000,
                "max_context_window": 872000,
                "input_modalities": ["text", "image"]
            }]
        });

        let models = parse_models(CatalogShape::Codex, &json);
        let astra = models.first().unwrap();
        assert_eq!(astra.id, "gpt-6-astra");
        assert_eq!(astra.advertised, Some(true));
        assert_eq!(astra.context_window, Some(872_000));
        assert_eq!(
            astra
                .effort_levels
                .as_ref()
                .unwrap()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "xhigh", "max", "ultra"]
        );
    }

    #[test]
    fn parses_kimi_platform_capability_fields() {
        // The Kimi Code platform's live response shape (recorded 2026-07 from
        // GET https://api.kimi.com/coding/v1/models): every entry advertises
        // its context length, reasoning/thinking support, and image input;
        // K3 additionally lists its effort tiers.
        let json = serde_json::json!({
            "data": [
                {
                    "id": "kimi-for-coding",
                    "display_name": "kimi-for-coding",
                    "context_length": 262144,
                    "supports_reasoning": true,
                    "supports_image_in": true,
                    "supports_video_in": true,
                    "supports_thinking_type": "only"
                },
                {
                    "id": "k3",
                    "display_name": "k3",
                    "context_length": 1048576,
                    "supports_reasoning": true,
                    "supports_image_in": true,
                    "supports_video_in": true,
                    "supports_thinking_type": "only",
                    "think_efforts": {
                        "support": true,
                        "valid_efforts": ["max"],
                        "default_effort": "max"
                    }
                }
            ],
            "object": "list"
        });
        let models = parse_models(CatalogShape::OpenAi, &json);
        assert_eq!(models.len(), 2);
        let k3 = &models[1];
        assert_eq!(k3.id, "k3");
        assert_eq!(k3.context_window, Some(1_048_576));
        assert_eq!(k3.reasoning, Some(true));
        assert_eq!(k3.vision, Some(true));
        assert_eq!(k3.effort_levels, Some(vec!["max".to_string()]));
        // The legacy entry has no effort field → None, not an empty vec.
        assert_eq!(models[0].id, "kimi-for-coding");
        assert_eq!(models[0].context_window, Some(262_144));
        assert_eq!(models[0].effort_levels, None);
    }

    #[test]
    fn parses_openrouter_catalog_capability_fields() {
        let json = serde_json::json!({
            "data": [
                {
                    "id": "nex-agi/nex-n2.5-pro:free",
                    "context_length": 262144,
                    "architecture": {
                        "input_modalities": ["text", "image"],
                        "output_modalities": ["text"]
                    },
                    "top_provider": { "max_completion_tokens": 235929 },
                    "supported_parameters": [
                        "max_tokens", "reasoning", "reasoning_effort", "tool_choice", "tools"
                    ],
                    "reasoning": {
                        "mandatory": false,
                        "supported_efforts": ["high", "medium", "none"],
                        "default_effort": "high"
                    }
                },
                {
                    "id": "example/image-generator",
                    "architecture": {
                        "input_modalities": ["text"],
                        "output_modalities": ["image"]
                    }
                }
            ]
        });

        let models = parse_models(CatalogShape::OpenAi, &json);
        assert_eq!(models.len(), 1);
        let nex = &models[0];
        assert_eq!(nex.context_window, Some(262_144));
        assert_eq!(nex.max_output_tokens, Some(235_929));
        assert_eq!(nex.reasoning, Some(true));
        assert_eq!(nex.thinking, Some(ReasoningSupport::ReasoningContent));
        assert_eq!(nex.tool_call, Some(true));
        assert_eq!(nex.vision, Some(true));
        assert_eq!(
            nex.effort_levels,
            Some(vec!["high".into(), "medium".into(), "none".into()])
        );
    }

    #[test]
    fn parses_copilot_nested_capability_fields() {
        // GitHub Copilot's live `/models` shape (per `@vscode/copilot-api`'s
        // `CCAModel`): capability data is nested under `capabilities`, unlike
        // Kimi's flat fields. A reasoning model advertises a non-empty
        // `reasoning_effort` tier list; a non-reasoning chat model has none.
        let json = serde_json::json!({
            "data": [
                {
                    "id": "gpt-5",
                    "name": "GPT-5",
                    "object": "model",
                    "model_picker_enabled": true,
                    "capabilities": {
                        "type": "chat",
                        "family": "gpt-5",
                        "limits": {
                            "max_context_window_tokens": 272_000,
                            "max_output_tokens": 128_000,
                            "max_prompt_tokens": 200_000
                        },
                        "supports": {
                            "adaptive_thinking": false,
                            "streaming": true,
                            "tool_calls": true,
                            "vision": true,
                            "reasoning_effort": ["low", "medium", "high"]
                        }
                    }
                },
                {
                    "id": "gpt-4o",
                    "name": "GPT-4o",
                    "object": "model",
                    "model_picker_enabled": true,
                    "capabilities": {
                        "type": "chat",
                        "family": "gpt-4o",
                        "limits": {
                            "max_context_window_tokens": 128_000,
                            "max_output_tokens": 16_384,
                            "max_prompt_tokens": 96_000
                        },
                        "supports": {
                            "adaptive_thinking": true,
                            "streaming": true,
                            "tool_calls": true,
                            "vision": true
                        }
                    }
                },
                {
                    "id": "text-embedding-3-small",
                    "name": "Embedding V3 small",
                    "object": "model",
                    "capabilities": {
                        "type": "embeddings"
                    }
                }
            ]
        });
        let models = parse_models(CatalogShape::OpenAi, &json);
        // The embeddings entry is filtered out — only chat models surface.
        assert_eq!(models.len(), 2);
        let gpt5 = models.iter().find(|m| m.id == "gpt-5").unwrap();
        assert_eq!(gpt5.context_window, Some(272_000));
        assert_eq!(gpt5.max_output_tokens, Some(128_000));
        assert_eq!(gpt5.reasoning, Some(true));
        assert_eq!(gpt5.thinking, Some(ReasoningSupport::ReasoningContent));
        assert_eq!(gpt5.tool_call, Some(true));
        assert_eq!(gpt5.vision, Some(true));
        assert_eq!(
            gpt5.effort_levels,
            Some(vec![
                "low".to_string(),
                "medium".to_string(),
                "high".to_string()
            ])
        );
        let gpt4o = models.iter().find(|m| m.id == "gpt-4o").unwrap();
        assert_eq!(gpt4o.context_window, Some(128_000));
        // `adaptive_thinking` explicitly declares reasoning despite the absent
        // `reasoning_effort` vocabulary.
        assert_eq!(gpt4o.reasoning, Some(true));
        assert_eq!(gpt4o.effort_levels, None);
        assert_eq!(gpt4o.thinking, Some(ReasoningSupport::AnthropicAdaptive));
    }

    #[test]
    fn copilot_metadata_preserves_picker_and_endpoint_facts() {
        let json = serde_json::json!({
            "data": [
                {
                    "id": "claude-opus-4.7",
                    "name": "Claude Opus 4.7",
                    "model_picker_enabled": true,
                    "supported_endpoints": ["/v1/messages"],
                    "capabilities": {
                        "type": "chat",
                        "family": "claude-opus",
                        "limits": {
                            "max_context_window_tokens": 144_000,
                            "max_output_tokens": 64_000
                        },
                        "supports": {
                            "adaptive_thinking": true,
                            "tool_calls": true,
                            "vision": true,
                            "reasoning_effort": ["low", "medium", "high"]
                        }
                    }
                },
                {
                    "id": "internal-title-model",
                    "name": "Internal title model",
                    "model_picker_enabled": false,
                    "policy": { "state": "disabled", "terms": "requires Copilot Business" },
                    "supported_endpoints": ["/responses"],
                    "capabilities": {
                        "type": "chat",
                        "family": "internal",
                        "limits": { "max_output_tokens": 1024 },
                        "supports": { "tool_calls": false }
                    }
                }
            ]
        });

        let models = parse_models(CatalogShape::OpenAi, &json);
        let claude = models
            .iter()
            .find(|model| model.id == "claude-opus-4.7")
            .unwrap();
        // `model_picker_enabled` is a *listing* declaration, so it lands on
        // `advertised`; availability comes from `policy.state` only.
        assert_eq!(claude.advertised, Some(true));
        assert_eq!(claude.availability, None);
        assert_eq!(claude.protocol, Some(WireProtocol::AnthropicMessages));
        assert_eq!(claude.family.as_deref(), Some("claude-opus"));
        assert_eq!(claude.thinking, Some(ReasoningSupport::AnthropicAdaptive));
        assert_eq!(claude.tool_call, Some(true));
        assert_eq!(claude.max_output_tokens, Some(64_000));

        let remote = claude.remote_metadata();
        assert_eq!(remote.protocol, Some(WireProtocol::AnthropicMessages));
        assert_eq!(
            remote.effort_levels,
            Some(vec![
                nuo_contracts::EffortLevel::Known(nuo_contracts::Effort::Low),
                nuo_contracts::EffortLevel::Known(nuo_contracts::Effort::Medium),
                nuo_contracts::EffortLevel::Known(nuo_contracts::Effort::High)
            ])
        );
        assert_eq!(remote.thinking, Some(ReasoningSupport::AnthropicAdaptive));

        let internal = models
            .iter()
            .find(|model| model.id == "internal-title-model")
            .unwrap();
        // Both axes independently: the vendor's picker flag is a listing
        // declaration, and `policy.state:"disabled"` is the availability one,
        // carrying the vendor's own `terms` as the verbatim reason.
        assert_eq!(internal.advertised, Some(false));
        assert_eq!(
            internal.availability,
            Some(Availability::locked(Some(
                "requires Copilot Business".to_string()
            )))
        );
        assert_eq!(internal.protocol, Some(WireProtocol::Responses));
    }

    #[test]
    fn copilot_unconfigured_policy_is_undeclared_not_disabled() {
        // `policy.state` is a closed vocabulary. Only an explicit `disabled`
        // is a declaration; `unconfigured` means "no policy set" and must
        // deserialize to undeclared — never to unavailable (ADR-0273).
        let json = serde_json::json!({
            "data": [{
                "id": "o5-mini",
                "name": "o5-mini",
                "model_picker_enabled": true,
                "policy": { "state": "unconfigured", "terms": "" },
                "supported_endpoints": ["/chat/completions"],
                "capabilities": {
                    "type": "chat",
                    "family": "o5",
                    "limits": { "max_output_tokens": 2048 },
                    "supports": { "tool_calls": true }
                }
            }]
        });
        let models = parse_models(CatalogShape::OpenAi, &json);
        assert_eq!(models[0].availability, None);
        assert_eq!(models[0].advertised, Some(true));
    }

    #[test]
    fn thinking_type_field_wins_over_legacy_reasoning_bool() {
        // `supports_thinking_type` is the newer, authoritative field: "no"
        // must override a stray `supports_reasoning: true`.
        let json = serde_json::json!({
            "data": [
                { "id": "a", "supports_reasoning": true, "supports_thinking_type": "no" },
                { "id": "b", "supports_reasoning": false, "supports_thinking_type": "both" }
            ]
        });
        let models = parse_models(CatalogShape::OpenAi, &json);
        assert_eq!(models[0].reasoning, Some(false));
        assert_eq!(models[1].reasoning, Some(true));
    }

    #[test]
    fn parses_anthropic_data_ids() {
        // Anthropic's /v1/models returns the same {data:[{id}]} shape; no
        // capability fields are advertised. The `display_name` it rides along
        // is ingested as the presentation-only label (never the identity).
        let json = serde_json::json!({
            "data": [
                { "id": "claude-opus-4-8", "display_name": "Claude Opus 4.8" },
                { "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5" }
            ]
        });
        let models = parse_models(CatalogShape::Anthropic, &json);
        let mut got: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
        got.sort();
        assert_eq!(got, vec!["claude-opus-4-8", "claude-sonnet-5"]);
        assert_eq!(models[0].context_window, None);
        assert_eq!(models[0].reasoning, None);
        assert_eq!(models[0].name.as_deref(), Some("Claude Opus 4.8"));
    }

    #[test]
    fn openai_data_models_carry_no_label_when_the_endpoint_publishes_none() {
        // The stock OpenAI-compatible `/models` shape is `{id, object,
        // created, owned_by}` — no label. Absence must stay `None` (the
        // surfaces then render the bare id), not an empty string.
        let json = serde_json::json!({
            "data": [
                { "id": "glm-5.2", "object": "model", "owned_by": "zhipu" }
            ]
        });
        let models = parse_models(CatalogShape::OpenAi, &json);
        assert_eq!(models[0].id, "glm-5.2");
        assert_eq!(models[0].name, None);
        assert_eq!(models[0].remote_metadata().name, None);
    }

    #[test]
    fn a_label_repeating_the_id_collapses_to_none() {
        // Kimi advertises `display_name` equal to the id (`"k3"`), and a
        // models.dev entry may repeat its id as `name`. Storing that would
        // render the same string twice on the row, so it collapses to "no
        // label" and the surfaces show the bare id.
        let json = serde_json::json!({
            "data": [
                { "id": "k3", "display_name": "k3" },
                { "id": "glm-5.2", "display_name": "  " }
            ]
        });
        let models = parse_models(CatalogShape::OpenAi, &json);
        assert_eq!(models[0].name.as_deref(), Some("k3"));
        assert_eq!(models[0].remote_metadata().name, None);
        assert_eq!(models[1].remote_metadata().name, None);
    }

    #[test]
    fn google_display_name_is_read_from_display_name_not_the_resource_path() {
        // Gemini's `name` is the resource path (`models/gemini-2.5-pro`) that
        // becomes the id; the human label is a separate `displayName` field.
        // Reading the label from `name` would echo the id back.
        let json = serde_json::json!({
            "models": [{
                "name": "models/gemini-2.5-pro",
                "displayName": "Gemini 2.5 Pro",
                "supportedGenerationMethods": ["generateContent"]
            }]
        });
        let models = parse_models(CatalogShape::Google, &json);
        assert_eq!(models[0].id, "gemini-2.5-pro");
        assert_eq!(models[0].name.as_deref(), Some("Gemini 2.5 Pro"));
    }

    #[test]
    fn parses_google_models_stripping_prefix_and_filtering_non_text() {
        let json = serde_json::json!({
            "models": [
                {
                    "name": "models/gemini-2.5-flash",
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/gemini-2.5-pro",
                    "supportedGenerationMethods": ["generateContent"]
                },
                // An embedding-only model must be excluded.
                {
                    "name": "models/text-embedding-004",
                    "supportedGenerationMethods": ["embedContent"]
                },
                // A model with no methods array is kept (best-effort).
                { "name": "models/gemini-3-pro-preview" }
            ]
        });
        let mut got: Vec<String> = parse_models(CatalogShape::Google, &json)
            .into_iter()
            .map(|model| model.id)
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec!["gemini-2.5-flash", "gemini-2.5-pro", "gemini-3-pro-preview"]
        );
    }

    #[test]
    fn catalog_shape_validation_distinguishes_empty_from_malformed() {
        assert!(
            validate_catalog_shape(CatalogShape::OpenAi, &serde_json::json!({ "data": [] }))
                .is_ok()
        );
        assert!(matches!(
            validate_catalog_shape(
                CatalogShape::OpenAi,
                &serde_json::json!({ "error": "unauthorized" })
            ),
            Err(ModelListError::Parse(_))
        ));
        assert!(matches!(
            validate_catalog_shape(
                CatalogShape::Google,
                &serde_json::json!({ "error": "bad key" })
            ),
            Err(ModelListError::Parse(_))
        ));
    }

    #[test]
    fn catalog_shape_maps_wire_protocols() {
        assert_eq!(
            CatalogShape::from_wire_protocol(WireProtocol::AnthropicMessages),
            CatalogShape::Anthropic
        );
        assert_eq!(
            CatalogShape::from_wire_protocol(WireProtocol::GoogleGemini),
            CatalogShape::Google
        );
        assert_eq!(
            CatalogShape::from_wire_protocol(WireProtocol::ChatCompletions),
            CatalogShape::OpenAi
        );
        assert_eq!(
            CatalogShape::from_wire_protocol(WireProtocol::Responses),
            CatalogShape::OpenAi
        );
    }
}
