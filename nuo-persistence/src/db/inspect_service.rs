//! Scoped inspect retrieval, retention, and deletion (ADR-0279).
//!
//! Phase E substrate: an indexed, paginated, scoped retrieval service over the
//! canonical fact/artifact substrate, plus bounded garbage collection and a
//! recoverable, observable deletion path. It lives inside `nuo_persistence::db`
//! because it reads through a `Connection` (ADR-0231 one-door).
//!
//! Realized invariants:
//! - `INV-RET-01`: retrieval is authorized by session/branch; a handle confers
//!   no authority.
//! - `INV-RET-02`: reads are paged and bounded; a cursor binds content hash,
//!   query, and revision, and a changed query cannot reuse a cursor.
//! - `INV-RET-03`: deletion commits state first, then reclaims; it is
//!   observable and resumable across a crash.
//! - `INV-RET-04`: GC uses bounded batches and read leases; a leased artifact is
//!   never concurrently collected.
//! - `INV-RET-05`: deleting a requirement fact cascades to open task state.

use nuo_contracts::context_lifecycle::{
    AuthScope, Capture, CursorBinding, Deletion, InspectError, InspectStatus, PageLimits,
    Representation, Validity,
};
use rusqlite::{Connection, OptionalExtension, params};

/// A page of retrieved content (ADR-0279 §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPage {
    /// The returned text (already truncated to the page limits).
    pub content: String,
    /// Separated status fields, never collapsed.
    pub status: InspectStatus,
    /// The cursor to continue, when more content remains.
    pub next_cursor: Option<String>,
    /// Tokens returned.
    pub tokens: u64,
    /// Bytes returned.
    pub bytes: u64,
}

/// Why a GC batch stopped (ADR-0279 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionProgress {
    /// Objects collected in this batch.
    pub collected: u32,
    /// Whether a continuation remains (more work after the batch bound).
    pub more: bool,
    /// The next sequence to resume from.
    pub next_seq: u64,
}

/// The outcome of a deletion job (ADR-0279 §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionReport {
    /// The job identity.
    pub job_id: String,
    /// Target kind (`fact`/`artifact`).
    pub target_kind: String,
    /// Target identity.
    pub target_id: String,
    /// Derived records invalidated (summaries, indexes, request copies).
    pub derived_invalidated: u32,
    /// Whether task state was cascaded (a requirement deletion under an open
    /// task).
    pub task_cascaded: bool,
    /// Whether content reclamation completed in this call.
    pub reclaimed: bool,
}

/// Indexed, scoped inspect retrieval over the canonical substrate (ADR-0279 §1).
pub struct InspectService<'a> {
    conn: &'a Connection,
}

impl<'a> InspectService<'a> {
    /// Bind to an open connection.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Read a fact page under an authorization scope.
    ///
    /// The handle is an address; authority comes from `auth`. A fact whose
    /// session/branch is not covered is denied, and a deleted fact reports
    /// `Purged`/`DeletePending` rather than an empty success.
    pub fn read_fact(
        &self,
        auth: &AuthScope,
        fact_id: &str,
        query: Option<&str>,
        cursor: Option<&CursorBinding>,
        limits: PageLimits,
    ) -> Result<ArtifactPage, InspectError> {
        let started = std::time::Instant::now();
        if limits.bytes == 0 || limits.tokens == 0 || limits.compute_ms == 0 {
            return Err(InspectError::BudgetExceeded);
        }
        // Fetch only fixed metadata before authorization; never hydrate a log
        // to answer a page request. The fact hash was computed at publication.
        let row: Option<(String, String, String, u64, u64)> = self.conn.query_row(
            "SELECT branch_origin, deletion, payload_hash, seq, length(CAST(payload_json AS BLOB))
             FROM facts WHERE session_id=?1 AND id=?2",
            params![auth.session_id, fact_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional().map_err(|_| InspectError::Corrupt)?;
        let (branch, deletion, content_hash, seq, length) = row.ok_or(InspectError::NotFound)?;
        if !auth.covers_branch(&branch.into()) { return Err(InspectError::NotAuthorized); }
        match deletion.as_str() {
            "purged" => return Err(InspectError::Purged),
            "delete_pending" => return Err(InspectError::Expired),
            "present" => {},
            _ => return Err(InspectError::Corrupt),
        }
        if content_hash.len() != 64 { return Err(InspectError::Corrupt); }
        let start = cursor.map(|c| c.offset).unwrap_or(0);
        if start > length || cursor.is_some_and(|c| !c.matches(&content_hash, query.unwrap_or(""), seq)) {
            return Err(InspectError::CursorMismatch);
        }
        let ceiling = limits.bytes.min(64 * 1024);
        let bytes: Vec<u8> = self.conn.query_row(
            "SELECT substr(CAST(payload_json AS BLOB), ?3, ?4) FROM facts
             WHERE session_id=?1 AND id=?2 AND deletion='present'",
            params![auth.session_id, fact_id, start.saturating_add(1), ceiling], |r| r.get(0),
        ).optional().map_err(|_| InspectError::Corrupt)?.ok_or(InspectError::Purged)?;
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(e) if e.error_len().is_none() => std::str::from_utf8(&bytes[..e.valid_up_to()]).map_err(|_| InspectError::Corrupt)?,
            Err(_) => return Err(InspectError::CursorMismatch),
        };
        let content = nuo_contracts::tokenizer::truncate_str_to_tokens(text, limits.tokens.min(8192) as usize).to_owned();
        let count = content.len() as u64;
        if count == 0 && start < length { return Err(InspectError::BudgetExceeded); }
        if started.elapsed().as_millis() >= limits.compute_ms as u128 { return Err(InspectError::BudgetExceeded); }
        // Recheck deletion at delivery, including changes made through another
        // connection while this bounded page was decoded.
        let readable: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM facts WHERE session_id=?1 AND id=?2 AND deletion='present')",
            params![auth.session_id, fact_id], |r| r.get(0),
        ).map_err(|_| InspectError::Corrupt)?;
        if !readable { return Err(InspectError::Purged); }
        Ok(ArtifactPage {
            tokens: nuo_contracts::tokenizer::count_tokens(&content) as u64,
            bytes: count,
            content,
            next_cursor: (start + count < length).then(|| serialize_cursor(&CursorBinding {
                content_hash, query: query.unwrap_or("").into(), revision: seq, offset: start + count,
            })),
            status: InspectStatus {
                deletion: Deletion::Present, capture: Capture::Complete,
                validity: Validity::Unknown, representation: Representation::Excerpt,
            },
        })
    }
}

#[cfg(test)]
fn fact_content_hash(payload: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(payload.as_bytes()))
}

fn serialize_cursor(cursor: &CursorBinding) -> String {
    serde_json::to_string(cursor).unwrap_or_default()
}

/// Grant a read lease on an artifact, protecting it from GC (`INV-RET-04`).
pub fn grant_read_lease(
    conn: &Connection,
    session_id: &str,
    lease_id: &str,
    artifact_id: Option<&str>,
    fact_id: Option<&str>,
    expires_at_ms: u64,
) -> Result<(), InspectError> {
    conn.execute(
        "INSERT OR REPLACE INTO read_leases
            (session_id, lease_id, artifact_id, fact_id, expires_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session_id, lease_id, artifact_id, fact_id, expires_at_ms],
    )
    .map_err(|_| InspectError::Corrupt)?;
    Ok(())
}

/// A bounded mark/sweep batch over orphaned artifact references (ADR-0279 §5).
///
/// Collects at most `batch_objects` rows or until `batch_ms` elapsed, saving a
/// continuation. An artifact referenced by a live read lease is never swept.
pub fn collect_batch(
    conn: &Connection,
    session_id: &str,
    now_ms: u64,
    batch_objects: u32,
    batch_ms: u64,
) -> Result<CollectionProgress, InspectError> {
    let start = std::time::Instant::now();
    let last_seq: i64 = conn
        .query_row(
            "SELECT last_seq FROM gc_continuations WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| InspectError::Corrupt)?
        .unwrap_or(0);

    // Candidates: artifact manifests whose deletion is pending/purged, or whose
    // only references are gone, and which no live lease protects.
    let mut stmt = conn
        .prepare(
            "SELECT artifact_id, deletion FROM artifact_manifests
             WHERE session_id = ?1 AND deletion <> 'present'
             ORDER BY artifact_id ASC",
        )
        .map_err(|_| InspectError::Corrupt)?;
    let rows: Vec<(String, String)> = stmt
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|_| InspectError::Corrupt)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| InspectError::Corrupt)?;

    let mut collected = 0u32;
    for (artifact_id, _) in rows {
        if collected >= batch_objects || start.elapsed().as_millis() as u64 >= batch_ms {
            break;
        }
        if has_live_lease(conn, session_id, &artifact_id, now_ms)? {
            continue; // INV-RET-04: never collect a leased artifact.
        }
        let refs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM artifact_refs WHERE session_id = ?1 AND artifact_id = ?2",
                params![session_id, artifact_id],
                |row| row.get(0),
            )
            .map_err(|_| InspectError::Corrupt)?;
        if refs == 0 {
            conn.execute(
                "DELETE FROM artifact_manifests WHERE session_id = ?1 AND artifact_id = ?2",
                params![session_id, artifact_id],
            )
            .map_err(|_| InspectError::Corrupt)?;
            collected += 1;
        }
    }
    let more = collected >= batch_objects;
    let next_seq = last_seq + collected as i64;
    conn.execute(
        "INSERT INTO gc_continuations (session_id, last_seq, updated_at_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET last_seq = excluded.last_seq, updated_at_ms = excluded.updated_at_ms",
        params![session_id, next_seq, now_ms],
    )
    .map_err(|_| InspectError::Corrupt)?;
    Ok(CollectionProgress {
        collected,
        more,
        next_seq: next_seq as u64,
    })
}

fn has_live_lease(
    conn: &Connection,
    session_id: &str,
    artifact_id: &str,
    now_ms: u64,
) -> Result<bool, InspectError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM read_leases
             WHERE session_id = ?1 AND artifact_id = ?2 AND expires_at_ms > ?3",
            params![session_id, artifact_id, now_ms],
            |row| row.get(0),
        )
        .map_err(|_| InspectError::Corrupt)?;
    Ok(count > 0)
}

/// Commit a deletion job's state before reclaiming (ADR-0279 §6, `INV-RET-03`).
///
/// The job is observable (a row exists) and resumable (state is durable), and a
/// requirement deletion cascades to open task state (`INV-RET-05`).
pub fn begin_deletion(
    conn: &Connection,
    session_id: &str,
    job_id: &str,
    target_kind: &str,
    target_id: &str,
    scope: &str,
    now_ms: u64,
) -> Result<(), InspectError> {
    let tx = conn.unchecked_transaction().map_err(|_| InspectError::Corrupt)?;
    let existing: Option<(String, String, String)> = tx.query_row(
        "SELECT target_kind,target_id,scope FROM deletion_jobs WHERE session_id=?1 AND job_id=?2",
        params![session_id,job_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional().map_err(|_| InspectError::Corrupt)?;
    if let Some(existing) = existing {
        return if existing == (target_kind.into(),target_id.into(),scope.into()) { Ok(()) } else { Err(InspectError::Corrupt) };
    }
    let sql = match target_kind {
        "fact" => "UPDATE facts SET deletion='delete_pending' WHERE session_id=?1 AND id=?2 AND deletion='present'",
        "artifact" => "UPDATE artifact_manifests SET deletion='delete_pending' WHERE session_id=?1 AND artifact_id=?2 AND deletion='present'",
        _ => return Err(InspectError::NotFound),
    };
    tx.execute(sql, params![session_id,target_id]).map_err(|_| InspectError::Corrupt)?;
    tx.execute(
        "INSERT INTO deletion_jobs
            (session_id, job_id, target_kind, target_id, scope, state, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
        params![session_id, job_id, target_kind, target_id, scope, now_ms],
    ).map_err(|_| InspectError::Corrupt)?;
    tx.commit().map_err(|_| InspectError::Corrupt)
}

/// Execute a committed deletion job: mark facts purged, cascade task state, and
/// report derived invalidations. Resumable and idempotent.
pub fn execute_deletion(
    conn: &Connection,
    session_id: &str,
    job_id: &str,
    now_ms: u64,
) -> Result<DeletionReport, InspectError> {
    let tx = conn.unchecked_transaction().map_err(|_| InspectError::Corrupt)?;
    let conn = &tx;
    let (target_kind, target_id): (String, String) = conn
        .query_row(
            "SELECT target_kind, target_id FROM deletion_jobs
             WHERE session_id = ?1 AND job_id = ?2",
            params![session_id, job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| InspectError::Corrupt)?
        .ok_or(InspectError::NotFound)?;

    conn.execute(
        "UPDATE deletion_jobs SET state = 'reclaiming', updated_at_ms = ?3
         WHERE session_id = ?1 AND job_id = ?2",
        params![session_id, job_id, now_ms],
    )
    .map_err(|_| InspectError::Corrupt)?;

    let mut task_cascaded = false;
    let derived_invalidated: u32;

    if target_kind == "fact" {
        // Purge the payload (the one sanctioned replacement) and record a
        // tombstone; a raw DELETE is forbidden by the schema trigger.
        let parents: Option<String> = conn
            .query_row(
                "SELECT parent_ids FROM facts WHERE session_id = ?1 AND id = ?2",
                params![session_id, target_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| InspectError::Corrupt)?;
        if let Some(parents) = parents {
            conn.execute(
                "UPDATE facts SET payload_json = '{\"termination\":{\"reason\":\"purged\"}}',
                                  artifact_refs = '[]', deletion = 'purged'
                 WHERE session_id = ?1 AND id = ?2",
                params![session_id, target_id],
            )
            .map_err(|_| InspectError::Corrupt)?;
            conn.execute(
                "INSERT OR REPLACE INTO tombstones (session_id, fact_id, parent_ids, purged_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![session_id, target_id, parents, now_ms],
            )
            .map_err(|_| InspectError::Corrupt)?;
        }

        // INV-RET-05: cascade to open task state that references this fact.
        let cascaded = conn
            .execute(
                "UPDATE task_revisions SET open = 0
                 WHERE session_id = ?1 AND objective_fact_id = ?2 AND open = 1",
                params![session_id, target_id],
            )
            .map_err(|_| InspectError::Corrupt)?;
        task_cascaded = cascaded > 0;
        // Derived summaries that reference the purged fact are invalidated.
        derived_invalidated = conn
            .execute(
                "DELETE FROM checkpoints
                 WHERE session_id = ?1 AND mandatory_fact_refs LIKE '%' || ?2 || '%'",
                params![session_id, target_id],
            )
            .map_err(|_| InspectError::Corrupt)? as u32;
    } else {
        // Artifact deletion: mark purged and drop its references so GC reclaims.
        conn.execute(
            "UPDATE artifact_manifests SET deletion = 'purged'
             WHERE session_id = ?1 AND artifact_id = ?2",
            params![session_id, target_id],
        )
        .map_err(|_| InspectError::Corrupt)?;
        derived_invalidated = conn
            .execute(
                "DELETE FROM artifact_refs WHERE session_id = ?1 AND artifact_id = ?2",
                params![session_id, target_id],
            )
            .map_err(|_| InspectError::Corrupt)? as u32;
    }

    conn.execute(
        "UPDATE deletion_jobs SET state = 'done', updated_at_ms = ?3
         WHERE session_id = ?1 AND job_id = ?2",
        params![session_id, job_id, now_ms],
    )
    .map_err(|_| InspectError::Corrupt)?;

    tx.commit().map_err(|_| InspectError::Corrupt)?;
    Ok(DeletionReport {
        job_id: job_id.to_string(),
        target_kind,
        target_id,
        derived_invalidated,
        task_cascaded,
        reclaimed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::initialize_in_memory_db;

    fn seed(conn: &Connection) {
        conn.execute(
            "INSERT INTO sessions_v2 (id, created_at_s, updated_at_s) VALUES ('s1', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO branches (branch_id, session_id, revision) VALUES ('main', 's1', 1)",
            [],
        )
        .unwrap();
    }

    fn insert_fact(conn: &Connection, id: &str, seq: i64, text: &str) {
        conn.execute(
            "INSERT INTO facts (session_id, id, branch_origin, seq, round_id, turn_id, payload_json, source_authority, sensitivity, payload_hash)
             VALUES ('s1', ?1, 'main', ?2, 'r1', 't1', ?3, 'user', 'internal', ?4)",
            params![id, seq, format!("{{\"user_message\":{{\"text\":\"{text}\"}}}}"), fact_content_hash(&format!("{{\"user_message\":{{\"text\":\"{text}\"}}}}"))],
        )
        .unwrap();
    }

    fn auth() -> AuthScope {
        AuthScope {
            session_id: "s1".into(),
            branches: vec!["main".into()],
        }
    }

    fn limits() -> PageLimits {
        PageLimits {
            tokens: 2048,
            bytes: 4096,
            compute_ms: 1000,
        }
    }

    #[test]
    fn deletion_revokes_reads_before_collection_and_rejects_job_rebinding() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn); insert_fact(&conn, "f1", 1, "retained constraint");
        begin_deletion(&conn, "s1", "job", "fact", "f1", "session", 1).unwrap();
        assert!(matches!(InspectService::new(&conn).read_fact(&auth(), "f1", None, None, limits()), Err(InspectError::Expired)));
        assert!(begin_deletion(&conn, "s1", "job", "fact", "f2", "session", 2).is_err());
    }

    #[test]
    fn unicode_pages_obey_both_token_and_byte_limits() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn); insert_fact(&conn, "f1", 1, &"日本語😀".repeat(100));
        let service = InspectService::new(&conn);
        let mut cursor = None;
        let mut restored = String::new();
        loop {
            let page = service.read_fact(&auth(), "f1", None, cursor.as_ref(), PageLimits { tokens: 8, bytes: 17, compute_ms: 1000 }).unwrap();
            assert!(page.tokens <= 8); assert!(page.bytes <= 17);
            restored.push_str(&page.content);
            match page.next_cursor { Some(next) => cursor = Some(serde_json::from_str::<CursorBinding>(&next).unwrap()), None => break }
        }
        assert_eq!(restored, format!("{{\"user_message\":{{\"text\":\"{}\"}}}}", "日本語😀".repeat(100)));
    }

    #[test]
    fn a_foreign_scope_cannot_read_a_handle() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn);
        insert_fact(&conn, "f1", 1, "hello");
        let svc = InspectService::new(&conn);
        let stranger = AuthScope {
            session_id: "s2".into(),
            branches: vec!["main".into()],
        };
        assert_eq!(
            svc.read_fact(&stranger, "f1", None, None, limits())
                .unwrap_err(),
            InspectError::NotFound
        );
        let other_branch = AuthScope {
            session_id: "s1".into(),
            branches: vec!["feature".into()],
        };
        assert_eq!(
            svc.read_fact(&other_branch, "f1", None, None, limits())
                .unwrap_err(),
            InspectError::NotAuthorized
        );
    }

    #[test]
    fn a_missing_fact_is_not_found_not_empty() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn);
        let svc = InspectService::new(&conn);
        assert_eq!(
            svc.read_fact(&auth(), "ghost", None, None, limits())
                .unwrap_err(),
            InspectError::NotFound
        );
    }

    #[test]
    fn a_changed_query_cannot_reuse_a_cursor() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn);
        insert_fact(&conn, "f1", 1, &"x".repeat(20_000));
        let svc = InspectService::new(&conn);
        // First page returns a cursor.
        let _page = svc
            .read_fact(&auth(), "f1", Some("a"), None, limits())
            .unwrap();
        let cursor = CursorBinding {
            content_hash: fact_content_hash(&format!(
                "{{\"user_message\":{{\"text\":\"{}\"}}}}",
                "x".repeat(20_000)
            )),
            query: "a".into(),
            revision: 1,
            offset: 0,
        };
        // Same content/query/revision is fine.
        assert!(
            svc.read_fact(&auth(), "f1", Some("a"), Some(&cursor), limits())
                .is_ok()
        );
        // A different query is refused.
        assert_eq!(
            svc.read_fact(&auth(), "f1", Some("b"), Some(&cursor), limits())
                .unwrap_err(),
            InspectError::CursorMismatch
        );
    }

    #[test]
    fn reads_are_bounded_by_the_byte_ceiling() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn);
        insert_fact(&conn, "f1", 1, &"y".repeat(10_000));
        let svc = InspectService::new(&conn);
        let page = svc
            .read_fact(
                &auth(),
                "f1",
                None,
                None,
                PageLimits {
                    tokens: 2048,
                    bytes: 100,
                    compute_ms: 1000,
                },
            )
            .unwrap();
        assert!(page.bytes <= 100);
        assert!(page.next_cursor.is_some(), "more content remains");
    }

    #[test]
    fn a_purged_fact_reports_purged_not_empty() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn);
        insert_fact(&conn, "f1", 1, "secret");
        conn.execute("UPDATE facts SET deletion = 'purged' WHERE id = 'f1'", [])
            .unwrap();
        let svc = InspectService::new(&conn);
        assert_eq!(
            svc.read_fact(&auth(), "f1", None, None, limits())
                .unwrap_err(),
            InspectError::Purged
        );
    }

    #[test]
    fn gc_never_collects_a_leased_artifact() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn);
        conn.execute(
            "INSERT INTO artifact_manifests
                (session_id, artifact_id, content_hash, media_type, stream, capture, byte_count, retention, sensitivity, deletion, created_at_ms)
             VALUES ('s1', 'a1', 'h', 'text/plain', 'stdout', 'complete', 10, 'session_bound', 'internal', 'purged', 0)",
            [],
        )
        .unwrap();
        // A live lease protects it.
        grant_read_lease(&conn, "s1", "lease-1", Some("a1"), None, 10_000).unwrap();
        let progress = collect_batch(&conn, "s1", 5_000, 1000, 100).unwrap();
        assert_eq!(progress.collected, 0, "a leased artifact must not be swept");
        let still_there: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM artifact_manifests WHERE artifact_id = 'a1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_there, 1);

        // After the lease expires, it is collectible.
        let progress = collect_batch(&conn, "s1", 20_000, 1000, 100).unwrap();
        assert_eq!(progress.collected, 1);
    }

    #[test]
    fn gc_is_bounded_by_the_batch_object_ceiling() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn);
        for i in 0..10 {
            conn.execute(
                "INSERT INTO artifact_manifests
                    (session_id, artifact_id, content_hash, media_type, stream, capture, byte_count, retention, sensitivity, deletion, created_at_ms)
                 VALUES ('s1', ?1, 'h', 'text/plain', 'stdout', 'complete', 10, 'session_bound', 'internal', 'purged', 0)",
                [format!("a{i}")],
            )
            .unwrap();
        }
        let progress = collect_batch(&conn, "s1", 1_000, 3, 100_000).unwrap();
        assert_eq!(progress.collected, 3);
        assert!(progress.more, "a continuation remains");
    }

    #[test]
    fn deletion_is_observable_and_cascades_task_state() {
        let conn = initialize_in_memory_db().unwrap();
        seed(&conn);
        insert_fact(&conn, "f1", 1, "requirement");
        conn.execute(
            "INSERT INTO task_revisions
                (session_id, task_id, revision, objective_fact_id, requirements, open_items, open, created_at_ms)
             VALUES ('s1', 't1', 1, 'f1', '[]', '[]', 1, 0)",
            [],
        )
        .unwrap();
        begin_deletion(&conn, "s1", "job-1", "fact", "f1", "session", 100).unwrap();
        // Observable: the job row exists before reclamation.
        let state: String = conn
            .query_row(
                "SELECT state FROM deletion_jobs WHERE job_id = 'job-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "pending");

        let report = execute_deletion(&conn, "s1", "job-1", 200).unwrap();
        assert!(
            report.task_cascaded,
            "an open task referencing the fact is closed"
        );
        assert!(report.reclaimed);

        // The fact is purged and a tombstone exists.
        let deletion: String = conn
            .query_row("SELECT deletion FROM facts WHERE id = 'f1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(deletion, "purged");
        let tomb: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tombstones WHERE fact_id = 'f1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tomb, 1);
        // The task is closed.
        let open: i64 = conn
            .query_row(
                "SELECT open FROM task_revisions WHERE task_id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 0);
    }
}
