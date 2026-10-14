//! Generic OAuth2 authentication engine for Nuo (ADR-0027).
//!
//! # Layering
//!
//! - **The standard** (RFC 7636 PKCE, RFC 8628 device flow, loopback listener,
//!   token endpoint/refresh, JWT inspection) lives in
//!   [`crate::oauth`]. This crate never re-implements it.
//! - **This crate** is the stateful workflow layer: login sessions, the
//!   per-connection credential source, and the extension ports below.
//! - **Vendors** own their `OAuthConfig`, device grant, enricher, refresh and
//!   credential repair, and implement [`OAuthProvider`] in their own crate.
//!
//! The engine names **no** vendor. It resolves a provider's behaviour through
//! [`register_oauth_provider`] at the composition root (`[INV-PROV-03]`,
//! `[INV-PROV-07]`): a new subscription surface adds code only in its vendor
//! crate plus one registration.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod credential_source;
pub mod enricher;
pub mod manual;
pub mod oauth;
pub mod provider;
pub mod session;

pub use credential_source::OAuthCredentialSource;
pub use enricher::{OAuthTokenEnricher, StandardOAuthEnricher};
pub use manual::parse_authorization_response;
pub use oauth::AuthError;
pub use provider::{
    DeviceLogin, OAuthProvider, oauth_config, oauth_provider, register_oauth_provider,
};
pub use session::*;

pub use nuo_provider::credentials::{
    CredentialHost, CredentialSession, CredentialStore, CredentialStoreError, DeviceIdentity,
    FileCredentialStore, FileDeviceIdentity, InMemoryCredentialStore, PerProcessIdentity, TokenSet,
};
pub use nuo_model_codec::LoginMethod;
pub use nuo_model_codec::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, OAuthConfigBuilder, PkceMode, PortMode,
    TokenRequestFormat,
};
