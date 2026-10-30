//! The `codebuddy-cn` provider channel: the **domestic** ingress of Tencent AI
//! CodeBuddy, hosted at `copilot.tencent.com/v2` (ADR-0094 dual-line origin
//! pinning, [ADR-0097] multi-realm channel bifurcation).
//!
//! This crate owns everything specific to the domestic realm:
//!
//! - the provider id `codebuddy-cn` and the 1:1 physical root URL,
//! - the client identity block whose `X-Domain` is this realm's own host
//!   (`copilot.tencent.com`) — never a shared cross-realm constant,
//! - the domestic model baseline (the CLI's internal-line catalog:
//!   Hunyuan 4/3, DeepSeek V4, Kimi K3, GLM 5.3, MiniMax M2.5),
//! - the subscription OAuth surface (`codebuddy-cn` ids, domestic origin)
//!   driven by the realm-agnostic engine in `nuo-provider-codebuddy-auth`,
//! - the BYOK credential lane gated on the domestic realm
//!   ([INV-PROV-REALM-01]): a CLI-stored token minted by the international
//!   line is refused outright here.
//!
//! The login UX is the vendor's browser web-handoff: `auth/state` → browser
//! open → `auth/token` state poll (and `auth/token/refresh` rotation), all
//! mounted under the domestic origin. Model requests speak OpenAI-compatible
//! Chat Completions SSE with `reasoning_content` thinking deltas; streaming
//! is mandatory (`stream: true`).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod oauth;
pub mod spec;

pub const PROVIDER_ID: &str = "codebuddy-cn";

/// The domestic (Tencent Cloud / copilot) auth origin (ADR-0094).
pub const CODEBUDDY_CN_ORIGIN: &str = "https://copilot.tencent.com";

/// This realm's client-identity host header. Composed onto every inference
/// request alongside the shared, realm-agnostic `CODEBUDDY_CLIENT_HEADERS`
/// block — `X-Domain` names the account realm and is therefore a *per-channel
/// fact* ([INV-PROV-REALM-01], ADR-0097), never a shared constant.
pub const CODEBUDDY_CN_X_DOMAIN: &str = "copilot.tencent.com";

/// Domains whose minted credentials this channel accepts
/// ([INV-PROV-REALM-01]). Pasted keys carry no realm stamp and always resolve.
pub const ALLOWED_CREDENTIAL_DOMAINS: &[&str] = &["copilot.tencent.com", "www.codebuddy.cn"];

pub use oauth::providers;
pub use spec::{CODEBUDDY_CN_BUILTIN_MODELS, MODEL_PROVIDER_SPEC};

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
    fn oauth_surface_is_bound_to_the_domestic_realm() {
        let surface = CodeBuddyRealmSurface::new(CODEBUDDY_CN_ORIGIN, &[PROVIDER_ID], "");
        assert_eq!(surface.ids(), &[PROVIDER_ID]);
        assert_eq!(surface.origin(), "https://copilot.tencent.com");
        assert_eq!(
            surface.route_base(),
            "https://copilot.tencent.com/v2/plugin"
        );
    }

    /// The *real* wire path: an endpoint built for this channel's root stamps
    /// this realm's host as `X-Domain` — derived from its own ingress URL, so
    /// the header cannot disagree with the host the request rides
    /// ([INV-PROV-REALM-01]). This is the assertion the shared-constant world
    /// could never make honestly.
    #[test]
    fn endpoints_of_this_channel_stamp_the_domestic_realm() {
        let ep = Endpoint::from_static_key(
            "k",
            "hy4-preview",
            "https://copilot.tencent.com/v2/chat/completions",
            PROVIDER_ID,
        );
        assert_eq!(
            ep.headers()
                .iter()
                .find(|(k, _)| *k == "X-Domain")
                .map(|(_, v)| v.as_str()),
            Some("copilot.tencent.com")
        );
        // The shared preset itself must never have inherited a realm header.
        assert!(
            !nuo_provider_codebuddy_auth::CODEBUDDY_CLIENT_HEADERS
                .iter()
                .any(|(k, _)| *k == "X-Domain")
        );
    }

    /// A CLI-stored token minted by the international line must be refused
    /// outright by this channel's realm gate ([INV-PROV-REALM-01]).
    #[test]
    #[should_panic(expected = "INV-PROV-REALM-01")]
    fn realm_gate_refuses_international_tokens() {
        let foreign = StoredCredentials {
            token: "intl-token".into(),
            kind: nuo_provider_codebuddy_auth::auth_file::CodeBuddyCredentialKind::Bearer,
            nickname: None,
            domain: Some("codebuddy.ai".into()),
        };
        let _ = realm_gate(foreign, ALLOWED_CREDENTIAL_DOMAINS);
    }

    /// A domestic mint (the consumer-facing login origin `www.codebuddy.cn`)
    /// is accepted; pasted keys carry no realm stamp and resolve as-is.
    #[test]
    fn realm_gate_accepts_domestic_mints_and_unstamped_pastes() {
        let domestic = StoredCredentials {
            token: "cn-token".into(),
            kind: nuo_provider_codebuddy_auth::auth_file::CodeBuddyCredentialKind::Bearer,
            nickname: None,
            domain: Some("www.codebuddy.cn".into()),
        };
        assert_eq!(
            realm_gate(domestic, ALLOWED_CREDENTIAL_DOMAINS)
                .domain
                .as_deref(),
            Some("www.codebuddy.cn")
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
            "cn-conn",
            nuo_host::SecretString::from("cn-key"),
            ALLOWED_CREDENTIAL_DOMAINS,
        );
        let auth = source.resolve_auth().await.expect("resolves");
        assert_eq!(auth.token.expose_secret(), "cn-key");
    }
}
