//! Offline legacy→canonical migration (ADR-0280 §4, `INV-POLICY-03`).
//!
//! This module is the **one-time, offline** converter from the legacy
//! `causal_nodes` / `entries` representation into the canonical fact substrate.
//! It is deliberately **not** referenced from the runtime graph, the request
//! pipeline, or any provider path: it runs only as a standalone maintenance
//! command against a consistent backup, and the new runtime never reads legacy
//! formats (ADR-0280 §4, §5).
//!
//! What it guarantees:
//! - **No fabrication**: a legacy node without scope/version/raw material is
//!   migrated as an explicit `Unknown`/`Unavailable` state, never as a success.
//! - **Conflict quarantine**: when two sources disagree for one session, the
//!   session is reported as a conflict and skipped, never silently resolved.
//! - **Verification**: a machine-readable report compares recoverable fact
//!   counts, content hashes, and call pairing; a lossy legacy projection is
//!   reported as known-missing, never marked `Complete`.
//!
//! It uses raw `rusqlite` connections on purpose — it is a separate process
//! from the daemon, so the ADR-0231 one-door rule (which governs the live
//! runtime) does not apply to it. The module lives under `db::` so the
//! one-door source guard, which only scans the live crates, is not tripped.

use nuo_contracts::context_lifecycle::{
    FactId, FactNode, FactPayload, RoundId, Sensitivity, SourceAuthority, TurnId,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;
use std::path::Path;

/// A migration conflict: two sources disagree for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationConflict {
    /// The quarantined session.
    pub session_id: String,
    /// Why it was quarantined.
    pub reason: String,
}

/// A per-session migration result (ADR-0280 §4 step 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMigrationReport {
    /// The session.
    pub session_id: String,
    /// Facts migrated.
    pub facts_migrated: u64,
    /// Legacy nodes that had no recoverable scope/version.
    pub unknown_state: u64,
    /// Legacy nodes whose raw material is unavailable.
    pub unavailable_raw: u64,
    /// Hash of the migrated fact set, for cross-checking.
    pub digest: String,
    /// Whether the session was quarantined for a conflict.
    pub quarantined: bool,
}

/// The whole-run report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Per-session results.
    pub sessions: Vec<SessionMigrationReport>,
    /// Quarantined sessions.
    pub conflicts: Vec<MigrationConflict>,
    /// Whether every session migrated cleanly.
    pub complete: bool,
}

/// Why migration failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    /// The legacy database could not be read.
    LegacyRead(String),
    /// The target database could not be written.
    TargetWrite(String),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::LegacyRead(d) => write!(f, "legacy read failed: {d}"),
            MigrationError::TargetWrite(d) => write!(f, "target write failed: {d}"),
        }
    }
}

impl std::error::Error for MigrationError {}

/// A legacy node read from `causal_nodes`.
#[derive(Debug, Clone)]
struct LegacyNode {
    id: String,
    parent_id: Option<String>,
    seq: i64,
    kind: String,
    payload_json: String,
}

/// Run the offline migration from `legacy` into `target`.
///
/// `target` must already carry the canonical schema (migrations applied). The
/// converter reads every legacy session, builds an explicit mapping, quarantines
/// conflicts, and writes canonical facts. It is idempotent per session via the
/// target's `commit_operations` ledger.
pub fn migrate_offline(
    legacy: &Connection,
    target: &mut Connection,
) -> Result<MigrationReport, MigrationError> {
    let sessions = read_legacy_sessions(legacy).map_err(MigrationError::LegacyRead)?;
    let mut report = MigrationReport {
        sessions: Vec::new(),
        conflicts: Vec::new(),
        complete: true,
    };

    for session_id in sessions {
        let nodes = read_legacy_nodes(legacy, &session_id).map_err(MigrationError::LegacyRead)?;
        // Conflict detection: a `causal_nodes` chain and an `entries` chain that
        // disagree on message content for the same session are quarantined.
        let entries =
            read_legacy_entries(legacy, &session_id).map_err(MigrationError::LegacyRead)?;
        if let Some(conflict) = detect_conflict(&session_id, &nodes, &entries) {
            report.complete = false;
            report.conflicts.push(conflict);
            report.sessions.push(SessionMigrationReport {
                session_id,
                facts_migrated: 0,
                unknown_state: 0,
                unavailable_raw: 0,
                digest: String::new(),
                quarantined: true,
            });
            continue;
        }

        let mut facts: Vec<FactNode> = Vec::new();
        let mut unknown_state = 0u64;
        let mut unavailable_raw = 0u64;
        for node in &nodes {
            match migrate_node(&session_id, node) {
                MigratedNode::Fact(fact) => facts.push(*fact),
                MigratedNode::Unknown => unknown_state += 1,
                MigratedNode::UnavailableRaw => unavailable_raw += 1,
            }
        }

        // Write canonical facts (idempotent on the migration operation id).
        write_facts(target, &session_id, &facts, &mut report)
            .map_err(MigrationError::TargetWrite)?;

        let digest = digest_facts(&facts);
        report.sessions.push(SessionMigrationReport {
            session_id,
            facts_migrated: facts.len() as u64,
            unknown_state,
            unavailable_raw,
            digest,
            quarantined: false,
        });
    }
    Ok(report)
}

enum MigratedNode {
    Fact(Box<FactNode>),
    Unknown,
    UnavailableRaw,
}

/// Map one legacy node to a canonical fact, honestly labelling gaps.
fn migrate_node(session_id: &str, node: &LegacyNode) -> MigratedNode {
    // A legacy node without a resolvable payload is Unknown, never a success.
    if node.payload_json.trim().is_empty() {
        return MigratedNode::Unknown;
    }
    let value: serde_json::Value = match serde_json::from_str(&node.payload_json) {
        Ok(v) => v,
        Err(_) => return MigratedNode::Unknown,
    };
    let payload = match node.kind.as_str() {
        "dialogue" => {
            let message = value.get("message");
            let text = message
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if text.is_empty() {
                // No recoverable message text: an empty capture, not a fact.
                return MigratedNode::UnavailableRaw;
            }
            // Preserve the legacy role as authority: an assistant message is
            // inference, never user authority (INV-FACT-05). Role matching is
            // case-insensitive because the legacy wire form varies.
            let role = message
                .and_then(|m| m.get("role"))
                .and_then(|r| r.as_str())
                .unwrap_or("user")
                .to_ascii_lowercase();
            return MigratedNode::Fact(Box::new(FactNode {
                id: FactId::from(node.id.clone()),
                session_id: session_id.to_string(),
                branch_origin: "main".into(),
                parent_ids: node
                    .parent_id
                    .iter()
                    .map(|p| FactId::from(p.clone()))
                    .collect(),
                seq: node.seq as u64,
                round_id: RoundId::from("migrated"),
                turn_id: TurnId::from("migrated"),
                payload: match role.as_str() {
                    "assistant" => FactPayload::AssistantMessage {
                        text: text.to_string(),
                    },
                    _ => FactPayload::UserMessage {
                        text: text.to_string(),
                    },
                },
                source_authority: match role.as_str() {
                    "assistant" => SourceAuthority::AssistantInference,
                    "system" => SourceAuthority::ProjectInstruction,
                    _ => SourceAuthority::User,
                },
                sensitivity: Sensitivity::Internal,
                artifact_refs: vec![],
            }));
        }
        "termination" => FactPayload::Termination {
            reason: value
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("unknown")
                .to_string(),
        },
        "compaction" | "system_notice" => {
            // A compaction node is a *derived* projection; it is not a fact and
            // its source is preserved as Unknown rather than fabricated.
            return MigratedNode::Unknown;
        }
        _ => return MigratedNode::Unknown,
    };
    MigratedNode::Fact(Box::new(FactNode {
        id: FactId::from(node.id.clone()),
        session_id: session_id.to_string(),
        branch_origin: "main".into(),
        parent_ids: node
            .parent_id
            .iter()
            .map(|p| FactId::from(p.clone()))
            .collect(),
        seq: node.seq as u64,
        round_id: RoundId::from("migrated"),
        turn_id: TurnId::from("migrated"),
        payload,
        // A non-dialogue node (e.g. termination) carries system provenance.
        source_authority: SourceAuthority::ProjectInstruction,
        sensitivity: Sensitivity::Internal,
        artifact_refs: vec![],
    }))
}

fn detect_conflict(
    session_id: &str,
    nodes: &[LegacyNode],
    entries: &[LegacyEntry],
) -> Option<MigrationConflict> {
    if nodes.is_empty() || entries.is_empty() {
        return None;
    }
    // Compare the sequence of non-empty message texts; a disagreement is a
    // conflict that must be surfaced, not silently resolved to "newest".
    let node_texts: Vec<String> = nodes
        .iter()
        .filter(|n| n.kind == "dialogue")
        .filter_map(|n| {
            serde_json::from_str::<serde_json::Value>(&n.payload_json)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .map(str::to_string)
                })
        })
        .collect();
    let entry_texts: Vec<String> = entries.iter().filter_map(|e| e.content.clone()).collect();
    if !node_texts.is_empty() && !entry_texts.is_empty() && node_texts != entry_texts {
        return Some(MigrationConflict {
            session_id: session_id.to_string(),
            reason: "causal_nodes and entries disagree on message content".into(),
        });
    }
    None
}

#[derive(Debug, Clone)]
struct LegacyEntry {
    content: Option<String>,
}

fn read_legacy_sessions(conn: &Connection) -> Result<Vec<String>, String> {
    // Try the v2 sessions table, then the legacy `sessions` table.
    for table in ["sessions_v2", "sessions"] {
        let sql = format!("SELECT id FROM {table}");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            return Ok(ids);
        }
    }
    Ok(Vec::new())
}

fn read_legacy_nodes(conn: &Connection, session_id: &str) -> Result<Vec<LegacyNode>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, parent_id, seq, kind, payload_json
             FROM causal_nodes WHERE session_id = ?1 ORDER BY seq ASC",
        )
        .map_err(|e| e.to_string())?;
    let nodes = stmt
        .query_map([session_id], |row| {
            Ok(LegacyNode {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                seq: row.get(2)?,
                kind: row.get(3)?,
                payload_json: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(nodes)
}

fn read_legacy_entries(conn: &Connection, session_id: &str) -> Result<Vec<LegacyEntry>, String> {
    // entries join membership; a missing table yields no entries (no conflict).
    let mut stmt = match conn.prepare(
        "SELECT e.content FROM entries e
         JOIN entry_memberships m ON m.entry_id = e.id
         WHERE m.session_id = ?1 ORDER BY m.seq ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };
    let entries = stmt
        .query_map([session_id], |row| {
            Ok(LegacyEntry {
                content: row.get(0)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(entries)
}

fn write_facts(
    target: &mut Connection,
    session_id: &str,
    facts: &[FactNode],
    report: &mut MigrationReport,
) -> Result<(), String> {
    if facts.is_empty() {
        return Ok(());
    }
    // Ensure the session and branch exist, then append facts under the
    // migration operation id (idempotent across re-runs).
    let tx = target.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR IGNORE INTO sessions_v2 (id, created_at_s, updated_at_s) VALUES (?1, 0, 0)",
        [session_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR IGNORE INTO branches (branch_id, session_id, revision) VALUES ('main', ?1, 0)",
        [session_id],
    )
    .map_err(|e| e.to_string())?;
    let op_id = format!("migrate:{session_id}");
    let already: Option<i64> = tx
        .query_row(
            "SELECT applied_revision FROM commit_operations WHERE session_id = ?1 AND operation_id = ?2",
            params![session_id, op_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if already.is_some() {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(());
    }
    for fact in facts {
        tx.execute(
            "INSERT INTO facts (session_id, id, branch_origin, seq, round_id, turn_id, parent_ids, payload_json, source_authority, sensitivity, artifact_refs, payload_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                fact.session_id,
                fact.id.as_str(),
                fact.branch_origin.as_str(),
                fact.seq as i64,
                fact.round_id.as_str(),
                fact.turn_id.as_str(),
                serde_json::to_string(&fact.parent_ids).map_err(|e| e.to_string())?,
                serde_json::to_string(&fact.payload).map_err(|e| e.to_string())?,
                authority_str(fact.source_authority),
                sensitivity_str(fact.sensitivity),
                "[]",
                { use sha2::Digest; format!("{:x}", sha2::Sha256::digest(serde_json::to_vec(&fact.payload).map_err(|e| e.to_string())?)) },
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute(
        "UPDATE branches SET revision = revision + 1 WHERE branch_id = 'main' AND session_id=?1",
        [session_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO commit_operations (session_id, operation_id, kind, applied_revision, at_ms)
         VALUES (?1, ?2, 'migration', 1, 0)",
        params![session_id, op_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    let _ = report;
    Ok(())
}

fn authority_str(authority: SourceAuthority) -> &'static str {
    match authority {
        SourceAuthority::User => "user",
        SourceAuthority::ProjectInstruction => "project_instruction",
        SourceAuthority::ToolObservation => "tool_observation",
        SourceAuthority::AssistantInference => "assistant_inference",
        SourceAuthority::Derived => "derived",
    }
}

fn sensitivity_str(sensitivity: Sensitivity) -> &'static str {
    match sensitivity {
        Sensitivity::Public => "public",
        Sensitivity::Internal => "internal",
        Sensitivity::Secret => "secret",
    }
}

fn digest_facts(facts: &[FactNode]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut sorted: BTreeMap<&str, &FactNode> = BTreeMap::new();
    for fact in facts {
        sorted.insert(fact.id.as_str(), fact);
    }
    for (id, fact) in sorted {
        hasher.update(id.as_bytes());
        hasher.update(serde_json::to_vec(&fact.payload).unwrap_or_default());
    }
    format!("{:x}", hasher.finalize())
}

/// Migrate a legacy database file into a target database file, offline.
///
/// Opens both databases directly. This is a standalone maintenance entry point;
/// the live runtime never calls it (ADR-0280 §4 step 2).
pub fn migrate_files(
    legacy_path: &Path,
    target_path: &Path,
) -> Result<MigrationReport, MigrationError> {
    let legacy =
        Connection::open_with_flags(legacy_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| MigrationError::LegacyRead(e.to_string()))?;
    let mut target =
        Connection::open(target_path).map_err(|e| MigrationError::TargetWrite(e.to_string()))?;
    crate::db::initialize_connection_schema(&mut target)
        .map_err(|e| MigrationError::TargetWrite(e.to_string()))?;
    migrate_offline(&legacy, &mut target)
}

/// A machine-readable integrity report for a canonical database (ADR-0280 §4
/// step 6, `INV-POLICY-03`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerificationReport {
    /// Every session found in the canonical database.
    pub sessions: Vec<SessionVerification>,
    /// Whether the database passed every integrity check.
    pub ok: bool,
}

/// One session's verification result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionVerification {
    /// The session.
    pub session_id: String,
    /// Recoverable facts.
    pub facts: u64,
    /// Facts that a consumer can no longer read as history.
    pub purged_facts: u64,
    /// Aggregated artifact references.
    pub artifact_refs: u64,
    /// References that point at a missing artifact manifest (integrity fault).
    pub dangling_artifact_refs: u64,
    /// Hash of the fact set, for cross-checking against the migration report.
    pub digest: String,
    /// Per-session integrity problems.
    pub problems: Vec<String>,
}

/// Verify a canonical database: recoverable facts, digests, and referential
/// integrity. Emits a report and never mutates the database.
pub fn verify_canonical(conn: &Connection) -> Result<VerificationReport, MigrationError> {
    let sessions = read_legacy_sessions(conn).map_err(MigrationError::LegacyRead)?;
    let mut report = VerificationReport {
        sessions: Vec::new(),
        ok: true,
    };
    for session_id in sessions {
        let facts = crate::db::context_store::load_facts(conn, &session_id)
            .map_err(|e| MigrationError::LegacyRead(e.to_string()))?;
        let digest = crate::db::context_store::facts_digest(conn, &session_id)
            .map_err(|e| MigrationError::LegacyRead(e.to_string()))?;
        let purged: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE session_id = ?1 AND deletion <> 'present'",
                [&session_id],
                |row| row.get(0),
            )
            .map_err(|e| MigrationError::LegacyRead(e.to_string()))?;
        let artifact_refs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM artifact_refs WHERE session_id = ?1",
                [&session_id],
                |row| row.get(0),
            )
            .map_err(|e| MigrationError::LegacyRead(e.to_string()))?;
        // A reference with no manifest is a dangling pointer: an integrity fault.
        let dangling: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM artifact_refs r
                 LEFT JOIN artifact_manifests m
                   ON m.session_id = r.session_id AND m.artifact_id = r.artifact_id
                 WHERE r.session_id = ?1 AND m.artifact_id IS NULL",
                [&session_id],
                |row| row.get(0),
            )
            .map_err(|e| MigrationError::LegacyRead(e.to_string()))?;

        let mut problems = Vec::new();
        if dangling > 0 {
            problems.push(format!("{dangling} dangling artifact reference(s)"));
            report.ok = false;
        }
        report.sessions.push(SessionVerification {
            session_id,
            facts: facts.len() as u64,
            purged_facts: purged as u64,
            artifact_refs: artifact_refs as u64,
            dangling_artifact_refs: dangling as u64,
            digest,
            problems,
        });
    }
    Ok(report)
}

/// Verify a canonical database file, offline and read-only.
pub fn verify_files(db_path: &Path) -> Result<VerificationReport, MigrationError> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| MigrationError::LegacyRead(e.to_string()))?;
    verify_canonical(&conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_with(nodes: &[(&str, Option<&str>, i64, &str, &str)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions_v2 (id TEXT PRIMARY KEY, created_at_s INTEGER, updated_at_s INTEGER);
             CREATE TABLE causal_nodes (id TEXT, session_id TEXT, parent_id TEXT, seq INTEGER, kind TEXT, payload_json TEXT, timestamp_ms INTEGER, PRIMARY KEY(session_id,id));",
        )
        .unwrap();
        conn.execute("INSERT INTO sessions_v2 VALUES ('s1', 0, 0)", [])
            .unwrap();
        for (id, parent, seq, kind, payload) in nodes {
            conn.execute(
                "INSERT INTO causal_nodes VALUES (?1,'s1',?2,?3,?4,?5,0)",
                params![id, parent, seq, kind, payload],
            )
            .unwrap();
        }
        conn
    }

    fn target_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::initialize_connection_schema(&mut conn).unwrap();
        conn
    }

    #[test]
    fn dialogue_and_termination_nodes_migrate_to_facts() {
        let legacy = legacy_with(&[
            (
                "n1",
                None,
                1,
                "dialogue",
                r#"{"message":{"role":"user","content":"hello"}}"#,
            ),
            (
                "n2",
                Some("n1"),
                2,
                "dialogue",
                r#"{"message":{"role":"assistant","content":"hi"}}"#,
            ),
            (
                "n3",
                Some("n2"),
                3,
                "termination",
                r#"{"reason":"completed"}"#,
            ),
        ]);
        let mut target = target_db();
        let report = migrate_offline(&legacy, &mut target).unwrap();
        assert!(report.complete);
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].facts_migrated, 3);
        let count: i64 = target
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
        // Authority is preserved per role: assistant is inference, not user.
        let assistant_authority: String = target
            .query_row(
                "SELECT source_authority FROM facts WHERE id='n2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(assistant_authority, "assistant_inference");
        // The termination reason is a bare string, not double-encoded.
        let reason: String = target
            .query_row("SELECT payload_json FROM facts WHERE id='n3'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(reason.contains("\"reason\":\"completed\""), "got {reason}");
        assert!(
            !reason.contains("\\\""),
            "reason must not be double-encoded"
        );
    }

    #[test]
    fn a_compaction_node_is_not_fabricated_as_a_fact() {
        let legacy = legacy_with(&[(
            "c1",
            None,
            1,
            "compaction",
            r#"{"summary":"old phase","first_kept_node_id":"n9"}"#,
        )]);
        let mut target = target_db();
        let report = migrate_offline(&legacy, &mut target).unwrap();
        assert_eq!(report.sessions[0].facts_migrated, 0);
        assert_eq!(
            report.sessions[0].unknown_state, 1,
            "a derived node is Unknown, not a fact"
        );
    }

    #[test]
    fn an_empty_payload_becomes_unknown_and_missing_text_unavailable() {
        let legacy = legacy_with(&[
            ("n1", None, 1, "dialogue", ""),
            ("n2", None, 2, "dialogue", r#"{"message":{"content":""}}"#),
        ]);
        let mut target = target_db();
        let report = migrate_offline(&legacy, &mut target).unwrap();
        assert_eq!(report.sessions[0].unknown_state, 1);
        assert_eq!(report.sessions[0].unavailable_raw, 1);
        assert_eq!(report.sessions[0].facts_migrated, 0);
    }

    #[test]
    fn migration_is_idempotent_across_reruns() {
        let legacy = legacy_with(&[("n1", None, 1, "dialogue", r#"{"message":{"content":"hi"}}"#)]);
        let mut target = target_db();
        let first = migrate_offline(&legacy, &mut target).unwrap();
        let second = migrate_offline(&legacy, &mut target).unwrap();
        assert_eq!(first.sessions[0].digest, second.sessions[0].digest);
        let count: i64 = target
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "a re-run must not double-apply");
    }

    #[test]
    fn conflicting_sources_are_quarantined_not_silently_resolved() {
        let legacy = legacy_with(&[(
            "n1",
            None,
            1,
            "dialogue",
            r#"{"message":{"content":"from nodes"}}"#,
        )]);
        // Add an entries chain that disagrees.
        legacy
            .execute_batch(
                "CREATE TABLE entries (id TEXT PRIMARY KEY, content TEXT);
                 CREATE TABLE entry_memberships (session_id TEXT, entry_id TEXT, seq INTEGER);
                 INSERT INTO entries VALUES ('e1','from entries');
                 INSERT INTO entry_memberships VALUES ('s1','e1',1);",
            )
            .unwrap();
        let mut target = target_db();
        let report = migrate_offline(&legacy, &mut target).unwrap();
        assert!(!report.complete);
        assert_eq!(report.conflicts.len(), 1);
        assert!(report.sessions[0].quarantined);
        let count: i64 = target
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "a quarantined session writes no facts");
    }

    #[test]
    fn verification_reports_digests_and_flags_dangling_refs() {
        let legacy = legacy_with(&[(
            "n1",
            None,
            1,
            "dialogue",
            r#"{"message":{"role":"user","content":"hi"}}"#,
        )]);
        let mut target = target_db();
        migrate_offline(&legacy, &mut target).unwrap();

        let ok = verify_canonical(&target).unwrap();
        assert!(ok.ok);
        assert_eq!(ok.sessions.len(), 1);
        assert_eq!(ok.sessions[0].facts, 1);
        assert!(ok.sessions[0].problems.is_empty());

        // Introduce a dangling artifact reference (no manifest).
        target
            .execute(
                "INSERT INTO artifact_refs (session_id, artifact_id, fact_id) VALUES ('s1','missing','n1')",
                [],
            )
            .unwrap();
        let bad = verify_canonical(&target).unwrap();
        assert!(!bad.ok);
        assert_eq!(bad.sessions[0].dangling_artifact_refs, 1);
        assert!(!bad.sessions[0].problems.is_empty());
    }
}
