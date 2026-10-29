//! Runtime substrate — host ports, execution environment, durability sinks,
//! hooks, inflight deduplication, offstream memory, the provider sentinel,
//! workspace journal capture, and harness extensions.
pub mod durability;
pub mod execution;
pub mod extension;
pub(crate) mod hook_runner;
pub mod hooks;
pub mod host;
pub mod inflight;
pub(crate) mod message;
pub mod no_provider;
pub mod offstream;
pub mod retry;
pub(crate) mod sync;
pub(crate) mod time;
pub mod workspace_journal;

pub use workspace_journal::middleware::{
    CallCoordinates, PendingWrite, begin as journal_begin, finish as journal_finish,
};
pub use workspace_journal::{
    CaptureOutcome, CapturedOp, CapturedPath, JournalOpToken, JournalRow, PreImageStore,
    WorkspaceJournalPort,
};
