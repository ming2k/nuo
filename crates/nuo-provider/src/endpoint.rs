//! Shared connection configuration and typed request/response carriers.
//!
//! Every concrete provider carries the same connection fields —
//! `credentials`, `model`, `base_url`, `client_profile`, `id`.
//! [`Endpoint`] factors that out so each provider struct keeps only the fields
//! unique to its wire format.
use std::sync::Arc;

pub use nuo_inference_wire::client_identity::*;
use nuo_inference_wire::{CredentialSource, ResolvedAuth, SecretString, static_credential};

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
    /// Client profile specifying the User-Agent and client identity headers.
    pub client_profile: ClientProfile,
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
            client_profile: ClientProfile::Native,
            id: id.into(),
            session_id: None,
            fallback_session_id: Arc::new(std::sync::OnceLock::new()),
            catalog_source: String::new(),
            display_name: String::new(),
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

    /// Whether this endpoint uses dynamic OAuth credentials.
    pub fn is_oauth(&self) -> bool {
        self.credentials.is_oauth()
    }

    /// Stamp the user-agent header value, updating the client profile.
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.client_profile = ClientProfile::from_user_agent(&user_agent.into());
        self
    }

    /// Stamp a client profile directly onto this endpoint.
    pub fn with_client_profile(mut self, profile: impl Into<ClientProfile>) -> Self {
        self.client_profile = profile.into();
        self
    }

    /// Stamp a client identity directly onto this endpoint.
    pub fn with_client_identity(mut self, identity: &ClientIdentity) -> Self {
        self.client_profile = identity.clone();
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
        let is_opencode_client = matches!(self.client_profile, ClientProfile::OpenCode)
            || self.client_profile.user_agent().starts_with("opencode/");

        if !is_opencode_relay && !is_opencode_client {
            return Vec::new();
        }

        let sid = session_override
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.effective_session_id());

        let mut headers = Vec::new();
        if is_opencode_relay {
            headers.push(("x-opencode-session", sid.to_string()));
            if !is_opencode_client {
                headers.push(("x-opencode-client", "cli".to_string()));
            }
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
        mut req: nuo_egress::request::RequestBuilder,
        session_override: Option<&str>,
    ) -> nuo_egress::request::RequestBuilder {
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
        mut req: nuo_egress::request::RequestBuilder,
        auth: &ResolvedAuth,
    ) -> nuo_egress::request::RequestBuilder {
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
        self.client_profile.user_agent()
    }

    /// Return the resolved [`ClientProfile`] for this endpoint.
    pub fn client_profile(&self) -> &ClientProfile {
        &self.client_profile
    }

    /// Return the resolved [`ClientIdentity`] for this endpoint.
    pub fn client_identity(&self) -> &ClientIdentity {
        &self.client_profile
    }

    /// Return the client-identity headers to attach to every request.
    pub fn headers(&self) -> Vec<(&str, &str)> {
        self.client_profile.headers()
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_profile_presets_have_matching_user_agents_and_headers() {
        for preset in ClientProfile::PRESETS {
            assert!(!preset.id().is_empty());
            assert!(!preset.label().is_empty());
            assert!(!preset.user_agent().is_empty());

            let parsed = ClientProfile::from_id(preset.id()).expect("parses from canonical id");
            assert_eq!(&parsed, preset);

            if *preset != ClientProfile::Copilot {
                let detected = ClientProfile::from_user_agent(preset.user_agent());
                assert_eq!(
                    &detected,
                    preset,
                    "detected from UA: {}",
                    preset.user_agent()
                );
            }
        }
    }

    #[test]
    fn opencode_relay_emits_session_and_request_headers() {
        let ep = Endpoint::from_static_key(
            "test-key",
            "glm-5.2",
            "https://opencode.ai/inference/openai/v1/chat/completions",
            "opencode-plan",
        )
        .with_client_profile(ClientProfile::OpenCode)
        .with_session_id("ses_wire_affinity_999");

        assert_eq!(ep.user_agent(), OPENCODE_USER_AGENT);
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
