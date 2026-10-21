//! Egress timing telemetry and observation.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressObservation {
    #[default]
    Unreported,
    PooledConnection,
    ColdConnection,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressTimings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_sent_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_ready_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u64>,
    #[serde(default)]
    pub retransmits: u32,
    #[serde(default)]
    pub observation: EgressObservation,
    #[serde(skip)]
    pub dispatch_at: Option<std::time::Instant>,
}

/// Shared handle for recording and reading attempt telemetry.
#[derive(Debug, Clone, Default)]
pub struct EgressTelemetry(Arc<Mutex<Option<EgressTimings>>>);

impl EgressTelemetry {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    pub fn record(&self, timings: EgressTimings) {
        if let Ok(mut lock) = self.0.lock() {
            *lock = Some(timings);
        }
    }

    pub fn publish(&self, timings: EgressTimings) {
        self.record(timings);
    }

    pub fn snapshot(&self) -> Option<EgressTimings> {
        self.0.lock().ok().and_then(|lock| *lock)
    }

    pub fn read(&self) -> Option<EgressTimings> {
        self.snapshot()
    }
}
