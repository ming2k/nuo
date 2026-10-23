---
id: ADR-0094
title: "Tencent CodeBuddy Subscription Lane: Browser Web-Handoff OAuth, Dual-Line Origin Pinning, and Credential-Kind Header Split"
status: accepted
date: 2026-11-27
scope: provider/nuo-provider-codebuddy, provider/nuo-server, oauth/nuo-oauth, provider/nuo-provider, tui/nuo-tui, docs/reference-openapi
superseded_by: null
negative_knowledge: true
---

# 0094. Tencent CodeBuddy Subscription Lane: Browser Web-Handoff OAuth, Dual-Line Origin Pinning, and Credential-Kind Header Split

- Status: Accepted
- Date: 2026-11-27
- Deciders: Runtime, Capability, Interface Teams
- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md)
- Verification: Reverse-engineered from `@tencent-ai/codebuddy-code` v2.172.1 (dist/codebuddy.js account-scoped auth service, dist-server/types/**, embedded dist/web-ui/docs/cn/cli/*.md, product.json authentication.attributes) and cross-checked against a live logged-in `Tencent-Cloud.coding-copilot.info` credential file on 2026-10-23. Live capture: access token ~60d TTL (`expiresIn: 5184000`), refresh token ~90d TTL (`refreshExpiresIn: 7776000`), scope `openid profile offline_access email`, `tokenType: "Bearer"`, Keycloak-style `notBeforePolicy` + `sessionState` claim pair.

---

## Context and Problem Statement

Nuo's `codebuddy` provider previously shipped **BYOK-only** — the credential source resolved a static key or a disk-scraped token, rode `X-API-Key`, and had no login flow of its own. Reverse engineering showed that this modeled the vendor wrongly:

1. CodeBuddy's interactive login is a **first-class subscription surface** — an OIDC bearer JWT mints by a *browser web-handoff + state polling* protocol, persisted by the CLI in `Tencent-Cloud.coding-copilot.info`. ADR-0014 says exactly this shape belongs on the subscription lane ([INV-LANE-01]); the BYOK-only preset inverted the lane boundary.
2. The upstream wire recognizes **two distinct credential kinds** (`auth.tokenType = "Bearer"` vs `"ApiKey"`) that map to **different header shapes** (`Authorization: Bearer` vs `X-API-Key`). The old source collapsed them into one opaque token — a classic modeling error ADR-0014 warns against ("collapsing them into one 'auth mode' flag is the classic modeling error").
3. Credentials are **bound to a single auth account line** (domestic `copilot.tencent.com` vs international `www.codebuddy.ai`). The CLI's own `product.json` declares both lines (internalDomain vs externalDomain in `authentication.attributes`); mixing them on refresh would invalidate the pair. The old source was single-line only.
4. The wire's identity block (`X-User-Id` / `X-Enterprise-Id` / `X-Id-Source` / `X-Userinfo` / `X-Auth-Method`) is what the server actually consults for claim shape — not the OIDC JWT alone. The old source produced none of these.

## Decision Drivers

- Driver 1: Model the vendor's actual protocol family, not a simplifying placeholder (mechanism over exhortation, AGENTS.md §3).
- Driver 2: Preserve the dual-lane boundary codified by ADR-0014 — OAuth subscription should be the default lane for every vendor whose native CLI ships an account login, and BYOK inference should never be the *default* for such vendors.
- Driver 3: Reuse ADR-0027's ownership topology: vendor OAuth behavior lives in the vendor crate behind the `nuo-oauth` typed port, never inside the engine.

## Architectural Decisions

### Subsystem Boundaries & Structural Changes

- `providers/nuo-provider-codebuddy/src/oauth.rs` — **new vendor surface** implementing the `nuo-oauth` port:
  - `begin_device_login` drives the vendor's three-step web-handoff: `POST {origin}/v2/plugin/auth/state?platform=CLI` (bearerless, `X-No-*` suppression headers) → open the returned `authUrl` in the browser (no local callback server binds) → poll `GET {origin}/v2/plugin/auth/token?state={state}` at ~1s until the wrapped `data.data` carries the token pair. The `OAuthConfig` declares `default_login_method: Device` + `DeviceFlowMode::custom("codebuddy-web-handoff")` because the engine dispatches vendor flows only through the `Device` branch (`crate::nuo_oauth::session`'s `OAuthLoginFlow::Vendor`, the Qoder pattern); the label is cosmetic — the URL is the vendor's browser URL and no RFC device code is ever requested.
  - `refresh_token` overrides the RFC path: CodeBuddy rotates **both tokens in one call** via `POST {origin}/v2/plugin/auth/token/refresh` with `X-Refresh-Token` + `X-Auth-Refresh-Source: plugin` headers — not a `grant_type=refresh_token` form post.
  - `CodeBuddyOAuthEnricher` pins a durable `codebuddy_origin` attribute per credential so a later refresh always stays on sniffing-free ground (`stored_origin` reads the attribute; the domestic surface is the fallback default).
  - Two registered surfaces: `codebuddy` (domestic, `copilot.tencent.com`) and `codebuddy-intl` (international, `www.codebuddy.ai`).
- `crates/nuo-server/src/provider_registry.rs::build_credential_source` — the CodeBuddy dialect dispatch now splits by `ConnectionAuth::is_oauth()`: subscription lane → `OAuthCredentialSource`, BYOK lane → the existing `CodeBuddyCredentialSource` (Bearer or ApiKey, lane resolved inside).
- `crates/nuo-provider/src/connection_auth.rs` — `codebuddy` / `codebuddy-intl` default to `LoginMethod::Device` in `default_login_method` (the engine's label for "vendor-custom flow"; the vendor URL is still browser-opened, not a polled device code) and the registered `OAuthProvider` is the sole entry. `supports_login_method(Device)` is the only supported method.
- `crates/nuo-tui/src/providers.rs` — the `codebuddy` connection template's `auth` switches from `ConnectionAuth::ApiKey` to `ConnectionAuth::subscription_const("codebuddy")`, so the TUI now offers browser login first and treats a pasted key as the BYOK opt-in path.
- `providers/nuo-provider-codebuddy/src/credential.rs` — the BYOK source gains a typed [`CodeBuddyCredentialKind`] (Bearer / ApiKey) resolved from the stored file's `auth.tokenType` and projected onto `ResolvedAuth` via a new [`CodeBuddyBearerCredential`] marker extension, so the downstream header layer can choose `Authorization: Bearer` vs `X-API-Key` by typestate rather than substring sniffing.
- `docs/reference/openapi/codebuddy.yml` — carries the reverse-engineered `/plugin/auth/state`, `/plugin/auth/token`, `/plugin/auth/token/refresh` schemas, the full enterprise identity block, the `AuthCredentialPair` / `TokenRefreshResponse` wrapped shapes, and the live-captured model catalog superset.

### Normative Invariants Established

- `[INV-CB-01] Subscription lane is the default for `codebuddy` connections`: a connection created without an explicit API key resolves its bearer through the OAuth credential source, never through the BYOK loader. Enforced by `build_credential_source` dispatching `Dialect::CodeBuddy` on `ConnectionAuth::is_oauth()`.
- `[INV-CB-02] The web-handoff never binds a local callback server`: the vendor's browser flow is keyed by a `state` correlation token, not by a loopback redirect. `OAuthConfig.browser_login` stays `false` and the default method routes through `DeviceFlowMode::Custom`. Enforced by `oauth::preset` unit test `preset_declares_browser_web_handoff_not_rfc_device`.
- `[INV-CB-03] Refresh is a paired rotation, not an RFC grant`: the vendor's refresh returns a wrapped `{accessToken, refreshToken}` pair; applying an RFC `grant_type=refresh_token` form post would 401. Enforced by routing the refresh through `refresh_at` (POST + `X-Refresh-Token` + `X-Auth-Refresh-Source: plugin`) in `CodeBuddyOAuthProvider::refresh_token`, never through the engine's default path.
- `[INV-CB-04] Origin pinning is durable and attribution-bound`: every credential stores its minting origin (`codebuddy_origin`) in the token attributes and every refresh replays it. Mixing lines across a rotation would invalidate the pair at the identity provider. Enforced by `CodeBuddyOAuthEnricher` on both `on_login_success` and `on_refresh_success`.
- `[INV-CB-05] Credential kind is typed, header shape is decided by typestate`: `Auth.tokenType = "Bearer"` produces `CodeBuddyBearerCredential`, `"ApiKey"` produces nothing (the downstream header layer reads the marker's presence). Enforced by `credential::tests::token_type_mapping` and the `from_token_type` mapping.

## Consequences

- **Positive**: Nuo's CodeBuddy connection gets real account-login UX (browser sign-in, no manual token copy-paste); tokens rotate automatically on the pre-provider-skew window; both domestic and international lines are addressable; the openapi reference now documents the auth surface a maintainer would otherwise have to re-derive from decompilation.
- **Negative & Trade-offs**: Adds a vendor-specific `begin_device_login` and `refresh_token` override — the two places the vendor's protocol intentionally deviates from RFC. The engine must not be extended to accommodate them; the typed ports already exist for that (ADR-0027), and pushing *more* vendor behavior into the engine would recreate the coupling ADR-0027 was hired to prevent.

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): Ship the CLI's `apiKeyHelper` glue
- **Why considered**: The CLI's own BYOK helper (`apiKeyHelper` script + `CODEBUDDY_CODE_API_KEY_HELPER_TTL_MS`) is a documented way to supply a bearer token without writing an OAuth flow.
- **Why rejected**: A helper script is a *provisioning* aid, not a first-class login surface. Using it would put the burden on the user to write, host, and keep fresh a shell script — directly against the ADR-0014 subscription-lane favor for every vendor whose native CLI already owns an account login. BYOK via `apiKeyHelper` remains supported (the CLI's own behavior is understood), but Nuo does not *prefer* it.

### Alternative B (Rejected): Token-scrape the CLI's `.info` file as the primary flow
- **Why considered**: The CLI's storage format is a stable, readable JSON blob on disk; a credential source reading it directly gives instant login parity without any OAuth implementation.
- **Why rejected**: It is a **read-only projection**, not a *rotation-aware* surface — refresh would never happen, only re-scrape, and the CLI could invalidate the token mid-turn. It additionally entrenches a file path dependent on the CLI's own `CodeBuddyExtension` install directory, which is not part of the vendor's product API and clearly not portable across the four OS install layouts. It is preserved in the BYOK credential source as a **fallback** (still useful for users who've already logged in via the CLI and just want Nuo to ride the existing session), never as the subscription lane.

### Alternative C (Rejected): Some RFC-style Authorization-Code + PKCE redirect
- **Why considered**: The vendor's auth surface is OIDC-shaped (bearer JWT, scope incl. `openid profile offline_access email`, `sessionState`), suggesting a loopback redirect could work if a discovery document were available.
- **Why rejected**: Product config explicitly does **not publish** `authorize_url` / `token_url` for a loopback registration — the CLI never binds a local port and the server's `auth/state` route returns a URL the user's browser opens on the *auth origin*, never `127.0.0.1`. Forcing PKCE would be protocol fiction; the vendor surface takes the browser web-handoff protocol as-is.

## Links

- Related ADRs: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md) (dual-lane origin), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md) (vendor-crate ownership + typed ports), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md) (provider inversion).
- Wire reference: [docs/reference/openapi/codebuddy.yml](../reference/openapi/codebuddy.yml).
