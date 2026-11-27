//! Tencent AI CodeBuddy provider channels (`codebuddy-cn` and `codebuddy-intl`)
//! and their shared, realm-agnostic web-handoff authentication kernel.
//!
//! Both physical lines drive the identical vendor web-handoff protocol
//! (reverse-engineered from `@tencent-ai/codebuddy-code` v2.172.1) and wrapped
//! credential envelope, while strictly isolating realm-specific facts
//! ([INV-PROV-REALM-01], [INV-PROV-CHANNEL-01]):
//!
//! - [`cn`] (`codebuddy-cn`): domestic line at `copilot.tencent.com/v2`
//! - [`intl`] (`codebuddy-intl`): international line at `www.codebuddy.ai/v2`
//!
//! Each realm module owns its own provider id, root URL, `X-Domain`,
//! `ALLOWED_CREDENTIAL_DOMAINS`, model catalog baseline, and `OAuthProvider`
//! surface.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub(crate) mod auth;
pub mod cn;
pub mod intl;

pub use auth::{CODEBUDDY_CLIENT_HEADERS, CODEBUDDY_USER_AGENT, CODEBUDDY_VERSION};
pub use cn::MODEL_PROVIDER_SPEC as CN_MODEL_PROVIDER_SPEC;
pub use intl::INTL_MODEL_PROVIDER_SPEC;

/// OAuth surfaces for both CodeBuddy realms (`codebuddy-cn` and `codebuddy-intl`).
pub mod oauth {
    use std::sync::Arc;

    use nuo_provider::auth::OAuthProvider;

    /// Returns both realm-bound OAuth providers.
    pub fn providers() -> Vec<Arc<dyn OAuthProvider>> {
        let mut out = crate::cn::oauth::providers();
        out.extend(crate::intl::oauth::providers());
        out
    }
}

/// Constructs the BYOK credential source for the given CodeBuddy channel id
/// (`"codebuddy-intl"` vs `"codebuddy-cn"`), enforcing that channel's
/// `[INV-PROV-REALM-01]` domain gate.
pub fn byok_credential_source(
    provider_id: &str,
    connection_name: impl Into<String>,
    api_key: nuo_host::SecretString,
) -> std::sync::Arc<dyn nuo_protocol::auth::CredentialSource> {
    let allowed = match provider_id {
        intl::PROVIDER_ID => intl::ALLOWED_CREDENTIAL_DOMAINS,
        _ => cn::ALLOWED_CREDENTIAL_DOMAINS,
    };
    std::sync::Arc::new(auth::CodeBuddyByokSource::new(
        connection_name,
        api_key,
        allowed,
    ))
}
