//! Transport timing telemetry, owned by the protocol ledger.
//!
//! The values are pure data shared with durable request accounting; the
//! transport records them through this re-export.

pub use nuo_protocol::token_ledger::{TransportObservation, TransportTelemetry, TransportTimings};
