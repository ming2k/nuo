//! Universal HTTP egress, connection pooling, SSE stream reassembly, and network telemetry for Nuo.
//!
//! Substrate crate providing outbound network connectivity across all Nuo components.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod client;
pub mod egress;
pub mod error;
pub mod http;
pub mod network;
pub mod request;
pub mod sse;
pub mod telemetry;

pub use ::http::StatusCode;
pub use client::Client;
pub use egress::{Egress, HttpResponse, NuoNetEgress, RequestParts};
pub use error::{EgressError, retry_after_ms};
pub use http::{Http, Reply, Request};
pub use network::{direct_client, shared_client};
pub use request::RequestBuilder;
pub use sse::{data_payloads, payloads_from_chunks};
pub use telemetry::{EgressObservation, EgressTelemetry, EgressTimings};
