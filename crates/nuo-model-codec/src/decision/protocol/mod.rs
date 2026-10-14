//! Protocol dispatch and wire format drivers for System One models.

pub mod typesafe;

use crate::decision::endpoint::DecisionEndpoint;
use crate::decision::types::{DecisionRequest, DecisionResponse};
use crate::error::Result;
use http::HeaderMap;
use serde::{Deserialize, Serialize};

/// Supported System One decision protocol dialects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DecisionProtocol {
    /// TypeSafe System One API (POST /v1/systemone, powering Jev).
    #[default]
    #[serde(rename = "typesafe-system-one", alias = "system-one", alias = "jev")]
    TypeSafeSystemOne,
}

impl DecisionProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TypeSafeSystemOne => "typesafe-system-one",
        }
    }
}

/// Builds request targeting the endpoint's configured protocol dialect.
pub fn build_request(
    endpoint: &DecisionEndpoint,
    token: &str,
    request: &DecisionRequest,
) -> (String, HeaderMap, serde_json::Value) {
    match endpoint.protocol {
        DecisionProtocol::TypeSafeSystemOne => typesafe::build_request(endpoint, token, request),
    }
}

/// Parses the vendor response into [`DecisionResponse`] according to protocol dialect.
pub fn parse_response(endpoint: &DecisionEndpoint, json_val: &serde_json::Value) -> Result<DecisionResponse> {
    match endpoint.protocol {
        DecisionProtocol::TypeSafeSystemOne => typesafe::parse_response(json_val),
    }
}
