//! The kernel's durability port: where a fact batch goes, and what the kernel
//! is allowed to claim about it.

use futures::future::BoxFuture;
use nuo_wire::SessionDelta;

/// What a sink acknowledges after an append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    /// The highest fact sequence covered by this acknowledgement.
    pub durable_upto: u64,
    /// Facts the sink discarded by its own policy (retention, dedup, redaction).
    pub discarded: Vec<String>,
}

impl Ack {
    /// An acknowledgement covering `durable_upto` with nothing discarded.
    pub fn durable(durable_upto: u64) -> Self {
        Self {
            durable_upto,
            discarded: Vec::new(),
        }
    }
}

/// How a sink is doing, for degradation reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SinkHealth {
    /// Appends are being accepted and acknowledged.
    #[default]
    Healthy,
    /// Appends are being accepted but are not reaching durable storage.
    Degraded,
    /// Appends are failing.
    Failing,
}

impl SinkHealth {
    pub fn is_healthy(self) -> bool {
        matches!(self, Self::Healthy)
    }
}

/// A durability failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkError {
    /// What went wrong, for the operator.
    pub message: String,
    /// Whether retrying the same batch could succeed. A sink that knows a
    /// failure is permanent says so, and the kernel stops retrying.
    pub retryable: bool,
}

impl SinkError {
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SinkError {}

/// Where a fact batch goes.
pub trait FactSink: Send + Sync + 'static {
    /// Append one batch. All-or-nothing: either every node in the batch is
    /// durable when this returns, or none is.
    fn append(&self, batch: SessionDelta) -> BoxFuture<'static, Result<Ack, SinkError>>;

    /// Current health, for degradation reporting.
    fn health(&self) -> SinkHealth {
        SinkHealth::Healthy
    }
}

/// A sink that stores nothing and acknowledges everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl FactSink for NullSink {
    fn append(&self, batch: SessionDelta) -> BoxFuture<'static, Result<Ack, SinkError>> {
        let watermark = batch.new_watermark_seq;
        Box::pin(async move { Ok(Ack::durable(watermark)) })
    }
}

pub mod conformance;
pub mod record;

pub use conformance::{ConformanceReport, Hydrate, MemorySink, run_conformance};
pub use record::*;

