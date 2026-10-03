//! Architectural guard for the one-door rule (ADR-0231).
//!
//! The compile-time half of the rule is the engine's privacy: `DatabaseEngine`
//! is `pub(crate)`, so no other crate can name a connection. That covers the
//! *type*, not the *library*: `rusqlite` is an ordinary dependency and any
//! module could open a second connection to `nuo.db` — which is exactly the
//! failure this ADR exists to prevent (concurrent writers losing to `database
//! is locked`, silently dropping records after burning the busy timeout).
//!
//! This test is the mechanized half. It fails the moment a new bypass appears,
//! so the discipline does not depend on a reviewer noticing.
//!
//! Deliberately a *source scan* rather than a compile-time lint: the rule is
//! about where SQLite handles may be created, which no type can express.
//! False positives are cheap to fix (add a door in `db.rs`); false negatives
//! are the incident.

// Assertion failures should stop the run immediately.
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

/// The one module allowed to name the engine or a raw `rusqlite` handle.
const THE_DOOR: &str = "nuo-persistence/src/db.rs";

/// Child modules of the door (`src/db/*.rs`): still inside `nuo-persistence::db`,
/// so the ADR-0231 boundary is unchanged when `db.rs` is split into a directory.
const THE_DOOR_DIR: &str = "nuo-persistence/src/db/";

/// This guard names the forbidden patterns, so it exempts itself.
const THE_GUARD: &str = "nuo-persistence/tests/it/one_door.rs";

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .expect("crate lives one level below the workspace root")
        .to_path_buf()
}

/// Every `.rs` file under `root`, skipping generated and vendored trees.
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(
                    name.as_ref(),
                    "target" | "node_modules" | "vendor" | ".git" | "docs" | "assets"
                ) {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn is_exempt(path: &Path, root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let rel = rel.to_string_lossy().replace('\\', "/");
    rel == THE_DOOR
        || rel.starts_with(THE_DOOR_DIR)
        || rel == THE_GUARD
        || rel == "nuo-persistence/src/role_memory.rs" // ADR-0248: isolated role-scoped SQLite store
}

/// Concatenated source of the door module: `db.rs` plus every `src/db/*.rs`
/// child. The door is a module directory now, so the shape assertions must
/// span it rather than a single file.
fn door_text() -> String {
    let root = workspace_root();
    let mut text = std::fs::read_to_string(root.join(THE_DOOR)).expect("the door exists");
    let dir = root.join("nuo-persistence/src/db");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .collect();
        paths.sort();
        for path in paths {
            text.push('\n');
            text.push_str(&std::fs::read_to_string(&path).expect("door child is readable"));
        }
    }
    text
}

/// Patterns that mean "this file opens or names a database connection".
///
/// `Connection::open` / `open_in_memory` / `initialize_db` are the ways a new
/// SQLite handle comes into existence; `DatabaseEngine` additionally covers
/// in-crate reach around the door.
const FORBIDDEN: &[&str] = &[
    "DatabaseEngine",
    "Connection::open",
    "initialize_db",
    "rusqlite::Connection",
];

#[test]
fn only_the_persistence_door_names_a_connection() {
    let root = workspace_root();
    let sources = rust_sources(&root);
    assert!(
        sources.len() > 200,
        "source scan found only {} files under {} — the walker is broken, not the tree",
        sources.len(),
        root.display()
    );

    let mut violations = Vec::new();
    for path in &sources {
        if is_exempt(path, &root) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            // A mention in prose is documentation, not a bypass.
            if line.trim_start().starts_with("//") {
                continue;
            }
            for pattern in FORBIDDEN {
                if line.contains(pattern) {
                    violations.push(format!(
                        "{}:{}: {}",
                        path.strip_prefix(&root).unwrap_or(path).display(),
                        index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "ADR-0231: a SQLite connection may only be created in {THE_DOOR}.\n\
         Every mutation goes through the single-writer actor (`PersistenceHandle`);\n\
         every read through the reader it hands out (`handle.reader()`).\n\
         Offending lines:\n{}",
        violations.join("\n")
    );
}

/// The rule has exactly two ways in. Assert both still exist, so a future
/// "cleanup" that deletes one fails here instead of leaving a crate with no
/// way to persist or read.
#[test]
fn the_door_exposes_exactly_two_ways_in() {
    let text = door_text();

    for needle in [
        "pub(crate) struct DatabaseEngine",
        "pub struct PersistenceHandle",
        "pub fn reader(&self) -> Result<DbReader>",
        "pub struct DbReader",
        "pub fn get_persistence_handle()",
    ] {
        assert!(
            text.contains(needle),
            "ADR-0231 invariant missing from {THE_DOOR}: {needle}"
        );
    }

    // The engine must stay crate-private: a `pub struct DatabaseEngine` is the
    // whole rule undone in one word.
    assert!(
        !text.contains("\npub struct DatabaseEngine"),
        "DatabaseEngine must stay crate-private (ADR-0231)"
    );
}

// ---------------------------------------------------------------------------
// The coupling the door depends on
// ---------------------------------------------------------------------------

/// A synchronous verb must complete on **every** runtime flavor, because
/// routing writes through the actor put a blocking bridge on paths that run
/// under all of them (a `#[tokio::test]`, a daemon worker, a plain thread, a
/// `spawn_blocking` task).
///
/// This is a regression test, not a formality: the first version of the bridge
/// deadlocked under exactly the flavor below. `run_blocking` takes the
/// thread path on a current-thread runtime while `PersistenceHandle::spawn` is
/// free to leave the supervisor as a *task* on that same runtime — so the
/// `join` waits on the one thread that could ever serve the ack. The pair must
/// be fixed together; this test fails if either half drifts back.
fn a_sync_write_round_trips() {
    use nuo_persistence::db::PersistenceHandle;

    let tmp = tempfile::tempdir().expect("tempdir");
    let handle = PersistenceHandle::spawn(tmp.path().join("nuo.db"), None);

    // A streaming verb (actor command + ack) and a plain blocking one.
    handle
        .set_kv_blocking("door:probe".to_string(), "v".to_string())
        .expect("set_kv_blocking must not deadlock");
    handle
        .save_input_history_blocking(
            vec![nuo_wire::HistoryEntry {
                text: "probe".to_string(),
                session_id: None,
                workspace: None,
                created_at_ms: 1,
            }],
            false,
        )
        .expect("save_input_history_blocking must not deadlock");

    let reader = handle.reader().expect("reader");
    assert_eq!(
        reader.get_kv("door:probe").expect("get_kv").as_deref(),
        Some("v")
    );
}

/// Single-threaded runtime: the bridge must move the wait off the only thread.
#[tokio::test(flavor = "current_thread")]
async fn sync_verbs_complete_on_a_current_thread_runtime() {
    a_sync_write_round_trips();
}

/// Multi-threaded runtime: the bridge may park a worker, but must still ack.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_verbs_complete_on_a_multi_thread_runtime() {
    a_sync_write_round_trips();
}

/// A single-worker multi-thread runtime is the sharpest case for
/// `block_in_place`: the bridge parks the only worker, so the runtime has to
/// grow a replacement thread for the supervisor to keep serving.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_verbs_complete_with_a_single_worker() {
    a_sync_write_round_trips();
}

/// And from inside a `spawn_blocking` task, where the caller is on a blocking
/// pool thread rather than a runtime worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_verbs_complete_from_a_blocking_pool_thread() {
    tokio::task::spawn_blocking(a_sync_write_round_trips)
        .await
        .expect("blocking task must not panic");
}
