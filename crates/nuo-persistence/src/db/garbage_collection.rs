//! Garbage collection and CAS blob ledger sweeps.
//!
//! Provides zero-reference transcript entry purging and CAS blob reference tracking.
use std::collections::HashSet;

use rusqlite::{Connection, Result};

/// Retrieve all content hashes that are still reachable.
///
/// Liveness is **derived from the tables that can reach the bytes**, not from a
/// denormalized ledger that has to be kept in sync. Two independent owners exist:
///
/// 1. **Transcript entries** — a body may live in the CAS, recorded in the
///    `blob_refs` ledger in the same transaction as the save that introduces it.
/// 2. **Workspace journal rows** (ADR-0125) — `before_hash` / `after_hash` are the
///    pre- and post-images a reversal and a `/diff` read.
///
/// The union is deliberate. Registering journal hashes into `blob_refs` would
/// collide with the transcript's own key `(session_id, hash)`: two owners sharing
/// one row means pruning either one clears the other's liveness, and a transcript
/// that happened to share content with a pre-image would lose its bytes when the
/// journal row expired. Reading the journal directly makes each owner's
/// contribution exactly its live rows — a pruned row stops contributing with no
/// bookkeeping at all, so drift is unrepresentable rather than merely unlikely.
pub(crate) fn live_blob_hashes(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT hash FROM blob_refs
         UNION
         SELECT before_hash FROM workspace_effects
             WHERE before_hash IS NOT NULL AND expired_at_ms IS NULL
         UNION
         SELECT after_hash FROM workspace_effects
             WHERE after_hash IS NOT NULL AND expired_at_ms IS NULL",
    )?;
    let live = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect();
    Ok(live)
}

/// Reclaim transcript entries that no session membership references.
///
/// Entries are global facts shared by identity across forks, so only zero-reference
/// rows are removable; the foreign key from `entry_memberships` makes the two tables'
/// agreement structural, and entry insert + membership insert share one transaction,
/// so a GC pass can never observe the half of a pair. Returns the rows reclaimed.
pub(crate) fn collect_entry_garbage(conn: &Connection) -> Result<usize> {
    let reclaimed = conn.execute(
        "DELETE FROM entries WHERE NOT EXISTS (
            SELECT 1 FROM entry_memberships m WHERE m.entry_id = entries.id
        )",
        [],
    )?;
    Ok(reclaimed)
}
