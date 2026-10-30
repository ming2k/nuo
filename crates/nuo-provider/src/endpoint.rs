//! Shared connection configuration and typed request/response carriers.
//!
//! Every concrete provider carries the same connection fields —
//! `credentials`, `model`, `base_url`, `client_profile`, `id`.
//! [`Endpoint`] factors that out so each provider struct keeps only the fields
//! unique to its wire format.
use std::sync::Arc;

use crate::auth::{CredentialSource, ResolvedAuth, static_credential};
use nuo_host::SecretString;

/// The connection fields every provider shares.
///
/// A provider-specific struct embeds this as `pub endpoint: Endpoint` and adds
/// only its wire-format-unique fields (e.g. Anthropic's `max_tokens` /
/// `thinking`). `id` is the stable provider/solution id surfaced via
/// [`crate::capability::Provider::provider_id`] so assistant responses can be
/// attributed to the logical channel even after a mid-session switch.
#[derive(Clone)]
pub struct Endpoint {
    /// Dynamic or static credential source.
    pub credentials: Arc<dyn CredentialSource>,
    /// Model id sent on the wire (`model` field of the request body).
    pub model: String,
    /// Full endpoint URL. For OpenAI/Anthropic this is the chat-completions /
    /// `/messages` path; for Google it is the versioned base
    /// (`.../v1beta`) to which the per-call model path is appended.
    pub base_url: String,
    /// Resolved User-Agent header value sent with requests.
    pub user_agent: String,
    /// Resolved custom HTTP headers sent with requests.
    pub headers: Vec<(String, String)>,
    /// Stable attribution id (`provider_id()`).
    pub id: String,
    /// Optional session identifier for sticky routing / session affinity.
    pub session_id: Option<String>,
    pub(crate) fallback_session_id: Arc<std::sync::OnceLock<String>>,
    /// The catalog the model came from, as the provider names it (Qoder's
    /// `source`: `"system"` / `"custom"`). Carried on the wire by dialects
    /// whose surface declares a `CatalogSource` model carrier. Empty when the
    /// catalog advertises no provenance.
    pub catalog_source: String,
    /// The model's display label from the catalog, when it publishes one.
    /// Presentation only — it never becomes the wire identity, but
    /// a dialect whose surface declares a `DisplayName` carrier stamps it into
    /// the request envelope's `model_config.display_name`.
    pub display_name: String,
    /// Lazily derived ingress host (`base_url`'s authority, ports stripped).
    /// Feeds the CodeBuddy `X-Domain` realm header.
    ingress_host: std::sync::OnceLock<String>,
}

impl Endpoint {
    /// Construct an endpoint with a dynamic [`CredentialSource`].
    pub fn new(
        credentials: Arc<dyn CredentialSource>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            credentials,
            model: model.into(),
            base_url: base_url.into(),
            user_agent: crate::NUO_USER_AGENT.to_string(),
            headers: Vec::new(),
            id: id.into(),
            session_id: None,
            fallback_session_id: Arc::new(std::sync::OnceLock::new()),
            catalog_source: String::new(),
            display_name: String::new(),
            ingress_host: std::sync::OnceLock::new(),
        }
    }

    /// Declare the model's catalog provenance (the `source` the provider names)
    /// and its display label.
    pub fn with_catalog_provenance(
        mut self,
        catalog_source: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        self.catalog_source = catalog_source.into();
        self.display_name = display_name.into();
        self
    }

    /// Construct an endpoint with a static API key string.
    pub fn from_static_key(
        api_key: impl Into<SecretString>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self::new(static_credential(api_key), model, base_url, id)
    }

    /// Construct an endpoint with dynamic credentials (alias for [`Self::new`]).
    pub fn with_credentials(
        credentials: Arc<dyn CredentialSource>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self::new(credentials, model, base_url, id)
    }

    /// Attach a dynamic credential source to this endpoint.
    pub fn with_credentials_source(mut self, credentials: Arc<dyn CredentialSource>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Resolve the live authentication credentials for an outbound request.
    pub async fn resolve_auth(&self) -> Result<ResolvedAuth, String> {
        self.credentials.resolve_auth().await
    }

    /// Refresh in reaction to a rejection of the token used by this request.
    pub async fn force_refresh_auth_after(
        &self,
        rejected_access: &nuo_host::SecretString,
    ) -> Result<ResolvedAuth, String> {
        self.credentials
            .force_refresh_after_rejection(rejected_access)
            .await
    }

    /// Whether this endpoint uses dynamic negotiated credentials (ADR-0098).
    pub fn is_negotiated(&self) -> bool {
        self.credentials.is_negotiated()
    }

    /// Set the User-Agent header value.
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Set the User-Agent in place.
    pub fn set_user_agent(&mut self, user_agent: impl Into<String>) {
        self.user_agent = user_agent.into();
    }

    /// Attach an extra HTTP header to this endpoint.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Attach extra HTTP headers to this endpoint.
    pub fn with_headers(mut self, headers: impl IntoIterator<Item = (String, String)>) -> Self {
        self.headers.extend(headers);
        self
    }

    /// Stamp an attribution id after construction.
    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }

    /// Stamp the attribution id in place.
    pub fn set_id(&mut self, id: String) {
        self.id = id;
    }

    /// Stamp a session identifier onto this endpoint for sticky routing / session affinity.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Stamp a session identifier in place.
    pub fn set_session_id(&mut self, session_id: Option<String>) {
        self.session_id = session_id;
    }

    /// The explicitly configured session identifier, if any.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Return an effective session identifier for affinity and sticky routing.
    pub fn effective_session_id(&self) -> &str {
        if let Some(id) = self.session_id.as_deref()
            && !id.trim().is_empty()
        {
            return id;
        }
        self.fallback_session_id
            .get_or_init(|| format!("ses_{}", uuid::Uuid::new_v4().simple()))
            .as_str()
    }

    /// Return all dynamic session-affinity and request-tracking HTTP headers.
    pub fn session_affinity_headers(
        &self,
        session_override: Option<&str>,
    ) -> Vec<(&'static str, String)> {
        let is_opencode_relay =
            self.id.starts_with("opencode") || self.base_url.contains("opencode.ai");
        let is_opencode_client = self.user_agent.starts_with("opencode/");

        if !is_opencode_relay && !is_opencode_client {
            return Vec::new();
        }

        let sid = session_override
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.effective_session_id());

        let mut headers = Vec::new();
        if is_opencode_relay {
            headers.push(("x-opencode-session", sid.to_string()));
            headers.push(("x-opencode-client", "cli".to_string()));
            headers.push((
                "x-opencode-request",
                format!("req_{}", uuid::Uuid::new_v4().simple()),
            ));
        } else if is_opencode_client {
            headers.push(("x-session-affinity", sid.to_string()));
            headers.push(("X-Session-Id", sid.to_string()));
        }

        headers
    }

    /// Attach session-affinity and request-tracking headers to an outbound HTTP request builder.
    pub fn attach_session_affinity_headers(
        &self,
        mut req: crate::transport::RequestBuilder,
        session_override: Option<&str>,
    ) -> crate::transport::RequestBuilder {
        for (name, val) in self.session_affinity_headers(session_override) {
            req = req.header(name, val);
        }
        req
    }

    /// Headers that scope a resolved credential to the account it belongs to.
    pub fn auth_scoped_headers(&self, _auth: &ResolvedAuth) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    /// Attach the credential-scoping headers to an outbound request builder.
    pub fn attach_auth_scoped_headers(
        &self,
        mut req: crate::transport::RequestBuilder,
        auth: &ResolvedAuth,
    ) -> crate::transport::RequestBuilder {
        for (name, val) in self.auth_scoped_headers(auth) {
            req = req.header(name, val);
        }
        req
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn model_id(&self) -> &str {
        &self.model
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Return the headers to attach to every request.
    ///
    /// For the CodeBuddy dialect this additionally stamps `X-Domain` from
    /// **this endpoint's own ingress host** ([INV-PROV-REALM-01] by
    /// construction): the header names the account realm the request rides,
    /// and it cannot disagree with the host the connection was built for,
    /// because it *is* that host. A channel crate that pinned a different
    /// origin would produce a different `X-Domain` with zero coordination.
    pub fn headers(&self) -> Vec<(&str, String)> {
        let mut headers: Vec<(&str, String)> = self
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        if self.id == "codebuddy" || self.id.starts_with("codebuddy") {
            // The realm header is derived from *this endpoint's own ingress
            // host* — structurally incapable of disagreeing with the host the
            // request is sent to ([INV-PROV-REALM-01]).
            headers.retain(|(name, _)| *name != "X-Domain");
            headers.push(("X-Domain", self.ingress_host().to_string()));
        }
        headers
    }

    /// The host component of this endpoint's ingress URL (scheme stripped).
    fn ingress_host(&self) -> &str {
        // Base URLs are API roots (`https://host[:port]/prefix`); the domain
        // header carries the bare host.
        self.ingress_host.get_or_init(|| {
            let rest = self
                .base_url
                .strip_prefix("https://")
                .or_else(|| self.base_url.strip_prefix("http://"))
                .unwrap_or(&self.base_url);
            let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            let host = authority
                .rsplit_once(':')
                .filter(|(_, port)| port.parse::<u16>().is_ok())
                .map(|(host, _)| host)
                .unwrap_or(authority);
            host.to_ascii_lowercase()
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `X-Domain` is derived from the endpoint's own ingress host: a
    /// domestic-built endpoint rides `copilot.tencent.com`, an
    /// international-built one rides `www.codebuddy.ai` — the header cannot
    /// disagree with the host it is attached to ([INV-PROV-REALM-01]).
    #[test]
    fn codebuddy_endpoints_stamp_their_own_realm_as_x_domain() {
        let domestic = Endpoint::from_static_key(
            "k",
            "hy4-preview",
            "https://copilot.tencent.com/v2/chat/completions",
            "codebuddy-cn",
        );
        assert_eq!(
            domestic
                .headers()
                .iter()
                .find(|(k, _)| *k == "X-Domain")
                .map(|(_, v)| v.as_str()),
            Some("copilot.tencent.com")
        );

        let international = Endpoint::from_static_key(
            "k",
            "gpt-5.6-sol",
            "https://www.codebuddy.ai/v2/chat/completions",
            "codebuddy-intl",
        );
        assert_eq!(
            international
                .headers()
                .iter()
                .find(|(k, _)| *k == "X-Domain")
                .map(|(_, v)| v.as_str()),
            Some("www.codebuddy.ai")
        );
    }

    /// Non-RFC-3986 shapes are still parsed deterministically: ports are
    /// stripped, case is normalized, and non-CodeBuddy endpoints are untouched.
    #[test]
    fn ingress_host_parsing_is_deterministic_and_scoped() {
        let ep = Endpoint::from_static_key(
            "k",
            "m",
            "https://COPilot.Tencent.com:443/v2/chat/completions",
            "codebuddy-cn",
        );
        assert_eq!(
            ep.headers()
                .iter()
                .find(|(k, _)| *k == "X-Domain")
                .map(|(_, v)| v.as_str()),
            Some("copilot.tencent.com")
        );

        // A non-CodeBuddy endpoint must not gain the header.
        let openai = Endpoint::from_static_key("k", "m", "https://api.openai.com/v1", "openai");
        assert!(
            !openai.headers().iter().any(|(k, _)| *k == "X-Domain"),
            "X-Domain is a CodeBuddy-only contract"
        );
    }

    #[test]
    fn opencode_relay_emits_session_and_request_headers() {
        let ep = Endpoint::from_static_key(
            "test-key",
            "glm-5.2",
            "https://opencode.ai/inference/openai/v1/chat/completions",
            "opencode-go",
        )
        .with_user_agent("opencode/2.0.32")
        .with_session_id("ses_wire_affinity_999");

        assert_eq!(ep.user_agent(), "opencode/2.0.32");
        assert_eq!(ep.effective_session_id(), "ses_wire_affinity_999");

        let headers = ep.session_affinity_headers(None);
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "x-opencode-session" && v == "ses_wire_affinity_999")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "x-opencode-request" && v.starts_with("req_"))
        );
    }
}
