//! Session IR persistence bridge for DatabaseEngine (ADR-0241).
//!
//! Implements isomorphic serialization and hydration between SQLite and
//! [`nuo_contracts::SessionIR`]:
//! - `causal_nodes`: stores immutable fact nodes and DAG edges.
//! - `session_policies`: stores declarative policy, capabilities, and budgets.
//! - `sessions_v2`: stores session working cursor, execution status, and timestamps.

use nuo_contracts::{
    CausalNode, ExecutionStatus, NodeKind, NodePayload, SessionDelta, SessionIR, SessionPolicy,
    SessionState, SuspensionReason, SystemNoticePayload,
};
use rusqlite::{Connection, OptionalExtension, Result, params};

/// Schema initialization for Session IR tables (Migration 17 / v15 schema).
pub fn initialize_session_ir_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        -- 1. Core session working state and cursor table (headless, free from TUI layout fields)
        CREATE TABLE IF NOT EXISTS sessions_v2 (
            id                    TEXT PRIMARY KEY,
            parent_session_id     TEXT REFERENCES sessions_v2(id) ON DELETE SET NULL,
            active_leaf           TEXT,
            status                TEXT NOT NULL DEFAULT 'idle',
            suspension_payload    TEXT,
            pending_notifications TEXT NOT NULL DEFAULT '[]',
            round_counter         INTEGER NOT NULL DEFAULT 0,
            created_at_s          INTEGER NOT NULL,
            updated_at_s          INTEGER NOT NULL
        );

        -- 2. Declarative session policies and governing rules
        CREATE TABLE IF NOT EXISTS session_policies (
            session_id            TEXT PRIMARY KEY REFERENCES sessions_v2(id) ON DELETE CASCADE,
            rules_json            TEXT NOT NULL,
            capabilities_json     TEXT NOT NULL,
            guardrails_json       TEXT NOT NULL,
            budget_json           TEXT NOT NULL,
            updated_at_s          INTEGER NOT NULL
        );

        -- 3. Immutable causal graph nodes replacing legacy entries, entry_memberships, and JSON trees
        CREATE TABLE IF NOT EXISTS causal_nodes (
            id                    TEXT NOT NULL,
            session_id            TEXT NOT NULL REFERENCES sessions_v2(id) ON DELETE CASCADE,
            parent_id             TEXT,
            seq                   INTEGER NOT NULL,
            kind                  TEXT NOT NULL CHECK (kind IN ('dialogue','compaction','termination','system_notice')),
            payload_json          TEXT NOT NULL,
            timestamp_ms          INTEGER NOT NULL,
            PRIMARY KEY (session_id, id),
            UNIQUE(session_id, seq)
        );

        CREATE INDEX IF NOT EXISTS idx_causal_nodes_session_seq ON causal_nodes(session_id, seq ASC);
        CREATE INDEX IF NOT EXISTS idx_causal_nodes_parent ON causal_nodes(session_id, parent_id);
        "#,
    )?;
    Ok(())
}

/// Save a SessionDelta into SQLite in O(Δ) time (INV-SESSION-05).
pub fn save_session_delta(conn: &Connection, delta: &SessionDelta) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let run = || -> Result<()> {
        // 1. Upsert sessions_v2 row
        let (status_str, suspension_str) = match &delta.state_update.status {
            ExecutionStatus::Idle => ("idle", None),
            ExecutionStatus::Running { .. } => ("running", None),
            ExecutionStatus::Suspended { reason } => (
                "suspended",
                Some(
                    serde_json::to_string(reason)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                ),
            ),
        };

        let notifications_str = serde_json::to_string(&delta.state_update.pending_notifications)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        conn.execute(
            r#"
            INSERT INTO sessions_v2 (
                id, parent_session_id, active_leaf, status, suspension_payload,
                pending_notifications, round_counter, created_at_s, updated_at_s
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
            ON CONFLICT(id) DO UPDATE SET
                parent_session_id = COALESCE(excluded.parent_session_id, sessions_v2.parent_session_id),
                active_leaf = excluded.active_leaf,
                status = excluded.status,
                suspension_payload = excluded.suspension_payload,
                pending_notifications = excluded.pending_notifications,
                round_counter = excluded.round_counter,
                updated_at_s = excluded.updated_at_s;
            "#,
            params![
                delta.session_id,
                delta.parent_session_id.as_deref(),
                delta.state_update.active_leaf,
                status_str,
                suspension_str,
                notifications_str,
                delta.state_update.round_counter,
                delta.updated_at_s,
            ],
        )?;

        // 2. Insert new causal nodes (O(Δ))
        if !delta.new_nodes.is_empty() {
            let mut stmt = conn.prepare_cached(
                r#"
                INSERT OR IGNORE INTO causal_nodes (
                    id, session_id, parent_id, seq, kind, payload_json, timestamp_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);
                "#,
            )?;

            for node in &delta.new_nodes {
                let kind_str = match node.kind {
                    NodeKind::Dialogue => "dialogue",
                    NodeKind::Observation => "observation",
                    NodeKind::Compaction => "compaction",
                    NodeKind::Termination => "termination",
                    NodeKind::SystemNotice => "system_notice",
                };
                let payload_str = serde_json::to_string(&node.payload)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

                stmt.execute(params![
                    node.id,
                    delta.session_id,
                    node.parent_id,
                    node.seq,
                    kind_str,
                    payload_str,
                    node.timestamp_ms,
                ])?;
            }
        }

        // 3. Upsert policy if modified
        if let Some(ref policy) = delta.policy_update {
            let rules_str = serde_json::to_string(&policy.rules)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let caps_str = serde_json::to_string(&policy.capabilities)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let guard_str = serde_json::to_string(&policy.guardrails)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let budget_str = serde_json::to_string(&policy.budget)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

            conn.execute(
                r#"
                INSERT INTO session_policies (
                    session_id, rules_json, capabilities_json, guardrails_json, budget_json, updated_at_s
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(session_id) DO UPDATE SET
                    rules_json = excluded.rules_json,
                    capabilities_json = excluded.capabilities_json,
                    guardrails_json = excluded.guardrails_json,
                    budget_json = excluded.budget_json,
                    updated_at_s = excluded.updated_at_s;
                "#,
                params![
                    delta.session_id,
                    rules_str,
                    caps_str,
                    guard_str,
                    budget_str,
                    delta.updated_at_s,
                ],
            )?;
        }
        Ok(())
    };

    match run() {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Hydrate a full SessionIR from SQLite (INV-SESSION-02).
pub fn load_session_ir(conn: &Connection, session_id: &str) -> Result<Option<SessionIR>> {
    // 1. Query sessions_v2 table
    let session_row = conn
        .query_row(
            r#"
            SELECT id, parent_session_id, active_leaf, status, suspension_payload,
                   pending_notifications, round_counter, created_at_s, updated_at_s
            FROM sessions_v2 WHERE id = ?1
            "#,
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, u64>(6)?,
                    row.get::<_, u64>(7)?,
                    row.get::<_, u64>(8)?,
                ))
            },
        )
        .optional()?;

    let Some((
        id,
        parent_session_id,
        active_leaf,
        status_str,
        suspension_str,
        notifications_str,
        round_counter,
        created_at_s,
        updated_at_s,
    )) = session_row
    else {
        return Ok(None);
    };

    // 2. Decode ExecutionStatus
    let status = match status_str.as_str() {
        "idle" => ExecutionStatus::Idle,
        "running" => ExecutionStatus::Running {
            turn: round_counter,
            started_at_ms: updated_at_s * 1000,
        },
        "suspended" => {
            let reason: SuspensionReason = suspension_str
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(SuspensionReason::NeedsInput {
                    prompt: "Resumed suspended session".into(),
                });
            ExecutionStatus::Suspended { reason }
        }
        _ => ExecutionStatus::Idle,
    };

    let pending_notifications: Vec<SystemNoticePayload> =
        serde_json::from_str(&notifications_str).unwrap_or_default();

    let mut timelines = std::collections::HashMap::new();
    timelines.insert(
        "main".to_string(),
        nuo_contracts::TimelineCursor {
            id: "main".to_string(),
            name: "Mainline".to_string(),
            kind: nuo_contracts::TimelineKind::Main,
            head_node: active_leaf.clone(),
            forked_from_node: None,
            created_at_s,
            updated_at_s,
        },
    );

    let state = SessionState {
        active_leaf,
        active_timeline: "main".to_string(),
        timelines,
        compaction_horizon: None,
        status,
        pending_notifications,
        pruning_exhausted: false,
        round_counter,
    };

    // 3. Load SessionPolicy
    let policy = conn
        .query_row(
            r#"
            SELECT rules_json, capabilities_json, guardrails_json, budget_json
            FROM session_policies WHERE session_id = ?1
            "#,
            params![session_id],
            |row| {
                let rules_json: String = row.get(0)?;
                let caps_json: String = row.get(1)?;
                let guard_json: String = row.get(2)?;
                let budget_json: String = row.get(3)?;

                let rules = serde_json::from_str(&rules_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                let capabilities = serde_json::from_str(&caps_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                let guardrails = serde_json::from_str(&guard_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                let budget = serde_json::from_str(&budget_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

                Ok(SessionPolicy {
                    rules,
                    capabilities,
                    guardrails,
                    budget,
                })
            },
        )
        .optional()?
        .unwrap_or_default();

    // 4. Load CausalNodes ordered by seq
    let mut stmt = conn.prepare(
        r#"
        SELECT id, parent_id, seq, kind, payload_json, timestamp_ms
        FROM causal_nodes WHERE session_id = ?1 ORDER BY seq ASC
        "#,
    )?;

    let mut ir = SessionIR {
        session_id: id,
        parent_session_id,
        created_at_s,
        updated_at_s,
        history: nuo_contracts::CausalGraph::new(),
        state,
        policy,
    };

    let rows = stmt.query_map(params![session_id], |row| {
        let node_id: String = row.get(0)?;
        let parent_id: Option<String> = row.get(1)?;
        let seq: u64 = row.get(2)?;
        let kind_str: String = row.get(3)?;
        let payload_json: String = row.get(4)?;
        let timestamp_ms: u64 = row.get(5)?;

        let kind = match kind_str.as_str() {
            "dialogue" => NodeKind::Dialogue,
            "observation" => NodeKind::Observation,
            "compaction" => NodeKind::Compaction,
            "termination" => NodeKind::Termination,
            "system_notice" => NodeKind::SystemNotice,
            _ => NodeKind::Dialogue,
        };

        let payload: NodePayload = serde_json::from_str(&payload_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?;

        Ok(CausalNode {
            id: node_id,
            parent_id,
            seq,
            timestamp_ms,
            kind,
            payload,
        })
    })?;

    for node_res in rows {
        let node = node_res?;
        ir.history.insert_node(node);
    }

    Ok(Some(ir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_contracts::TerminationReason;
    use nuo_contracts::message::{Message, Role};
    use rusqlite::Connection;

    #[test]
    fn test_session_ir_persistence_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_session_ir_schema(&conn).unwrap();

        // 1. Create SessionIR and append events
        let mut policy = SessionPolicy::default();
        policy.rules.system_persona = Some("Rust Core Engineer".into());
        policy.capabilities.enabled_tools = vec!["run_command".into(), "read_text".into()];

        let mut ir = SessionIR::new("session-roundtrip-test", policy.clone(), 1000);

        let id1 = ir.append_message(
            "node-1",
            1_000_000,
            Message::new(Role::User, "Build project"),
        );
        let id2 = ir.append_message(
            "node-2",
            1_001_000,
            Message::new(Role::Assistant, "Building..."),
        );
        let id3 = ir.record_termination(
            "node-term",
            1_002_000,
            TerminationReason::UserInterrupt,
            Some("Compiling target...".into()),
            Some("run_1".into()),
            Some(1000),
        );

        // Suspend session for approval
        ir.state.status = ExecutionStatus::Suspended {
            reason: SuspensionReason::NeedsApproval {
                tool_call_id: "call_run_deploy".into(),
                action: "deploy to production".into(),
            },
        };

        // 2. Drain delta and save
        let mut delta = ir.drain_delta(0);
        delta.policy_update = Some(policy.clone());
        save_session_delta(&conn, &delta).unwrap();

        // 3. Hydrate from database
        let hydrated_ir = load_session_ir(&conn, "session-roundtrip-test")
            .unwrap()
            .expect("session must exist");

        // 4. Verify exact forensic restoration
        assert_eq!(hydrated_ir.session_id, "session-roundtrip-test");
        assert_eq!(hydrated_ir.history.nodes.len(), 3);
        assert_eq!(hydrated_ir.state.active_leaf, Some(id3));
        assert_eq!(
            hydrated_ir.policy.rules.system_persona.as_deref(),
            Some("Rust Core Engineer")
        );

        match &hydrated_ir.state.status {
            ExecutionStatus::Suspended { reason } => match reason {
                SuspensionReason::NeedsApproval {
                    tool_call_id,
                    action,
                } => {
                    assert_eq!(tool_call_id, "call_run_deploy");
                    assert_eq!(action, "deploy to production");
                }
                _ => panic!("unexpected suspension reason"),
            },
            _ => panic!("expected suspended status"),
        }

        // Verify nodes
        let n1 = hydrated_ir.history.get_node(&id1).unwrap();
        assert_eq!(n1.seq, 1);
        let n2 = hydrated_ir.history.get_node(&id2).unwrap();
        assert_eq!(n2.seq, 2);
        let n3 = hydrated_ir
            .history
            .get_node(&hydrated_ir.state.active_leaf.unwrap())
            .unwrap();
        assert_eq!(n3.kind, NodeKind::Termination);

        // Verify incremental delta save: append 1 more node
        let id4 = ir.append_message("node-4", 1_003_000, Message::new(Role::User, "Resume work"));
        let delta2 = ir.drain_delta(3);
        assert_eq!(delta2.new_nodes.len(), 1);
        assert_eq!(delta2.new_nodes[0].id, id4);

        save_session_delta(&conn, &delta2).unwrap();
        let hydrated2 = load_session_ir(&conn, "session-roundtrip-test")
            .unwrap()
            .expect("session must exist");
        assert_eq!(hydrated2.history.nodes.len(), 4);
        assert_eq!(hydrated2.state.active_leaf, Some(id4));
    }
}
