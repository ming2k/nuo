//! Root integration entry (ADR-0178).
//!
//! Every integration suite in the workspace is one module under this entry, so
//! the whole tier links once instead of producing one test binary per crate and
//! per file. Shared helpers live in [`common`].
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../common/mod.rs"]
mod common;

mod acp;
mod engine;
mod governance;
mod inference_wire;
mod mcp;
mod model_routing_invariants;
mod nuo_host;
mod persistence;
mod protocol_layering;
mod provider_qoder;
mod server;
mod tool;
