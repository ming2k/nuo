//! Unit tests for the catalog modules: runtime derivation of routes from
//! connections + presets + catalog cache, credential resolution, per-route
//! reasoning, the fitted-model overlay, and live catalog sync.

use super::derive::{
    DerivationInputs, derive_channel, derive_entries, resolve_credential, route_models,
};
use super::picker::channel_model_info;
use super::sync::source_identity_for_connection;
use super::{
    build_picker_state, build_routes, refresh_connection_models_for_etag, sync_connection_routes,
    sync_fitted_model_registry, sync_remote_routes,
};
use nuo_persistence::config::{
    Config, Credentials, FittedModelInfo, ModelListCacheState, RemoteCatalogCache,
};
use nuo_persistence::connections::{Connection, Connections};
use nuo_persistence::route_settings::RouteSettingsStore;
use nuo_protocol::{
    ConnectionAuth, ConnectionFilterPolicy, Effort, NamedFilterPolicy, OpenAiResponsesDialect,
    WireProtocol,
};
use nuo_provider::catalog::Transport;
use nuo_provider::{CredentialStore, TokenSet};
use nuo_protocol::model_providers::DEEPSEEK_BUILTIN_MODELS;
use nuo_provider::route_for_model;

use std::sync::Mutex;

/// Tests that mutate process-wide env vars or the paths override must
/// serialize against each other so the parallel subagent never observes a
/// half-set environment or a foreign `Dirs`.
static ENV_GUARD: Mutex<()> = Mutex::new(());
static TEST_INIT: std::sync::Once = std::sync::Once::new();

mod catalog;
mod derivation;
mod dialect;
mod picker;
mod policy;
mod routes;
mod sync;

pub(crate) use self::fixtures::*;
pub(crate) use super::*;

mod fixtures;
