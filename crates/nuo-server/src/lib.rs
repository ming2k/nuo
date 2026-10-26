//! The session runtime layer between the runtime crate (`nuo-harness`) and
//! the peer frontends (`nuo-tui` and the web client).
//!
//! # Why this crate exists
//!
//! Historically `nuo` was a single process: one TUI driving one agent
//! background task over a pair of `mpsc` channels. When the TUI process
//! exited, the agent task died with it. That model cannot serve a browser
//! frontend, which needs a long-running host holding multiple concurrent
//! sessions that several clients can subscribe to.
//!
//! This crate owns the per-session state that makes that possible — the
//! session driver, its handlers, and the `/serve` WebSocket bridge that
//! translates the wire protocol (`AgentRequest`/`AgentResponse`, both
//! `Serialize`/`Deserialize`) to and from the in-process channels.
//!
//! # Architecture today
//!
//! The assembly factory has landed as [`bootstrap::assemble`]: it builds one
//! frontend-neutral session harness ([`session_driver::SessionDriver`] plus
//! its channels) per call, and the application binary (`nuo`) goes through it.
//! The multi-session host and the unified session server have landed on top of
//! it — the "one session per process" posture is gone:
//!
//! - [`registry::SessionRegistry`] owns every live session across every
//!   project, one [`registry::HostedSession`] per assembled harness, and
//!   lazily resumes persisted sessions on attach.
//! - [`host`] is the server runtime; the `nuo` binary runs it via
//!   `nuo start --fg`, or a frontend starts it on demand.
//! - Clients — the `nuo-tui` TUI and headless CLI — talk to the server over
//!   the [`serve`] WebSocket/UDS control plane: owner-only native IPC by default
//!   (a Unix domain socket or Windows Named Pipe), plus TCP with a bearer
//!   token when started `--public`. The wire protocol contracts live in
//!   `nuo-protocol` (`nuo-wire`), which publishes the typed wire protocol
//!   the server drives ([`serve`] re-exports the envelope types).
//! - [`serve_discovery`] publishes the global `server.json` record clients
//!   use to find the server; on graceful shutdown the server tears every
//!   hosted session down through the registry, firing each one's
//!   SessionEnd hooks.
//!
//! # Dependency posture
//!
//! `nuo-server` depends on `nuo-harness` (the cognitive runtime and the built-in tools),
//! `nuo-persistence` (persistence), the `providers/nuo-provider-*` crates (providers),
//! `nuo-mcp` (the MCP connector protocol; this crate owns each live
//! `McpRuntime` because it controls connection lifetime), and `nuo-wire`
//! (vocabulary — the former `nuo-contracts`, absorbed by ADR-0006).
//! Slash-command discovery and project scaffolding live here as the
//! `startup`/`project` modules. Agent-owned stateful tools are assembled
//! inside `nuo-harness`. This crate does **not** depend on the `nuo` binary
//! crate — frontends depend on this crate, never the reverse.
//!
//! # Identity posture
//!
//! This crate is application-neutral: it holds no product name, mission, or
//! preset profile. The embedding binary supplies an
//! [`nuo_wire::AgentIdentity`] to `Agent::new` / `from_toolset` and binds
//! an [`nuo_wire::AgentRoleProfile`] via `apply_preset`.
//! The `nuo` binary keeps the coding identity; a forked child reuses
//! the primary agent's identity (`Agent::identity()`) rather than naming a product here.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod agent_setup;
pub mod archivist;
pub mod archivist_service;
pub mod background_jobs;
pub mod bootstrap;
pub mod credentials_host;
pub mod durability_sink;
pub mod export;
pub mod handlers_chat;
pub mod handlers_history;
pub mod handlers_permission;
pub mod handlers_provider;
pub mod handlers_session;
pub mod handlers_slash;
pub mod handlers_websearch;
pub mod health_http;
pub mod hooks;
pub mod host;
pub mod hypervisor;
pub mod input_completion;
pub mod kernel_host;
pub mod log_rotate;
pub mod mcp;
pub mod monitor;
pub mod offstream;
pub mod project;
#[cfg(test)]
mod provider_baseline_fidelity_tests;
pub mod provider_baselines;
pub mod provider_registry;
pub mod registry;
pub mod search_lexical;
pub mod serve;
pub mod serve_discovery;
pub mod session_driver;
pub mod session_view;
pub mod shutdown;
pub mod skills_host;
pub mod slash_handler;
pub mod startup;
pub mod task_fault_tolerance;
pub mod task_ledger;
pub mod ui_bridge;
pub mod usage_cache;
pub mod wire_channel;

pub use archivist::{archivist_address, build_archivist};
pub use background_jobs::{BackgroundJobEvent, BackgroundJobManager, SessionJobService};
pub use credentials_host::{host as credential_host, store as credential_store};
pub use durability_sink::StoreSink;
pub use hypervisor::Hypervisor;
pub use kernel_host::kernel_host;
pub use mcp::{McpCatalog, McpRuntime};
pub use session_driver::SessionDriver;
pub use skills_host::DurableWorkspaceTrust;
pub use ui_bridge::{CopyOutcome, UiBridge};

/// Authoritative runtime configuration shared across all hosted sessions.
pub type SharedConfig = std::sync::Arc<tokio::sync::RwLock<nuo_persistence::config::Config>>;
/// Authoritative model recency telemetry shared across all hosted sessions.
pub type SharedConnectionUsage =
    std::sync::Arc<tokio::sync::RwLock<nuo_persistence::connection_usage::ConnectionUsage>>;

// NOTE: identity (`agent_code`/`ServerUiBridge`) lives in the application
// layer (the `nuo` binary's own `identity` module), not here, so this crate
// stays application-neutral. A forked child reuses the primary agent's
// identity via `Agent::identity()`.
