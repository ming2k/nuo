//! Alibaba Qoder's request identity material and durable auth store representation.

use nuo_host::SecretString;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Alibaba Qoder's request identity: the typed, provider-owned material the
/// COSY signing layer consumes.
#[derive(Clone, PartialEq, Eq)]
pub struct QoderRequestIdentity {
    /// The Qoder account's user id (`Cosy-User` header, payload `uid`).
    pub uid: String,
    /// The machine's AES key hex — generated once per device, persisted in
    /// the auth store, and stable across sessions.
    pub machine_key_hex: SecretString,
    /// Whether the user agreed to Qoder's data policy (`Cosy-Data-Policy`).
    pub data_policy_agreed: bool,
    /// Organization scope, when the account has one (`Cosy-Organization-Id`).
    pub organization_id: Option<String>,
    /// Organization tags, comma-joined in order (`Cosy-Organization-Tags`).
    pub organization_tags: Vec<String>,
    /// The inference endpoint the server elected for this account
    /// (`https://api*.qoder.sh`), from the center region-endpoints sync —
    /// see the integration doc §3.1a. `None` = never synced; callers fall
    /// back to the pinned `MODEL_PROVIDER_SPEC.root_url`.
    pub infer_endpoint: Option<String>,
}

impl QoderRequestIdentity {
    /// The inference root this identity carries: the elected endpoint when
    /// one was synced, else the caller-provided (pinned) base URL.
    pub fn infer_root<'a>(&'a self, pinned: &'a str) -> &'a str {
        self.infer_endpoint.as_deref().unwrap_or(pinned)
    }

    /// Identity payload plaintext for the AES layer (the `info` field's
    /// pre-encryption form).
    pub fn identity_payload_json(&self, bearer: &str, email: &str) -> String {
        serde_json::json!({
            "uid": self.uid,
            "aid": "",
            "name": "Muta",
            "email": email,
            "security_oauth_token": bearer,
        })
        .to_string()
    }
}

impl fmt::Debug for QoderRequestIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QoderRequestIdentity")
            .field("uid", &self.uid)
            .field("machine_key_hex", &"[REDACTED]")
            .field("data_policy_agreed", &self.data_policy_agreed)
            .field("organization_id", &self.organization_id)
            .field("organization_tags", &self.organization_tags)
            .field("infer_endpoint", &self.infer_endpoint)
            .finish()
    }
}

/// The durable form of [`QoderRequestIdentity`] — what the auth store serializes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QoderStoredIdentity {
    pub uid: String,
    /// AES key hex (32 lowercase hex chars); the device's signing identity.
    pub machine_key_hex: SecretString,
    #[serde(default)]
    pub data_policy_agreed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    #[serde(default)]
    pub organization_tags: Vec<String>,
    /// Server-elected inference endpoint (`https://api*.qoder.sh`), synced
    /// once from the center surface. Absent on pre-election credentials —
    /// `default` keeps the auth store backward-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infer_endpoint: Option<String>,
}

impl QoderStoredIdentity {
    /// Lift the stored identity into the typed request identity.
    pub fn to_request_identity(&self) -> QoderRequestIdentity {
        QoderRequestIdentity {
            uid: self.uid.clone(),
            machine_key_hex: self.machine_key_hex.clone(),
            data_policy_agreed: self.data_policy_agreed,
            organization_id: self.organization_id.clone(),
            organization_tags: self.organization_tags.clone(),
            infer_endpoint: self.infer_endpoint.clone(),
        }
    }
}
