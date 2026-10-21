//! Wire protocol formatting and parsing for supported model vendors.

pub mod anthropic_messages;
pub mod google_generate_content;
pub mod google_interactions;
pub mod openai_chat_completions;
pub mod openai_responses;

use crate::endpoint::Endpoint;
use crate::error::Result;
use crate::types::{WireRequest, WireResponse};
use crate::wire_protocol::WireProtocol;

/// Builds the HTTP request (URL, headers, and body payload) tailored to the wire protocol and stream mode.
pub fn build_request(
    endpoint: &Endpoint,
    token: &str,
    request: &WireRequest,
    is_stream: bool,
) -> (String, http::HeaderMap, serde_json::Value) {
    match endpoint.protocol {
        WireProtocol::OpenAiChatCompletions => {
            openai_chat_completions::build_request(endpoint, token, request, is_stream)
        }
        WireProtocol::OpenAiResponses => {
            openai_responses::build_request(endpoint, token, request, is_stream)
        }
        WireProtocol::AnthropicMessages => {
            anthropic_messages::build_request(endpoint, token, request, is_stream)
        }
        WireProtocol::GoogleGenerateContent => {
            google_generate_content::build_request(endpoint, token, request, is_stream)
        }
        WireProtocol::GoogleInteractions => {
            google_interactions::build_request(endpoint, token, request, is_stream)
        }
    }
}

/// Parses the vendor JSON response into unified [`WireResponse`].
pub fn parse_response(endpoint: &Endpoint, json_val: &serde_json::Value) -> Result<WireResponse> {
    match endpoint.protocol {
        WireProtocol::OpenAiChatCompletions => openai_chat_completions::parse_response(json_val),
        WireProtocol::OpenAiResponses => openai_responses::parse_response(json_val),
        WireProtocol::AnthropicMessages => anthropic_messages::parse_response(json_val),
        WireProtocol::GoogleGenerateContent => google_generate_content::parse_response(json_val),
        WireProtocol::GoogleInteractions => google_interactions::parse_response(json_val),
    }
}
