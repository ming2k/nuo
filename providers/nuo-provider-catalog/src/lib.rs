//! Live remote model-catalog discovery for Nuo provider channels (ADR-0027 §2).
//!
//! Moved here from `nuo-provider-adapters/src/list_models.rs`. Owns the
//! `CatalogShape` → parser map and the `CatalogDiscovery` implementation that
//! the composition root registers.
//!
//! The `CatalogParser` family is shape-keyed, not provider-keyed: a provider
//! that reuses a shape costs zero new code (`ADR-0260`). Two parsers delegate
//! to their owning provider crate, which is why this crate depends on them:
//!
//! - `OpenAiCatalogParser`/`AnthropicCatalogParser`/`GoogleCatalogParser`/`CodexCatalogParser`
//!   are self-contained here.
//! - `OpencodeConsoleCatalogParser` → `nuo_provider_opencode::parse_config_catalog`.
//! - `SceneMapCatalogParser` → `nuo_provider_qoder::parse_scene_catalog`.
//!
//! `ANTHROPIC_VERSION` is re-exported from `nuo-provider-anthropic` because the
//! Anthropic parser stamps it on the catalog request.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

// ── Ports & envelopes (unchanged home: the contract leaf) ──
pub use nuo_provider::catalog::{
    CatalogSignature, CatalogSigning, DiscoveredModel, ModelListError, RemoteCatalogOptions,
    RemoteCatalogRequest, RemoteCatalogUpdate,
};
pub use nuo_model_codec::CatalogShape;

// ── Body moved verbatim from list_models.rs ──
mod list_models;
pub use list_models::*;
