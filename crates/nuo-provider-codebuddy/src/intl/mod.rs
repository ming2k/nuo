//! The `codebuddy-intl` provider channel: the **international** ingress of
//! Tencent AI CodeBuddy (CodeBuddy AI), hosted at `www.codebuddy.ai/v2`
//! ([ADR-0097] multi-realm channel bifurcation; ADR-0094 externalDomain).

pub mod oauth;
pub mod spec;

pub const PROVIDER_ID: &str = "codebuddy-intl";

/// The international (CodeBuddy AI) auth origin (ADR-0094 externalDomain).
pub const CODEBUDDY_INTL_ORIGIN: &str = "https://www.codebuddy.ai";

/// This realm's client-identity host header. Composed onto every inference
/// request alongside the shared, realm-agnostic `CODEBUDDY_CLIENT_HEADERS`
/// block — `X-Domain` names the account realm and is therefore a *per-channel
/// fact* ([INV-PROV-REALM-01], ADR-0097), never a shared constant.
pub const CODEBUDDY_INTL_X_DOMAIN: &str = "www.codebuddy.ai";

/// Domains whose minted credentials this channel accepts
/// ([INV-PROV-REALM-01]). Pasted keys carry no realm stamp and always resolve.
pub const ALLOWED_CREDENTIAL_DOMAINS: &[&str] = &["codebuddy.ai"];

pub use oauth::providers;
pub use spec::{CODEBUDDY_INTL_BUILTIN_MODELS, INTL_MODEL_PROVIDER_SPEC};

#[cfg(test)]
mod realm_tests {
    use super::*;
    use crate::auth::auth_file::{
        CodeBuddyByokSource, CodeBuddyCredentialKind, StoredCredentials, realm_gate,
    };
    use crate::auth::handoff::CodeBuddyRealmSurface;
    use nuo_protocol::auth::CredentialSource as _;
    use nuo_provider::Endpoint;
    use nuo_provider::auth::OAuthProvider as _;

    /// The registered OAuth surface must be bound to exactly one realm and
    /// answer only this channel's id ([INV-PROV-CHANNEL-01]).
    #[test]
    fn oauth_surface_is_bound_to_the_international_realm() {
        let surface = CodeBuddyRealmSurface::new(CODEBUDDY_INTL_ORIGIN, &[PROVIDER_ID], "");
        assert_eq!(surface.ids(), &[PROVIDER_ID]);
        assert_eq!(surface.origin(), "https://www.codebuddy.ai");
        assert_eq!(surface.route_base(), "https://www.codebuddy.ai/v2/plugin");
    }

    /// The *real* wire path: an endpoint built for this channel's root stamps
    /// this realm's host as `X-Domain` — derived from its own ingress URL, so
    /// the header cannot disagree with the host the request rides
    /// ([INV-PROV-REALM-01]).
    #[test]
    fn endpoints_of_this_channel_stamp_the_international_realm() {
        let ep = Endpoint::from_static_key(
            "k",
            "gpt-5.6-sol",
            "https://www.codebuddy.ai/v2/chat/completions",
            PROVIDER_ID,
        );
        assert_eq!(
            ep.headers()
                .iter()
                .find(|(k, _)| *k == "X-Domain")
                .map(|(_, v)| v.as_str()),
            Some("www.codebuddy.ai")
        );
        // The shared preset itself must never have inherited a realm header.
        assert!(
            !crate::CODEBUDDY_CLIENT_HEADERS
                .iter()
                .any(|(k, _)| *k == "X-Domain")
        );
    }

    /// A CLI-stored token minted by the domestic line must be refused outright
    /// by this channel's realm gate ([INV-PROV-REALM-01]).
    #[test]
    #[should_panic(expected = "INV-PROV-REALM-01")]
    fn realm_gate_refuses_domestic_tokens() {
        let foreign = StoredCredentials {
            token: "cn-token".into(),
            kind: CodeBuddyCredentialKind::Bearer,
            nickname: None,
            domain: Some("www.codebuddy.cn".into()),
        };
        let _ = realm_gate(foreign, ALLOWED_CREDENTIAL_DOMAINS);
    }

    /// An international mint (`codebuddy.ai`) is accepted; pasted keys carry
    /// no realm stamp and resolve as-is.
    #[test]
    fn realm_gate_accepts_international_mints_and_unstamped_pastes() {
        let international = StoredCredentials {
            token: "intl-token".into(),
            kind: CodeBuddyCredentialKind::Bearer,
            nickname: None,
            domain: Some("codebuddy.ai".into()),
        };
        assert_eq!(
            realm_gate(international, ALLOWED_CREDENTIAL_DOMAINS)
                .domain
                .as_deref(),
            Some("codebuddy.ai")
        );
        let paste = StoredCredentials {
            token: "pasted".into(),
            kind: CodeBuddyCredentialKind::ApiKey,
            nickname: None,
            domain: None,
        };
        let _ = realm_gate(paste, ALLOWED_CREDENTIAL_DOMAINS);
    }

    /// The BYOK source wired for this realm resolves pastes without a realm
    /// stamp and inherits the gate for discovered CLI files.
    #[tokio::test]
    async fn byok_source_of_this_realm_resolves_static_keys() {
        let source = CodeBuddyByokSource::new(
            "intl-conn",
            nuo_host::SecretString::from("intl-key"),
            ALLOWED_CREDENTIAL_DOMAINS,
        );
        let auth = source.resolve_auth().await.expect("resolves");
        assert_eq!(auth.token.expose_secret(), "intl-key");
    }
}
