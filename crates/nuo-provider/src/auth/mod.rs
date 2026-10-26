//! Authentication, credential resolution, and OAuth2/PKCE token negotiation.

pub mod browser;
pub mod config;
pub mod core;
pub mod credential_source;
pub mod device;
pub mod enricher;
pub mod error;
pub mod manual;
pub mod pkce;
pub mod provider;
pub mod session;
pub mod token;

pub use crate::credentials::{
    CredentialHost, CredentialSession, CredentialStore, CredentialStoreError, DeviceIdentity,
    FileCredentialStore, FileDeviceIdentity, InMemoryCredentialStore, PerProcessIdentity, TokenSet,
};
pub use browser::{CallbackOutcome, CallbackServer, render_page};
pub use config::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, OAuthConfigBuilder, PkceMode, PortMode,
    TokenRequestFormat,
};
pub use core::{
    ChatGptAuthMetadata, CopilotAuthMetadata, CredentialSource, ExtensionMap, GoogleAuthMetadata,
    PreflightValidator, ResolvedAuth, StaticCredentialSource, static_credential,
};
pub use credential_source::OAuthCredentialSource;
pub use device::{
    DeviceCodeResponse, poll_device_code, poll_device_code_with, request_device_code,
    request_device_code_at,
};
pub use enricher::{OAuthTokenEnricher, StandardOAuthEnricher, build_token_set_with_enricher};
pub use error::{AuthError, token_error_code};
pub use futures::future::BoxFuture;
pub use manual::parse_authorization_response;
pub use pkce::{PkceCodes, new_nonce, new_state, random_string, s256_challenge};
pub use provider::{
    DeviceLogin, OAuthProvider, oauth_config, oauth_provider, register_oauth_provider,
};
pub use session::{
    BrowserLogin, OAuth, OAuthLoginPrompt, OAuthLoginSession, build_token_set_from_login,
};
pub use token::{
    ACCESS_TOKEN_REFRESH_SKEW_MS, TokenResponse, access_token_expiry_ms, access_token_is_expiring,
    build_authorize_url, exchange_code, jwt_claims, jwt_exp_ms, percent_encode_form_pairs,
    percent_encode_form_value, post_form, refresh_access_token,
};
