//! Content-residency and dialog-sovereignty invariants (ADR-0116, ADR-0118).
//!
//! These are *module-graph* checks, not behavioural ones: they fail when a
//! retired concept or a forbidden dependency is re-introduced, which is exactly
//! the failure mode a per-function test cannot catch. The behavioural halves of
//! the invariants live where they can be tested directly:
//!
//! - `[INV-LAYER-04]` / `[INV-TRANSCRIPT-01]`: `nuo_view::model::layer` and
//!   `nuo_view::model::thread` unit tests prove the routing and admission gates
//!   (`ContentLayer` has no transcript arm; `ThreadDoc::admit` refuses
//!   operation/telemetry/ephemera).
//! - `[INV-DIALOG-01]`: this file.
//!
//! Run with `cargo test -p nuo --test content_purity`.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

fn workspace_root() -> PathBuf {
    nuo::dev::find_workspace_root()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "rs"))
        .collect()
}

/// `[INV-LAYER-02]`: the presentation layer is a property of the surface, so no
/// wire payload may carry it. `NoticeSurface` and its producer-side setter were
/// deleted; a re-introduction would silently restore "the transcript is the
/// default destination".
#[test]
fn no_content_payload_carries_a_presentation_layer() {
    let root = workspace_root();
    let mut offenders = Vec::new();
    for dir in ["crates", "providers", "tools"] {
        for file in rust_files(&root.join(dir)) {
            // Prose that names the retired concept is *negative knowledge* and
            // must survive; only live code may not.
            if file.ends_with("tests/content_purity.rs") {
                continue;
            }
            for (n, line) in read(&file).lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for needle in ["NoticeSurface", "with_surface("] {
                    if line.contains(needle) {
                        offenders.push(format!("{}:{}: {needle}", file.display(), n + 1));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a presentation layer must be derived from the surface, never carried on the content \
         (ADR-0116):\n{}",
        offenders.join("\n")
    );
}

/// `[INV-DIALOG-01]`: a dialog must be *unable* to write into a thread. The
/// capability check is structural — the dialog surface tree may not name the
/// session store, the fact sink, or the transcript document at all.
#[test]
fn dialog_surfaces_hold_no_thread_capability() {
    let root = workspace_root();
    let dialog_tree = root.join("crates/nuo/src/shell/surfaces");
    let overlays = root.join("crates/nuo/src/shell/overlays");

    // Retired/forbidden capabilities: persisting into a session, admitting a
    // transcript row, or addressing a thread document directly.
    const FORBIDDEN: [&str; 5] = [
        "SessionStore",
        "FactSink",
        "ThreadDoc",
        "TranscriptEntry",
        "mutate_commands",
    ];

    let mut offenders = Vec::new();
    for file in rust_files(&dialog_tree)
        .into_iter()
        .chain(rust_files(&overlays))
    {
        let text = read(&file);
        for needle in FORBIDDEN {
            if text.contains(needle) {
                offenders.push(format!("{}: {needle}", file.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a dialog conversation must not hold a thread capability (ADR-0118):\n{}",
        offenders.join("\n")
    );
}

/// `[INV-TRANSCRIPT-03]`: the classification is only a guarantee if it is
/// exhaustive. Rust enforces that, so this test guards the *shape* of the
/// guarantee: the classification must stay a `match self` with no wildcard arm,
/// because a wildcard would silently classify every future event.
#[test]
fn round_event_classification_has_no_wildcard_arm() {
    let root = workspace_root();
    let events = read(&root.join("crates/nuo-protocol/src/events.rs"));
    let start = events
        .find("pub fn class(&self) -> EventClass {")
        .expect("RoundEvent::class must exist");
    let body = &events[start..];
    let end = body.find("\n    }\n").expect("class() body must close");
    let body = &body[..end];
    assert!(
        !body.contains("_ =>"),
        "RoundEvent::class must stay exhaustive with no wildcard arm: a wildcard would admit \
         every future variant to whatever class the arm hides in (ADR-0117)"
    );
    // And the class must actually be consulted by the admission path.
    let edits = read(&root.join("crates/nuo/src/shell/event_loop/mutations.rs"));
    assert!(
        edits.contains("class: nuo_protocol::EventClass"),
        "TranscriptEdit::Append must carry the event class (ADR-0117)"
    );
    let apply = read(&root.join("crates/nuo/src/shell/event_loop/apply.rs"));
    assert!(
        apply.contains("admits_dialogue()") && apply.contains("admits_anchor()"),
        "the transcript edit applier must gate admission on the event class (ADR-0117)"
    );
}
