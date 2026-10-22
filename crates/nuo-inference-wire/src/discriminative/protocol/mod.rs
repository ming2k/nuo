//! Protocol dispatch and wire format drivers for System One discriminative models.

pub mod typesafe;

use crate::discriminative::endpoint::DiscriminativeEndpoint;
use crate::discriminative::types::{DiscriminativeRequest, DiscriminativeResponse};
use crate::error::Result;
use http::HeaderMap;
use serde::{Deserialize, Serialize};

/// Supported System One discriminative protocol dialects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DiscriminativeProtocol {
    /// TypeSafe System One API (POST /v1/systemone, powering Jev).
    #[default]
    #[serde(rename = "typesafe-system-one", alias = "system-one", alias = "jev")]
    TypeSafeSystemOne,
}

impl DiscriminativeProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TypeSafeSystemOne => "typesafe-system-one",
        }
    }
}

/// Builds request targeting the endpoint's configured protocol dialect.
pub fn build_request(
    endpoint: &DiscriminativeEndpoint,
    token: &str,
    request: &DiscriminativeRequest,
) -> (String, HeaderMap, serde_json::Value) {
    match endpoint.protocol {
        DiscriminativeProtocol::TypeSafeSystemOne => {
            typesafe::build_request(endpoint, token, request)
        }
    }
}

/// Parses the vendor response into [`DiscriminativeResponse`] according to protocol dialect.
pub fn parse_response(
    endpoint: &DiscriminativeEndpoint,
    json_val: &serde_json::Value,
) -> Result<DiscriminativeResponse> {
    match endpoint.protocol {
        DiscriminativeProtocol::TypeSafeSystemOne => typesafe::parse_response(json_val),
    }
}
