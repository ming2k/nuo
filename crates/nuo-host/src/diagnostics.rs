//! Runtime diagnostics telemetry and persistent event journal.
//!
//! Tracks lifecycle, takeover, and transport disconnect events
//! so failures are observable across processes without contending on database locks.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum bytes for the diagnostics journal before pruning older entries.
const MAX_DIAGNOSTICS_FILE_BYTES: u64 = 1_048_576; // 1 MB
/// Target count of entries retained after pruning.
const PRUNE_RETAIN_COUNT: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticEvent {
    pub timestamp_ms: u64,
    pub pid: u32,
    pub process_kind: String,
    pub event_kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// The filesystem path to the persistent runtime diagnostics journal.
pub fn diagnostics_file() -> PathBuf {
    crate::paths::get()
        .log_dir()
        .join("runtime-diagnostics.jsonl")
}

/// Record a diagnostic event to the persistent journal.
///
/// If file size exceeds `MAX_DIAGNOSTICS_FILE_BYTES`, the oldest entries are pruned.
pub fn record_event(process_kind: &str, event_kind: &str, summary: &str, details: Option<&str>) {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let event = DiagnosticEvent {
        timestamp_ms,
        pid: std::process::id(),
        process_kind: process_kind.to_string(),
        event_kind: event_kind.to_string(),
        summary: summary.to_string(),
        details: details.map(|s| s.to_string()),
    };

    let path = diagnostics_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Best-effort prune if oversized before appending
    if let Ok(metadata) = std::fs::metadata(&path) {
        if metadata.len() > MAX_DIAGNOSTICS_FILE_BYTES {
            prune_diagnostics_file(&path);
        }
    }

    if let Ok(json) = serde_json::to_string(&event) {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{json}");
        }
    }
}

/// Retrieve the most recent diagnostic events up to `limit`.
pub fn recent_events(limit: usize) -> Vec<DiagnosticEvent> {
    let path = diagnostics_file();
    let Ok(file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<DiagnosticEvent>(trimmed) {
            events.push(event);
        }
    }
    if events.len() > limit {
        events.split_off(events.len() - limit)
    } else {
        events
    }
}

fn prune_diagnostics_file(path: &Path) {
    let lock_path = path.with_extension("lock");
    let Ok(_lock) = crate::lock::ProcessLock::acquire(&lock_path) else {
        return;
    };
    let events = recent_events(PRUNE_RETAIN_COUNT);
    let tmp_path = path.with_extension(format!("prune.{}.tmp", std::process::id()));
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)
    {
        for ev in events {
            if let Ok(line) = serde_json::to_string(&ev) {
                let _ = writeln!(file, "{line}");
            }
        }
        let _ = std::fs::rename(&tmp_path, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostic_event_serialization_roundtrip() {
        let ev = DiagnosticEvent {
            timestamp_ms: 1730419200000,
            pid: 12345,
            process_kind: "client".to_string(),
            event_kind: "link_lost".to_string(),
            summary: "IPC outbox channel closed".to_string(),
            details: Some("broken pipe".to_string()),
        };

        let json = serde_json::to_string(&ev).unwrap();
        let decoded: DiagnosticEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, decoded);
    }
}
