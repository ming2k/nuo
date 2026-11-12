//! Monitor row upserts.

use super::*;

pub fn upsert_session_row(rows: &mut Vec<MonitoredThread>, row: MonitoredThread) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
}

pub fn upsert_task_row(rows: &mut Vec<MonitoredTask>, row: MonitoredTask) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
}
