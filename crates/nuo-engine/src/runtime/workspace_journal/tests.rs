//! Capture contracts, exercised against a real temporary directory.
//!
//! The subject under test is the *observation*: what the filesystem held before
//! and after a call, and what the classification says about whether that
//! observation buys the right to reverse. The store is an in-memory double so the
//! tests say nothing about CAS layout — only about capture semantics.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MemStore {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemStore {
    fn len(&self) -> usize {
        self.blobs.lock().unwrap().len()
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

fn write_accesses(path: &str) -> ToolAccesses {
    ToolAccesses::write_file(path.to_string())
}

fn read_write_accesses(path: &str) -> ToolAccesses {
    ToolAccesses::read_write_file(path.to_string())
}

/// The core promise: an edit's pre-image is the bytes that were there, read
/// independently of what the tool reported.
#[test]
fn edit_captures_the_pre_image_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "original content").unwrap();
    let store = MemStore::default();

    let outcome = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &read_write_accesses("a.rs"),
        &store,
    )
    .unwrap();

    assert_eq!(outcome.effect, EffectClass::FileBytes);
    assert_eq!(outcome.paths.len(), 1);
    let capture = &outcome.paths[0];
    assert_eq!(capture.op, CapturedOp::Edit, "the file existed");
    assert_eq!(capture.path, dir.path().join("a.rs"));

    let hash = capture.before_hash.as_deref().expect("pre-image captured");
    assert_eq!(
        store.get(hash).as_deref(),
        Some(b"original content".as_slice()),
        "the stored pre-image is exactly what the disk held"
    );
    assert_eq!(outcome.stored_hashes, vec![hash.to_string()]);
}

/// A write to an absent path is a *creation*, and the operation is decided from
/// the pre-state rather than from the tool's self-description. This is what makes
/// reversal delete the file instead of writing empty content into it.
#[test]
fn write_to_absent_path_is_a_creation_with_no_pre_image() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    let outcome = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &write_accesses("brand-new.rs"),
        &store,
    )
    .unwrap();

    let capture = &outcome.paths[0];
    assert_eq!(capture.op, CapturedOp::Create);
    assert_eq!(capture.before_hash, None);
    assert!(outcome.stored_hashes.is_empty(), "nothing to store");
    assert_eq!(store.len(), 0);
}

/// A file-mutating tool that declares no locatable path cannot have a pre-image
/// captured, so it is promoted to `Opaque` rather than promising a reversal it
/// cannot perform.
#[test]
fn unlocatable_mutation_fails_closed_to_opaque_and_captures_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    let outcome = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &ToolAccesses::none(),
        &store,
    )
    .unwrap();

    assert_eq!(outcome.effect, EffectClass::Opaque);
    assert!(outcome.paths.is_empty());
    assert!(outcome.stored_hashes.is_empty());
}

/// An `Opaque` ceiling is never demoted by a clean access list: the tool said it
/// runs arbitrary execution, and no observation may launder that.
#[test]
fn opaque_ceiling_is_never_demoted_by_declared_accesses() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    for ceiling in [
        EffectClass::Opaque,
        EffectClass::ProcessEffect,
        EffectClass::ExternalEffect,
    ] {
        let outcome = prepare(dir.path(), ceiling, &write_accesses("a.rs"), &store).unwrap();
        assert_eq!(outcome.effect, ceiling);
        assert!(
            outcome.paths.is_empty(),
            "{ceiling:?} is a receipt, not a recovery point"
        );
    }
}

/// An under-declared risk ceiling is corrected upward by an observed write.
#[test]
fn observed_write_promotes_a_none_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "x").unwrap();
    let store = MemStore::default();

    let outcome = prepare(
        dir.path(),
        EffectClass::None,
        &write_accesses("a.rs"),
        &store,
    )
    .unwrap();
    assert_eq!(outcome.effect, EffectClass::FileBytes);
    assert_eq!(outcome.paths.len(), 1);
}

/// A recursive write names a subtree; the journal records per-path pre-images, so
/// it cannot capture one. Fail closed.
#[test]
fn recursive_write_claim_cannot_be_captured_and_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    let outcome = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &ToolAccesses::write_tree("src"),
        &store,
    )
    .unwrap();

    assert_eq!(outcome.effect, EffectClass::Opaque);
    assert!(outcome.paths.is_empty());
}

/// `All` is the global-exclusive marker: it names no path, so there is nothing to
/// capture.
#[test]
fn all_claim_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    let outcome = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &ToolAccesses::all(),
        &store,
    )
    .unwrap();

    assert_eq!(outcome.effect, EffectClass::Opaque);
    assert!(outcome.paths.is_empty());
}

/// Read-side declarations are not mutations and must not produce captures.
#[test]
fn read_only_accesses_capture_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "x").unwrap();
    let store = MemStore::default();

    let outcome = prepare(
        dir.path(),
        EffectClass::None,
        &ToolAccesses::read_file("a.rs".to_string()),
        &store,
    )
    .unwrap();
    assert_eq!(outcome.effect, EffectClass::None);
    assert!(outcome.paths.is_empty());

    // A search is read-side too, even though it claims a subtree.
    let searched = prepare(
        dir.path(),
        EffectClass::None,
        &ToolAccesses::search_tree("."),
        &store,
    )
    .unwrap();
    assert_eq!(searched.effect, EffectClass::None);
    assert!(searched.paths.is_empty());
}

/// An unreadable pre-image aborts the capture, so the caller never mutates what
/// it could not observe.
#[test]
fn unreadable_pre_image_is_a_capture_failure() {
    let dir = tempfile::tempdir().unwrap();
    // A directory is readable as a path but not as bytes: this is exactly the
    // "exists but cannot be observed" case a mutation must not proceed past.
    std::fs::create_dir(dir.path().join("adir")).unwrap();
    let store = MemStore::default();

    let err = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &write_accesses("adir"),
        &store,
    )
    .expect_err("capturing a directory must fail");
    assert!(err.contains("cannot capture pre-image"), "got: {err}");
}

/// The post-image is observed after the call, and a deletion yields `None`.
#[test]
fn completion_observes_post_state_including_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let kept = dir.path().join("kept.rs");
    let removed = dir.path().join("removed.rs");
    std::fs::write(&kept, "before").unwrap();
    std::fs::write(&removed, "before").unwrap();
    let store = MemStore::default();

    let captures = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &ToolAccesses::none()
            .with_file(
                ToolFileAccessOperation::ReadWrite,
                "kept.rs".to_string(),
                false,
            )
            .with_file(
                ToolFileAccessOperation::ReadWrite,
                "removed.rs".to_string(),
                false,
            ),
        &store,
    )
    .unwrap()
    .paths;
    assert_eq!(captures.len(), 2);

    // The call runs and produces a new state.
    std::fs::write(&kept, "after").unwrap();
    std::fs::remove_file(&removed).unwrap();

    let observed = complete(&captures, &store).unwrap();
    let by_path: HashMap<_, _> = observed.into_iter().collect();
    let kept_after = by_path.get(&kept).unwrap().as_deref().unwrap();
    assert_eq!(store.get(kept_after).as_deref(), Some(b"after".as_slice()));
    assert_eq!(
        by_path.get(&removed).unwrap(),
        &None,
        "a path that is gone afterwards has no post-image"
    );
}

/// Two different pre-states hash differently, so the journal can distinguish
/// "this file already had that content" from "this call is what changed it".
#[test]
fn distinct_contents_get_distinct_hashes_and_identical_contents_share_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    std::fs::write(dir.path().join("a.rs"), "one").unwrap();
    let first = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &write_accesses("a.rs"),
        &store,
    )
    .unwrap();
    std::fs::write(dir.path().join("b.rs"), "two").unwrap();
    let second = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &write_accesses("b.rs"),
        &store,
    )
    .unwrap();
    std::fs::write(dir.path().join("c.rs"), "one").unwrap();
    let third = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &write_accesses("c.rs"),
        &store,
    )
    .unwrap();

    assert_ne!(
        first.paths[0].before_hash, second.paths[0].before_hash,
        "different bytes must hash differently"
    );
    assert_eq!(
        first.paths[0].before_hash, third.paths[0].before_hash,
        "identical bytes share a hash (content addressing)"
    );
}

#[test]
fn relative_paths_resolve_against_the_workspace_and_normalize_lexically() {
    let root = Path::new("/ws/project");
    assert_eq!(
        normalize(root, Path::new("src/lib.rs")),
        Path::new("/ws/project/src/lib.rs")
    );
    assert_eq!(
        normalize(root, Path::new("./src/lib.rs")),
        Path::new("/ws/project/src/lib.rs")
    );
    assert_eq!(
        normalize(root, Path::new("src/../lib.rs")),
        Path::new("/ws/project/lib.rs"),
        "parent components must not survive as text"
    );
    assert_eq!(
        normalize(root, Path::new("/abs/file.rs")),
        Path::new("/abs/file.rs"),
        "an absolute path is already resolved"
    );
    // Symlinks are deliberately NOT resolved: the journal's identity for a file
    // is its lexical path, so a retargeted symlink does not silently redirect a
    // later `/undo`.
    assert_eq!(
        normalize(root, Path::new("link/to/file.rs")),
        Path::new("/ws/project/link/to/file.rs")
    );
}
