//! Shared Tencent CodeBuddy authentication kernel.
//!
//! This crate is the **realm-agnostic** protocol library behind the two
//! physical CodeBuddy provider channels:
//!
//! - `nuo-provider-codebuddy-cn` — domestic line (`copilot.tencent.com`)
//! - `nuo-provider-codebuddy-intl` — international line (`www.codebuddy.ai`)
//!
//! It registers **no** `ModelProviderSpec`, declares **no** endpoint constants,
//! and embeds itself in **no** registry. Per [ADR-0097], the physical host
//! binding is the exclusive property of a channel crate; this kernel exists
//! because both lines drive the identical vendor web-handoff protocol
//! (reverse-engineered from `@tencent-ai/codebuddy-code` v2.172.1) and the
//! identical wrapped credential envelope — protocol shape is a *shared
//! mechanism*, while origin/realm/model-catalog are *per-channel facts*.
//!
//! # The CodeBuddy login protocol (not an RFC grant)
//!
//! CodeBuddy's login is **not** a standard RFC 8628 device grant and **not** a
//! loopback PKCE redirect — it is a *browser web-handoff + state polling*
//! protocol (the CLI launches the user's default browser and never receives a
//! local callback), mounted under `{origin}/v2/plugin`:
//!
//! 1. `POST {origin}/v2/plugin/auth/state?platform=CLI` (no bearer) — mints an
//!    anonymous login session and returns `data.authUrl` (the page the user
//!    signs in on) plus `data.state`, the correlation key for step 2.
//! 2. `GET {origin}/v2/plugin/auth/token?state={state}` — polled (with the
//!    same no-identity header block) until the web sign-in completes, then
//!    returns the wrapped pair `data.data.{accessToken, refreshToken,
//!    tokenType, expiresIn, refreshExpiresIn, scope, sessionState,
//!    notBeforePolicy, domain}`.
//! 3. `POST {origin}/v2/plugin/auth/token/refresh` with `X-Refresh-Token` +
//!    `X-Auth-Refresh-Source: plugin` headers — rotates both tokens and
//!    returns the same wrapped shape.
//!
//! Model requests speak `Authorization: Bearer <accessToken>` (CLI login,
//! [`CodeBuddyCredentialKind::Bearer`]) or `X-API-Key: <key>` (pasted platform
//! key, [`CodeBuddyCredentialKind::ApiKey`]).
//!
//! # Realm firewall ([INV-PROV-REALM-01])
//!
//! Every credential envelope carries the vendor-declared `domain` field (the
//! account line that minted it, e.g. `www.codebuddy.cn` or `codebuddy.ai`).
//! [`StoredCredentials::domain`] surfaces it; channel crates MUST reject
//! credentials whose domain belongs to the other realm. Mixing lines across a
//! rotation invalidates the pair at the identity provider.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod auth_file;
pub mod envelope;
pub mod handoff;

/// Tencent AI CodeBuddy application version emulated by nuo.
pub const CODEBUDDY_VERSION: &str = "2.172.1";

/// User-Agent header value sent for Tencent AI CodeBuddy client profile.
pub const CODEBUDDY_USER_AGENT: &str = "CLI/2.172.1 CodeBuddy/2.172.1";

/// The realm-agnostic share of the CodeBuddy client-identity block.
pub const CODEBUDDY_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Product", "SaaS"),
    ("X-IDE-Type", "CLI"),
    ("X-IDE-Name", "CodeBuddy"),
    ("X-IDE-Version", "2.172.1"),
    ("X-Agent-Intent", "craft"),
    ("X-Agent-Runtime", "local-agent"),
];

pub use auth_file::*;
pub use envelope::*;
pub use handoff::*;
