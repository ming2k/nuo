//! Alibaba Qoder device-authorization flow.
//!
//! Qoder's device flow is a Qoder-flavored PKCE protocol, not RFC 8628 and
//! not ChatGPT's two-step exchange:
//!
//! 1. The client generates a `verifier` (43–128 chars), `challenge =
//!    base64url(SHA256(verifier))`, a `nonce` (uuid), and its machine id.
//! 2. It opens the browser page
//!    `https://qoder.com/device/selectAccounts?challenge=…&challenge_method=S256
//!    &nonce=…&machine_id=…&client_id=…` — there is **no** device-code
//!    endpoint; the browser URL itself is the device code.
//! 3. It polls `GET openapi.qoder.sh/api/v1/deviceToken/poll?…`. A `404`
//!    means still pending (keep polling at 1s); a `200` carries the `dt-`
//!    prefixed device token plus its `drt-` refresh token (~30-day device
//!    token lifetime per the protocol docs).
//! 4. Inference needs the *inference* token, which the device token
//!    exchanges via the OpenAPI surface (same `jobToken` family a pasted
//!    personal-access token uses).
//!
//! Timeout is 300s (the CLI's own deadline), not the 15-minute RFC default.

use serde::Deserialize;
use sha2::Digest;

use nuo_provider::{CredentialHost, CredentialStore, TokenSet};
use crate::identity::{QoderRequestIdentity, QoderStoredIdentity};
use nuo_provider_transport::oauth::{AuthError, TokenResponse};
use nuo_model_codec::{ResolvedAuth, SecretString};

pub fn stored_qoder_request_identity(
    store: &dyn CredentialStore,
    connection_id: &str,
) -> Option<QoderRequestIdentity> {
    store
        .read(connection_id)
        .ok()??
        .get_json_attr::<QoderStoredIdentity>("qoder")
        .map(|q| q.to_request_identity())
}

/// Poll cadence while the user has not approved yet (the CLI's own value).
const QODER_POLL_INTERVAL_MS: u64 = 1_000;
/// Total device-flow deadline (the CLI's own 300s).
const QODER_FLOW_DEADLINE_MS: u64 = 300 * 1_000;
/// Safety margin added to each sleep so we never wake exactly on a boundary.
const POLLING_SAFETY_MARGIN_MS: u64 = 100;

/// The nonce + verifier pair that identifies one pending authorization.
/// Qoder's flow has no server-side "device code" — these client-side values
/// ARE the correlation key.
#[derive(Debug, Clone)]
pub struct QoderDeviceSession {
    pub nonce: String,
    pub verifier: String,
    pub challenge: String,
    pub machine_id: String,
    pub client_id: String,
    pub authorize_url: String,
    /// Alibaba UMID device token carried by qodercli ≥1.1.57. Optional: the
    /// official client omits the parameter when the token is not ready, and
    /// the server accepts authorize URLs without it. Never fabricate or
    /// reuse one across devices — it anchors the risk layer's fingerprint.
    pub machine_token: Option<String>,
}

impl QoderDeviceSession {
    /// Generate a fresh session with default international authorize URL.
    /// `machine_id` must be the connection's stable machine UUID (36 chars) —
    /// it is what the server fingerprints.
    pub fn new(machine_id: &str, client_id: &str) -> Self {
        Self::with_authorize_url(
            machine_id,
            client_id,
            "https://qoder.com/device/selectAccounts",
        )
    }

    /// Generate a fresh session with a custom authorize URL (e.g. CN line).
    pub fn with_authorize_url(machine_id: &str, client_id: &str, authorize_url: &str) -> Self {
        Self::with_machine_token(machine_id, client_id, authorize_url, None)
    }

    /// Generate a fresh session carrying the client's UMID machine token
    /// (qodercli ≥1.1.57 appends `machine_token` to the authorize URL when
    /// it has one). `None` keeps the 1.1.34 wire shape.
    pub fn with_machine_token(
        machine_id: &str,
        client_id: &str,
        authorize_url: &str,
        machine_token: Option<String>,
    ) -> Self {
        let verifier = new_verifier();
        let challenge = base64_url_no_pad(&sha2::Sha256::digest(verifier.as_bytes()));
        Self {
            nonce: uuid::Uuid::new_v4().to_string(),
            verifier,
            challenge,
            machine_id: machine_id.to_string(),
            client_id: client_id.to_string(),
            authorize_url: authorize_url.to_string(),
            machine_token,
        }
    }

    /// The browser URL the user opens to approve. This doubles as the
    /// "device code" prompt (no separate user_code exists). Mirrors the
    /// official client: `machine_token` is appended only when present.
    pub fn user_url(&self) -> String {
        let mut url = format!(
            "{}?challenge={}&challenge_method=S256\
&nonce={}&machine_id={}&client_id={}",
            self.authorize_url, self.challenge, self.nonce, self.machine_id, self.client_id
        );
        if let Some(token) = self
            .machine_token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            url.push_str("&machine_token=");
            url.push_str(&percent(token));
        }
        url
    }
}

/// 43-character verifier: 86 hex chars of entropy trimmed — the CLI uses
/// random hex too, so the shape matches what the server accepts.
fn new_verifier() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut state = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    let mut out = String::with_capacity(43);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    while out.len() < 43 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push(HEX[((state >> 33) & 0xf) as usize] as char);
    }
    out
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The device-token poll response (`dt-` access, `drt-` refresh).
///
/// Field-name drift (qodercli 1.1.57): the poll endpoint now returns the
/// access token under `token`; 1.1.34-era clients used
/// `accessToken`/`device_token`. All three spellings are accepted.
#[derive(Debug, Deserialize)]
pub struct QoderDeviceToken {
    #[serde(alias = "accessToken", alias = "device_token", alias = "token")]
    pub access_token: SecretString,
    #[serde(default, alias = "refreshToken")]
    pub refresh_token: Option<SecretString>,
    #[serde(default)]
    pub uid: Option<String>,
    /// Milliseconds since the epoch (qodercli's `expireTime`); seconds-shaped
    /// values (>1e12) are normalized on the wire below.
    #[serde(default, alias = "expireTime", alias = "expire_time")]
    pub expire_time: Option<u64>,
}

/// The device-token poll endpoint (OpenAPI surface, international line).
const DEVICE_POLL_URL: &str = "https://openapi.qoder.sh/api/v1/deviceToken/poll";
/// The jobToken exchange endpoint (OpenAPI surface).
const JOB_TOKEN_EXCHANGE_URL: &str = "https://openapi.qoder.sh/api/v1/jobToken/exchange";

/// Poll the deviceToken endpoint until approval or deadline. `404` = pending.
pub async fn poll_device_token(
    client: &nuo_provider_transport::http::Http,
    session: &QoderDeviceSession,
) -> Result<QoderDeviceToken, crate::oauth::AuthError> {
    poll_device_token_at(
        client,
        DEVICE_POLL_URL,
        session,
        sleep_ms,
        QODER_FLOW_DEADLINE_MS,
    )
    .await
}

/// Test-injectable variant: explicit endpoint, sleep, and clock.
pub async fn poll_device_token_at<S, Fut>(
    client: &nuo_provider_transport::http::Http,
    endpoint: &str,
    session: &QoderDeviceSession,
    sleep: S,
    deadline_ms: u64,
) -> Result<QoderDeviceToken, crate::oauth::AuthError>
where
    S: Fn(u64) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    let start = std::time::Instant::now();
    // Consecutive-429 counter for backoff (reset on any non-429 response).
    let mut attempts: u32 = 0;
    loop {
        if start.elapsed().as_millis() as u64 >= deadline_ms {
            return Err(crate::oauth::AuthError::Timeout);
        }
        let url = format!(
            "{endpoint}?nonce={}&verifier={}&challenge_method=S256",
            percent(&session.nonce),
            percent(&session.verifier)
        );
        let request = nuo_provider_transport::http::Request::new(netune::Method::GET, &url)
            .header("accept", "application/json");
        let response = client
            .send(request)
            .await
            .map_err(|e| crate::oauth::AuthError::Transport(format!("device poll failed: {e}")))?;
        let status = response.status;
        let text = response.body;
        if status.is_success() {
            let token: QoderDeviceToken = serde_json::from_str(&text).map_err(|e| {
                crate::oauth::AuthError::Decode(format!("device token parse failed: {e}"))
            })?;
            return Ok(token);
        }
        let code = status.as_u16();
        if code == 429 {
            // Rate-limited (the /device/selectAccounts page renders 429 from
            // /device/redirect as the same "Parameter invalid" dialog, which
            // invites retry storms — break the loop with backoff instead of
            // failing fast). Exponential backoff, capped well under the flow
            // deadline so repeated 429s still end in AuthError::Timeout.
            attempts = attempts.saturating_add(1);
            let backoff = QODER_POLL_INTERVAL_MS.saturating_mul(1 << attempts.min(3));
            sleep(backoff + POLLING_SAFETY_MARGIN_MS).await;
            continue;
        }
        if code != 404 {
            return Err(crate::oauth::AuthError::TokenEndpoint {
                status: code,
                body: text,
            });
        }
        sleep(QODER_POLL_INTERVAL_MS + POLLING_SAFETY_MARGIN_MS).await;
    }
}

/// Production polling loop with the CLI's 300s deadline.
pub async fn poll_device_token_with<S, Fut>(
    client: &nuo_provider_transport::http::Http,
    session: &QoderDeviceSession,
    sleep: S,
) -> Result<QoderDeviceToken, crate::oauth::AuthError>
where
    S: Fn(u64) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    poll_device_token_at(
        client,
        DEVICE_POLL_URL,
        session,
        sleep,
        QODER_FLOW_DEADLINE_MS,
    )
    .await
}

/// Complete a device-flow login: poll for the device token and return it as
/// the token response. Mirrors qodercli ≥1.1.34, which adopts the polled
/// device token directly (its credential's `refreshStrategy` is
/// `"device-token"`); the inference token is *not* exchanged at login.
pub async fn device_login(
    client: &nuo_provider_transport::http::Http,
    session: &QoderDeviceSession,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let device = poll_device_token(client, session).await?;
    Ok(TokenResponse {
        access_token: device.access_token,
        refresh_token: device.refresh_token,
        id_token: None,
        token_type: Some("Bearer".to_string()),
        expires_in: device.expire_time.map(|ms| {
            if ms > 1_000_000_000_000 {
                ms / 1000
            } else {
                ms
            }
        }),
        scope: None,
        qoder_uid: device.uid,
    })
}

/// Exchange a Qoder credential (a `pt-` personal-access token) for the
pub async fn exchange_inference_token(
    client: &nuo_provider_transport::http::Http,
    credential: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    exchange_inference_token_at(client, JOB_TOKEN_EXCHANGE_URL, credential).await
}

/// Test-injectable variant with an explicit endpoint.
pub async fn exchange_inference_token_at(
    client: &nuo_provider_transport::http::Http,
    endpoint: &str,
    credential: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let request = nuo_provider_transport::http::Request::new(netune::Method::POST, endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&serde_json::json!({ "personal_token": credential }));
    let response = client
        .send(request)
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("exchange failed: {e}")))?;
    let status = response.status;
    let text = response.body;
    if !status.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    let parsed: TokenResponse = serde_json::from_str(&text)
        .map_err(|e| crate::oauth::AuthError::Decode(format!("exchange parse failed: {e}")))?;
    parsed.validate()
}

/// The device-token refresh endpoint (OpenAPI surface, international line).
const DEVICE_TOKEN_REFRESH_URL: &str = "https://openapi.qoder.sh/api/v1/deviceToken/refresh";

/// The OpenAPI userinfo endpoint: a plain-bearer `GET` returning the account's
/// `id` (the uid the signed surfaces carry as `Cosy-User`).
const USERINFO_URL: &str = "https://openapi.qoder.sh/api/v1/userinfo";

/// Rotate a `dt-` device token with its `drt-` device refresh token
/// (`POST /api/v1/deviceToken/refresh` with a JSON `refresh_token` body —
/// verified against the live endpoint: a missing field yields
/// `DeviceRefreshTokenRequired`, a wrong prefix yields
/// `DeviceRefreshTokenPrefixInvalid`).
pub async fn refresh_device_token(
    client: &nuo_provider_transport::http::Http,
    refresh_token: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    refresh_device_token_at(client, DEVICE_TOKEN_REFRESH_URL, refresh_token).await
}

/// Resolve the account uid from the OpenAPI userinfo endpoint.
///
/// The signed surfaces carry the account uid as `Cosy-User`, and the model
/// catalog's signature **requires** it (verified live: omitting it yields
/// `403 code 101` even with a byte-correct signature). The device-token flow
/// learns the uid from its token response; the personal-access-token flow has
/// no such response, so the uid is fetched here with a plain bearer — no COSY
/// signature, per the OpenAPI surface.
pub async fn fetch_uid(
    client: &nuo_provider_transport::http::Http,
    bearer: &str,
) -> Result<String, crate::oauth::AuthError> {
    fetch_uid_at(client, USERINFO_URL, bearer).await
}

/// Test-injectable variant with an explicit endpoint.
pub async fn fetch_uid_at(
    client: &nuo_provider_transport::http::Http,
    endpoint: &str,
    bearer: &str,
) -> Result<String, crate::oauth::AuthError> {
    let request = nuo_provider_transport::http::Request::new(netune::Method::GET, endpoint)
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {bearer}"));
    let response = client
        .send(request)
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("userinfo failed: {e}")))?;
    if !response.status.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: response.status.as_u16(),
            body: response.body,
        });
    }
    let value: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|e| crate::oauth::AuthError::Decode(format!("userinfo parse failed: {e}")))?;
    value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| crate::oauth::AuthError::Decode("userinfo response has no id".to_string()))
}

/// Test-injectable variant with an explicit endpoint.
pub async fn refresh_device_token_at(
    client: &nuo_provider_transport::http::Http,
    endpoint: &str,
    refresh_token: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let request = nuo_provider_transport::http::Request::new(netune::Method::POST, endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&serde_json::json!({ "refresh_token": refresh_token }));
    let response = client
        .send(request)
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("refresh failed: {e}")))?;
    let status = response.status;
    let text = response.body;
    if !status.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    let device: QoderDeviceToken = serde_json::from_str(&text).map_err(|e| {
        crate::oauth::AuthError::Decode(format!("device refresh parse failed: {e}"))
    })?;
    Ok(TokenResponse {
        access_token: device.access_token,
        refresh_token: device.refresh_token,
        id_token: None,
        token_type: Some("Bearer".to_string()),
        expires_in: device.expire_time.map(|ms| {
            if ms > 1_000_000_000_000 {
                ms / 1000
            } else {
                ms
            }
        }),
        scope: None,
        qoder_uid: device.uid,
    })
}

fn percent(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// Generate a fresh machine AES key as 32 lowercase hex characters.
///
/// The key is generated once per device at login and persisted with the
/// connection — rotating it would look like device churn to Qoder's risk
/// layer. The generator lives beside [`CosyIdentity`] in `nuo-llm-client` so
/// generation and consumption share one definition; this re-export keeps the
/// credential layer's call site unchanged.
pub use crate::wire::generate_machine_key_hex;

/// Credential source for a pasted Qoder personal-access token (`pt-…`).
///
/// Qoder runs this as ApiKey auth on the COSY surface, but the surface still
/// demands the full typed request identity (machine key, uid, org scope).
/// This source owns that material: the machine key is a per-device AES key
/// persisted beside the credentials (stable across processes — rotating it
/// would look like device churn), and the uid is resolved from the OpenAPI
/// userinfo endpoint and cached in the same slot.
///
/// The uid is **required** by the model catalog's signature (the service
/// rejects a catalog request without `Cosy-User` — see the integration doc
/// §5.2), so an unresolved uid blocks the catalog sync even though inference tolerates
/// its absence. Existing credentials minted with an empty uid are backfilled
/// once, transparently.
pub struct QoderApiKeyCredentialSource {
    connection_id: String,
    token: SecretString,
    identity: std::sync::Mutex<Option<crate::QoderRequestIdentity>>,
    /// Where the per-device identity is persisted (the same store OAuth
    /// credentials use: one identity home per connection).
    store: std::sync::Arc<dyn CredentialStore>,
}

impl std::fmt::Debug for QoderApiKeyCredentialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QoderApiKeyCredentialSource")
            .field("connection_id", &self.connection_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl QoderApiKeyCredentialSource {
    pub fn new(host: &CredentialHost, connection_id: impl Into<String>, token: SecretString) -> Self {
        Self {
            connection_id: connection_id.into(),
            token,
            identity: std::sync::Mutex::new(None),
            store: std::sync::Arc::clone(host.store()),
        }
    }

    fn store(&self) -> &dyn CredentialStore {
        self.store.as_ref()
    }

    /// The per-device identity slot, persisted in the auth store under the
    /// connection id (same file OAuth credentials use — one identity home
    /// per connection).
    pub(crate) async fn load_or_create_identity(
        &self,
    ) -> Result<crate::QoderRequestIdentity, String> {
        if let Some(existing) = self
            .identity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Ok(existing);
        }
        let stored_identity = self
            .store()
            .read(&self.connection_id)
            .map_err(|e| e.to_string())?
            .and_then(|tokens| {
                tokens.get_json_attr::<crate::QoderStoredIdentity>("qoder")
            });
        if let Some(stored) = stored_identity {
            let uid = if stored.uid.is_empty() {
                self.resolve_and_persist_uid(&stored).await
            } else {
                stored.uid.clone()
            };
            let mut identity = stored.to_request_identity();
            identity.uid = uid;
            // Best-effort one-shot election: a credential created before the
            // election existed carries `infer_endpoint: None`. A failed sync
            // leaves it None (pinned root stays authoritative, §3.1a).
            if identity.infer_endpoint.is_none() {
                identity.infer_endpoint = self.elect_and_persist_endpoint(&identity).await;
            }
            *self.identity.lock().unwrap_or_else(|e| e.into_inner()) = Some(identity.clone());
            return Ok(identity);
        }
        // First use: mint the device identity, resolve the uid, and persist
        // both through the cross-process lock (the store's transactional write
        // path).
        let uid = self.resolve_uid().await?;
        let identity = crate::QoderRequestIdentity {
            uid: uid.clone(),
            machine_key_hex: SecretString::from(generate_machine_key_hex()),
            data_policy_agreed: true,
            organization_id: None,
            organization_tags: Vec::new(),
            infer_endpoint: None,
        };
        let mut session = self
            .store()
            .lock()
            .await
            .map_err(|e| format!("could not lock the credential store: {e}"))?;
        let mut entry = session
            .get(&self.connection_id)
            .unwrap_or_else(|| TokenSet {
                access: self.token.clone(),
                refresh: String::new().into(),
                expires_ms: i64::MAX,
                id_token: None,
                token_type: None,
                scope: None,
                user_email: None,
                attributes: serde_json::Map::new(),
            });
        // Never clobber a live bearer with our placeholder if OAuth rotated
        // concurrently: keep the existing access token when present.
        if entry.access.expose_secret().trim().is_empty() {
            entry.access = self.token.clone();
        }
        entry.set_json_attr(
            "qoder",
            &QoderStoredIdentity {
                uid: uid.clone(),
                machine_key_hex: identity.machine_key_hex.clone(),
                data_policy_agreed: true,
                organization_id: None,
                organization_tags: Vec::new(),
                infer_endpoint: None,
            },
        );
        session.set(&self.connection_id, entry);
        session.commit().await.map_err(|e| e.to_string())?;
        // One-shot endpoint election for the freshly minted identity.
        let mut identity = identity;
        identity.infer_endpoint = self.elect_and_persist_endpoint(&identity).await;
        *self.identity.lock().unwrap_or_else(|e| e.into_inner()) = Some(identity.clone());
        Ok(identity)
    }

    /// Elect the inference endpoint from the center region map and persist it
    /// beside the stored identity (best-effort; `None` on any failure keeps
    /// the pinned `MODEL_PROVIDER_SPEC.root_url` authoritative — §3.1a).
    async fn elect_and_persist_endpoint(
        &self,
        identity: &crate::QoderRequestIdentity,
    ) -> Option<String> {
        if identity.infer_endpoint.is_some() {
            return identity.infer_endpoint.clone();
        }
        let Ok(client) = nuo_provider_transport::http::Http::control_plane() else {
            return None;
        };
        let elected =
            crate::elect_infer_endpoint(&client, self.token.expose_secret())
                .await
                .ok()?;
        if let Ok(mut session) = self.store().lock().await
            && let Some(mut entry) = session.get(&self.connection_id)
            && let Some(mut stored) = entry.get_json_attr::<QoderStoredIdentity>("qoder")
        {
            stored.infer_endpoint = Some(elected.clone());
            entry.set_json_attr("qoder", &stored);
            session.set(&self.connection_id, entry);
            if let Err(error) = session.commit().await {
                tracing::warn!(connection = %self.connection_id, %error, "qoder: could not persist the elected endpoint");
            }
        }
        Some(elected)
    }

    /// Resolve the account uid for this PAT, or `""` if the lookup fails.
    ///
    /// A failed lookup is non-fatal here: inference tolerates an absent
    /// `Cosy-User` (the catalog does not — see §5.2 of the integration doc).
    /// Callers that need the uid for a signed catalog get an explicit failure
    /// from the catalog fetch instead.
    async fn resolve_uid(&self) -> Result<String, String> {
        let client = nuo_provider_transport::http::Http::control_plane()
            .map_err(|error| format!("could not build the userinfo client: {error}"))?;
        match fetch_uid(&client, self.token.expose_secret()).await {
            Ok(uid) => Ok(uid),
            Err(error) => {
                tracing::warn!(
                    connection = %self.connection_id,
                    %error,
                    "qoder: could not resolve the account uid; the signed catalog will fail until it is resolved"
                );
                Ok(String::new())
            }
        }
    }

    /// Backfill a stored identity's empty uid and persist it.
    ///
    /// The signed surfaces require a non-empty uid inside `info` (an empty uid
    /// yields `403 code 101` even with a correct signature), so an empty uid on
    /// an existing credential is repaired once and written back. Org scope is
    /// preserved from the stored record — only the uid is filled in.
    async fn resolve_and_persist_uid(&self, stored: &QoderStoredIdentity) -> String {
        let Ok(uid) = self.resolve_uid().await else {
            return String::new();
        };
        if uid.is_empty() {
            return uid;
        }
        if let Ok(mut session) = self.store().lock().await
            && let Some(mut entry) = session.get(&self.connection_id)
        {
            let mut updated = stored.clone();
            updated.uid.clone_from(&uid);
            entry.set_json_attr("qoder", &updated);
            session.set(&self.connection_id, entry);
            if let Err(error) = session.commit().await {
                tracing::warn!(connection = %self.connection_id, %error, "qoder: could not persist resolved uid");
            }
        } else if std::env::var("MUTA_QODER_DEBUG").is_ok() {
            eprintln!("QODER_UID_PERSIST_SKIPPED connection={}", self.connection_id);
        }
        uid
    }
}
impl nuo_model_codec::CredentialSource for QoderApiKeyCredentialSource {
    fn resolve_auth<'a>(&'a self) -> futures::future::BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            let identity = self.load_or_create_identity().await?;
            Ok(ResolvedAuth::new(self.token.clone()).with_extension(identity))
        })
    }

    fn force_refresh<'a>(&'a self) -> futures::future::BoxFuture<'a, Result<ResolvedAuth, String>> {
        // Static PAT: nothing to rotate; the identity is already minted.
        Box::pin(async move {
            let identity = self.load_or_create_identity().await?;
            Ok(ResolvedAuth::new(self.token.clone()).with_extension(identity))
        })
    }

    fn is_ready(&self) -> bool {
        !self.token.expose_secret().trim().is_empty()
    }

    fn is_oauth(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn userinfo_resolves_the_uid_from_the_id_field() {
        let (addr, _hits, _keep) = spawn_scripted_server(vec![(
            200,
            r#"{"id":"9bdb6daa-be3f-45d6-8eee-a1f74dd18553","name":"Ming"}"#.to_string(),
        )])
        .await;
        let client = nuo_provider_transport::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/userinfo");
        let uid = fetch_uid_at(&client, &url, "dt-token")
            .await
            .expect("userinfo resolves");
        assert_eq!(uid, "9bdb6daa-be3f-45d6-8eee-a1f74dd18553");
    }

    #[tokio::test]
    async fn userinfo_without_an_id_fails_closed() {
        // An empty or absent `id` must not silently mint an empty uid: the
        // catalog's signature requires `Cosy-User`, so a missing uid is a hard
        // error rather than a request that will fail upstream.
        let (addr, _hits, _keep) =
            spawn_scripted_server(vec![(200, r#"{"name":"Ming"}"#.to_string())]).await;
        let client = nuo_provider_transport::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/userinfo");
        assert!(fetch_uid_at(&client, &url, "dt-token").await.is_err());
    }

    #[test]
    fn session_url_carries_the_full_pkce_contract() {
        let session = QoderDeviceSession::new(
            "0f8e2b1a-1111-4222-8333-444455556666",
            "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb",
        );
        let url = session.user_url();
        assert!(
            url.starts_with("https://qoder.com/device/selectAccounts?"),
            "{url}"
        );
        assert!(url.contains("challenge_method=S256"));
        assert!(url.contains("client_id=e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb"));
        assert!(url.contains("machine_id=0f8e2b1a"));
        assert!(url.contains(&format!("nonce={}", session.nonce)));
        // Challenge is unpadded base64url of SHA256(verifier).
        let expected = base64_url_no_pad(&sha2::Sha256::digest(session.verifier.as_bytes()));
        assert!(url.contains(&format!("challenge={expected}")));
    }

    #[test]
    fn session_url_respects_custom_authorize_url() {
        let session = QoderDeviceSession::with_authorize_url(
            "0f8e2b1a-1111-4222-8333-444455556666",
            "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb",
            "https://qoder.cn/device/selectAccounts",
        );
        let url = session.user_url();
        assert!(
            url.starts_with("https://qoder.cn/device/selectAccounts?"),
            "{url}"
        );
        assert!(url.contains("client_id=e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb"));
    }

    #[test]
    fn verifier_meets_the_length_floor() {
        let session = QoderDeviceSession::new("m", "c");
        assert!(session.verifier.len() >= 43);
        assert!(session.verifier.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn percent_encodes_query_values() {
        assert_eq!(percent("abc-_.~"), "abc-_.~");
        assert_eq!(percent("a b+c"), "a%20b%2Bc");
    }

    #[tokio::test]
    async fn poll_surfaces_non_404_errors_immediately() {
        // The owned transport has no per-client base-URL override, so bind a
        // raw TCP server and hand its URL to the injectable endpoint.
        let (addr, hits, _keep) = spawn_scripted_server(vec![(500, "boom".to_string())]).await;
        let session = QoderDeviceSession::new("m", "c");
        let client = nuo_provider_transport::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/deviceToken/poll");
        let result = poll_device_token_at(&client, &url, &session, |_| async {}, 5_000).await;
        assert!(matches!(
            result,
            Err(crate::oauth::AuthError::TokenEndpoint { status: 500, .. })
        ));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn poll_treats_404_as_pending_then_succeeds() {
        let (addr, hits, _keep) = spawn_scripted_server(vec![
            (404, "not found".to_string()),
            (
                200,
                r#"{"access_token":"dt-abc","refresh_token":"jrt-x","uid":"u1"}"#.to_string(),
            ),
        ])
        .await;
        let session = QoderDeviceSession::new("m", "c");
        let client = nuo_provider_transport::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/deviceToken/poll");
        let result = poll_device_token_at(&client, &url, &session, |_| async {}, 5_000).await;
        let token = result.unwrap();
        assert_eq!(token.access_token.expose_secret(), "dt-abc");
        assert_eq!(token.uid.as_deref(), Some("u1"));
        // One pending poll + one success.
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn poll_treats_429_as_rate_limit_with_backoff() {
        // Two 429s then success: the flow must retry with backoff instead of
        // surfacing TokenEndpoint (the web page renders 429 from
        // /device/redirect as "Parameter invalid", which invites retry
        // storms — the poller breaks that loop itself).
        let (addr, hits, _keep) = spawn_scripted_server(vec![
            (429, "slow down".to_string()),
            (429, "slow down".to_string()),
            (
                200,
                r#"{"token":"dt-abc","refresh_token":"jrt-x"}"#.to_string(),
            ),
        ])
        .await;
        let session = QoderDeviceSession::new("m", "c");
        let client = nuo_provider_transport::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/deviceToken/poll");
        let result = poll_device_token_at(&client, &url, &session, |_| async {}, 60_000).await;
        let token = result.unwrap();
        assert_eq!(token.access_token.expose_secret(), "dt-abc");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn poll_accepts_1_1_57_token_field_name() {
        // qodercli 1.1.57 renamed the poll response field to `token`; the
        // 1.1.34 spellings must keep decoding too.
        for body in [
            r#"{"token":"dt-abc"}"#,
            r#"{"accessToken":"dt-abc"}"#,
            r#"{"device_token":"dt-abc"}"#,
        ] {
            let parsed: QoderDeviceToken = serde_json::from_str(body).unwrap();
            assert_eq!(parsed.access_token.expose_secret(), "dt-abc");
        }
    }

    #[tokio::test]
    async fn device_login_adopts_the_device_token_without_exchange() {
        // qodercli ≥1.1.34 uses refreshStrategy="device-token": the polled
        // `dt-` token IS the credential. Exchanging it as a `personal_token`
        // against /api/v1/jobToken/exchange is what produced the login-time
        // "token endpoint returned HTTP 400 BadRequest" regression.
        let (addr, hits, _keep) = spawn_scripted_server(vec![(
            200,
            r#"{"token":"dt-abc","refreshToken":"drt-xyz","uid":"u1","expireTime":1700000000000}"#
                .to_string(),
        )])
        .await;
        let session = QoderDeviceSession::new("m", "c");
        let client = nuo_provider_transport::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/deviceToken/poll");
        let mut session = session;
        session.authorize_url = url.replace("/api/v1/deviceToken/poll", "/device/selectAccounts");
        // Poll directly (device_login wraps poll_device_token with prod URLs).
        let token = poll_device_token_at(&client, &url, &session, |_| async {}, 5_000)
            .await
            .unwrap();
        let login = TokenResponse {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            id_token: None,
            token_type: Some("Bearer".to_string()),
            expires_in: token.expire_time.map(|ms| {
                if ms > 1_000_000_000_000 {
                    ms / 1000
                } else {
                    ms
                }
            }),
            scope: None,
            qoder_uid: token.uid,
        };
        assert!(login.access_token.expose_secret().starts_with("dt-"));
        assert_eq!(
            login.refresh_token.as_ref().map(|r| r.expose_secret()),
            Some("drt-xyz")
        );
        assert_eq!(login.qoder_uid.as_deref(), Some("u1"));
        // Second-normalized expiry from the ms epoch value.
        assert_eq!(login.expires_in, Some(1_700_000_000));
        // Exactly one HTTP hit: no exchange round-trip.
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn device_refresh_posts_the_json_refresh_token() {
        let (addr, hits, _keep) = spawn_scripted_server(vec![(
            200,
            r#"{"token":"dt-new","refreshToken":"drt-new2","expireTime":1700000012000}"#
                .to_string(),
        )])
        .await;
        let client = nuo_provider_transport::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/deviceToken/refresh");
        let token = refresh_device_token_at(&client, &url, "drt-old")
            .await
            .unwrap();
        assert_eq!(token.access_token.expose_secret(), "dt-new");
        assert_eq!(
            token.refresh_token.as_ref().map(|r| r.expose_secret()),
            Some("drt-new2")
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn user_url_appends_machine_token_only_when_present() {
        let session = QoderDeviceSession::with_machine_token(
            "mid",
            "cid",
            "https://qoder.com/device/selectAccounts",
            Some("mt-1".to_string()),
        );
        assert!(session.user_url().ends_with("&machine_token=mt-1"));

        // Absent / blank tokens keep the 1.1.34 wire shape.
        let plain = QoderDeviceSession::with_machine_token(
            "mid",
            "cid",
            "https://qoder.com/device/selectAccounts",
            None,
        );
        assert!(!plain.user_url().contains("machine_token"));
        let blank = QoderDeviceSession::with_machine_token(
            "mid",
            "cid",
            "https://qoder.com/device/selectAccounts",
            Some("   ".to_string()),
        );
        assert!(!blank.user_url().contains("machine_token"));
    }

    #[tokio::test]
    async fn exchange_maps_the_job_token_response() {
        let addr = server_single(
            200,
            r#"{"access_token":"jt-infer","refresh_token":"jrt-refresh","expires_in":86400,"token_type":"Bearer"}"#,
        )
        .await;
        let client = nuo_provider_transport::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/jobToken/exchange");
        let result = exchange_inference_token_at(&client, &url, "pt-user-token").await;
        let token = result.unwrap();
        assert_eq!(token.access_token.expose_secret(), "jt-infer");
    }

    // ── test helpers ────────────────────────────────────────────────────────

    /// Minimal HTTP test server serving a scripted list of (status, body)
    /// responses, one per connection, repeating the last one indefinitely.
    async fn spawn_scripted_server(
        script: Vec<(u16, String)>,
    ) -> (
        std::net::SocketAddr,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_task = Arc::clone(&hits);
        let script = Arc::new(tokio::sync::Mutex::new(script));
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 2048];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                let step = {
                    let mut script = script.lock().await;
                    if script.len() > 1 {
                        script.remove(0)
                    } else {
                        script[0].clone()
                    }
                };
                hits_task.fetch_add(1, Ordering::SeqCst);
                let reason = if step.0 == 200 { "OK" } else { "ERR" };
                let payload = format!(
                    "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    step.0,
                    reason,
                    step.1.len(),
                    step.1
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, payload.as_bytes()).await;
                let _ = tokio::io::AsyncWriteExt::shutdown(&mut socket).await;
            }
        });
        (addr, hits, task)
    }

    async fn server_single(status: u16, body: &str) -> std::net::SocketAddr {
        spawn_scripted_server(vec![(status, body.to_string())])
            .await
            .0
    }
}
