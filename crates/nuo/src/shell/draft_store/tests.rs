use super::*;

fn draft(text: &str) -> ComposerDraft {
    ComposerDraft::capture(text.to_string(), text.len(), Vec::new(), Vec::new(), 0)
}

fn live<'a>(text: &'a str) -> LiveDraft<'a> {
    LiveDraft {
        text,
        cursor_position: text.len(),
        pending_images: &[],
        pending_text_pastes: &[],
        input_scroll: 0,
    }
}

fn image_draft(mime: &str) -> ComposerDraft {
    ComposerDraft::capture(
        "[Image #1]".to_string(),
        10,
        vec![ImagePart {
            mime: mime.to_string(),
            data: "QUJD".to_string(),
        }],
        Vec::new(),
        0,
    )
}

fn temp_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn park_then_parked_round_trips_without_touching_the_filesystem() {
    let mut ledger = DraftLedger::in_memory();
    assert!(ledger.park_live("t1", live("hello")));
    assert_eq!(ledger.parked("t1"), Some(&draft("hello")));
    assert!(!ledger.is_empty());
    assert_eq!(ledger.known.len(), 1);
}

#[test]
fn parking_an_empty_draft_removes_the_entry() {
    let mut ledger = DraftLedger::in_memory();
    ledger.park_live("t1", live("hello"));
    assert!(ledger.park_live("t1", live("")));
    assert_eq!(ledger.parked("t1"), None);
    assert!(ledger.is_empty());
    assert!(
        !ledger.park_live("t1", live("")),
        "clearing an absent draft is not a change"
    );
}

#[test]
fn an_empty_thread_id_is_never_parked() {
    // A pre-attach client has no thread identity; parking under "" would
    // create a file no restore path can ever name.
    let mut ledger = DraftLedger::in_memory();
    assert!(!ledger.park_live("", live("orphan")));
    assert!(ledger.is_empty());
}

#[test]
fn reparking_an_identical_draft_copies_nothing_and_does_not_dirty_it() {
    let dir = temp_root();
    let mut ledger = DraftLedger::load(dir.path().to_path_buf());
    assert!(ledger.park_live("t1", live("stable")));
    assert!(ledger.is_pending());
    ledger.flush();
    assert!(!ledger.is_pending(), "a successful flush clears the marks");
    assert!(
        !ledger.park_live("t1", live("stable")),
        "an unchanged draft must not be copied or scheduled"
    );
    assert!(!ledger.is_pending());
}

#[test]
fn a_changed_caret_alone_is_a_change() {
    let mut ledger = DraftLedger::in_memory();
    ledger.park_live("t1", live("abc"));
    let moved = LiveDraft {
        text: "abc",
        cursor_position: 1,
        pending_images: &[],
        pending_text_pastes: &[],
        input_scroll: 0,
    };
    assert!(ledger.park_live("t1", moved));
    assert_eq!(ledger.parked("t1").map(|d| d.cursor_position), Some(1));
}

#[test]
fn a_flushed_draft_survives_a_fresh_ledger() {
    let dir = temp_root();
    {
        let mut ledger = DraftLedger::load(dir.path().to_path_buf());
        ledger.park_live("thread-a", live("half-typed question"));
        ledger.flush();
    }
    // A new process, same state directory.
    let mut reopened = DraftLedger::load(dir.path().to_path_buf());
    assert_eq!(
        reopened.parked("thread-a"),
        Some(&draft("half-typed question"))
    );
}

#[test]
fn attachment_payloads_survive_the_round_trip() {
    let dir = temp_root();
    {
        let mut ledger = DraftLedger::load(dir.path().to_path_buf());
        ledger.park("t-img", image_draft("image/png"));
        ledger.flush();
    }
    let mut reopened = DraftLedger::load(dir.path().to_path_buf());
    let restored = reopened.parked("t-img").expect("draft");
    assert_eq!(restored.pending_images.len(), 1);
    assert_eq!(restored.pending_images[0].mime, "image/png");
    assert_eq!(restored.pending_images[0].data, "QUJD");
}

#[test]
fn forgetting_unlinks_the_file_so_a_restart_cannot_resurrect_it() {
    let dir = temp_root();
    {
        let mut ledger = DraftLedger::load(dir.path().to_path_buf());
        ledger.park_live("t1", live("gone soon"));
        ledger.flush();
        let path = ledger.path_for("t1");
        assert!(path.exists());
        ledger.forget("t1");
        ledger.flush();
        assert!(!path.exists(), "the file must be unlinked, not blanked");
    }
    let mut reopened = DraftLedger::load(dir.path().to_path_buf());
    assert_eq!(reopened.parked("t1"), None);
}

#[test]
fn an_evicted_draft_is_re_read_from_disk() {
    let dir = temp_root();
    let mut ledger = DraftLedger::load(dir.path().to_path_buf());
    ledger.park_live("t1", live("evict me"));
    ledger.flush();
    ledger.entries.clear();
    ledger.recency.clear();
    assert_eq!(ledger.parked("t1"), Some(&draft("evict me")));
}

#[test]
fn an_unreadable_file_is_ignored_and_left_in_place() {
    let dir = temp_root();
    let mut ledger = DraftLedger::load(dir.path().to_path_buf());
    ledger.park_live("broken", live("x"));
    ledger.flush();
    let path = ledger.path_for("broken");
    std::fs::write(&path, b"{ this is not json").expect("corrupt it");

    let mut reopened = DraftLedger::load(dir.path().to_path_buf());
    assert_eq!(reopened.parked("broken"), None);
    assert!(
        path.exists(),
        "a file this version cannot read may be the only copy of the text"
    );
}

#[test]
fn a_payload_naming_another_thread_is_never_adopted() {
    let dir = temp_root();
    let path = dir.path().join(draft_file_name("t1"));
    let stored = StoredDraft {
        thread_id: "somebody-else".to_string(),
        draft: draft("not yours"),
    };
    host::fsutil::atomic_write_json(&path, &stored).expect("write");

    let mut ledger = DraftLedger::load(dir.path().to_path_buf());
    assert_eq!(
        ledger.parked("t1"),
        None,
        "trusting the file name over the payload would graft one thread's draft onto another"
    );
    assert_eq!(
        ledger.parked("somebody-else"),
        Some(&draft("not yours")),
        "the payload is still readable under the id it names"
    );
}

#[test]
fn file_names_are_distinct_for_ids_that_sanitize_alike() {
    let a = draft_file_name("a/b");
    let b = draft_file_name("a:b");
    assert_ne!(a, b);
    assert!(a.ends_with(".json"));
    assert!(!a.contains('/'));
    assert_eq!(draft_file_name("plain-id"), draft_file_name("plain-id"));
}

#[test]
fn residency_is_capped_by_bytes_and_keeps_the_newest() {
    let mut ledger = DraftLedger::in_memory();
    let big = "x".repeat(RESIDENT_BUDGET_BYTES / 2);
    for index in 0..5 {
        ledger.park_live(&format!("t{index}"), live(&big));
    }
    assert!(
        ledger.entries.len() < 5,
        "five half-budget drafts cannot all stay resident"
    );
    assert!(
        ledger.parked("t4").is_some(),
        "the most recently parked draft is never evicted"
    );
}

#[test]
fn server_scopes_isolate_drafts_with_identical_thread_ids() {
    let dir = temp_root();
    {
        let mut a = DraftLedger::load_for_scope(dir.path().to_path_buf(), "https://server-a");
        let mut b = DraftLedger::load_for_scope(dir.path().to_path_buf(), "https://server-b");
        a.park_live("shared-id", live("from A"));
        a.flush();
        b.park_live("shared-id", live("from B"));
        b.flush();
    }
    let mut a = DraftLedger::load_for_scope(dir.path().to_path_buf(), "https://server-a");
    let mut b = DraftLedger::load_for_scope(dir.path().to_path_buf(), "https://server-b");
    assert_eq!(a.parked("shared-id"), Some(&draft("from A")));
    assert_eq!(b.parked("shared-id"), Some(&draft("from B")));
}

#[test]
fn reconcile_missing_unlinks_deleted_threads_and_preserves_active_and_live() {
    let dir = temp_root();
    let mut ledger = DraftLedger::load_for_scope(dir.path().to_path_buf(), "local");
    ledger.park_live("deleted-1", live("stale draft"));
    ledger.park_live("active-unpersisted", live("currently typing"));
    ledger.park_live("still-alive", live("keep me"));
    ledger.flush();

    let deleted_path = ledger.path_for("deleted-1");
    let active_path = ledger.path_for("active-unpersisted");
    let alive_path = ledger.path_for("still-alive");
    assert!(deleted_path.exists() && active_path.exists() && alive_path.exists());

    let removed = ledger.reconcile_missing(
        &["deleted-1".to_string(), "active-unpersisted".to_string()],
        Some("active-unpersisted"),
    );
    assert_eq!(removed, 1);
    assert!(
        !deleted_path.exists(),
        "confirmed-missing draft must be unlinked"
    );
    assert!(
        active_path.exists(),
        "active thread draft must never be unlinked"
    );
    assert!(
        alive_path.exists(),
        "existing thread draft must stay intact"
    );
    assert_eq!(ledger.parked("deleted-1"), None);
    assert_eq!(ledger.parked("still-alive"), Some(&draft("keep me")));
}

#[test]
fn a_draft_larger_than_the_whole_budget_still_park() {
    let mut ledger = DraftLedger::in_memory();
    let huge = "y".repeat(RESIDENT_BUDGET_BYTES + 1);
    ledger.park_live("only", live(&huge));
    assert_eq!(
        ledger.parked("only").map(|d| d.text.len()),
        Some(huge.len()),
        "the draft the user is working in is never evicted by its own size"
    );
}

#[test]
fn dirty_draft_is_never_evicted_before_flush_and_is_evicted_after() {
    let dir = temp_root();
    let mut ledger = DraftLedger::load(dir.path().to_path_buf());
    let big_a = "a".repeat(RESIDENT_BUDGET_BYTES / 2 + 1024);
    let big_b = "b".repeat(RESIDENT_BUDGET_BYTES / 2 + 1024);
    ledger.park_live("thread-a", live(&big_a));
    ledger.park_live("thread-b", live(&big_b));
    assert!(ledger.entries.contains_key("thread-a"));
    ledger.flush();
    assert!(!ledger.entries.contains_key("thread-a"));
    let mut reloaded = DraftLedger::load(dir.path().to_path_buf());
    assert_eq!(
        reloaded.parked("thread-a").map(|d| d.text.len()),
        Some(big_a.len())
    );
    assert_eq!(
        reloaded.parked("thread-b").map(|d| d.text.len()),
        Some(big_b.len())
    );
}
