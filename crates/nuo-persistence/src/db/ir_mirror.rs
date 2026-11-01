//! Fact-level mirror of the transcript into the canonical Session IR.
//!
//! The transcript is the message SSOT; `causal_nodes` is the *fact* substrate
//! the Session IR reads. Nothing on the live turn path ever wrote those rows, so
//! `SessionStore::session_ir()` always hydrated an empty graph and every
//! consumer of it (compaction included) silently degraded to its no-op branch.
//!
//! `commit_turn` is the live seam: the one place every completed turn — and its
//! tool results — passes through. Mirroring there keeps the two views convergent
//! without a second writer racing the transcript. It is idempotent: the entry id
//! is the node id, so `INSERT OR IGNORE` on `(session_id, id)` writes each
//! message node exactly once.
//!
//! # The compacted horizon
//!
//! `StateUpdate` carries `active_leaf` but not `compaction_horizon`, and
//! `load_session_ir` had no source for it, so it hardcoded `None` — every folded
//! session silently un-folded on reload. A compacted session *does* record its
//! horizon durably: the transcript's own `Compact` directive holds the
//! checkpoint's `seq`. [`derive_horizon`] maps that back to a node id, and it is
//! stored on the newest node's `provider_meta` — the established home for
//! protocol-private state that must survive a verbatim round trip (the Gemini
//! thought-signature lane uses it too). It is never sent on the wire, so it costs
//! the model nothing.

use rusqlite::{Connection, OptionalExtension, Result, params};

/// One planned `causal_nodes` row.
struct PlannedNode {
    id: String,
    parent_id: Option<String>,
    seq: u64,
    kind: &'static str,
    payload: String,
}

/// Build the rows for a transcript, tagging the newest one with the horizon.
fn plan_nodes(
    data: &crate::session::SessionData,
    initial_parent: Option<String>,
) -> Vec<PlannedNode> {
    let entries = &data.transcript.entries;
    let mut rows: Vec<PlannedNode> = Vec::with_capacity(entries.len());
    // A commit receives only the *delta* (entries above the durable watermark),
    // so a chain restarted at `None` here would leave every commit as its own
    // orphaned segment — a graph `linear_path` can only walk two nodes into. The
    // segment's first node links to whatever fact is already newest.
    let mut previous_id: Option<String> = initial_parent;

    for entry in entries {
        let Some(message) = entry.to_message() else {
            // `state` entries are projections, not dialogue: the causal graph
            // has no node kind for them.
            continue;
        };
        // A checkpoint is a projection of the IR's own `compaction` node, which
        // `append_compaction` already created and which carries the summary plus
        // the fold's file manifest. Mirroring the rendered message as well would
        // put two checkpoints in one graph — one the horizon, one a plain message
        // that `linear_path` would happily walk straight through.
        if message
            .origin
            .as_ref()
            .is_some_and(|o| o.kind == nuo_protocol::InjectionKind::CompactionCheckpoint)
        {
            continue;
        }
        rows.push(PlannedNode {
            id: entry.id.clone(),
            parent_id: previous_id.clone(),
            seq: entry.seq,
            kind: "dialogue",
            payload: serde_json::to_string(&nuo_protocol::NodePayload::Message { message })
                .unwrap_or_default(),
        });
        previous_id = Some(entry.id.clone());
    }

    rows
}

/// Mirror a committed transcript into `causal_nodes` (idempotent).
///
/// Sequence numbers are the transcript's own `entry.seq` — the same sequence
/// `entry_memberships` uses — so the fact table and the membership table stay
/// joinable and a node's identity is its entry's identity.
pub fn mirror_transcript(
    conn: &Connection,
    data: &crate::session::SessionData,
    replaced: bool,
) -> Result<()> {
    // `threads` owns the session identity; `causal_nodes.session_id` references
    // it, so a mirror that ran before the thread row existed would be dropped by
    // the foreign key. A missing thread is an upstream bug, not a reason to lose
    // facts.
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM threads WHERE id = ?1",
            params![data.id],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(());
    }

    // A full rewrite means the transcript was *replaced* — a rebuild, a leaf
    // switch, a fork hydration. The graph mirrors the transcript, so the old
    // facts are no longer this session's facts and keeping them would both
    // duplicate the conversation and collide on `UNIQUE(session_id, seq)` (a
    // rebuilt transcript restarts its sequence at zero).
    if replaced {
        conn.execute(
            "DELETE FROM causal_nodes WHERE session_id = ?1",
            params![data.id],
        )?;
    }

    let initial_parent = crate::db::ir_mirror::newest_leaf(conn, &data.id)?;
    let rows = plan_nodes(data, initial_parent);
    if rows.is_empty() {
        return Ok(());
    }

    let mut stmt = conn.prepare_cached(
        r#"
        INSERT INTO causal_nodes (
            id, session_id, parent_id, seq, kind, payload_json, timestamp_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(session_id, id) DO UPDATE SET
            -- Repair the causal edge only. A legacy backfill (migration 17) wrote
            -- every node with `parent_id = NULL`, which makes the graph a flat bag
            -- that `linear_path` cannot walk. Payload and seq stay untouched: a
            -- fact's content is immutable, its lineage was never recorded.
            parent_id = excluded.parent_id;
        "#,
    )?;

    for node in &rows {
        stmt.execute(params![
            node.id,
            data.id,
            node.parent_id,
            node.seq as i64,
            node.kind,
            node.payload,
            data.updated_at as i64 * 1000,
        ])?;
    }
    Ok(())
}

/// Read the compaction horizon back out of the graph.
///
/// A `compaction` node *is* its own horizon — the kernel records
/// `state.compaction_horizon` on the fold, but the durable fact needs no parallel
/// marker: `NodeKind` already says what it is. Reading it back this way means the
/// fold survives a reload through every path that persists facts, including
/// `commit_session_ir` (which writes a delta, never a full transcript mirror).
pub fn read_horizon(conn: &Connection, session_id: &str) -> Result<Option<String>> {
    conn.query_row(
        r#"
        SELECT id FROM causal_nodes
        WHERE session_id = ?1 AND kind = 'compaction'
        ORDER BY seq DESC LIMIT 1
        "#,
        params![session_id],
        |row| row.get(0),
    )
    .optional()
}

/// The newest *position* in the session's lineage — the live cursor.
///
/// Compaction nodes are horizons, not positions: a fold is the highest-sequence
/// fact in the graph, so treating it as the cursor would make the tail
/// unreachable from the leaf.
///
/// `threads.active_leaf` is the authority when it is set (a fork or a leaf
/// switch writes it). It is `NULL` for a session that only ever committed plain
/// turns, because the transcript save path does not maintain it; the newest fact
/// is then the only honest answer, and it is the same one `ExecutionRecord`
/// makes when it hydrates facts without a cursor.
pub fn newest_leaf(conn: &Connection, session_id: &str) -> Result<Option<String>> {
    conn.query_row(
        r#"
        SELECT id FROM causal_nodes
        WHERE session_id = ?1 AND kind <> 'compaction'
        ORDER BY seq DESC LIMIT 1
        "#,
        params![session_id],
        |row| row.get(0),
    )
    .optional()
}

/// The Session IR's declarative policy, projected from one thread row.
///
/// `thread_policies` carries the thread's real configuration in typed columns
/// (`system_role`, `workspace_root`, `unattended`, `disabled_tools_json`,
/// `provider_pin`, the budget columns). The IR reads a JSON *object* per policy
/// group instead, and nothing on the live path wrote those four columns — they
/// kept the schema's `{}` placeholder. That was not merely a missing mirror: an
/// object with no fields failed `SessionPolicy`'s deserializer, and because
/// `load_session_ir` decoded the policy before the node rows, one placeholder
/// discarded the session's entire causal graph.
///
/// Emitting a complete object per group keeps the two representations in step by
/// construction, since both are derived from the same `SessionData` in the same
/// statement.
pub struct IrPolicyJson {
    pub rules_json: String,
    pub capabilities_json: String,
    pub guardrails_json: String,
    pub budget_json: String,
}

/// Project the IR policy columns from a thread's own state.
pub fn project_policy(data: &crate::session::SessionData) -> IrPolicyJson {
    let empty = String::from("{}");
    let encode =
        |value: serde_json::Value| serde_json::to_string(&value).unwrap_or_else(|_| empty.clone());

    let rules = serde_json::json!({
        "system_persona": data.role,
        "workspace_root": data.workspace.as_ref().map(|w| w.root.to_string_lossy().into_owned()),
        "project_rules": Vec::<String>::new(),
    });
    let mut disabled: Vec<&String> = data.disabled_tools.iter().collect();
    disabled.sort();
    let capabilities = serde_json::json!({
        "enabled_tools": Vec::<String>::new(),
        "disabled_tools": disabled,
        // `ProviderSelection.connection` is the field; `provider` is only a serde
        // alias for older payloads, so reading `provider` off the serialized object
        // would have yielded `null` for every session and silently dropped the pin.
        "provider_pin": data.provider_selection.as_ref().map(|s| s.connection.clone()),
    });
    let guardrails = serde_json::json!({
        "unattended": data.unattended,
        "require_approval_tools": Vec::<String>::new(),
    });
    // The thread row has no budget columns of its own; the IR budget is the
    // documented default until a caller seeds a per-session policy.
    let budget = serde_json::to_value(nuo_protocol::BudgetPolicy::default())
        .unwrap_or_else(|_| serde_json::json!({}));

    IrPolicyJson {
        rules_json: encode(rules),
        capabilities_json: encode(capabilities),
        guardrails_json: encode(guardrails),
        budget_json: encode(budget),
    }
}
