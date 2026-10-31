//! Prompt input history storage implementation.
//!
//! Tracks terminal prompt interactions with deduplication, bounding against `HISTORY_CAP`.
use rusqlite::{Connection, Result, params};

/// Record a prompt into `input_history`, respecting `dedup` and the global `HISTORY_CAP`.
pub(crate) fn record_input_history(
    conn: &Connection,
    entry: &nuo_protocol::HistoryEntry,
    dedup: bool,
) -> Result<()> {
    conn.execute("BEGIN IMMEDIATE", [])?;
    let res = (|| -> Result<()> {
        if dedup {
            conn.execute(
                "DELETE FROM input_history WHERE text = ?1",
                params![entry.text],
            )?;
        } else if let Some(session_id) = &entry.session_id {
            let latest_same: bool = conn
                .query_row(
                    "SELECT text = ?1 FROM input_history WHERE session_id = ?2 ORDER BY created_at_ms DESC, id DESC LIMIT 1",
                    params![entry.text, session_id],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if latest_same {
                return Ok(());
            }
        }

        conn.execute(
            r#"
            INSERT INTO input_history (text, session_id, workspace, created_at_ms)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![
                entry.text,
                entry.session_id,
                entry.workspace,
                entry.created_at_ms as i64,
            ],
        )?;

        conn.execute(
            r#"
            DELETE FROM input_history WHERE id NOT IN (
                SELECT id FROM input_history ORDER BY created_at_ms DESC, id DESC LIMIT ?1
            )
            "#,
            params![nuo_protocol::HISTORY_CAP as i64],
        )?;

        Ok(())
    })();

    match res {
        Ok(()) => {
            conn.execute("COMMIT", [])?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

/// Load the newest prompt history entries up to `limit`.
pub(crate) fn load_input_history(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<nuo_protocol::HistoryEntry>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT text, session_id, workspace, created_at_ms
        FROM input_history
        ORDER BY created_at_ms DESC, id DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        let text: String = row.get(0)?;
        let session_id: Option<String> = row.get(1)?;
        let workspace: Option<String> = row.get(2)?;
        let created_at_ms: i64 = row.get(3)?;
        Ok(nuo_protocol::HistoryEntry {
            text,
            session_id,
            workspace,
            created_at_ms: created_at_ms as u64,
        })
    })?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// Persist or batch-merge a list of history entries into SQLite.
pub(crate) fn save_input_history(
    conn: &Connection,
    entries: &[nuo_protocol::HistoryEntry],
    dedup: bool,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    if entries.len() == 1 {
        return record_input_history(conn, &entries[0], dedup);
    }

    conn.execute("BEGIN IMMEDIATE", [])?;
    let res = (|| -> Result<()> {
        let mut insert_stmt = conn.prepare(
            r#"
            INSERT INTO input_history (text, session_id, workspace, created_at_ms)
            VALUES (?1, ?2, ?3, ?4)
            "#,
        )?;

        let mut delete_dedup_stmt = if dedup {
            Some(conn.prepare("DELETE FROM input_history WHERE text = ?1")?)
        } else {
            None
        };

        for entry in entries {
            if let Some(del_stmt) = &mut delete_dedup_stmt {
                del_stmt.execute(params![entry.text])?;
            }
            insert_stmt.execute(params![
                entry.text,
                entry.session_id,
                entry.workspace,
                entry.created_at_ms as i64,
            ])?;
        }

        conn.execute(
            r#"
            DELETE FROM input_history WHERE id NOT IN (
                SELECT id FROM input_history ORDER BY created_at_ms DESC, id DESC LIMIT ?1
            )
            "#,
            params![nuo_protocol::HISTORY_CAP as i64],
        )?;

        Ok(())
    })();

    match res {
        Ok(()) => {
            conn.execute("COMMIT", [])?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

/// Delete all prompt history records.
pub(crate) fn clear_input_history(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM input_history", [])?;
    Ok(())
}

/// Delete a specific prompt history record by text and timestamp.
pub(crate) fn delete_input_history_entry(
    conn: &Connection,
    text: &str,
    created_at_ms: u64,
) -> Result<usize> {
    let deleted = if created_at_ms > 0 {
        conn.execute(
            "DELETE FROM input_history WHERE text = ?1 AND created_at_ms = ?2",
            params![text, created_at_ms as i64],
        )?
    } else {
        conn.execute("DELETE FROM input_history WHERE text = ?1", params![text])?
    };
    Ok(deleted)
}
