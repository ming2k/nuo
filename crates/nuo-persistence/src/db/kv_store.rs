//! Key-value storage implementation against SQLite `kv_store`.
//!
//! Provides flat string KV, prefix search, and typed JSON serialization/deserialization.
use rusqlite::{Connection, OptionalExtension, Result, params};
use serde::Deserialize;

/// Set (or overwrite) a key in the unified KV store.
pub(crate) fn set_kv(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO kv_store (key, value, updated_at) VALUES (?1, ?2, strftime('%s','now')) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )?
    .execute(params![key, value])?;
    Ok(())
}

/// Fetch a key from the unified KV store.
pub(crate) fn get_kv(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.prepare_cached("SELECT value FROM kv_store WHERE key = ?1")?
        .query_row(params![key], |row| row.get(0))
        .optional()
}

/// Delete a key from the unified KV store.
pub(crate) fn delete_kv(conn: &Connection, key: &str) -> Result<bool> {
    let affected = conn
        .prepare_cached("DELETE FROM kv_store WHERE key = ?1")?
        .execute(params![key])?;
    Ok(affected > 0)
}

/// List keys with a given prefix, ordered descending.
pub(crate) fn list_kv_keys_with_prefix(conn: &Connection, prefix: &str) -> Result<Vec<String>> {
    let pattern = format!("{prefix}%");
    let mut stmt =
        conn.prepare_cached("SELECT key FROM kv_store WHERE key LIKE ?1 ORDER BY key DESC")?;
    let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
    let mut keys = Vec::new();
    for k in rows {
        keys.push(k?);
    }
    Ok(keys)
}

/// Retrieve and deserialize a JSON value from `kv_store`.
pub(crate) fn get_json<T: for<'de> Deserialize<'de>>(
    conn: &Connection,
    key: &str,
) -> Result<Option<T>> {
    if let Some(raw_json) = get_kv(conn, key)? {
        match serde_json::from_str::<T>(&raw_json) {
            Ok(val) => Ok(Some(val)),
            Err(err) => {
                tracing::warn!(key = %key, error = %err, "Failed to deserialize JSON from kv_store");
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}
