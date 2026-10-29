//! Provider-owned tuning knobs, re-exported for the local East-West protocol.
//!
//! The contract itself lives in [`nuo_provider::tuning`] — it is provider
//! vocabulary (which knobs exist, what they accept, how they reach the wire),
//! not IPC framing. This module only republishes it so frontends and the server
//! share one type across the protocol boundary rather than echoing a parallel
//! definition that could drift.
pub use nuo_provider::tuning::*;
