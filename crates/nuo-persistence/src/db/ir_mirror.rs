//! Fact-level mirror of the transcript into the canonical Session IR.
//!
//! The transcript is the message SSOT; `causal_nodes` is the *fact* substrate
//! the Session IR reads. Nothing on the live turn path ever wrote those rows, so
//! `SessionStore::session_ir()` always hydrated an empty graph and every
//! consumer of it (compaction included) silently degraded to its no-op branch.
//!
//! `commit_turn` is the live seam: the one place every completed turn — and its
//! tool results — passes through. Mirroring there keeps the two views convergent
//! without a second writer racing the transcript.
//!
//! The mirror is **incremental**: a save mirrors only the entries above the
//! durable membership watermark it is already writing, never the whole
//! transcript. The earlier implementation planned every entry on every save —
//! re-serializing the complete conversation to JSON per ReAct turn — which made
//! a turn's persistence cost grow with the session's history (seconds per turn
//! on a long session, and the dominant WAL producer). Node identity is the
//! entry id, so a replay over an already-mirrored range is still a no-op.
//!
//! # The compacted horizon
//!
//! `StateUpdate` carries `active_leaf` but not `compaction_horizon`, and
//! `load_session_ir` had no source for it, so it hardcoded `None` — every folded
//! session silently un-folded on reload. A compacted session *does* record its
//! horizon durably: the transcript's own `Compact` directive holds the
//! checkpoint's `seq`. `derive_horizon` maps that back to a node id, and it is
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

/// Build the rows for `entries` (a delta slice on the hot path), tagging the
/// newest one with the horizon.
fn plan_nodes(
    entries: &[nuo_protocol::TranscriptEntry],
    initial_parent: Option<String>,
) -> Vec<PlannedNode> {
    let mut rows: Vec<PlannedNode> = Vec::with_capacity(entries.len());
    // A commit receives only the *delta* (entries above the durable watermark),
    // so a chain restarted at `None` here would leave every commit as its own
    // orphaned segment — a graph `linear_path` can only walk two nodes into. The
    // segment's first node links to whatever fact is already newest.
    //
    // This tracks the newest **dialogue** fact, never a compaction anchor: a
    // checkpoint is appended at the transcript's end (its seq is the largest)
    // but belongs *before* the kept tail. Chaining facts through it would make
    // the horizon an ancestor of the tail and hide the tail from the walk.
    let mut previous_dialogue: Option<String> = initial_parent;

    for entry in entries {
        match &entry.payload {
            // `state` entries are projections, not dialogue.
            nuo_protocol::EntryPayload::State(_) => continue,
            nuo_protocol::EntryPayload::Compaction(meta) => {
                // The entry *is* the fold. Parent the horizon to the last
                // folded node (a sibling anchor, per `INV-CHK-01`) and leave
                // `previous_dialogue` untouched so later facts attach to the
                // kept tail. This single rule is what makes the graph
                // reconstructible from the transcript alone.
                rows.push(PlannedNode {
                    id: entry.id.clone(),
                    parent_id: meta
                        .split_parent_node_id
                        .clone()
                        .or_else(|| previous_dialogue.clone()),
                    seq: entry.seq,
                    kind: "compaction",
                    payload: serde_json::to_string(&nuo_protocol::NodePayload::Compaction {
                        summary: entry.content.clone().unwrap_or_default(),
                        first_kept_node_id: meta.first_kept_node_id.clone(),
                        tokens_before: meta.tokens_before,
                        read_files: meta.read_files.clone(),
                        modified_files: meta.modified_files.clone(),
                        belief_state: None,
                    })
                    .unwrap_or_default(),
                });
            }
            nuo_protocol::EntryPayload::Message(_) => {
                // A checkpoint written before the typed payload existed has no
                // structure to derive a horizon from. It is not a dialogue
                // fact either; do not smuggle it into the chain. (No production
                // database contains one; the format is a clean break.)
                if entry.origin == Some(nuo_protocol::EntryOrigin::Checkpoint) {
                    continue;
                }
                let Some(message) = entry.to_message() else {
                    continue;
                };
                rows.push(PlannedNode {
                    id: entry.id.clone(),
                    parent_id: previous_dialogue.clone(),
                    seq: entry.seq,
                    kind: "dialogue",
                    payload: serde_json::to_string(&nuo_protocol::NodePayload::Message { message })
                        .unwrap_or_default(),
                });
                previous_dialogue = Some(entry.id.clone());
            }
        }
    }

    rows
}

/// Mirror `entries` (a whole transcript on a rewrite, the appended slice on a
/// delta save) into `causal_nodes`.
///
/// Sequence numbers are the transcript's own `entry.seq` — the same sequence
/// `entry_memberships` uses — so the fact table and the membership table stay
/// joinable and a node's identity is its entry's identity.
///
/// `replace` deletes the session's graph first: a rebuilt transcript restarts
/// its sequence at zero, so keeping the old facts would duplicate the
/// conversation and violate `UNIQUE(session_id, seq)`. A delta save links its
/// first node to the graph's newest fact; the caller is responsible for having
/// verified that the graph is contiguous with the slice (see
/// `super::session_store`'s coverage check, which rebuilds instead of
/// orphaning when it is not).
///
/// Returns the number of planned nodes — the direct measure of the write's
/// work.
pub fn mirror_transcript(
    conn: &Connection,
    session_id: &str,
    updated_at: u64,
    entries: &[nuo_protocol::TranscriptEntry],
    replace: bool,
) -> Result<usize> {
    // `threads` owns the session identity; `causal_nodes.session_id` references
    // it, so a mirror that ran before the thread row existed would be dropped by
    // the foreign key. A missing thread is an upstream bug, not a reason to lose
    // facts.
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM threads WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(0);
    }

    if replace {
        conn.execute(
            "DELETE FROM causal_nodes WHERE session_id = ?1",
            params![session_id],
        )?;
        // The column is a *lineage position*, not the session's last message:
        // after a rebuild it names a node that no longer exists. Leaving it set
        // makes `load_session_ir` prefer a dangling leaf over the rebuilt
        // graph's real tail, which resolves to an empty active branch. Clearing
        // it restores the documented fallback (`newest_position`).
        conn.execute(
            "UPDATE threads SET active_leaf = NULL WHERE id = ?1",
            params![session_id],
        )?;
    }

    let initial_parent = if replace {
        None
    } else {
        newest_position(conn, session_id)?.map(|(id, _)| id)
    };
    let rows = plan_nodes(entries, initial_parent);
    if rows.is_empty() {
        return Ok(0);
    }

    let mut stmt = conn.prepare_cached(
        r#"
        INSERT INTO causal_nodes (
            id, session_id, parent_id, seq, kind, payload_json, timestamp_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(session_id, id) DO UPDATE SET
            -- Lineage is write-once. A legacy backfill (migration 17) wrote
            -- every node with `parent_id = NULL`, so filling a *missing* edge
            -- is a repair. Rewriting an edge that already exists is not: a
            -- delta that re-mirrors an already-present entry would point that
            -- entry at the newest node, and the chain's head then points into
            -- its own tail — a cycle `linear_path` walks forever (the
            -- 22 GB server OOM). Facts are immutable; so is their lineage.
            parent_id = excluded.parent_id
        WHERE causal_nodes.parent_id IS NULL;
        "#,
    )?;

    for node in &rows {
        stmt.execute(params![
            node.id,
            session_id,
            node.parent_id,
            node.seq as i64,
            node.kind,
            node.payload,
            updated_at as i64 * 1000,
        ])?;
    }
    Ok(rows.len())
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
    Ok(newest_position(conn, session_id)?.map(|(id, _)| id))
}

/// The lineage position a new fact attaches to.
///
/// `threads.active_leaf` is the authority when set: a fold moves the cursor to
/// the checkpoint (or the kept tail), and appending to the highest surviving
/// node instead would build a segment the fold's cursor cannot reach. Sessions
/// that only ever appended leave it `NULL`; the newest non-compaction node is
/// then the only honest answer.
///
/// `seq` is what the save path compares against the appended slice's first
/// sequence to decide whether the graph is contiguous with the transcript or
/// must be rebuilt.
pub(crate) fn newest_position(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<(String, u64)>> {
    let active_leaf: Option<String> = conn
        .query_row(
            "SELECT active_leaf FROM threads WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(leaf) = active_leaf
        && let Some(seq) = conn
            .query_row(
                "SELECT seq FROM causal_nodes WHERE session_id = ?1 AND id = ?2",
                params![session_id, leaf],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
    {
        return Ok(Some((leaf, seq.max(0) as u64)));
    }
    conn.query_row(
        r#"
        SELECT id, seq FROM causal_nodes
        WHERE session_id = ?1 AND kind <> 'compaction'
        ORDER BY seq DESC LIMIT 1
        "#,
        params![session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// True when the stored lineage cannot be walked in append order.
///
/// A well-formed graph is append-ordered: a node's parent was written before
/// it, so `parent.seq < child.seq` (compaction anchors included — they are
/// appended at `max_seq + 1` and point at the folded node). Any edge at or
/// past its own child is therefore the signature of a rewritten parent; a
/// self-edge is the degenerate case. A cycle necessarily contains at least one
/// such non-decreasing edge, so this indexed probe finds the corruption that
/// turns `linear_path` into a loop without paying for full cycle detection on
/// every drain.
pub(crate) fn has_corrupt_lineage(conn: &Connection, session_id: &str) -> Result<bool> {
    conn.query_row(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM causal_nodes c
            LEFT JOIN causal_nodes p
              ON p.session_id = c.session_id AND p.id = c.parent_id
            WHERE c.session_id = ?1
              AND (
                    c.parent_id = c.id
                 OR (c.parent_id IS NOT NULL AND p.id IS NULL)
                 OR p.seq >= c.seq
              )
            LIMIT 1
        )
        "#,
        params![session_id],
        |row| row.get(0),
    )
}

/// True when the graph no longer covers the transcript's last mirrorable fact.
///
/// This is the lost-graph probe. A cursor can honestly say "built through seq N"
/// while the rows themselves are gone (a derived-table rebuild, a bad manual
/// delete, a partially applied repair). Comparing the graph's highest seq
/// against the last mirrorable entry detects that even when no new fact is
/// pending; without it `session_ir()` would happily hydrate an empty horizon.
pub(crate) fn has_missing_coverage(conn: &Connection, session_id: &str) -> Result<bool> {
    let graph_max: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), -1) FROM causal_nodes WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )?;
    let entry_max: i64 = conn.query_row(
        r#"
        SELECT COALESCE(MAX(m.seq), -1)
          FROM entry_memberships m
          JOIN entries e ON e.id = m.entry_id
         WHERE m.session_id = ?1
           AND e.kind = 'message'
           AND e.role IS NOT NULL
        "#,
        params![session_id],
        |row| row.get(0),
    )?;
    Ok(graph_max < entry_max)
}

/// True when a dialogue entry exists in `(after_seq, before_seq)` that the
/// graph has not mirrored. `after_seq == None` means "from the beginning".
///
/// This is the coverage probe behind the save path's repair decision: a graph
/// that is missing any dialogue fact in the appended range cannot simply have
/// the new tail linked onto it — the resulting segment would be unreachable
/// from the root and invisible to `linear_path`.
pub(crate) fn has_unmirrored_dialogue(
    conn: &Connection,
    session_id: &str,
    after_seq: Option<u64>,
    before_seq: u64,
) -> Result<bool> {
    let after = after_seq.map(|seq| seq as i64).unwrap_or(-1);
    conn.query_row(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM entry_memberships m
            JOIN entries e ON e.id = m.entry_id
            WHERE m.session_id = ?1
              AND m.seq > ?2
              AND m.seq < ?3
              AND e.kind = 'message'
              AND e.role IS NOT NULL
            LIMIT 1
        )
        "#,
        params![session_id, after, before_seq as i64],
        |row| row.get(0),
    )
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
