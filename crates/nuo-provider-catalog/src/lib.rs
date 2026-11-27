//! Live remote model-catalog discovery for standard industry catalog shapes
//! (`OpenAi`, `Anthropic`, `Google`).
//!
//! This crate is strictly vendor-neutral: it depends on no `provider-*` crate
//! and carries no provider-specific request signing or private response formats.
//! Providers with custom catalog protocols implement discovery in their own
//! crate and are wired at the composition root.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// ── Ports & envelopes (unchanged home: the contract leaf) ──
pub use nuo_protocol::CatalogShape;
pub use nuo_provider::catalog::{
    CatalogSignature, CatalogSigning, DiscoveredModel, ModelListError, RemoteCatalogOptions,
    RemoteCatalogRequest, RemoteCatalogUpdate,
};

// ── Body moved verbatim from list_models.rs ──
mod list_models;
pub use list_models::*;
