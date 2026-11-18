//! SQLite adapter for the durable `ThreadStore` port (RFC-0003 / ADR-0173).
//!
//! The adapter maps `Thread` / `ThreadFact` / `BranchState` onto the
//! `threads`, `thread_branches`, `causal_nodes`, and `thread_policies` tables.
//! It performs no semantic folding or request projection: those belong to
//! `ThreadRuntime` and `RequestView`.
//!
//! `rusqlite::Connection` is synchronous, so every operation runs on the
//! blocking pool. The adapter is intentionally a single-connection wrapper;
//! production wiring hands it the persistence writer handle rather than opening
//! a second connection.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use protocol::{
    Ack, BlobRef, BranchId, BranchPolicy, BranchState, CommandAck, CommandEnvelope, FactContext,
    FactId, FactKind, FactPayload, GcReport, THREAD_SCHEMA_VERSION, Thread, ThreadError,
    ThreadFact, ThreadGraph, ThreadId, ThreadPolicy, ThreadRevision, ThreadState, ThreadStatus,
    ThreadStore, ThreadStoreError, Trust,
};
use rusqlite::{Connection, OptionalExtension, params};

/// SQLite-backed `ThreadStore`.
pub struct SqliteThreadStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteThreadStore {
    /// Wrap an already-migrated connection.
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    /// Share an existing writer connection. Production wiring uses this so the
    /// thread store never opens a second connection beside the single-writer
    /// door; tests use [`Self::new`].
    pub fn from_shared(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Open a database file and run the standard migrations.
    pub fn open(path: impl AsRef<std::path::Path>) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        let mut conn = Connection::open(path)?;
        crate::db::migrations::initialize_connection_schema(&mut conn, Some(path))?;
        Ok(Self::new(conn))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn status_to_sql(status: ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Active => "running",
        ThreadStatus::Idle => "idle",
        ThreadStatus::Archived => "archived",
        ThreadStatus::Deleted => "terminated",
    }
}

fn status_from_sql(status: &str) -> ThreadStatus {
    match status {
        "idle" => ThreadStatus::Idle,
        "archived" => ThreadStatus::Archived,
        "terminated" => ThreadStatus::Deleted,
        _ => ThreadStatus::Active,
    }
}

fn kind_to_sql(kind: FactKind) -> &'static str {
    match kind {
        FactKind::Message => "dialogue",
        FactKind::Observation => "observation",
        FactKind::Compaction => "compaction",
        FactKind::Termination => "termination",
        FactKind::SystemNotice => "system_notice",
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, ThreadStoreError> {
    serde_json::to_string(value).map_err(|error| ThreadStoreError::Backend(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(value: &str, what: &str) -> Result<T, ThreadStoreError> {
    serde_json::from_str(value)
        .map_err(|error| ThreadStoreError::Backend(format!("invalid {what}: {error}")))
}

fn insert_fact(
    conn: &Connection,
    thread_id: &ThreadId,
    fact: &ThreadFact,
) -> Result<bool, ThreadStoreError> {
    let payload = encode(&fact.payload)?;
    let context = encode(&fact.context)?;
    let inserted = conn
        .prepare_cached(
            r#"
            INSERT OR IGNORE INTO causal_nodes (
                id, thread_id, parent_id, seq, kind, payload_json, timestamp_ms, context_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![
            fact.id.0,
            thread_id.0,
            fact.parent_id.as_ref().map(|id| id.0.as_str()),
            fact.seq as i64,
            kind_to_sql(fact.kind),
            payload,
            fact.timestamp_ms as i64,
            context,
        ])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    if inserted == 0 {
        let existing: Option<(String, String)> = conn
            .prepare_cached(
                "SELECT payload_json, context_json FROM causal_nodes WHERE thread_id = ?1 AND id = ?2",
            )
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .query_row(params![thread_id.0, fact.id.0], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let Some((existing_payload, existing_context)) = existing else {
            return Err(ThreadStoreError::Backend(
                "fact insert was ignored".to_string(),
            ));
        };
        if existing_payload != payload || existing_context != context {
            return Err(ThreadStoreError::Invalid(ThreadError::DuplicateFact(
                fact.id.clone(),
            )));
        }
        return Ok(false);
    }
    Ok(true)
}

fn upsert_branch(
    conn: &Connection,
    thread_id: &ThreadId,
    branch: &BranchState,
) -> Result<(), ThreadStoreError> {
    conn.prepare_cached(
        r#"
        INSERT INTO thread_branches (
            thread_id, branch_id, name, head_fact_id, forked_from_fact_id,
            compaction_horizon_fact_id, policy_json, created_at_ms, updated_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(thread_id, branch_id) DO UPDATE SET
            name = excluded.name,
            head_fact_id = excluded.head_fact_id,
            forked_from_fact_id = excluded.forked_from_fact_id,
            compaction_horizon_fact_id = excluded.compaction_horizon_fact_id,
            policy_json = excluded.policy_json,
            updated_at_ms = excluded.updated_at_ms
        "#,
    )
    .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    .execute(params![
        thread_id.0,
        branch.id.0,
        branch.name,
        branch.head.as_ref().map(|id| id.0.as_str()),
        branch.forked_from.as_ref().map(|id| id.0.as_str()),
        branch.compaction_horizon.as_ref().map(|id| id.0.as_str()),
        encode(&branch.policy)?,
        branch.created_at_ms as i64,
        branch.updated_at_ms as i64,
    ])
    .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    Ok(())
}

fn load_branches(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<BTreeMap<BranchId, BranchState>, ThreadStoreError> {
    let mut stmt = conn
        .prepare_cached(
            r#"
            SELECT branch_id, name, head_fact_id, forked_from_fact_id,
                   compaction_horizon_fact_id, policy_json, created_at_ms, updated_at_ms
              FROM thread_branches
             WHERE thread_id = ?1
            "#,
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    let rows = stmt
        .query_map(params![thread_id.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    let mut branches = BTreeMap::new();
    for row in rows {
        let (id, name, head, forked_from, horizon, policy, created, updated) =
            row.map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let branch = BranchState {
            id: BranchId(id),
            name,
            head: head.map(FactId::from_raw),
            forked_from: forked_from.map(FactId::from_raw),
            compaction_horizon: horizon.map(FactId::from_raw),
            policy: decode(&policy, "branch policy")?,
            created_at_ms: created as u64,
            updated_at_ms: updated as u64,
        };
        branches.insert(branch.id.clone(), branch);
    }
    Ok(branches)
}

fn load_facts(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<Vec<ThreadFact>, ThreadStoreError> {
    let mut stmt = conn
        .prepare_cached(
            r#"
            SELECT id, parent_id, seq, payload_json, timestamp_ms, context_json
              FROM causal_nodes
             WHERE thread_id = ?1
             ORDER BY seq ASC
            "#,
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    let rows = stmt
        .query_map(params![thread_id.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    let mut facts = Vec::new();
    for row in rows {
        let (id, parent, seq, payload, timestamp, context) =
            row.map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let payload: FactPayload = decode(&payload, "fact payload")?;
        let context: FactContext = context
            .as_deref()
            .map(|value| decode(value, "fact context"))
            .transpose()?
            .unwrap_or_default();
        facts.push(ThreadFact::new(
            FactId(id),
            parent.map(FactId::from_raw),
            seq as u64,
            timestamp as u64,
            payload,
            context,
        ));
    }
    Ok(facts)
}

impl SqliteThreadStore {
    fn create_blocking(&self, thread: Thread) -> Result<(), ThreadStoreError> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let id = &thread.id.0;
        let exists: Option<String> = tx
            .prepare_cached("SELECT id FROM threads WHERE id = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .query_row(params![id], |row| row.get(0))
            .optional()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        if exists.is_some() {
            return Err(ThreadStoreError::AlreadyExists(thread.id));
        }
        tx.prepare_cached(
            r#"
            INSERT INTO threads (
                id, status, parent_thread_id, spawned_from_fact_id, active_branch_id,
                active_leaf, round_counter, created_at_ms, updated_at_ms, revision
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![
            id,
            status_to_sql(thread.state.status),
            thread.parent_thread_id.as_ref().map(|id| id.0.as_str()),
            thread.spawned_from_fact_id.as_ref().map(|id| id.0.as_str()),
            thread.state.active_branch.0,
            thread
                .state
                .active_branch()
                .ok()
                .and_then(|branch| branch.head.as_ref())
                .map(|id| id.0.as_str()),
            thread.state.round_counter as i64,
            thread.created_at_ms as i64,
            thread.updated_at_ms as i64,
            thread.revision.0 as i64,
        ])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        tx.prepare_cached(
            "INSERT INTO thread_policies (thread_id, workspace_root, thread_policy_json, updated_at_ms) VALUES (?1, '', ?2, ?3)",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![id, encode(&thread.policy)?, thread.updated_at_ms as i64])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        for branch in thread.state.branches.values() {
            upsert_branch(&tx, &thread.id, branch)?;
        }
        for fact in thread.all_facts() {
            insert_fact(&tx, &thread.id, fact)?;
        }
        tx.commit()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        Ok(())
    }

    fn hydrate_blocking(&self, id: &ThreadId) -> Result<Option<Thread>, ThreadStoreError> {
        let conn = self.lock();
        load_thread(&conn, id)
    }
}

type ThreadRow = (
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
);

pub(crate) fn load_thread(
    conn: &Connection,
    id: &ThreadId,
) -> Result<Option<Thread>, ThreadStoreError> {
    let row: Option<ThreadRow> = conn
        .prepare_cached(
            r#"
                SELECT parent_thread_id, spawned_from_fact_id, status, active_branch_id,
                       active_leaf, round_counter, created_at_ms, updated_at_ms, revision
                  FROM threads WHERE id = ?1
                "#,
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .query_row(params![id.0], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
            ))
        })
        .optional()
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    let Some((
        parent,
        spawned_from,
        status,
        active_branch,
        active_leaf,
        round_counter,
        created,
        updated,
        revision,
    )) = row
    else {
        return Ok(None);
    };
    let mut branches = load_branches(conn, id)?;
    if branches.is_empty() {
        let main = BranchState {
            id: BranchId(active_branch.clone()),
            name: active_branch.clone(),
            head: active_leaf.map(FactId::from_raw),
            forked_from: None,
            compaction_horizon: None,
            policy: BranchPolicy::default(),
            created_at_ms: created as u64,
            updated_at_ms: updated as u64,
        };
        branches.insert(main.id.clone(), main);
    }
    let active_branch_id = BranchId(active_branch);
    if !branches.contains_key(&active_branch_id)
        && let Some(first) = branches.keys().next().cloned()
    {
        // The row's active branch was removed; the newest surviving
        // branch becomes the active one.
        branches.get_mut(&first);
    }
    let policy: ThreadPolicy = conn
        .prepare_cached("SELECT thread_policy_json FROM thread_policies WHERE thread_id = ?1")
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .query_row(params![id.0], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .map(|value| decode(&value, "thread policy"))
        .transpose()?
        .unwrap_or_default();
    let mut graph = ThreadGraph::new();
    for fact in load_facts(conn, id)? {
        // Legacy rows may have lost a parent; keep the fact and detach it
        // rather than failing the whole hydration.
        let mut fact = fact;
        if let Some(parent) = &fact.parent_id
            && !graph.contains(parent)
        {
            fact.parent_id = None;
        }
        graph.insert(fact).map_err(ThreadStoreError::Invalid)?;
    }
    Ok(Some(Thread {
        id: id.clone(),
        parent_thread_id: parent.map(ThreadId),
        created_at_ms: created as u64,
        updated_at_ms: updated as u64,
        graph,
        state: ThreadState {
            active_branch: active_branch_id,
            branches,
            round_counter: round_counter as u64,
            status: status_from_sql(&status),
        },
        policy,
        revision: ThreadRevision(revision as u64),
        schema_version: THREAD_SCHEMA_VERSION,
        spawned_from_fact_id: spawned_from.map(FactId::from_raw),
    }))
}

impl SqliteThreadStore {
    fn append_blocking(
        &self,
        delta: protocol::ThreadDelta,
        expected_revision: ThreadRevision,
    ) -> Result<Ack, ThreadStoreError> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let ack = append_in_tx(&tx, &delta, expected_revision)?;
        tx.commit()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        Ok(ack)
    }

    fn execute_blocking(
        &self,
        thread_id: &ThreadId,
        command: CommandEnvelope,
    ) -> Result<CommandAck, ThreadStoreError> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;

        let receipt: Option<String> = tx
            .prepare_cached(
                "SELECT ack_json FROM thread_commands WHERE thread_id = ?1 AND command_id = ?2",
            )
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .query_row(params![thread_id.0, command.meta.command_id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        if let Some(receipt) = receipt {
            let mut ack: CommandAck = decode(&receipt, "command receipt")?;
            ack.replayed = true;
            return Ok(ack);
        }

        check_lease(&tx, thread_id, command.meta.fencing_token)?;
        let mut thread = load_thread(&tx, thread_id)?
            .ok_or_else(|| ThreadStoreError::NotFound(thread_id.clone()))?;
        if thread.revision != command.meta.expected_revision {
            return Err(ThreadStoreError::RevisionConflict {
                expected: command.meta.expected_revision,
                actual: thread.revision,
            });
        }
        let old_seq = thread.max_seq();
        let outcome = thread.apply_command(command.command, crate::db::unix_ms())?;
        let delta = thread.drain_delta(old_seq);
        if !delta.new_facts.is_empty() || delta.revision > command.meta.expected_revision {
            append_in_tx(&tx, &delta, command.meta.expected_revision)?;
        }
        let ack = CommandAck {
            command_id: command.meta.command_id.clone(),
            revision: thread.revision,
            outcome,
            replayed: false,
        };
        tx.prepare_cached(
            "INSERT INTO thread_commands (thread_id, command_id, ack_json, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![
            thread_id.0,
            ack.command_id,
            encode(&ack)?,
            crate::db::unix_ms() as i64
        ])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        tx.commit()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        Ok(ack)
    }

    fn acquire_lease_blocking(
        &self,
        thread: &ThreadId,
        owner: &str,
        ttl: std::time::Duration,
    ) -> Result<protocol::WriterLease, ThreadStoreError> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let exists: Option<String> = tx
            .prepare_cached("SELECT id FROM threads WHERE id = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .query_row(params![thread.0], |row| row.get(0))
            .optional()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        if exists.is_none() {
            return Err(ThreadStoreError::NotFound(thread.clone()));
        }
        let now = crate::db::unix_ms();
        let current: Option<(String, i64, i64)> = tx
            .prepare_cached(
                "SELECT owner_id, fencing_token, expires_at_ms FROM thread_leases WHERE thread_id = ?1",
            )
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .query_row(params![thread.0], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        if let Some((current_owner, _, expires)) = &current
            && *expires > now as i64
            && current_owner != owner
        {
            return Err(ThreadStoreError::LeaseHeld {
                owner: current_owner.clone(),
                expires_at_ms: *expires as u64,
            });
        }
        let token: i64 = tx
            .prepare_cached("SELECT last_token FROM thread_lease_counters WHERE thread_id = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .query_row(params![thread.0], |row| row.get(0))
            .optional()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .unwrap_or(0)
            + 1;
        tx.prepare_cached(
            "INSERT INTO thread_lease_counters (thread_id, last_token) VALUES (?1, ?2)
             ON CONFLICT(thread_id) DO UPDATE SET last_token = excluded.last_token",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![thread.0, token])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let lease = protocol::WriterLease {
            thread_id: thread.clone(),
            owner_id: owner.to_string(),
            fencing_token: token as u64,
            expires_at_ms: now.saturating_add(ttl.as_millis() as u64),
        };
        tx.prepare_cached(
            "INSERT INTO thread_leases (thread_id, owner_id, fencing_token, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(thread_id) DO UPDATE SET
                 owner_id = excluded.owner_id,
                 fencing_token = excluded.fencing_token,
                 expires_at_ms = excluded.expires_at_ms",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![thread.0, owner, token, lease.expires_at_ms as i64])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        tx.commit()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        Ok(lease)
    }

    fn release_lease_blocking(
        &self,
        lease: &protocol::WriterLease,
    ) -> Result<(), ThreadStoreError> {
        let conn = self.lock();
        let current: Option<i64> = conn
            .prepare_cached("SELECT fencing_token FROM thread_leases WHERE thread_id = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .query_row(params![lease.thread_id.0], |row| row.get(0))
            .optional()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        match current {
            Some(token) if token as u64 == lease.fencing_token => {
                conn.prepare_cached("DELETE FROM thread_leases WHERE thread_id = ?1")
                    .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
                    .execute(params![lease.thread_id.0])
                    .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
                Ok(())
            }
            Some(_) => Err(ThreadStoreError::WriterLeaseLost),
            None => Ok(()),
        }
    }

    fn gc_blocking(
        &self,
        thread_id: &ThreadId,
        grace: std::time::Duration,
    ) -> Result<GcReport, ThreadStoreError> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let thread = load_thread(&tx, thread_id)?
            .ok_or_else(|| ThreadStoreError::NotFound(thread_id.clone()))?;
        let cutoff = crate::db::unix_ms().saturating_sub(grace.as_millis() as u64);
        let eligible: Vec<FactId> = thread
            .gc_eligible_facts()
            .into_iter()
            .filter(|id| {
                thread
                    .graph
                    .get(id)
                    .is_some_and(|fact| fact.timestamp_ms <= cutoff)
            })
            .collect();
        let mut facts_removed = 0;
        for id in eligible {
            tx.prepare_cached("DELETE FROM causal_nodes WHERE thread_id = ?1 AND id = ?2")
                .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
                .execute(params![thread_id.0, id.0])
                .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
            tx.prepare_cached("DELETE FROM thread_blob_refs WHERE thread_id = ?1 AND fact_id = ?2")
                .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
                .execute(params![thread_id.0, id.0])
                .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
            facts_removed += 1;
        }
        // Blob liveness is global: a hash shared by another thread must not
        // be swept because this thread stopped referencing it.
        let blobs_removed = tx
            .prepare_cached(
                "DELETE FROM thread_blobs WHERE hash NOT IN (SELECT hash FROM thread_blob_refs)",
            )
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute([])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        tx.commit()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        Ok(GcReport {
            facts_removed,
            blobs_removed,
        })
    }
}

fn append_in_tx(
    tx: &rusqlite::Transaction<'_>,
    delta: &protocol::ThreadDelta,
    expected_revision: ThreadRevision,
) -> Result<Ack, ThreadStoreError> {
    let current: Option<(i64, i64)> = tx
        .prepare_cached(
            "SELECT revision, COALESCE((SELECT MAX(seq) FROM causal_nodes WHERE thread_id = ?1), 0) FROM threads WHERE id = ?1",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .query_row(params![delta.thread_id.0], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    let Some((current_revision, current_seq)) = current else {
        return Err(ThreadStoreError::NotFound(delta.thread_id.clone()));
    };
    // Replay is checked before the expected revision: a transport retry
    // carries the original revision and must be acknowledged, not treated as
    // a stale command.
    if delta.revision <= ThreadRevision(current_revision as u64) {
        for fact in &delta.new_facts {
            let durable: Option<(String, String, i64)> = tx
                .prepare_cached(
                    "SELECT payload_json, COALESCE(context_json, '{}'), seq FROM causal_nodes
                      WHERE thread_id = ?1 AND id = ?2",
                )
                .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
                .query_row(params![delta.thread_id.0, fact.id.0], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .optional()
                .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
            let Some((payload, context, seq)) = durable else {
                return Err(ThreadStoreError::ReplayConflict(delta.thread_id.clone()));
            };
            if payload != encode(&fact.payload)?
                || context != encode(&fact.context)?
                || seq as u64 != fact.seq
            {
                return Err(ThreadStoreError::ReplayConflict(delta.thread_id.clone()));
            }
        }
        return Ok(Ack {
            durable_upto: current_seq as u64,
            revision: ThreadRevision(current_revision as u64),
            replayed: true,
        });
    }
    if ThreadRevision(current_revision as u64) != expected_revision {
        return Err(ThreadStoreError::RevisionConflict {
            expected: expected_revision,
            actual: ThreadRevision(current_revision as u64),
        });
    }
    if delta.previous_watermark_seq != current_seq as u64 {
        return Err(ThreadStoreError::WatermarkConflict {
            expected: current_seq as u64,
            actual: delta.previous_watermark_seq,
        });
    }
    let active_branch = delta
        .state_update
        .active_branch
        .clone()
        .unwrap_or_else(BranchId::main);
    let active_head = delta
        .state_update
        .branches
        .iter()
        .find(|branch| branch.id == active_branch)
        .and_then(|branch| branch.head.as_ref())
        .map(|id| id.0.clone());
    tx.prepare_cached(
        r#"
        UPDATE threads
           SET active_branch_id = ?2,
               active_leaf = ?3,
               status = COALESCE(?4, status),
               round_counter = COALESCE(?5, round_counter),
               revision = ?6,
               updated_at_ms = ?7
         WHERE id = ?1
        "#,
    )
    .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    .execute(params![
        delta.thread_id.0,
        active_branch.0,
        active_head,
        delta.state_update.status.map(status_to_sql),
        delta.state_update.round_counter.map(|value| value as i64),
        delta.revision.0 as i64,
        delta.updated_at_ms as i64,
    ])
    .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    for branch in &delta.state_update.branches {
        upsert_branch(tx, &delta.thread_id, branch)?;
    }
    for branch in &delta.state_update.removed_branches {
        tx.prepare_cached("DELETE FROM thread_branches WHERE thread_id = ?1 AND branch_id = ?2")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute(params![delta.thread_id.0, branch.0])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    }
    for fact in &delta.new_facts {
        insert_fact(tx, &delta.thread_id, fact)?;
        if let Some(reference) = fact.payload.blob_ref() {
            tx.prepare_cached(
                "INSERT INTO thread_blob_refs (thread_id, fact_id, hash, size, mime, preview, trust)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(thread_id, fact_id) DO UPDATE SET
                     hash = excluded.hash, size = excluded.size, mime = excluded.mime,
                     preview = excluded.preview, trust = excluded.trust",
            )
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute(params![
                delta.thread_id.0,
                fact.id.0,
                reference.hash,
                reference.size as i64,
                reference.mime,
                reference.preview,
                trust_to_sql(reference.trust),
            ])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        }
    }
    if let Some(policy) = &delta.policy_update {
        tx.prepare_cached(
            "UPDATE thread_policies SET thread_policy_json = ?2, updated_at_ms = ?3 WHERE thread_id = ?1",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![
            delta.thread_id.0,
            encode(policy)?,
            delta.updated_at_ms as i64
        ])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    }
    Ok(Ack {
        durable_upto: delta.new_watermark_seq,
        revision: delta.revision,
        replayed: false,
    })
}

fn role_to_sql(role: tool::Role) -> &'static str {
    match role {
        tool::Role::User => "user",
        tool::Role::Assistant => "assistant",
        tool::Role::System => "system",
        tool::Role::Tool => "tool",
    }
}

fn trust_to_sql(trust: Trust) -> &'static str {
    match trust {
        Trust::Trusted => "trusted",
        Trust::Untrusted => "untrusted",
    }
}

fn check_lease(conn: &Connection, thread: &ThreadId, token: u64) -> Result<(), ThreadStoreError> {
    let lease: Option<(i64, i64)> = conn
        .prepare_cached(
            "SELECT fencing_token, expires_at_ms FROM thread_leases WHERE thread_id = ?1",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .query_row(params![thread.0], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    match lease {
        Some((current, expires))
            if expires > crate::db::unix_ms() as i64 && current as u64 != token =>
        {
            Err(ThreadStoreError::WriterLeaseLost)
        }
        _ => Ok(()),
    }
}

/// SQLite blob store sharing the thread store's single connection.
pub struct SqliteBlobStore {
    conn: Arc<Mutex<Connection>>,
}

#[async_trait]
impl protocol::BlobStore for SqliteBlobStore {
    async fn put(
        &self,
        bytes: &[u8],
        mime: &str,
        trust: Trust,
    ) -> Result<BlobRef, ThreadStoreError> {
        let hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            hex::encode(hasher.finalize())
        };
        let preview = if bytes.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).into_owned())
        };
        let reference = BlobRef {
            hash: hash.clone(),
            size: bytes.len() as u64,
            mime: mime.to_string(),
            preview,
            trust,
        };
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        conn.prepare_cached(
            "INSERT INTO thread_blobs (hash, size, mime, data, preview, trust, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(hash) DO NOTHING",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![
            reference.hash,
            reference.size as i64,
            reference.mime,
            bytes,
            reference.preview,
            trust_to_sql(reference.trust),
            crate::db::unix_ms() as i64,
        ])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        Ok(reference)
    }

    async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>, ThreadStoreError> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        conn.prepare_cached("SELECT data FROM thread_blobs WHERE hash = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .query_row(params![hash], |row| row.get::<_, Vec<u8>>(0))
            .optional()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))
    }

    async fn delete(&self, hash: &str) -> Result<bool, ThreadStoreError> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let removed = conn
            .prepare_cached("DELETE FROM thread_blobs WHERE hash = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute(params![hash])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        Ok(removed > 0)
    }

    async fn retain_only(
        &self,
        hashes: &std::collections::BTreeSet<String>,
    ) -> Result<usize, ThreadStoreError> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let tx = conn
            .transaction()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let mut stmt = tx
            .prepare_cached("SELECT hash FROM thread_blobs")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        let existing: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .filter_map(Result::ok)
            .collect();
        drop(stmt);
        let mut removed = 0;
        {
            let mut del_stmt = tx
                .prepare_cached("DELETE FROM thread_blobs WHERE hash = ?1")
                .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
            for hash in existing {
                if !hashes.contains(&hash) {
                    removed += del_stmt
                        .execute(params![hash])
                        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
                }
            }
        }
        tx.commit()
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        Ok(removed)
    }
}

#[async_trait]
impl ThreadStore for SqliteThreadStore {
    async fn create(&self, thread: Thread) -> Result<(), ThreadStoreError> {
        let store = self.clone_handle();
        tokio::task::spawn_blocking(move || store.create_blocking(thread))
            .await
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    }

    async fn hydrate(&self, id: &ThreadId) -> Result<Option<Thread>, ThreadStoreError> {
        let store = self.clone_handle();
        let id = id.clone();
        tokio::task::spawn_blocking(move || store.hydrate_blocking(&id))
            .await
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    }

    async fn append(
        &self,
        delta: protocol::ThreadDelta,
        expected_revision: ThreadRevision,
    ) -> Result<Ack, ThreadStoreError> {
        let store = self.clone_handle();
        tokio::task::spawn_blocking(move || store.append_blocking(delta, expected_revision))
            .await
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    }

    async fn execute(
        &self,
        thread_id: &ThreadId,
        command: CommandEnvelope,
    ) -> Result<CommandAck, ThreadStoreError> {
        let store = self.clone_handle();
        let thread_id = thread_id.clone();
        tokio::task::spawn_blocking(move || store.execute_blocking(&thread_id, command))
            .await
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    }

    async fn acquire_lease(
        &self,
        thread: &ThreadId,
        owner: &str,
        ttl: std::time::Duration,
    ) -> Result<protocol::WriterLease, ThreadStoreError> {
        let store = self.clone_handle();
        let thread = thread.clone();
        let owner = owner.to_string();
        tokio::task::spawn_blocking(move || store.acquire_lease_blocking(&thread, &owner, ttl))
            .await
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    }

    async fn release_lease(&self, lease: &protocol::WriterLease) -> Result<(), ThreadStoreError> {
        let store = self.clone_handle();
        let lease = lease.clone();
        tokio::task::spawn_blocking(move || store.release_lease_blocking(&lease))
            .await
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    }

    async fn gc(
        &self,
        thread: &ThreadId,
        grace: std::time::Duration,
    ) -> Result<GcReport, ThreadStoreError> {
        let store = self.clone_handle();
        let thread = thread.clone();
        tokio::task::spawn_blocking(move || store.gc_blocking(&thread, grace))
            .await
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    }

    fn blobs(&self) -> Arc<dyn protocol::BlobStore> {
        Arc::new(SqliteBlobStore {
            conn: Arc::clone(&self.conn),
        })
    }
}

impl SqliteThreadStore {
    fn clone_handle(&self) -> SqliteThreadStore {
        SqliteThreadStore {
            conn: Arc::clone(&self.conn),
        }
    }
}

#[cfg(test)]
mod tests;

/// Index one fact into `fts_thread_facts` when it carries searchable text.
fn insert_fts_fact(
    conn: &Connection,
    thread_id: &str,
    fact: &ThreadFact,
) -> Result<(), ThreadStoreError> {
    let (role, content) = match &fact.payload {
        FactPayload::Message { message } => (role_to_sql(message.role), message.content.clone()),
        FactPayload::Observation {
            call_id,
            tool_name,
            lifecycle,
            ..
        } => (
            "tool",
            lifecycle.lower_to_message(call_id, tool_name).content,
        ),
        FactPayload::Compaction { summary, .. } => ("system", summary.clone()),
        FactPayload::Termination { .. } | FactPayload::SystemNotice { .. } => return Ok(()),
    };
    if content.trim().is_empty() {
        return Ok(());
    }
    conn.prepare_cached(
        "INSERT INTO fts_thread_facts (fact_id, thread_id, role, content) VALUES (?1, ?2, ?3, ?4)",
    )
    .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
    .execute(params![fact.id.0, thread_id, role, content])
    .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    Ok(())
}

/// Write a thread's facts, branches, blob references, and the derived FTS
/// index in the current transaction.
///
/// `replace` drops the previous graph and FTS rows first (a full transcript
/// rewrite). A delta write (`replace == false`) inserts only missing facts
/// without deleting prior turns' FTS entries: only newly inserted facts are
/// appended to `fts_thread_facts`, and the branch cursors are updated only when
/// present.
pub(crate) fn persist_thread_facts(
    conn: &Connection,
    thread: &Thread,
    replace: bool,
) -> Result<(), ThreadStoreError> {
    if replace {
        conn.prepare_cached("DELETE FROM causal_nodes WHERE thread_id = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute(params![thread.id.0])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        conn.prepare_cached("DELETE FROM thread_branches WHERE thread_id = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute(params![thread.id.0])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        conn.prepare_cached("DELETE FROM thread_blob_refs WHERE thread_id = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute(params![thread.id.0])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        conn.prepare_cached("DELETE FROM fts_thread_facts WHERE thread_id = ?1")
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute(params![thread.id.0])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    } else if thread.graph.is_empty() && thread.state.branches.is_empty() {
        return Ok(());
    }

    // A delta save carries only the appended slice, so its first fact has no
    // in-memory parent. Read the durable branch head *before* upserting the
    // incoming cursors (which already name the new tail); a full rewrite
    // deleted the graph and starts from a true root.
    let mut link_parent: Option<String> = if replace {
        None
    } else {
        conn.prepare_cached(
            "SELECT head_fact_id FROM thread_branches WHERE thread_id = ?1 AND branch_id = ?2",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .query_row(params![thread.id.0, thread.state.active_branch.0], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .flatten()
    };
    for branch in thread.state.branches.values() {
        upsert_branch(conn, &thread.id, branch)?;
    }
    for fact in thread.all_facts() {
        let mut fact = fact.clone();
        if fact.parent_id.is_none() {
            fact.parent_id = link_parent.as_ref().map(|id| FactId::from_raw(id.clone()));
        }
        link_parent = Some(fact.id.0.clone());
        let newly_inserted = insert_fact(conn, &thread.id, &fact)?;
        if let Some(reference) = fact.payload.blob_ref() {
            conn.prepare_cached(
                "INSERT INTO thread_blob_refs (thread_id, fact_id, hash, size, mime, preview, trust)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(thread_id, fact_id) DO UPDATE SET
                     hash = excluded.hash, size = excluded.size, mime = excluded.mime,
                     preview = excluded.preview, trust = excluded.trust",
            )
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
            .execute(params![
                thread.id.0,
                fact.id.0,
                reference.hash,
                reference.size as i64,
                reference.mime,
                reference.preview,
                trust_to_sql(reference.trust),
            ])
            .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
        }
        if replace || newly_inserted {
            insert_fts_fact(conn, &thread.id.0, &fact)?;
        }
    }
    let head = thread
        .state
        .active_branch()
        .ok()
        .and_then(|branch| branch.head.as_ref())
        .map(|id| id.0.clone());
    if replace || head.is_some() || !thread.state.branches.is_empty() {
        conn.prepare_cached(
            "UPDATE threads SET active_leaf = ?2, active_branch_id = ?3 WHERE id = ?1",
        )
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![thread.id.0, head, thread.state.active_branch.0])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;
    }
    conn.prepare_cached("UPDATE thread_policies SET thread_policy_json = ?2 WHERE thread_id = ?1")
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?
        .execute(params![thread.id.0, encode(&thread.policy)?])
        .map_err(|error| ThreadStoreError::Backend(error.to_string()))?;

    Ok(())
}
