//! Database schema catalog, migration execution, ledger, and verification.
//!
//! The DB is a single embedded SQLite file with one writer. Its durable schema
//! is an immutable baseline plus numbered migrations (ADR-0189). A database
//! either matches a version this binary knows exactly, or it is refused; there
//! is no legacy replay and no silent repair.

mod apply;
pub mod catalog;
mod history;
pub mod runner;
pub mod types;
mod verify;

pub use catalog::{CURRENT_DB_VERSION, MIGRATIONS, Migration};
#[cfg(test)]
pub(crate) use runner::migrate_to_for_tests;
pub use runner::*;
pub use types::{
    CAS_THRESHOLD_BYTES, HistorySearchResult, MAX_RETAINED_REQUEST_PROJECTIONS, ThreadRecord,
    ThreadTranscriptView,
};

#[cfg(test)]
mod tests;
