//! Nuo's application core: the session runtime, the provider/tool composition
//! root, the control-plane transport, and the product policy that binds them.
//!
//! # Role
//!
//! `nuo-server` is not a neutral socket service. It owns the live session
//! registry, the per-session driver, the provider and tool composition root,
//! the MCP runtime, the control-plane transport, and the product decisions that
//! used to be denied here (`.nuo/` scaffolding, the built-in slash vocabulary,
//! export branding). [ADR-0184](../../docs/adr/0184-server-is-the-application-core.md)
//! makes that role explicit.
//!
//! The **session runtime** remains frontend-neutral: `runtime/` knows no binary
//! name, preset, or slash-command vocabulary. Product policy lives in
//! `product/` and `composition/`.
//!
//! # Module topology
//!
//! ```text
//! composition/   bootstrap, startup, provider_registry, kernel_host, …
//! runtime/       host, registry, thread_driver, shutdown, mcp, background_jobs, …
//! handlers/      chat, history, permission, provider, slash, thread, websearch
//! transport/     serve, serve_discovery, wire_channel, health_http, ui_bridge
//! product/       project scaffolding, export
//! api.rs         the small facade the `nuo` binary consumes
//! ```
//!
//! The `nuo` binary depends on this crate; this crate never depends on the
//! binary.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub use nuo_engine;

pub mod api;
pub mod composition;
pub mod handlers;
pub mod product;
pub mod runtime;
pub mod transport;

pub use composition::bootstrap;
pub use composition::startup;
pub use runtime::archivist;
pub use runtime::background_jobs;
pub use runtime::hypervisor;
pub use runtime::mcp;
pub use runtime::registry;
pub use runtime::thread_driver;
pub use runtime::workspace_journal;
pub use transport::serve;
pub use transport::serve_discovery;
pub use transport::ui_bridge;

pub use runtime::archivist::{archivist_address, build_archivist};
pub use runtime::background_jobs::{BackgroundJobEvent, BackgroundJobManager, SessionJobService};
pub use composition::credentials_host::{host as credential_host, store as credential_store};
pub use runtime::hypervisor::Hypervisor;
pub use composition::kernel_host::kernel_host;
pub use runtime::mcp::{McpCatalog, McpRuntime};
pub use runtime::registry::{HostedThread, ThreadRegistry};
pub use composition::skills_host::DurableWorkspaceTrust;
pub use runtime::thread_driver::ThreadDriver;
pub use transport::ui_bridge::{CopyOutcome, UiBridge};

/// Authoritative runtime configuration shared across all hosted sessions.
pub type SharedConfig = std::sync::Arc<tokio::sync::RwLock<nuo_persistence::config::Config>>;
/// Authoritative model recency telemetry shared across all hosted sessions.
pub type SharedConnectionUsage =
    std::sync::Arc<tokio::sync::RwLock<nuo_persistence::connection_usage::ConnectionUsage>>;
