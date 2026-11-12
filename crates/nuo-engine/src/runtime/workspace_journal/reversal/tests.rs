//! Reversal contracts.
//!
//! The subject under test is the *refusal* discipline as much as the restore: an
//! `/undo` that overwrites work it did not produce is worse than one that does
//! nothing, so most of these tests assert that a reversal declines.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use nuo_persistence::workspace_effects::JournalOp;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MemStore {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemStore {
    fn put_bytes(&self, bytes: &[u8]) -> String {
        self.put(bytes).unwrap()
    }
}

impl PreImageStore for MemStore {
    fn put(&self, bytes: &[u8]) -> Result<String, String> {
        use sha2::Digest;
        let hash = format!("{:x}", sha2::Sha256::digest(bytes));
        self.blobs
            .lock()
            .unwrap()
            .insert(hash.clone(), bytes.to_vec());
        Ok(hash)
    }

    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        self.blobs.lock().unwrap().get(hash).cloned()
    }
}

/// Build a completed journal row describing `before` → `after` on `path`.
fn effect(
    path: &Path,
    before: Option<&str>,
    after: Option<&str>,
    store: &MemStore,
) -> WorkspaceEffect {
    WorkspaceEffect {
        id: 1,
        thread_id: "t".into(),
        round_id: "r1".into(),
        turn_id: "t1".into(),
        call_id: "c1".into(),
        seq: 1,
        tool_name: "edit_text".into(),
        path: Some(path.to_string_lossy().into_owned()),
        op: if before.is_none() {
            JournalOp::Create
        } else {
            JournalOp::Edit
        },
        effect_class: "file_bytes".into(),
        before_hash: before.map(|bytes| store.put_bytes(bytes.as_bytes())),
        after_hash: after.map(|bytes| store.put_bytes(bytes.as_bytes())),
        irreversible_reason: None,
        completed_at_ms: Some(1),
        reversed_at_ms: None,
        reversal_note: None,
        expired_at_ms: None,
        created_at_ms: 1,
    }
}

/// The happy path: the file is exactly as the thread left it, so the pre-image is
/// written back byte for byte.
#[test]
fn restores_the_pre_image_when_the_file_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "after").unwrap();
    let store = MemStore::default();
    let row = effect(&file, Some("before"), Some("after"), &store);

    let outcome = reverse(&row, Some(dir.path()), &store);
    let Reversal::Restored(changed) = outcome else {
        panic!("expected a restore, got {outcome:?}");
    };
    assert_eq!(changed.path, "a.rs", "reported workspace-relative");
    assert!(!changed.created);
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"before",
        "the bytes are identical to the pre-image, not merely similar"
    );
}

/// The refusal that matters most: the user (or a later round, or a formatter)
/// changed the file since this thread wrote it, so restoring would discard work
/// the journal did not produce.
#[test]
fn refuses_when_the_file_changed_since_this_thread_wrote_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "user edited this afterwards").unwrap();
    let store = MemStore::default();
    let row = effect(&file, Some("before"), Some("after"), &store);

    let outcome = reverse(&row, Some(dir.path()), &store);
    let Reversal::Refused(refusal) = outcome else {
        panic!("expected a refusal, got {outcome:?}");
    };
    assert_eq!(refusal.path, "a.rs");
    assert!(
        refusal
            .reason
            .contains("changed after this thread wrote it"),
        "the reason must name the actual cause, got: {}",
        refusal.reason
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"user edited this afterwards",
        "the user's work must be untouched"
    );
}

/// A file the thread created is removed on undo — but only if it still holds
/// exactly what was created.
#[test]
fn removes_a_created_file_that_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("new.rs");
    std::fs::write(&file, "created content").unwrap();
    let store = MemStore::default();
    let row = effect(&file, None, Some("created content"), &store);

    let outcome = reverse(&row, Some(dir.path()), &store);
    let Reversal::Restored(changed) = outcome else {
        panic!("expected a restore, got {outcome:?}");
    };
    assert!(changed.created);
    assert!(!file.exists(), "undoing a creation removes the file");
}

/// A created file that someone has since written to must survive: the creation
/// is no longer the only thing that happened to it.
#[test]
fn refuses_to_remove_a_created_file_that_was_later_edited() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("new.rs");
    std::fs::write(&file, "someone added more").unwrap();
    let store = MemStore::default();
    let row = effect(&file, None, Some("created content"), &store);

    let outcome = reverse(&row, Some(dir.path()), &store);
    let Reversal::Refused(refusal) = outcome else {
        panic!("expected a refusal, got {outcome:?}");
    };
    assert!(
        refusal
            .reason
            .contains("changed after this thread created it"),
        "got: {}",
        refusal.reason
    );
    assert!(file.exists());
}

/// A created file that is already gone is the desired end state, not an error.
#[test]
fn removing_an_already_absent_creation_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("gone.rs");
    let store = MemStore::default();
    let row = effect(&file, None, Some("created content"), &store);

    let outcome = reverse(&row, Some(dir.path()), &store);
    assert!(
        matches!(outcome, Reversal::Restored(_)),
        "an absent created file is already undone, got {outcome:?}"
    );
}

/// An edited file that was deleted afterwards must not be recreated: the user
/// removed it deliberately, and a revert is not an undelete.
#[test]
fn refuses_to_resurrect_a_file_deleted_after_the_edit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    let store = MemStore::default();
    let row = effect(&file, Some("before"), Some("after"), &store);
    // Nothing on disk: the file was deleted after the thread's write.

    let outcome = reverse(&row, Some(dir.path()), &store);
    let Reversal::Refused(refusal) = outcome else {
        panic!("expected a refusal, got {outcome:?}");
    };
    assert!(
        refusal
            .reason
            .contains("deleted after this thread wrote it"),
        "got: {}",
        refusal.reason
    );
    assert!(!file.exists(), "nothing may be recreated");
}

/// A row that is not addressable is skipped, not refused: the caller asked about
/// something a concurrent `/undo` may already have handled, and reporting a
/// refusal would be a lie about what happened.
#[test]
fn skips_rows_that_are_not_addressable() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "after").unwrap();
    let store = MemStore::default();

    // Already reversed.
    let mut reversed = effect(&file, Some("before"), Some("after"), &store);
    reversed.reversed_at_ms = Some(9);
    assert_eq!(
        reverse(&reversed, Some(dir.path()), &store),
        Reversal::Skipped
    );

    // Never completed: the journal cannot say whether this call changed anything.
    let mut unconfirmed = effect(&file, Some("before"), Some("after"), &store);
    unconfirmed.completed_at_ms = None;
    assert_eq!(
        reverse(&unconfirmed, Some(dir.path()), &store),
        Reversal::Skipped
    );

    // A class that promises nothing to restore.
    let mut opaque = effect(&file, Some("before"), Some("after"), &store);
    opaque.effect_class = "opaque".into();
    opaque.irreversible_reason = Some("ran with arbitrary execution authority".into());
    assert_eq!(
        reverse(&opaque, Some(dir.path()), &store),
        Reversal::Skipped
    );
}

/// A pre-image that the CAS has lost cannot be restored, and the refusal says so
/// rather than writing nothing and reporting success.
#[test]
fn refuses_when_the_pre_image_is_gone_from_the_cas() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "after").unwrap();
    let store = MemStore::default();
    let mut row = effect(&file, Some("before"), Some("after"), &store);
    row.before_hash = Some("a-hash-that-was-collected".into());

    let outcome = reverse(&row, Some(dir.path()), &store);
    let Reversal::Refused(refusal) = outcome else {
        panic!("expected a refusal, got {outcome:?}");
    };
    assert!(
        refusal.reason.contains("no longer available"),
        "got: {}",
        refusal.reason
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"after", "nothing written");
}

/// Two writes to one path must unwind through their own history: newest first.
/// Reversing oldest-first would compare the original pre-image against the second
/// write's post-image and refuse a file that was perfectly undoable.
#[test]
fn multiple_writes_to_one_path_unwind_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "v3").unwrap();
    let store = MemStore::default();

    let mut first = effect(&file, Some("v1"), Some("v2"), &store);
    first.id = 1;
    first.seq = 1;
    let mut second = effect(&file, Some("v2"), Some("v3"), &store);
    second.id = 2;
    second.seq = 2;

    let outcomes = reverse_all(&[first, second], Some(dir.path()), &store);
    // Both succeeded, and the end state is the original pre-image.
    for (_, outcome) in &outcomes {
        assert!(
            matches!(outcome, Reversal::Restored(_)),
            "each step must succeed, got {outcome:?}"
        );
    }
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"v1",
        "the chain unwinds all the way back to the original content"
    );
    // And the stamps are ordered newest first.
    assert_eq!(
        outcomes
            .iter()
            .map(|(stamp, _)| stamp.effect_id)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
}

/// A path outside the workspace is reported absolutely rather than as a
/// nonsensical relative path.
#[test]
fn paths_outside_the_workspace_are_reported_absolutely() {
    let workspace = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let file = elsewhere.path().join("a.rs");
    std::fs::write(&file, "after").unwrap();
    let store = MemStore::default();
    let row = effect(&file, Some("before"), Some("after"), &store);

    let outcome = reverse(&row, Some(workspace.path()), &store);
    let Reversal::Restored(changed) = outcome else {
        panic!("expected a restore, got {outcome:?}");
    };
    assert!(
        changed
            .path
            .starts_with(elsewhere.path().to_string_lossy().as_ref()),
        "an out-of-workspace path must stay absolute, got {}",
        changed.path
    );
}

/// A refusal is stamped with the reason, so a later `/diff` can explain why a
/// change was left in place instead of silently omitting it.
#[test]
fn refusals_are_stamped_with_their_reason() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "moved on").unwrap();
    let store = MemStore::default();
    let row = effect(&file, Some("before"), Some("after"), &store);

    let outcomes = reverse_all(&[row], Some(dir.path()), &store);
    let (stamp, _) = &outcomes[0];
    assert!(!stamp.restored);
    assert!(
        stamp.note.contains("changed after this thread wrote it"),
        "got: {}",
        stamp.note
    );
}
