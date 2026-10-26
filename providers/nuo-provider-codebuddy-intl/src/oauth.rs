//! The international subscription surface (`codebuddy-intl` ids) routed
//! through the realm-agnostic web-handoff engine.

use std::sync::Arc;

use nuo_provider::auth::OAuthProvider;
use nuo_provider_codebuddy_auth::handoff::CodeBuddyRealmSurface;

use crate::{ALLOWED_CREDENTIAL_DOMAINS, CODEBUDDY_INTL_ORIGIN, PROVIDER_ID};

/// Runtime sign-in prompt copy for the international line (CodeBuddy AI
/// account), fixed at surface construction.
pub const LOGIN_MESSAGE: &str =
    "Open the URL and sign in to your CodeBuddy AI (International) account.";

/// The bounded OAuth surface set this channel registers: exactly one realm.
pub fn providers() -> Vec<Arc<dyn OAuthProvider>> {
    vec![Arc::new(CodeBuddyRealmSurface::new(
        CODEBUDDY_INTL_ORIGIN,
        &[PROVIDER_ID],
        LOGIN_MESSAGE,
    ))]
}

/// The domains this channel's credentials may originate from (re-exported for
/// the server registry wiring).
pub fn allowed_credential_domains() -> &'static [&'static str] {
    ALLOWED_CREDENTIAL_DOMAINS
}
