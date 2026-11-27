//! `INV-EFFECT-07`: a credential file's bytes never enter the content store.
//!
//! This is the invariant with the worst failure mode in the whole journal. The store
//! outlives the session and has no expiry of its own, so a captured `.env` is a
//! second, long-lived, unencrypted copy of a live credential the user never asked to
//! have copied — and ADR-0032's `[INV-SEC-01]` already forbids exactly that class of
//! leakage for nuo's own state.

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

fn read_write_accesses(path: &str) -> ToolAccesses {
    ToolAccesses::read_write_file(path.to_string())
}

#[test]
fn credential_files_are_never_written_to_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = "OPENAI_API_KEY=sk-live-abcdef0123456789\nDB_PASSWORD=hunter2\n";
    std::fs::write(dir.path().join(".env"), secrets).unwrap();
    let store = MemStore::default();

    let outcome = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &read_write_accesses(".env"),
        &store,
    )
    .unwrap();

    assert_eq!(outcome.paths.len(), 1);
    let capture = &outcome.paths[0];
    assert!(
        !capture.capture.permits_capture(),
        "a .env must be classified as withheld"
    );
    assert_eq!(
        capture.before_hash, None,
        "no pre-image hash may be recorded"
    );
    assert!(
        outcome.stored_hashes.is_empty(),
        "nothing may be written to the store"
    );
    assert_eq!(store.len(), 0, "the store must be untouched");

    // ...and the post-image is not stored either, since it is equally a credential.
    let observed = complete(&outcome.paths, &store).unwrap();
    assert_eq!(observed[0].1, None);
    assert_eq!(store.len(), 0, "neither image may be captured");
}

#[test]
fn key_material_is_withheld_in_every_conventional_location() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();
    for relative in [
        "id_rsa",
        ".ssh/id_ed25519",
        "certs/server.pem",
        ".aws/credentials",
        ".git-credentials",
    ] {
        let absolute = dir.path().join(relative);
        std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        std::fs::write(&absolute, "-----BEGIN PRIVATE KEY-----\nsecret\n").unwrap();
        let outcome = prepare(
            dir.path(),
            EffectClass::FileBytes,
            &read_write_accesses(relative),
            &store,
        )
        .unwrap();
        assert!(
            !outcome.paths[0].capture.permits_capture(),
            "{relative} must be withheld"
        );
        assert!(outcome.stored_hashes.is_empty(), "{relative}");
    }
    assert_eq!(store.len(), 0, "no key material reached the store");
}

/// Ordinary source next to a secret is captured normally: the rule is per-path, not
/// per-project, or a single `.env` would disable undo for the whole repository.
#[test]
fn a_credential_file_does_not_taint_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(dir.path().join("app.rs"), "fn main() {}\n").unwrap();
    let store = MemStore::default();

    let before = store.len();
    let outcome = prepare(
        dir.path(),
        EffectClass::FileBytes,
        &ToolAccesses::none()
            .with_file(
                ToolFileAccessOperation::ReadWrite,
                ".env".to_string(),
                false,
            )
            .with_file(
                ToolFileAccessOperation::ReadWrite,
                "app.rs".to_string(),
                false,
            ),
        &store,
    )
    .unwrap();

    let withheld = outcome
        .paths
        .iter()
        .find(|capture| capture.path.ends_with(".env"))
        .expect("the .env row exists");
    let captured = outcome
        .paths
        .iter()
        .find(|capture| capture.path.ends_with("app.rs"))
        .expect("the source row exists");
    assert!(!withheld.capture.permits_capture());
    assert!(captured.capture.permits_capture());
    assert!(captured.before_hash.is_some(), "the source is captured");
    assert_eq!(store.len(), before + 1, "exactly one pre-image stored");
}
