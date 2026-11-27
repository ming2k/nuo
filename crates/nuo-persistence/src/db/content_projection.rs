//! Content-shard fact-graph projection helpers (ADR-0242, ADR-0255, ADR-0256).
//!
//! Maps the pure domain [`nuo_protocol::Thread`] aggregate onto and back from the
//! content-shard tables (`causal_nodes`, `thread_branches`, `thread_blob_refs`,
//! `fts_thread_facts`) joined with catalog lifecycle rows (`threads`).
//!
//! No `SqliteThreadStore` or lease/command tables live here: the per-session
//! append-only journal is the durable source of truth (ADR-0236, ADR-0256).

use std::collections::BTreeMap;

use nuo_protocol::{
    BranchId, BranchPolicy, BranchState, FactContext, FactId, FactKind, FactPayload,
    THREAD_SCHEMA_VERSION, Thread, ThreadFact, ThreadGraph, ThreadId, ThreadRevision, ThreadState,
    ThreadStatus, Trust,
};
use rusqlite::{Connection, OptionalExtension, params};

fn kind_to_sql(kind: FactKind) -> &'static str {
    match kind {
        FactKind::Message => "dialogue",
        FactKind::Observation => "observation",
        FactKind::Compaction => "compaction",
        FactKind::Termination => "termination",
        FactKind::SystemNotice => "system_notice",
    }
}

fn trust_to_sql(trust: Trust) -> &'static str {
    match trust {
        Trust::Trusted => "trusted",
        Trust::Untrusted => "untrusted",
    }
}

fn role_to_sql(role: nuo_tool::Role) -> &'static str {
    match role {
        nuo_tool::Role::User => "user",
        nuo_tool::Role::Assistant => "assistant",
        nuo_tool::Role::System => "system",
        nuo_tool::Role::Tool => "tool",
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

fn decode<T: serde::de::DeserializeOwned>(value: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(value).map_err(|error| format!("invalid {what}: {error}"))
}

fn insert_fact(conn: &Connection, thread_id: &ThreadId, fact: &ThreadFact) -> Result<bool, String> {
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
        .map_err(|error| error.to_string())?
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
        .map_err(|error| error.to_string())?;
    if inserted == 0 {
        let existing: Option<(String, String)> = conn
            .prepare_cached(
                "SELECT payload_json, context_json FROM causal_nodes WHERE thread_id = ?1 AND id = ?2",
            )
            .map_err(|error| error.to_string())?
            .query_row(params![thread_id.0, fact.id.0], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(|error| error.to_string())?;
        let Some((existing_payload, existing_context)) = existing else {
            return Err("fact insert was ignored".to_string());
        };
        if existing_payload != payload || existing_context != context {
            return Err(format!(
                "duplicate fact '{}' with conflicting payload",
                fact.id
            ));
        }
        return Ok(false);
    }
    Ok(true)
}

fn upsert_branch(
    conn: &Connection,
    thread_id: &ThreadId,
    branch: &BranchState,
) -> Result<(), String> {
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
    .map_err(|error| error.to_string())?
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
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn load_branches(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<BTreeMap<BranchId, BranchState>, String> {
    let mut stmt = conn
        .prepare_cached(
            r#"
            SELECT branch_id, name, head_fact_id, forked_from_fact_id,
                   compaction_horizon_fact_id, policy_json, created_at_ms, updated_at_ms
              FROM thread_branches
             WHERE thread_id = ?1
            "#,
        )
        .map_err(|error| error.to_string())?;
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
        .map_err(|error| error.to_string())?;
    let mut branches = BTreeMap::new();
    for row in rows {
        let (id, name, head, forked_from, horizon, policy, created, updated) =
            row.map_err(|error| error.to_string())?;
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

fn load_facts(conn: &Connection, thread_id: &ThreadId) -> Result<Vec<ThreadFact>, String> {
    let mut stmt = conn
        .prepare_cached(
            r#"
            SELECT id, parent_id, seq, payload_json, timestamp_ms, context_json
              FROM causal_nodes
             WHERE thread_id = ?1
             ORDER BY seq ASC
            "#,
        )
        .map_err(|error| error.to_string())?;
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
        .map_err(|error| error.to_string())?;
    let mut facts = Vec::new();
    for row in rows {
        let (id, parent, seq, payload, timestamp, context) =
            row.map_err(|error| error.to_string())?;
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

type CatalogThreadRow = (Option<String>, String, Option<String>, i64, i64, i64, i64);

/// Load a canonical thread aggregate by joining lifecycle metadata from
/// `catalog_conn` (`nuo.db`) with facts and branch cursors from `content_conn`
/// (`shards/content-XX.db`, ADR-0242, ADR-0255).
///
/// When `blob_store` is supplied, any offloaded message body (`content_blob` /
/// `{fact_id}:body`) or offloaded base64 image (`{fact_id}:img:{idx}`)
/// registered in `thread_blob_refs` is rehydrated transparently so shard reads
/// never return empty bodies or blank image attachments.
pub(crate) fn load_thread_from_planes(
    catalog_conn: &Connection,
    content_conn: &Connection,
    id: &ThreadId,
    blob_store: Option<&crate::blobs::BlobStore>,
) -> Result<Option<Thread>, String> {
    let row: Option<CatalogThreadRow> = catalog_conn
        .prepare_cached(
            r#"
            SELECT parent_thread_id, active_branch_id,
                   active_leaf, round_counter, created_at_ms, updated_at_ms, revision
              FROM threads WHERE id = ?1
            "#,
        )
        .map_err(|error| error.to_string())?
        .query_row(params![id.0], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((parent, active_branch, active_leaf, round_counter, created, updated, revision)) = row
    else {
        return Ok(None);
    };

    let mut branches = load_branches(content_conn, id)?;
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
    let active_branch = if branches.contains_key(&active_branch_id) {
        active_branch_id
    } else {
        branches
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(BranchId::main)
    };

    let mut graph = ThreadGraph::new();
    for fact in load_facts(content_conn, id)? {
        graph.insert(fact).map_err(|error| error.to_string())?;
    }

    if let Some(blob_store) = blob_store {
        rehydrate_graph_blobs(content_conn, &id.0, &mut graph, blob_store)?;
    }

    Ok(Some(Thread {
        id: id.clone(),
        parent_thread_id: parent.map(ThreadId::new),
        created_at_ms: created as u64,
        updated_at_ms: updated as u64,
        graph,
        state: ThreadState {
            active_branch,
            branches,
            round_counter: round_counter.max(0) as u64,
            status: ThreadStatus::Idle,
        },
        revision: ThreadRevision(revision.max(0) as u64),
        schema_version: THREAD_SCHEMA_VERSION,
    }))
}

/// Restore offloaded text bodies and base64 images on `FactPayload::Message`
/// facts using the content shard's `thread_blob_refs` ledger and CAS
/// `BlobStore` (ADR-0255).
fn rehydrate_graph_blobs(
    content_conn: &Connection,
    thread_id: &str,
    graph: &mut ThreadGraph,
    blob_store: &crate::blobs::BlobStore,
) -> Result<(), String> {
    let mut stmt = content_conn
        .prepare_cached("SELECT fact_id, hash FROM thread_blob_refs WHERE thread_id = ?1")
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map(params![thread_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut refs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for row in rows {
        let (key, hash) = row.map_err(|error| error.to_string())?;
        refs.insert(key, hash);
    }
    if refs.is_empty() {
        return Ok(());
    }
    graph.visit_messages_mut(|fact_id, message| {
        if message.content.is_empty() {
            let body_key = format!("{}:body", fact_id.0);
            let hash = message
                .content_blob
                .as_deref()
                .or_else(|| refs.get(&body_key).map(String::as_str));
            if let Some(hash) = hash
                && let Some(bytes) = blob_store.get(hash)
                && let Ok(text) = String::from_utf8(bytes)
            {
                message.content = text;
            }
        }
        if let Some(images) = message.images.as_mut() {
            for (idx, image) in images.iter_mut().enumerate() {
                if image.data.is_empty() {
                    let img_key = format!("{}:img:{idx}", fact_id.0);
                    if let Some(hash) = refs.get(&img_key)
                        && let Some(bytes) = blob_store.get(hash)
                        && let Ok(b64) = String::from_utf8(bytes)
                    {
                        image.data = b64;
                    }
                }
            }
        }
        Ok(())
    })
}

/// Index one fact into `fts_thread_facts` when it carries searchable text.
fn insert_fts_fact(conn: &Connection, thread_id: &str, fact: &ThreadFact) -> Result<(), String> {
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
    .map_err(|error| error.to_string())?
    .execute(params![fact.id.0, thread_id, role, content])
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Additional CAS blob reference carried by a transcript message (offloaded
/// oversized text via `content_blob` or offloaded base64 image via
/// `image_blobs`) so CAS garbage collection preserves live blobs (ADR-0255).
#[derive(Debug, Clone)]
pub(crate) struct ExtraBlobRef {
    pub(crate) fact_id_key: String,
    pub(crate) hash: String,
    pub(crate) size: u64,
    pub(crate) mime: String,
}

/// Write a thread's facts, branches, blob references, and FTS index into its
/// owning content shard without touching catalog tables (ADR-0242, ADR-0255).
pub(crate) fn persist_content_facts(
    conn: &Connection,
    thread: &Thread,
    replace: bool,
    extra_blob_refs: &[ExtraBlobRef],
) -> Result<(), String> {
    if replace {
        conn.prepare_cached("DELETE FROM causal_nodes WHERE thread_id = ?1")
            .map_err(|error| error.to_string())?
            .execute(params![thread.id.0])
            .map_err(|error| error.to_string())?;
        conn.prepare_cached("DELETE FROM thread_branches WHERE thread_id = ?1")
            .map_err(|error| error.to_string())?
            .execute(params![thread.id.0])
            .map_err(|error| error.to_string())?;
        conn.prepare_cached("DELETE FROM thread_blob_refs WHERE thread_id = ?1")
            .map_err(|error| error.to_string())?
            .execute(params![thread.id.0])
            .map_err(|error| error.to_string())?;
        conn.prepare_cached("DELETE FROM fts_thread_facts WHERE thread_id = ?1")
            .map_err(|error| error.to_string())?
            .execute(params![thread.id.0])
            .map_err(|error| error.to_string())?;
    } else if thread.graph.is_empty() && thread.state.branches.is_empty() {
        return Ok(());
    }

    let mut link_parent: Option<String> = if replace {
        None
    } else {
        conn.prepare_cached(
            "SELECT head_fact_id FROM thread_branches WHERE thread_id = ?1 AND branch_id = ?2",
        )
        .map_err(|error| error.to_string())?
        .query_row(params![thread.id.0, thread.state.active_branch.0], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()
        .map_err(|error| error.to_string())?
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
            .map_err(|error| error.to_string())?
            .execute(params![
                thread.id.0,
                fact.id.0,
                reference.hash,
                reference.size as i64,
                reference.mime,
                reference.preview,
                trust_to_sql(reference.trust),
            ])
            .map_err(|error| error.to_string())?;
        }
        if replace || newly_inserted {
            insert_fts_fact(conn, &thread.id.0, &fact)?;
        }
    }

    for extra in extra_blob_refs {
        conn.prepare_cached(
            "INSERT INTO thread_blob_refs (thread_id, fact_id, hash, size, mime, preview, trust)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'trusted')
             ON CONFLICT(thread_id, fact_id) DO UPDATE SET
                 hash = excluded.hash, size = excluded.size, mime = excluded.mime",
        )
        .map_err(|error| error.to_string())?
        .execute(params![
            thread.id.0,
            extra.fact_id_key,
            extra.hash,
            extra.size as i64,
            extra.mime,
        ])
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}
