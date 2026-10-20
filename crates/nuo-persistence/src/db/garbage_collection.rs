//! Garbage collection and CAS blob ledger sweeps.
//!
//! Provides zero-reference transcript entry purging and CAS blob reference tracking.
use std::collections::HashSet;

use rusqlite::{Connection, Result};

/// Retrieve all content hashes currently referenced in the durable blob ledger.
pub(crate) fn live_blob_hashes(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT hash FROM blob_refs")?;
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
