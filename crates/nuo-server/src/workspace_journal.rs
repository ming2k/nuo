//! The workspace effect journal's server-side wiring (ADR-0125).
//!
//! Two narrow adapters, and nothing else:
//!
//! 1. [`SessionJournalPort`] implements the harness's [`WorkspaceJournalPort`]
//!    over the session's single-writer door, so every journal row goes through the
//!    same connection as the transcript instead of a second writer.
//! 2. [`SessionBlobAdapter`] implements the harness's [`PreImageStore`] over the
//!    session's existing content-addressed store.
//!
//! Both are deliberately thin. The interesting decisions live in `nuo-harness`
//! (what to capture, when to veto, how to reverse) and `nuo-persistence` (the
//! schema, the immutability triggers, the queries). This module is only the seam
//! where those two meet a live session, so it is the one place that knows both
//! vocabularies and the one place a rename has to be reconciled.
//!
//! ## Why the port is installed per session rather than globally
//!
//! A journal row is owned by a thread, and the `workspace_effects` foreign key
//! enforces that. Installing the port on the *agent* (which is per session) rather
//! than on a process-global handle means a subagent with no session of its own has
//! no port and journals nothing — which is correct: its effects belong to its
//! parent's round, and it has no `/diff` of its own.

use std::sync::Arc;

use async_trait::async_trait;
use nuo_harness::runtime::{JournalOpToken, JournalRow, PreImageStore, WorkspaceJournalPort};
use nuo_persistence::blobs::BlobStore;
use nuo_persistence::db::PersistenceHandle;
use nuo_persistence::workspace_effects::NewOwnedEffect;

/// The harness's [`PreImageStore`] over a session's content-addressed store.
pub struct SessionBlobAdapter {
    blobs: BlobStore,
}

impl SessionBlobAdapter {
    pub fn new(blobs: BlobStore) -> Self {
        Self { blobs }
    }
}

impl PreImageStore for SessionBlobAdapter {
    fn put(&self, bytes: &[u8]) -> Result<String, String> {
        self.blobs.put(bytes)
    }

    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        self.blobs.get(hash)
    }
}

/// The harness's [`WorkspaceJournalPort`] over the session's single-writer door.
pub struct SessionJournalPort {
    writer: PersistenceHandle,
    store: SessionBlobAdapter,
}

impl SessionJournalPort {
    /// Build the port for a live session.
    ///
    /// The handle is cloned (it is the door, not a connection: every clone
    /// forwards to the same actor) so the port can outlive any one borrow.
    pub fn new(writer: PersistenceHandle, blobs: BlobStore) -> Self {
        Self {
            writer,
            store: SessionBlobAdapter::new(blobs),
        }
    }

    /// Wrap in the trait object the agent stores.
    pub fn into_shared(self) -> Arc<dyn WorkspaceJournalPort> {
        Arc::new(self)
    }
}

/// Convert the harness's row vocabulary into the storage layer's.
///
/// This is the boundary that makes a rename fail to compile instead of silently
/// writing a token the reader cannot parse. The `op` and `effect_class` strings are
/// already the durable spellings (both crates own the same constants), so only the
/// shape conversion happens here.
fn to_owned_effect(row: &JournalRow) -> NewOwnedEffect {
    NewOwnedEffect {
        thread_id: row.thread_id.clone(),
        round_id: row.round_id.clone(),
        turn_id: row.turn_id.clone(),
        call_id: row.call_id.clone(),
        seq: row.seq,
        tool_name: row.tool_name.clone(),
        path: row.path.clone(),
        op: match row.op {
            JournalOpToken::Create => nuo_persistence::workspace_effects::JournalOp::Create,
            JournalOpToken::Edit => nuo_persistence::workspace_effects::JournalOp::Edit,
            JournalOpToken::Delete => nuo_persistence::workspace_effects::JournalOp::Delete,
        },
        effect_class: row.effect_class.clone(),
        before_hash: row.before_hash.clone(),
        after_hash: None,
        irreversible_reason: row.irreversible_reason.clone(),
        created_at_ms: unix_ms(),
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[async_trait]
impl WorkspaceJournalPort for SessionJournalPort {
    fn store(&self) -> &dyn PreImageStore {
        &self.store
    }

    async fn append(&self, row: JournalRow) -> Result<i64, String> {
        // Write-ahead, unconditionally: the caller has declared that this effect has a
        // recovery point and that the mutation has not run yet. Nothing about the row's
        // fields is inspected — a withheld pre-image (`before_hash: None` because
        // capture was refused) is still pending completion, and inferring otherwise
        // stamped it complete before the tool ran.
        self.writer
            .append_workspace_effect(to_owned_effect(&row))
            .await
            .map_err(|error| error.to_string())
    }

    async fn append_receipt(&self, row: JournalRow, completed_at_ms: u64) -> Result<i64, String> {
        self.writer
            .append_completed_workspace_effect(to_owned_effect(&row), completed_at_ms)
            .await
            .map_err(|error| error.to_string())
    }

    async fn complete(
        &self,
        effect_id: i64,
        after_hash: Option<String>,
        completed_at_ms: u64,
    ) -> Result<bool, String> {
        self.writer
            .complete_workspace_effect(effect_id, after_hash, completed_at_ms)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Install the journal port on an agent and return the port for later use.
///
/// Returns the shared port so the caller (the slash-command handlers) can read the
/// journal back through the same handle it was written through.
pub fn install(
    agent: &nuo_harness::Agent,
    writer: PersistenceHandle,
    blobs: BlobStore,
) -> Arc<dyn WorkspaceJournalPort> {
    let port = SessionJournalPort::new(writer, blobs).into_shared();
    agent.set_workspace_journal(Arc::clone(&port));
    port
}

#[cfg(test)]
mod tests;
