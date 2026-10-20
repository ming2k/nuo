//! Shared transport engines, HTTP egress, SSE stream demuxing, and RFC OAuth primitives.
//!
//! Substrate transport crate consumed by concrete provider packages in `providers/nuo-provider-*`.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod client;
pub mod egress;
pub mod endpoint;
pub mod http;
pub mod json;
pub mod network;
pub mod pipeline;
pub mod prompt_cache;
pub mod request;
pub mod sse;
pub mod transport;
pub mod vision;

pub use ::http::StatusCode;
pub use client::Client;
pub use egress::{Egress, HttpResponse, NuoNetEgress, RequestParts};
pub use endpoint::*;
pub use http::{Http, Request};
pub use pipeline::TransportPipeline;
pub use prompt_cache::PromptCacheConfig;
pub use sse::{data_payloads, payloads_from_chunks};
pub use transport::{decode_response_json, ensure_success, retry_after_ms};
pub use vision::project_images_for_route;
