//! Shared connection configuration and typed request/response carriers.
//!
//! Every concrete provider carries the same connection fields —
//! `credentials`, `model`, `base_url`, `client_profile`, `id` — duplicated verbatim
//! across [`crate::protocol::openai::OpenAiChatCompletionsProvider`],
//! [`crate::protocol::anthropic::AnthropicMessagesProvider`], and
//! [`crate::protocol::google::GoogleProvider`]. [`Endpoint`] factors that out so each
//! provider struct keeps only the fields *unique* to its wire format.
//!
//! This is the analogue of vercel/ai's per-provider client configuration: the
//! shared transport concerns (where to send, how to authenticate, how to label
//! attribution) live in one place, while each API's request *shape* lives in
//! its own module.

use std::sync::Arc;

use nuo_model_codec::{CredentialSource, ResolvedAuth, SecretString, static_credential};

pub use nuo_model_codec::client_identity::*;

/// The connection fields every provider shares.
///
/// A provider-specific struct embeds this as `pub endpoint: Endpoint` and adds
/// only its wire-format-unique fields (e.g. Anthropic's `max_tokens` /
/// `thinking`). `id` is the stable provider/solution id surfaced via
/// [`nuo_model_codec::Provider::provider_id`] so assistant responses can be
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
    /// Presentation only — it never becomes the wire identity (ADR-0131), but
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
    /// and its display label. Both are wire-optional: only a dialect whose
    /// surface declares a matching [`ModelCarrier`](nuo_model_codec::wire_surface::ModelCarrier)
    /// binding stamps them.
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

    /// Stamp an attribution id after construction (the catalog does this with
    /// the config entry id).
    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }

    /// Stamp the attribution id in place (non-consuming variant for the
    /// registry, which builds the provider via a constructor and then sets the
    /// id from the channel entry id).
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
    ///
    /// If no session identifier was configured, generates and caches a stable
    /// fallback session identifier (`ses_<uuid>`) for this endpoint instance.
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
    ///
    /// Depending on the connection target and the active client profile:
    ///
    /// 1. When connecting to an OpenCode relay (`opencode.ai`): attaches
    ///    `x-opencode-session` (required by the Console inference surface for
    ///    sticky routing / KV-cache reuse), `x-opencode-request` (trace UUID),
    ///    and `x-opencode-client` (if not already in client headers).
    /// 2. When emulating an OpenCode client against non-OpenCode endpoints:
    ///    attaches `x-session-affinity` and `X-Session-Id` matching upstream
    ///    OpenCode client behavior.
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
        mut req: crate::request::RequestBuilder,
        session_override: Option<&str>,
    ) -> crate::request::RequestBuilder {
        for (name, val) in self.session_affinity_headers(session_override) {
            req = req.header(name, val);
        }
        req
    }

    /// Headers that scope a resolved credential to the account it belongs to.
    ///
    /// Derived entirely from typed [`ResolvedAuth`] metadata, never from the
    /// connection or provider name, so a static API key yields no headers and
    /// no wire branch here ever names a vendor (ADR-0267, ADR-0269). An
    /// OpenCode Console credential is meaningless without its workspace, so the
    /// org id rides on **every** protocol the surface exposes.
    pub fn auth_scoped_headers(&self, auth: &ResolvedAuth) -> Vec<(&'static str, String)> {
        auth.extension::<nuo_model_codec::OpencodeAuthMetadata>()
            .map(|org| vec![("x-opencode-org-id", org.org_id.clone())])
            .unwrap_or_default()
    }

    /// Attach the credential-scoping headers to an outbound request builder.
    pub fn attach_auth_scoped_headers(
        &self,
        mut req: crate::request::RequestBuilder,
        auth: &ResolvedAuth,
    ) -> crate::request::RequestBuilder {
        for (name, val) in self.auth_scoped_headers(auth) {
            req = req.header(name, val);
        }
        req
    }

    // accessors
    //
    // Provided once here so each provider forwards through its embedded
    // `endpoint` field instead of restating them. Naming note: these are
    // intentionally distinct from the `Provider` trait methods (`model`,
    // `provider_id`) that every concrete provider also implements, so there is
    // no name collision — the trait methods return owned `String`s and serve
    // the `dyn Provider` interface, while these borrow the underlying field.

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

            // Each preset round-trips through from_id
            let parsed = ClientProfile::from_id(preset.id()).expect("parses from canonical id");
            assert_eq!(&parsed, preset);

            // from_user_agent detects standard presets
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
    fn client_profile_headers_attached_for_emulated_clients() {
        let zcode = ClientProfile::ZCode;
        let zcode_headers = zcode.headers();
        assert!(
            zcode_headers
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Z Code")
        );
        assert!(
            zcode_headers
                .iter()
                .any(|(k, v)| *k == "X-ZCode-Agent" && *v == "glm")
        );

        let claude = ClientProfile::ClaudeCode;
        assert!(
            claude
                .headers()
                .iter()
                .any(|(k, v)| *k == "x-app" && *v == "claude-code")
        );

        let cline = ClientProfile::Cline;
        assert!(
            cline
                .headers()
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Cline")
        );

        let cursor = ClientProfile::Cursor;
        assert!(
            cursor
                .headers()
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Cursor")
        );

        let agy = ClientProfile::Antigravity;
        assert!(
            agy.headers()
                .iter()
                .any(|(k, v)| *k == "x-goog-api-client"
                    && *v == nuo_model_codec::client_identity::ANTIGRAVITY_API_CLIENT_HEADER)
        );

        let opencode = ClientProfile::OpenCode;
        assert!(
            opencode
                .headers()
                .iter()
                .any(|(k, v)| *k == "x-opencode-client" && *v == "cli")
        );

        let custom = ClientProfile::custom(
            "custom-agent/1.0",
            vec![("X-Custom-Foo".to_string(), "Bar".to_string())],
        );
        let custom_headers = custom.headers();
        assert_eq!(custom_headers.len(), 1);
        assert_eq!(custom_headers[0], ("X-Custom-Foo", "Bar"));
    }

    #[test]
    fn opencode_relay_emits_session_and_request_headers() {
        let ep = Endpoint::from_static_key(
            "test-key",
            "glm-5.2",
            "https://opencode.ai/inference/openai/v1/chat/completions",
            "opencode-go",
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

    #[test]
    fn opencode_relay_generates_fallback_session_id_when_unconfigured() {
        let ep = Endpoint::from_static_key(
            "test-key",
            "glm-5.2",
            "https://opencode.ai/inference/openai/v1/chat/completions",
            "opencode-go",
        );

        assert!(ep.session_id().is_none());
        let sid1 = ep.effective_session_id().to_string();
        assert!(sid1.starts_with("ses_"));
        // Stable across multiple calls
        let sid2 = ep.effective_session_id().to_string();
        assert_eq!(sid1, sid2);

        let headers = ep.session_affinity_headers(None);
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "x-opencode-session" && v == &sid1)
        );
    }

    #[test]
    fn opencode_client_emits_affinity_headers_against_standard_providers() {
        let ep = Endpoint::from_static_key(
            "test-key",
            "claude-3-5-sonnet",
            "https://api.anthropic.com/v1/messages",
            "anthropic",
        )
        .with_client_profile(ClientProfile::OpenCode)
        .with_session_id("ses_affinity456");

        let headers = ep.session_affinity_headers(None);
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "x-session-affinity" && v == "ses_affinity456")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Session-Id" && v == "ses_affinity456")
        );
    }

    #[test]
    fn org_scoped_credential_projects_workspace_header() {
        let ep = Endpoint::from_static_key(
            "st-token",
            "glm-5.2",
            "https://opencode.ai/inference/openai/v1/chat/completions",
            "gomain",
        );
        let auth =
            ResolvedAuth::new("st-token").with_extension(nuo_model_codec::OpencodeAuthMetadata {
                org_id: "wrk_workspace_1".to_string(),
            });

        assert_eq!(
            ep.auth_scoped_headers(&auth),
            vec![("x-opencode-org-id", "wrk_workspace_1".to_string())]
        );
    }

    #[test]
    fn static_credential_projects_no_workspace_header() {
        // Presence-gated on metadata: the connection id and the base URL are
        // deliberately OpenCode-shaped here, and still emit nothing.
        let ep = Endpoint::from_static_key(
            "sk-relay",
            "glm-5.2",
            "https://opencode.ai/inference/openai/v1/chat/completions",
            "opencode-go",
        );
        let auth = ResolvedAuth::new("sk-relay");
        assert!(ep.auth_scoped_headers(&auth).is_empty());
    }
}
