//! The `codebuddy-intl` provider channel: the **international** ingress of
//! Tencent AI CodeBuddy (CodeBuddy AI), hosted at `www.codebuddy.ai/v2`
//! ([ADR-0097] multi-realm channel bifurcation; ADR-0094 externalDomain).
//!
//! This crate owns everything specific to the international realm:
//!
//! - the provider id `codebuddy-intl` and the 1:1 physical root URL,
//! - the client identity block whose `X-Domain` is this realm's own host
//!   (`www.codebuddy.ai`), declared here rather than borrowed from any
//!   domestic constant,
//! - the international model baseline (the CLI's external-line catalog: the
//!   global GPT-family flagships carried by the vendor's international
//!   gateway, plus the `default` auto-router),
//! - the subscription OAuth surface (`codebuddy-intl` ids, international
//!   origin) driven by the realm-agnostic engine in
//!   `nuo-provider-codebuddy-auth` — the identical vendor web-handoff
//!   protocol as the domestic line, bound to a different identity realm,
//! - the BYOK credential lane gated on the international realm
//!   ([INV-PROV-REALM-01]): a CLI-stored token minted by the domestic line
//!   (`*.codebuddy.cn` / `copilot.tencent.com`) is refused outright here.
//!
//! The login UX is the vendor's browser web-handoff: `auth/state` → browser
//! open → `auth/token` state poll (and `auth/token/refresh` rotation), all
//! mounted under the international origin.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

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
    use nuo_provider::Endpoint;
    use nuo_provider::auth::CredentialSource as _;
    use nuo_provider::auth::OAuthProvider as _;
    use nuo_provider_codebuddy_auth::auth_file::{
        CodeBuddyByokSource, StoredCredentials, realm_gate,
    };
    use nuo_provider_codebuddy_auth::handoff::CodeBuddyRealmSurface;

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
    /// ([INV-PROV-REALM-01]). This is the assertion the shared-constant world
    /// could never make honestly.
    #[test]
    fn endpoints_of_this_channel_stamp_the_international_realm() {
        let ep = Endpoint::from_static_key(
            "k",
            "gpt-5.6-sol",
            "https://www.codebuddy.ai/v2/chat/completions",
            PROVIDER_ID,
        )
        .with_client_profile(nuo_provider::ClientProfile::CodeBuddy);
        assert_eq!(
            ep.headers()
                .iter()
                .find(|(k, _)| *k == "X-Domain")
                .map(|(_, v)| v.as_str()),
            Some("www.codebuddy.ai")
        );
        // The shared preset itself must never have inherited a realm header.
        assert!(
            !nuo_provider::client_identity::CODEBUDDY_CLIENT_HEADERS
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
            kind: nuo_provider_codebuddy_auth::auth_file::CodeBuddyCredentialKind::Bearer,
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
            kind: nuo_provider_codebuddy_auth::auth_file::CodeBuddyCredentialKind::Bearer,
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
            kind: nuo_provider_codebuddy_auth::auth_file::CodeBuddyCredentialKind::ApiKey,
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
