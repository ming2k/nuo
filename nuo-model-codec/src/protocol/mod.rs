//! Wire protocol formatting and parsing for supported model vendors.

pub mod anthropic;
pub mod google;
pub mod openai;

use crate::endpoint::{Endpoint, ProviderVendor};
use crate::error::Result;
use crate::types::{WireRequest, WireResponse};

/// Builds the HTTP request (URL, headers, and body payload) tailored to the vendor wire format and stream mode.
pub fn build_request(
    endpoint: &Endpoint,
    token: &str,
    request: &WireRequest,
    is_stream: bool,
) -> (String, http::HeaderMap, serde_json::Value) {
    match endpoint.vendor {
        ProviderVendor::OpenAi | ProviderVendor::DeepSeek | ProviderVendor::Ollama => {
            openai::build_request(endpoint, token, request, is_stream)
        }
        ProviderVendor::Anthropic => anthropic::build_request(endpoint, token, request, is_stream),
        ProviderVendor::Google => google::build_request(endpoint, token, request, is_stream),
    }
}

/// Parses the vendor JSON response into unified [`WireResponse`].
pub fn parse_response(endpoint: &Endpoint, json_val: &serde_json::Value) -> Result<WireResponse> {
    match endpoint.vendor {
        ProviderVendor::OpenAi | ProviderVendor::DeepSeek | ProviderVendor::Ollama => {
            openai::parse_response(json_val)
        }
        ProviderVendor::Anthropic => anthropic::parse_response(json_val),
        ProviderVendor::Google => google::parse_response(json_val),
    }
}
