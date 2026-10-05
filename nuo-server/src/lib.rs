//! The session runtime layer between the orchestration crate
//! (`nuo-harness`/`nuo-agent`) and the peer frontends (`nuo-tui` and the web
//! client).
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
//! The multi-session host and the unified session daemon have landed on top of
//! it — the "one session per process" posture is gone:
//!
//! - [`registry::SessionRegistry`] owns every live session across every
//!   project, one [`registry::HostedSession`] per assembled harness, and
//!   lazily resumes persisted sessions on attach.
//! - [`host`] is the daemon runtime; the `nuo` binary runs it via
//!   `nuo start --fg`, or a frontend starts it on demand.
//! - Clients — the `nuo-tui` TUI and the web client — talk to the daemon over
//!   the [`serve`] WebSocket control plane: owner-only native IPC by default
//!   (a Unix domain socket or Windows Named Pipe), plus TCP with a bearer
//!   token when started `--public`. The client side of that control plane
//!   lives in the dedicated SDK crate `nuo-client`, which publishes the same
//!   `nuo_client::wire` protocol the server drives ([`serve`] re-exports the
//!   envelope types), so the two cannot drift.
//! - [`serve_discovery`] publishes the global `daemon.json` record clients
//!   use to find the daemon; on graceful shutdown the daemon tears every
//!   hosted session down through the registry, firing each one's
//!   SessionEnd hooks.
//!
//! # Dependency posture
//!
//! `nuo-server` depends on `nuo-agent` (orchestration and the built-in tools),
//! `nuo-persistence` (persistence), `nuo-provider-adapters` (providers),
//! `nuo-mcp` (the MCP connector protocol; this crate owns each live
//! `McpRuntime` because it controls connection lifetime), and `nuo-wire`
//! (vocabulary — the former `nuo-contracts`, absorbed by ADR-0006).
//! Slash-command discovery and project scaffolding live here as the
//! `startup`/`project` modules. Agent-owned stateful tools are assembled
//! inside `nuo-agent`. This crate does **not** depend on the `nuo` binary
//! crate — frontends depend on this crate, never the reverse.
//!
//! # Identity posture
//!
//! This crate is application-neutral: it holds no product name, mission, or
//! preset profile. The embedding binary supplies an
//! [`nuo_wire::AgentIdentity`] to `Agent::new` / `from_toolset` and binds
//! an [`nuo_wire::AgentRoleProfile`] via `apply_preset`.
//! The `nuo` binary keeps the coding identity. The `/btw` side-session reuses
//! the primary agent's identity (`Agent::identity()`) rather than naming a product here.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agent_setup;
pub mod background_jobs;
pub mod bootstrap;
pub mod catalog;
pub mod durability_sink;
pub mod credentials_host;
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
pub mod input_completion;
pub mod kernel_host;
pub mod log_rotate;
pub mod monitor;
pub mod mcp;
pub mod offstream;
pub mod project;
pub mod registry;
pub mod search_lexical;
pub mod serve;
pub mod serve_discovery;
pub mod session_driver;
pub mod skills_host;
pub mod session_view;
pub mod shutdown;
pub mod side;
pub mod slash_handler;
pub mod startup;
pub mod task_fault_tolerance;
pub mod task_ledger;
pub mod archivist;
pub mod archivist_service;
pub mod hypervisor;
pub mod ui_bridge;
pub mod wire_channel;

pub use archivist::{archivist_address, build_archivist};
pub use background_jobs::{BackgroundJobEvent, BackgroundJobManager, SessionJobService};
pub use hypervisor::Hypervisor;
pub use session_driver::SessionDriver;
pub use credentials_host::{host as credential_host, store as credential_store};
pub use durability_sink::StoreSink;
pub use kernel_host::kernel_host;
pub use skills_host::DurableWorkspaceTrust;
pub use ui_bridge::{CopyOutcome, UiBridge};
pub use mcp::{McpCatalog, McpRuntime};

/// Authoritative runtime configuration shared across all hosted sessions (ADR-0209).
pub type SharedConfig = std::sync::Arc<tokio::sync::RwLock<nuo_persistence::config::Config>>;
/// Authoritative model recency telemetry shared across all hosted sessions (ADR-0209).
pub type SharedConnectionUsage =
    std::sync::Arc<tokio::sync::RwLock<nuo_persistence::connection_usage::ConnectionUsage>>;

// NOTE: identity (`agent_code`/`DaemonUiBridge`) lives in the application
// layer (the `nuo` binary's own `identity` module), not here, so this crate
// stays application-neutral. The `/btw` side session reuses the primary
// agent's identity via `Agent::identity()`.
