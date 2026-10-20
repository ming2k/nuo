//! SQLite FTS5 full-text transcript search implementation.
//!
//! Provides BM25 full-text search across transcript entries with workspace filtering
//! and relaxed recall fallback.
use rusqlite::{Connection, Result, params};

use super::{HistorySearchResult, map_search_row, sanitize_fts_query_joined};

/// Perform BM25 full-text search across transcript entries, optionally
/// filtered by workspace root. Hits join the owning session's title and
/// `updated_at` so a hit is presentable and rankable without a second
/// round-trip.
pub(crate) fn search_history(
    conn: &Connection,
    query: &str,
    filter: Option<&nuo_wire::WorkspaceFilter>,
    limit: usize,
) -> Result<Vec<HistorySearchResult>> {
    search_history_inner(conn, query, filter, limit, false)
}

/// Perform relaxed BM25 full-text search: the sanitized words are joined with OR instead of AND.
pub(crate) fn search_history_relaxed(
    conn: &Connection,
    query: &str,
    filter: Option<&nuo_wire::WorkspaceFilter>,
    limit: usize,
) -> Result<Vec<HistorySearchResult>> {
    search_history_inner(conn, query, filter, limit, true)
}

fn search_history_inner(
    conn: &Connection,
    query: &str,
    filter: Option<&nuo_wire::WorkspaceFilter>,
    limit: usize,
    match_any: bool,
) -> Result<Vec<HistorySearchResult>> {
    let clean_query = sanitize_fts_query_joined(query, match_any);
    if clean_query.is_empty() {
        return Ok(Vec::new());
    }

    let cols = "f.entry_id, f.session_id, s.workspace_root, s.title, f.role, \
                snippet(fts_entries, 3, '<b>', '</b>', '...', 16) AS snippet, \
                bm25(fts_entries) AS score";
    let mut results = Vec::new();
    match filter {
        Some(nuo_wire::WorkspaceFilter::Path(path)) => {
            let sql = format!(
                "SELECT {cols} FROM fts_entries f JOIN thread_list_view s ON f.session_id = s.id \
                 WHERE fts_entries MATCH ?1 AND s.workspace_root = ?2 ORDER BY score ASC LIMIT ?3;"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![clean_query, path.to_string_lossy(), limit as i64],
                map_search_row,
            )?;
            for item in rows {
                results.push(item?);
            }
        }
        Some(nuo_wire::WorkspaceFilter::Unbound) => {
            let sql = format!(
                "SELECT {cols} FROM fts_entries f JOIN thread_list_view s ON f.session_id = s.id \
                 WHERE fts_entries MATCH ?1 AND (s.workspace_root IS NULL OR s.workspace_root = '') ORDER BY score ASC LIMIT ?2;"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![clean_query, limit as i64], map_search_row)?;
            for item in rows {
                results.push(item?);
            }
        }
        _ => {
            let sql = format!(
                "SELECT {cols} FROM fts_entries f JOIN thread_list_view s ON f.session_id = s.id \
                 WHERE fts_entries MATCH ?1 ORDER BY score ASC LIMIT ?2;"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![clean_query, limit as i64], map_search_row)?;
            for item in rows {
                results.push(item?);
            }
        }
    }

    Ok(results)
}
