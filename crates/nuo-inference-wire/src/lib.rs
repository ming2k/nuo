//! The inference wire: vendor dialect codecs, SSE stream demuxing, and framing
//! for external model APIs (OpenAI, Anthropic, Google Gemini, DeepSeek).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod credentials;
pub mod discriminative;
pub mod endpoint;
pub mod error;
pub mod generative;

// Dual-lane modality separation per ADR-0086
pub use generative::*;
pub use generative::{
    json, loop_detector, protocol, stream, token_classification, types, wire_protocol,
};

pub use credentials::{CredentialsProvider, DynamicCredentials, StaticApiKey};
pub use endpoint::{Endpoint, ProviderVendor};
pub use error::{Result, WireError};
pub use wire_protocol::WireProtocol;
