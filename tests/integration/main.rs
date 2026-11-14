//! Root integration entry (ADR-0178).
//!
//! Every integration suite in the workspace is one module under this entry, so
//! the whole tier links once instead of producing one test binary per crate and
//! per file. Shared helpers live in [`common`].
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// Historical tests refer to `nuo_harness::`; keep the rename alias at the root
// so every moved module resolves it exactly as the old crate-root target did.
extern crate nuo_engine as nuo_harness;

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
