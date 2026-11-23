//! Per-session append-only journal: the durable source of truth for a session
//! (ADR-0236, ADR-0237).
//!
//! The module is split by concern:
//!
//! - [`frame`] defines the length-prefixed, CRC-protected on-disk unit;
//! - [`lane`] is the group-commit I/O runtime;
//! - [`session`] owns one session's segments, snapshots, and recovery.
//!
//! The SQLite store is a **derived projection** fed after the journal ack.
//! It may lag; it can be rebuilt; it is never on the commit critical path.

mod frame;
mod lane;
mod session;

pub(crate) use session::SessionJournal;

/// Finish a deletion whose crash window left segment or snapshot bodies under
/// a terminal `DELETED` marker. The marker already decided the outcome, so this
/// only reclaims bytes; it is idempotent and safe to run at bootstrap.
pub(crate) fn sweep_deleted(root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || !dir.join("DELETED").exists() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if name.starts_with("seg-") || name.starts_with("snap-") {
                let _ = std::fs::remove_file(file.path());
            }
        }
    }
}
