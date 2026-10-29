//! The write-ahead bracket, exercised against a real temporary directory and an
//! in-memory journal double.
//!
//! These tests assert the *ordering contract*, which is the whole reason the
//! middleware exists: the row exists before the mutation, and it is completed
//! with whatever is on disk afterwards — including when the tool failed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use nuo_tool::{RiskProfile, Tool, ToolAccesses, ToolContext, ToolError, ToolOutput};

/// One recorded journal row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recorded {
    row: JournalRow,
    after_hash: Option<String>,
    completed: bool,
}

#[derive(Default)]
struct MemJournal {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    rows: Mutex<Vec<Recorded>>,
}

impl PreImageStore for MemJournal {
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

#[async_trait]
impl WorkspaceJournalPort for MemJournal {
    fn store(&self) -> &dyn PreImageStore {
        self
    }

    async fn append(&self, row: JournalRow) -> Result<i64, String> {
        let mut rows = self.rows.lock().unwrap();
        rows.push(Recorded {
            row,
            after_hash: None,
            completed: false,
        });
        Ok(rows.len() as i64)
    }

    /// Mirrors the production adapter: a receipt arrives already complete, with no
    /// pending completion step. Implemented as its own entry point (rather than
    /// inferred from the row's fields inside `append`) so the double cannot quietly
    /// diverge from the adapter's semantics — the divergence that let a
    /// prematurely-completed row pass the test suite.
    async fn append_receipt(&self, row: JournalRow, _completed_at_ms: u64) -> Result<i64, String> {
        let mut rows = self.rows.lock().unwrap();
        // `JournalRow` carries no post-image: a receipt's content is what the call
        // reported, which is already part of its `effect_class`/reason and, for a
        // shell, the tool result. Nothing to record here beyond completion.
        rows.push(Recorded {
            after_hash: None,
            row,
            completed: true,
        });
        Ok(rows.len() as i64)
    }

    async fn complete(
        &self,
        effect_id: i64,
        after_hash: Option<String>,
        _completed_at_ms: u64,
    ) -> Result<bool, String> {
        let mut rows = self.rows.lock().unwrap();
        let index = (effect_id - 1) as usize;
        if let Some(record) = rows.get_mut(index)
            && !record.completed
        {
            record.after_hash = after_hash;
            record.completed = true;
            return Ok(true);
        }
        Ok(false)
    }
}

impl MemJournal {
    fn rows(&self) -> Vec<Recorded> {
        self.rows.lock().unwrap().clone()
    }
}

/// A stub tool whose declarations the tests control.
struct StubTool {
    name: &'static str,
    risk: RiskProfile,
    accesses: ToolAccesses,
}

impl StubTool {
    fn writer(name: &'static str, path: &str) -> Self {
        Self {
            name,
            risk: RiskProfile::IdempotentMutation,
            accesses: ToolAccesses::read_write_file(path.to_string()),
        }
    }

    fn shell(name: &'static str) -> Self {
        Self {
            name,
            risk: RiskProfile::ArbitraryExecution,
            accesses: ToolAccesses::all(),
        }
    }

    fn reader(name: &'static str, path: &str) -> Self {
        Self {
            name,
            risk: RiskProfile::ReadOnly,
            accesses: ToolAccesses::read_file(path.to_string()),
        }
    }
}

#[async_trait]
impl Tool for StubTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "stub"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn risk_profile(&self) -> RiskProfile {
        self.risk
    }

    fn accesses(&self, _arguments: &str) -> ToolAccesses {
        self.accesses.clone()
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::success("stub"))
    }
}

fn coordinates<'a>(thread_id: &'a str) -> CallCoordinates<'a> {
    CallCoordinates {
        thread_id,
        round_id: "r1",
        turn_id: "t1",
        call_id: "call-1",
        seq: 1,
    }
}

/// The ordering contract: the write-ahead row exists before the tool runs, with
/// the pre-image hash and no post-image.
#[tokio::test]
async fn begin_appends_the_pre_image_before_the_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "original").unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::writer("edit_text", "a.rs");

    let pending = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();

    assert_eq!(pending.rows.len(), 1);
    assert_eq!(pending.effect, EffectClass::FileBytes);
    let rows = journal.rows();
    assert_eq!(rows.len(), 1);
    let recorded = &rows[0];
    assert_eq!(recorded.row.op, JournalOpToken::Edit);
    assert_eq!(
        recorded.row.path.as_deref(),
        Some(file.to_string_lossy().as_ref())
    );
    assert_eq!(recorded.row.effect_class, "file_bytes");
    assert!(!recorded.completed, "the post-image cannot exist yet");
    // The stored pre-image is the bytes that were on disk.
    let hash = recorded.row.before_hash.as_deref().unwrap();
    assert_eq!(journal.get(hash).as_deref(), Some(b"original".as_slice()));
}

/// `finish` completes the row with what is on disk *after* the call.
#[tokio::test]
async fn finish_completes_with_the_post_image() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "original").unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::writer("edit_text", "a.rs");

    let pending = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();
    // The tool runs.
    std::fs::write(&file, "modified").unwrap();
    finish(&journal, pending).await;

    let rows = journal.rows();
    assert!(rows[0].completed);
    let after = rows[0].after_hash.as_deref().unwrap();
    assert_eq!(journal.get(after).as_deref(), Some(b"modified".as_slice()));
}

/// A tool that errored halfway may still have written bytes; the row must be
/// completed with what is actually there, or it would be permanently un-undoable.
#[tokio::test]
async fn finish_completes_even_when_the_call_failed_partway() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "original").unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::writer("edit_text", "a.rs");

    let pending = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();
    // A partial write: the tool wrote, then blew up.
    std::fs::write(&file, "half-written").unwrap();
    finish(&journal, pending).await;

    let rows = journal.rows();
    assert!(rows[0].completed, "the row must not stay unconfirmed");
    let after = rows[0].after_hash.as_deref().unwrap();
    assert_eq!(
        journal.get(after).as_deref(),
        Some(b"half-written".as_slice()),
        "the journal records what is there, not what the tool meant to write"
    );
}

/// A pure read is not journaled at all: this is the overwhelmingly common case
/// and must not touch the CAS or the writer.
#[tokio::test]
async fn reads_are_not_journaled() {
    let dir = tempfile::tempdir().unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::reader("read_text", "a.rs");

    let pending = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();

    assert!(pending.is_empty());
    assert!(journal.rows().is_empty(), "no row, no storage cost");
}

/// A call with no thread owner is skipped: a row nothing can find is storage
/// spent on a record no `/diff` will ever read.
#[tokio::test]
async fn calls_without_a_thread_owner_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "x").unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::writer("edit_text", "a.rs");

    let pending = begin(&journal, Some(dir.path()), coordinates(""), &tool, "{}")
        .await
        .unwrap();

    assert!(pending.is_empty());
    assert!(journal.rows().is_empty());
}

/// An arbitrary-execution call gets a receipt: recorded, classed, explained — and
/// with no pre-image, because it cannot be reversed.
#[tokio::test]
async fn shell_call_is_journaled_as_a_receipt_without_a_pre_image() {
    let dir = tempfile::tempdir().unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::shell("execute_command");

    let pending = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();
    assert!(pending.is_empty(), "nothing to complete after the call");

    let rows = journal.rows();
    assert_eq!(rows.len(), 1);
    let recorded = &rows[0];
    assert_eq!(recorded.row.effect_class, "opaque");
    assert_eq!(
        recorded.row.before_hash, None,
        "no pre-image for an opaque effect"
    );
    assert_eq!(
        recorded.row.path, None,
        "a shell command names no single file"
    );
    assert!(
        recorded
            .row
            .irreversible_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("arbitrary execution")),
        "the receipt must explain why it cannot be undone, got {:?}",
        recorded.row.irreversible_reason
    );
}

/// A file-mutating tool that declares no path fails closed: it is journaled as
/// `opaque`, so `/undo` refuses it explicitly instead of silently doing nothing.
#[tokio::test]
async fn unlocatable_file_mutation_is_journaled_as_opaque() {
    let dir = tempfile::tempdir().unwrap();
    let journal = MemJournal::default();
    let tool = StubTool {
        name: "mystery_writer",
        risk: RiskProfile::IdempotentMutation,
        accesses: ToolAccesses::none(),
    };

    let pending = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();
    assert!(pending.is_empty());

    let rows = journal.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.effect_class, "opaque");
    assert!(rows[0].row.irreversible_reason.is_some());
}

/// A creation is journaled as `create`, and after the call the post-image is the
/// new content — so reversal knows to remove rather than rewrite.
#[tokio::test]
async fn creation_is_journaled_with_no_pre_image() {
    let dir = tempfile::tempdir().unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::writer("write_file", "new.rs");

    let pending = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();
    assert_eq!(journal.rows()[0].row.op, JournalOpToken::Create);
    assert_eq!(journal.rows()[0].row.before_hash, None);

    std::fs::write(dir.path().join("new.rs"), "fresh").unwrap();
    finish(&journal, pending).await;

    let after = journal.rows()[0].after_hash.clone().unwrap();
    assert_eq!(journal.get(&after).as_deref(), Some(b"fresh".as_slice()));
}

/// Multiple declared paths get distinct rows with distinct seqs, so each can be
/// reversed independently.
#[tokio::test]
async fn multiple_paths_get_distinct_rows_with_densified_seqs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "a").unwrap();
    std::fs::write(dir.path().join("b.rs"), "b").unwrap();
    let journal = MemJournal::default();
    let tool = StubTool {
        name: "write_file",
        risk: RiskProfile::IdempotentMutation,
        accesses: ToolAccesses::read_write_file("a.rs".to_string()).with_file(
            nuo_tool::ToolFileAccessOperation::ReadWrite,
            "b.rs".to_string(),
            false,
        ),
    };

    begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();

    let rows = journal.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].row.seq, 1);
    assert_eq!(rows[1].row.seq, 2);
    assert_ne!(rows[0].row.path, rows[1].row.path);
}

/// A capture failure is a veto: the caller must not run the tool.
#[tokio::test]
async fn an_unreadable_pre_image_vetoes_the_call() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("adir")).unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::writer("edit_text", "adir");

    let error = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .expect_err("capture failure must veto");
    assert!(error.contains("cannot capture pre-image"), "got: {error}");
    assert!(
        journal.rows().is_empty(),
        "no row may be written for an uncapturable mutation"
    );
}

/// A recursive write claim cannot be captured per-path, so it is recorded as an
/// opaque receipt rather than a half-truth.
#[tokio::test]
async fn recursive_write_claim_is_journaled_as_opaque_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let journal = MemJournal::default();
    let tool = StubTool {
        name: "wipe_tree",
        risk: RiskProfile::IdempotentMutation,
        accesses: ToolAccesses::write_tree("src"),
    };

    begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();

    let rows = journal.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.effect_class, "opaque");
    assert!(rows[0].completed || rows[0].after_hash.is_none());
}

/// `INV-EFFECT-07` at the middleware layer: a withheld path is still *journaled*
/// (the change happened and `/diff` must show it) but carries the reason and no
/// pre-image, so `/undo` refuses it with an explanation instead of silently
/// omitting the call or failing on a missing blob.
#[tokio::test]
async fn withheld_paths_are_journaled_with_a_reason_and_no_preimage() {
    use nuo_tool::RiskProfile;

    struct SecretWriter;
    #[async_trait]
    impl Tool for SecretWriter {
        fn name(&self) -> &str {
            "write_file"
        }
        fn description(&self) -> &str {
            "writes a .env"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn risk_profile(&self) -> RiskProfile {
            RiskProfile::IdempotentMutation
        }
        fn accesses(&self, _arguments: &str) -> ToolAccesses {
            ToolAccesses::write_file(".env".to_string())
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success("wrote"))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
    let journal = MemJournal::default();
    let pending = begin(
        &journal,
        Some(dir.path()),
        coordinates("th"),
        &SecretWriter,
        "{}",
    )
    .await
    .unwrap();

    let recorded = &journal.rows()[0].row;
    // A file write stays `file_bytes` — it is not arbitrary execution — and the row
    // explains its own irreversibility.
    assert_eq!(recorded.effect_class, "file_bytes");
    assert!(
        recorded
            .irreversible_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("credentials")),
        "the row must explain why it cannot be undone, got {:?}",
        recorded.irreversible_reason
    );
    assert_eq!(recorded.before_hash, None);
    assert!(!pending.is_empty(), "the row is still journaled");

    // The post-image is not captured either: it is equally a credential.
    let blobs_before = journal.blobs.lock().unwrap().len();
    finish(&journal, pending).await;
    assert_eq!(
        journal.blobs.lock().unwrap().len(),
        blobs_before,
        "completing a withheld row must not store its post-image"
    );
    assert!(
        journal.rows()[0].after_hash.is_none(),
        "no post-image may be recorded for a withheld path"
    );
}

/// A **withheld** pre-image is still a write-ahead row: it must stay incomplete until
/// the call returns.
///
/// This pins a defect that the suite missed. The adapter used to choose the phase by
/// inspecting nullability (`before_hash.is_none() && irreversible_reason.is_some()`),
/// which read a *refused capture* as "this effect has no recovery point" and stamped
/// the row complete before the tool ran. Three things broke at once: the row asserted
/// completion before the mutation, a crash mid-write left a falsely-confirmed record,
/// and `finish` then called `complete` on an already-complete row and got `Ok(false)`
/// for every withheld path. The double did not reproduce it because it inferred
/// differently from the adapter — so the phase is now declared, and this test asserts
/// the declaration rather than a coincidence.
#[tokio::test]
async fn withheld_preimage_is_journaled_write_ahead_not_as_a_receipt() {
    use nuo_tool::RiskProfile;

    struct SecretWriter;
    #[async_trait]
    impl Tool for SecretWriter {
        fn name(&self) -> &str {
            "write_file"
        }
        fn description(&self) -> &str {
            "writes a .env"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn risk_profile(&self) -> RiskProfile {
            RiskProfile::IdempotentMutation
        }
        fn accesses(&self, _arguments: &str) -> ToolAccesses {
            ToolAccesses::write_file(".env".to_string())
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success("wrote"))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
    let journal = MemJournal::default();
    let pending = begin(
        &journal,
        Some(dir.path()),
        coordinates("th"),
        &SecretWriter,
        "{}",
    )
    .await
    .unwrap();

    // The row exists and is *incomplete*: the write has not happened yet.
    let recorded = &journal.rows()[0];
    assert_eq!(recorded.row.before_hash, None, "capture was refused");
    assert!(
        recorded.row.irreversible_reason.is_some(),
        "and the refusal is explained"
    );
    assert!(
        !recorded.completed,
        "a withheld pre-image still has a recovery window to protect \
         (the mutation has not run), so it must not be stamped complete"
    );

    // Completion is the phase that closes it, and it must actually take effect —
    // not report a lost race against a row that was never pending.
    super::super::middleware::finish(&journal, pending).await;
    assert!(
        journal.rows()[0].completed,
        "finish must complete the pending row"
    );
}

/// A genuine receipt — a shell effect — arrives already complete, because there is no
/// recovery point and therefore no window to protect.
#[tokio::test]
async fn shell_receipt_arrives_already_complete() {
    let dir = tempfile::tempdir().unwrap();
    let journal = MemJournal::default();
    let tool = StubTool::shell("execute_command");

    let pending = begin(&journal, Some(dir.path()), coordinates("th"), &tool, "{}")
        .await
        .unwrap();

    assert!(pending.is_empty(), "a receipt has nothing to complete");
    assert!(
        journal.rows()[0].completed,
        "a receipt is complete on arrival"
    );
}
