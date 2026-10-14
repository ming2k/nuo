//! The product's durability sink: the SQLite session store behind the kernel's
//! `FactSink` port.
//!
//! The store already speaks the port's language — `save_session_delta` writes a
//! batch, `load_session_ir` reads the facts back — so this adapter is thin on
//! purpose. What it adds is the port's *contract*: an acknowledgement that means
//! something, a health reading the kernel can report, and a conformance test
//! (`the_store_is_a_conformant_sink`) that makes "this store is a valid sink" a
//! checked claim rather than an assumption.
//!
//! # Why the store implements the port rather than a trait being extracted from it
//!
//! The design rejected abstracting the session store into a kernel trait: to be
//! useful such a trait would have to declare lineage, forking, projection, and
//! title semantics — the product's state model wearing a trait. The kernel's port
//! is stated in *facts*, and this adapter is the one place the two meet. The
//! direction is application → kernel, which is the only direction allowed
//! (ADR-0300 `[INV-SUBSTRATE-02]`).

use std::sync::Arc;

use futures::future::BoxFuture;
use nuo_harness::{Ack, FactSink, SinkError, SinkHealth};
use nuo_wire::{CausalNode, SessionDelta};
use nuo_persistence::db::{PersistenceError, get_persistence_handle};

/// One instance's facts, stored in the product's SQLite engine.
pub struct StoreSink {
    session_id: String,
}

impl StoreSink {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }

    /// As a port handle.
    pub fn handle(session_id: impl Into<String>) -> Arc<dyn FactSink> {
        Arc::new(Self::new(session_id))
    }
}

impl std::fmt::Debug for StoreSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreSink")
            .field("session_id", &self.session_id)
            .finish()
    }
}

impl FactSink for StoreSink {
    fn append(&self, batch: SessionDelta) -> BoxFuture<'static, Result<Ack, SinkError>> {
        let watermark = batch.new_watermark_seq;
        let batch_session = batch.session_id.clone();
        let expected_session = self.session_id.clone();
        Box::pin(async move {
            // A batch addressed to another instance is a programming error, and
            // storing it would put one instance's facts in another's record.
            // Refusing is the only honest answer; retrying cannot fix it.
            if batch_session != expected_session {
                return Err(SinkError::permanent(format!(
                    "batch is addressed to instance '{batch_session}' but this sink stores '{expected_session}'"
                )));
            }
            // The idempotency the port requires is the store's own: `save_session_delta`
            // writes nodes keyed by `(session_id, seq)`, so a replayed batch
            // rewrites the same rows with the same values.
            match get_persistence_handle().save_session_delta(batch).await {
                Ok(()) => Ok(Ack::durable(watermark)),
                Err(error) => Err(classify(error)),
            }
        })
    }

    fn health(&self) -> SinkHealth {
        // The writer is supervised (ADR-0196): a dead writer is a real state the
        // kernel should see, not something to discover at the next append.
        match get_persistence_handle().health() {
            nuo_persistence::db::WriterHealth::Healthy => SinkHealth::Healthy,
            // Recovering: the supervisor is retrying, so appends may still land —
            // degraded, and the kernel reports that rather than a false green.
            nuo_persistence::db::WriterHealth::Recovering { .. } => SinkHealth::Degraded,
            // Down: respawn attempts are exhausted; every append will fail.
            nuo_persistence::db::WriterHealth::Down { .. } => SinkHealth::Failing,
        }
    }
}

/// Map a store failure onto the port's retry verdict.
///
/// The distinction matters: a busy or closed writer is worth retrying, a rejected
/// value is not, and telling the kernel "retry" for something that cannot succeed
/// would spend the kernel's budget on a permanent condition.
fn classify(error: PersistenceError) -> SinkError {
    match error {
        PersistenceError::WriterDown => {
            SinkError::retryable("the persistence writer is down (it respawns with backoff)")
        }
        // An engine rejection is the store's verdict on this payload: retrying
        // the same bytes will be rejected the same way.
        PersistenceError::Engine(error) => {
            SinkError::permanent(format!("the store rejected the batch: {error}"))
        }
        // A panic was contained; the actor survived, so a retry is meaningful.
        PersistenceError::Poisoned(message) => {
            SinkError::retryable(format!("the store's handler panicked: {message}"))
        }
        // Encoding failed before any write: permanent for these bytes.
        PersistenceError::Encode(message) => {
            SinkError::permanent(format!("could not encode the batch: {message}"))
        }
        // An orderly shutdown is not a transient condition the kernel should
        // grind against.
        PersistenceError::Closed => {
            SinkError::permanent("the store was shut down".to_string())
        }
        PersistenceError::StaleRevision { expected, actual } => SinkError::permanent(format!(
            "the instance revision moved (expected {expected}, found {actual})"
        )),
        PersistenceError::OperationConflict { operation_id } => SinkError::permanent(format!(
            "operation '{operation_id}' was already committed with different content"
        )),
    }
}

/// Hydrate an instance's facts from the store.
///
/// The kernel's other half of the durability contract: the host
/// reads its own store and hands the facts in. Returns the instance's causal
/// nodes in sequence order, which is what `FactSink`'s order contract promises
/// and what the conformance suite checks.
pub async fn hydrate(session_id: &str) -> Result<Vec<CausalNode>, String> {
    let reader = get_persistence_handle()
        .reader()
        .map_err(|error| format!("could not obtain a reader: {error}"))?;
    let ir = reader
        .load_session_ir(session_id)
        .map_err(|error| format!("could not load instance '{session_id}': {error}"))?
        .ok_or_else(|| format!("instance '{session_id}' has no stored facts"))?;
    let mut nodes: Vec<CausalNode> = ir.history.nodes.into_values().collect();
    nodes.sort_by_key(|node| node.seq);
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_harness::{Hydrate, run_conformance};
    use nuo_wire::{
        ExecutionStatus, Message, NodeKind, NodePayload, Role, StateUpdate, SystemNoticePayload,
    };

    /// Sandbox the process-wide XDG roots, so the store this test drives is a
    /// temporary one and the developer's real database is never touched.
    fn sandbox() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir().unwrap();
        nuo_persistence::paths::set_test_default(Some(nuo_persistence::paths::Dirs {
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: None,
        }));
        std::fs::create_dir_all(&nuo_persistence::paths::get().config_dir).unwrap();
        (root, guard)
    }

    /// The store is a conformant sink.
    ///
    /// This is the claim the port's whole design rests on: a store used with the
    /// kernel must round-trip the kernel's facts losslessly, in order, with
    /// honest acknowledgements. If this test ever fails, the kernel's durability
    /// claims are unsound and the failure names the fact.
    #[tokio::test]
    async fn the_store_is_a_conformant_sink() {
        let (_root, _guard) = sandbox();
        let session_id = "conformance-store";

        let sink = StoreSink::new(session_id);
        let expected_session = session_id.to_string();
        let hydrate_fn: Hydrate = Box::new(move |_expected: Vec<CausalNode>| {
            let session_id = expected_session.clone();
            Box::pin(async move { hydrate(&session_id).await })
        });

        // The store upserts its own session row from the batch it receives, so
        // no seeding step is needed — which is itself worth knowing: a sink that
        // required pre-creation would not satisfy the port's append contract.
        let report = run_conformance(session_id, &sink, hydrate_fn).await;
        nuo_persistence::paths::set_test_default(None);
        report.assert_all_passed();
    }

    /// A batch addressed to another instance is refused as permanent, because no
    /// retry can make it correct.
    #[tokio::test]
    async fn a_batch_for_another_instance_is_refused_permanently() {
        let sink = StoreSink::new("instance-a");
        let mut batch = empty_batch("instance-b", 1);
        batch.new_nodes.push(node("n1", 1, "hi"));
        let error = sink
            .append(batch)
            .await
            .expect_err("a misaddressed batch must be refused");
        assert!(
            !error.retryable,
            "retrying a misaddressed batch cannot help: {error}"
        );
        assert!(error.message.contains("instance-b"), "{error}");
    }

    fn empty_batch(session_id: &str, watermark: u64) -> SessionDelta {
        SessionDelta {
            session_id: session_id.to_string(),
            parent_session_id: None,
            previous_watermark_seq: watermark.saturating_sub(1),
            new_watermark_seq: watermark,
            new_nodes: Vec::new(),
            state_update: StateUpdate {
                active_leaf: None,
                status: ExecutionStatus::Idle,
                round_counter: 0,
                pending_notifications: Vec::<SystemNoticePayload>::new(),
            },
            policy_update: None,
            updated_at_s: 1,
        }
    }

    fn node(id: &str, seq: u64, text: &str) -> CausalNode {
        CausalNode {
            id: id.to_string(),
            parent_id: None,
            seq,
            timestamp_ms: 1_000 * seq,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, text),
            },
        }
    }
}
