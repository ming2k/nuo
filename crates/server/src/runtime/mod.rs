//! Live session runtime: the server host, session registry, per-session driver,
//! lifecycle, MCP/background jobs, and the runtime services they depend on.
//!
//! This subtree is frontend-neutral. It must not depend on `product/`; the
//! product chooses how a session starts, not what the runtime is.

pub mod archivist;
pub mod archivist_service;
pub mod background_jobs;
pub mod hooks;
pub mod host;
pub mod hypervisor;
pub mod input_completion;
pub mod log_rotate;
pub mod mcp;
pub mod monitor;
pub mod offstream;
pub mod registry;
pub mod search_lexical;
pub mod shutdown;
pub mod task_fault_tolerance;
pub mod task_ledger;
pub mod thread_driver;
pub mod usage_cache;
pub mod workspace_journal;
