//! Re-export of the protocol-owned capability contract.
//!
//! The `Provider` trait and model-request vocabulary are pure contracts; the
//! protocol layer owns them. Transport implementations in this crate implement
//! the same trait.
pub use nuo_protocol::capability::*;
