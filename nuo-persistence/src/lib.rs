//! Durable state and configuration for the muta agent stack.
//!
//! `nuo-contracts` holds the pure domain (types & traits), zero I/O. This
//! crate sits one layer above it: the durable state and configuration a
//! frontend needs to actually run a session — config loading and validation,
//! path resolution, the event-sourced session store (which carries the
//! `/schedule` cron calendar as session-scoped state), blob storage, the
//! embedding index, and usage telemetry.
//!
//! This is the **local agent** persistence layer. It assumes a single-user
//! workstation: paths resolve through XDG `ProjectDirs` (ADR-0014's `Dirs`
//! is the single point of truth) and sessions are keyed by project root;
//! cross-process writes to shared files serialise through companion
//! `.lock` flocks (ADR-0018). Other scenarios the project may grow
//! (group-chat with multi-tenancy) will not fit this layer and should spawn
//! sibling crates sharing only `nuo-contracts`. See ADR-0005.
//!
//! Frontends depend on `nuo-contracts` + `nuo-persistence` and add their own
//! presentation layer. They must never need to reach into a sibling
//! frontend's crate; this is what keeps the CLI self-contained today and
//! a GUI reachable tomorrow.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod blobs;
pub mod cache;
pub mod config;
pub mod config_check;
pub mod connection_usage;
pub mod connections;
pub mod db;
// Application filesystem infrastructure now lives in `nuo-host`; these
// re-exports keep the daemon-side crates' stable import paths.
pub use nuo_host::fsutil;
pub use nuo_host::lock;
pub use nuo_host::paths;

pub mod asset_attestation;
pub mod instances;
pub mod model_providers;
pub mod provider_usage;
pub mod role_memory;
pub mod roles;
pub mod route_settings;
pub mod session;
pub mod tools;
pub mod usage_stats;
mod web_migration;
pub mod workspace_security;

pub use asset_attestation::AssetAttestationLedger;
pub use role_memory::{RecalledMemory, RoleMemoryEntry, RoleMemoryStore, get_role_memory_store};
pub use session::{CommitTurn, SessionStore};
pub use tools::{RecallMemoryTool, create_persistence_tools, register_persistence_tools};
