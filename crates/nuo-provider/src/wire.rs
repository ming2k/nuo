//! Provider-shaped request stages.
//!
//! A [`WireRequest`] is the projected, provider-specific JSON body. An
//! [`EncodedRequest`] is the bytes handed to the transport. Keeping the two
//! stages separate makes projection (which provider shape) and encoding (which
//! bytes) independently testable, and gives every provider one place to encode.
use serde_json::Value;

use nuo_protocol::{ProviderError, ProviderErrorKind, WireProtocol};

/// A provider-shaped request body before serialization.
#[derive(Debug, Clone)]
pub struct WireRequest {
    /// The wire protocol this body targets.
    pub protocol: WireProtocol,
    /// The provider-specific JSON body.
    pub body: Value,
}

impl WireRequest {
    pub fn new(protocol: WireProtocol, body: Value) -> Self {
        Self { protocol, body }
    }

    /// Serialize the body into transport-ready bytes.
    pub fn encode(&self) -> Result<EncodedRequest, ProviderError> {
        let body = serde_json::to_vec(&self.body).map_err(|error| {
            ProviderError::new(
                "wire",
                ProviderErrorKind::Protocol,
                format!(
                    "failed to serialize {} request: {error}",
                    self.protocol.as_str()
                ),
            )
        })?;
        Ok(EncodedRequest {
            protocol: self.protocol,
            body,
            content_type: "application/json",
        })
    }
}

/// An encoded request ready for the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedRequest {
    /// The wire protocol these bytes target.
    pub protocol: WireProtocol,
    /// The serialized body.
    pub body: Vec<u8>,
    /// The HTTP content type.
    pub content_type: &'static str,
}

impl EncodedRequest {
    pub fn as_bytes(&self) -> &[u8] {
        &self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_preserves_protocol_and_json_bytes() {
        let wire = WireRequest::new(
            WireProtocol::AnthropicMessages,
            serde_json::json!({"model": "claude", "messages": []}),
        );
        let encoded = wire.encode().expect("json value encodes");
        assert_eq!(encoded.protocol, WireProtocol::AnthropicMessages);
        assert_eq!(encoded.content_type, "application/json");
        let value: serde_json::Value =
            serde_json::from_slice(encoded.as_bytes()).expect("valid json");
        assert_eq!(value["model"], "claude");
    }
}
