//! Workspace effect capture: observing a mutating tool call so the journal can
//! describe — and, for reversible classes, reverse — what it changed (ADR-0125).
//!
//! This module owns the *observation*, not the *policy*. It answers three
//! questions about one tool call, and it answers them from the filesystem and the
//! tool's own declarations rather than from the model's prose:
//!
//! 1. **Which paths does this call mutate?** — from
//!    [`Tool::accesses`](nuo_wire::Tool::accesses), the same declaration the
//!    scheduler uses to serialize concurrent calls. Not from the argument JSON:
//!    a tool that declares a write is a tool that writes, and a tool that writes
//!    without declaring it is caught by `debug_assert_safe_targets`.
//! 2. **What was there before?** — a pre-image read and content-addressed
//!    before the tool runs.
//! 3. **What is there after?** — a post-image read after it returns.
//!
//! ## Why the pre-image is read, not trusted
//!
//! `ToolOutput::Patch` already carries `old` and `new` — the tool's own report of
//! what it changed. That report is **not** used as the pre-image source. A
//! tool's self-report can be incomplete (an `edit_text` that truncated a file it
//! also rewrote), stale (a write that raced another process), or simply wrong
//! (a bug). The journal's promise is to the *user's bytes*, so it records what
//! the filesystem actually held, read independently of the tool.
//!
//! ## The `Opaque` boundary is not a gap
//!
//! A call whose declared [`EffectClass`] is not
//! `FileBytes` gets a **receipt only**: no pre-image, no reversal promise. For
//! `execute_command` that is the whole point — the bash-policy module already
//! documents why command text cannot bound a shell's effects, and guessing here
//! would re-introduce that failure in the one place the user is told their work
//! is recoverable. See [`EffectClass::Opaque`].
//!
//! ## Crash ordering
//!
//! Capture and append happen **before** the mutation, in this order:
//!
//! ```text
//!   read pre-image  →  store bytes in CAS  →  append write-ahead row  →  mutate
//! ```
//!
//! Each step is a precondition of the next. A pre-image that never reached the
//! CAS must not produce a row (the row would reference content that does not
//! exist); a row that never committed must not produce a mutation (the write
//! would be silently irreversible). [`prepare`] therefore returns an error the
//! caller must treat as "do not execute this tool", and the harness does exactly
//! that.

use std::path::{Path, PathBuf};

use nuo_tool::{EffectClass, ToolAccesses, ToolFileAccessOperation};

/// One path a mutating call will touch, with the operation it will perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedPath {
    /// Absolute, lexically normalized path the call will mutate.
    pub path: PathBuf,
    /// What the call will do to it. Decided from the declaration *and* the
    /// observed pre-state: a `Write` to a path that does not exist is a
    /// [`JournalOpToken::Create`] regardless of how the tool describes itself.
    pub op: CapturedOp,
    /// Content hash of the pre-image, `None` when the path did not exist **or**
    /// when its content was withheld from capture.
    pub before_hash: Option<String>,
    /// Whether this path's bytes were eligible to be written into the store.
    ///
    /// A withheld path carries no pre-image, so it cannot be reversed — and the row
    /// must say so rather than presenting an undoable-looking change whose restore
    /// will fail. See [`nuo_tool::residency`].
    pub capture: nuo_tool::CaptureEligibility,
}

/// What a capture observed the call will do.
///
/// Narrower than [`JournalOp`](nuo_persistence::workspace_effects::JournalOp) in
/// the sense that it is decided at capture time from the pre-state, which is the
/// only moment that decision can be made honestly (`Edit` vs `Create` depends on
/// whether the file existed *before* the call).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturedOp {
    /// The path did not exist; the call brings it into being.
    Create,
    /// The path existed; the call rewrites it.
    Edit,
}

/// The result of capturing one mutating call: the rows the caller must append
/// before executing, and the paths whose post-image must be recorded after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOutcome {
    /// Reversibility class of the call, after reconciling its declared ceiling
    /// with the accesses actually observed.
    pub effect: EffectClass,
    /// Per-path captures. Empty for a non-reversible class: a receipt needs no
    /// pre-image, only the post-state, which the caller observes itself.
    pub paths: Vec<CapturedPath>,
    /// CAS hashes already stored, so the caller can register them for GC
    /// liveness. Only pre-images appear here; post-images are stored by
    /// [`complete`].
    pub stored_hashes: Vec<String>,
}

/// Port for content-addressed storage, so capture is testable without a real
/// blob store and the harness is not bound to `nuo-persistence`'s on-disk layout.
pub trait PreImageStore: Send + Sync {
    /// Store `bytes` and return their content hash. Implementations must be
    /// idempotent: storing the same bytes twice returns the same hash.
    fn put(&self, bytes: &[u8]) -> Result<String, String>;

    /// Read bytes by hash, `None` when absent.
    fn get(&self, hash: &str) -> Option<Vec<u8>>;
}

/// The mutating file accesses a call declares, in declaration order.
///
/// Only `Write` and `ReadWrite` count: a `Read` declaration is not a mutation,
/// and `Search` is read-side by definition. `ToolAccess::All` is the
/// global-exclusive marker — a call claiming unbounded state cannot name a path,
/// so it contributes nothing here and the call is classified `Opaque` upstream
/// by the tool's own ceiling.
fn mutating_paths(accesses: &ToolAccesses) -> Vec<(PathBuf, ToolFileAccessOperation)> {
    accesses
        .0
        .iter()
        .filter_map(|access| match access {
            nuo_tool::ToolAccess::File {
                operation,
                path,
                recursive,
            } if matches!(
                operation,
                ToolFileAccessOperation::Write | ToolFileAccessOperation::ReadWrite
            ) =>
            {
                // A recursive write names a subtree, not one file. The journal
                // records per-path pre-images, so a recursive claim cannot be
                // captured: promoting it to `Opaque` is the honest answer.
                if *recursive {
                    None
                } else {
                    Some((PathBuf::from(path), *operation))
                }
            }
            _ => None,
        })
        .collect()
}

/// Read a path's current bytes, distinguishing "absent" from "unreadable".
///
/// The distinction matters: an absent path is a legitimate `Create` pre-state
/// (`None`), while an unreadable one (permissions, a directory, invalid UTF-8) is
/// a failure to observe, and the caller must not mutate what it could not
/// capture.
fn read_pre_image(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot capture pre-image of {path:?}: {error}")),
    }
}

/// Capture one call's effects **before** it executes.
///
/// `effect_ceiling` is the tool's declared
/// [`effect_class`](nuo_wire::Tool::effect_class) and `accesses` its declared
/// accesses for these arguments; the two are reconciled here exactly as
/// [`effect_class_for`](nuo_wire::Tool::effect_class_for) does, so a tool cannot
/// claim reversibility it cannot support.
///
/// Returns [`CaptureOutcome::paths`] empty and [`CaptureOutcome::effect`] the
/// unreconciled ceiling when the call is not file-reversible: such a call is
/// journaled as a receipt and needs no pre-image.
pub fn prepare(
    workspace_root: &Path,
    effect_ceiling: EffectClass,
    accesses: &ToolAccesses,
    store: &dyn PreImageStore,
) -> Result<CaptureOutcome, String> {
    let mutating = mutating_paths(accesses);
    let effect = reconcile(effect_ceiling, &mutating);

    if effect != EffectClass::FileBytes {
        return Ok(CaptureOutcome {
            effect,
            paths: Vec::new(),
            stored_hashes: Vec::new(),
        });
    }

    let mut paths = Vec::with_capacity(mutating.len());
    let mut stored_hashes = Vec::new();
    for (declared, _operation) in mutating {
        let absolute = normalize(workspace_root, &declared);
        // Residency before capture (`INV-EFFECT-07`): a credential file's bytes must
        // never enter the store, which outlives the session and has no expiry of its
        // own. The call still runs and is still recorded — it simply cannot promise a
        // reversal, and says so.
        let capture = nuo_tool::residency::classify(&absolute);
        if !capture.permits_capture() {
            paths.push(CapturedPath {
                path: absolute,
                op: CapturedOp::Edit,
                before_hash: None,
                capture,
            });
            continue;
        }
        let pre = read_pre_image(&absolute)?;
        let (op, before_hash) = match pre {
            Some(bytes) => {
                let hash = store.put(&bytes)?;
                stored_hashes.push(hash.clone());
                (CapturedOp::Edit, Some(hash))
            }
            None => (CapturedOp::Create, None),
        };
        paths.push(CapturedPath {
            path: absolute,
            op,
            before_hash,
            capture,
        });
    }

    Ok(CaptureOutcome {
        effect,
        paths,
        stored_hashes,
    })
}

/// Capture the post-image of each captured path **after** the call returns.
///
/// Returns `(path, after_hash)` pairs; a path that no longer exists after the
/// call yields `None` (a delete, or a call that removed what it created). The
/// caller completes the write-ahead rows with these.
pub fn complete(
    captures: &[CapturedPath],
    store: &dyn PreImageStore,
) -> Result<Vec<(PathBuf, Option<String>)>, String> {
    let mut observed = Vec::with_capacity(captures.len());
    for capture in captures {
        // A withheld path's post-image is equally credential-bearing, so it is not
        // stored either. `None` means "no post-image recorded", which is exactly
        // right: the row cannot be compared against, so it cannot be reversed.
        if !capture.capture.permits_capture() {
            observed.push((capture.path.clone(), None));
            continue;
        }
        match std::fs::read(&capture.path) {
            Ok(bytes) => observed.push((capture.path.clone(), Some(store.put(&bytes)?))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                observed.push((capture.path.clone(), None));
            }
            Err(error) => {
                return Err(format!(
                    "cannot capture post-image of {:?}: {error}",
                    capture.path
                ));
            }
        }
    }
    Ok(observed)
}

/// Reconcile a declared ceiling with the observed mutating accesses.
///
/// Deliberately a local mirror of [`nuo_tool::reconciling`] over a narrower input
/// (path tuples rather than a `ToolAccesses`), because the two answer the same
/// question from different evidence: the tool crate reconciles against its own
/// declaration, this reconciles against what the capture actually resolved. Both
/// promote severity and neither demotes.
fn reconcile(ceiling: EffectClass, mutating: &[(PathBuf, ToolFileAccessOperation)]) -> EffectClass {
    match ceiling {
        // A file-mutating ceiling with no locatable path cannot capture a
        // pre-image, so it must not claim file reversibility.
        EffectClass::FileBytes if mutating.is_empty() => EffectClass::Opaque,
        // An observed write cannot be hidden behind a weaker self-description.
        EffectClass::None if !mutating.is_empty() => EffectClass::FileBytes,
        other => other,
    }
}

/// Resolve a declared path to an absolute, lexically normalized path.
///
/// Absolute already-absolute paths pass through; relative ones resolve against
/// the workspace root. Lexical normalization (no symlink resolution) is
/// deliberate: the journal's identity for a file must be stable across a session
/// even if a symlink is retargeted mid-way, and `/undo` writing through the
/// symlink it recorded is the behaviour a user expects.
pub fn normalize(workspace_root: &Path, declared: &Path) -> PathBuf {
    let joined = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        workspace_root.join(declared)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                // Pop only a normal component; a leading `..` at the root of a
                // relative path must survive rather than escape silently.
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// A journal `seq` for one call within a batch.
///
/// The journal's ordering unit is the *observation*, and the only honest ordering
/// is the one the journal itself assigns. Two constraints collide here:
///
/// - Rows must sort in the order the calls were observed, so `/undo` unwinds a
///   round newest-first correctly.
/// - Concurrent calls in one batch must never collide on a seq, or the ordering is
///   ambiguous exactly when several writes touched the same path.
///
/// A per-thread `MAX(seq) + 1` read would serialize every call behind the writer
/// for a value that carries no cross-process meaning. Instead the seq is derived
/// from the call's own identity (hash of round/turn/call id) inside a range that
/// is stable for a retried call and distinct for a distinct call. `id` — assigned
/// by SQLite's AUTOINCREMENT — remains the tiebreaker in every read, so a hash
/// collision degrades to "ordered by insertion", never to "unordered".
pub fn next_seq(round_id: &str, turn_id: &str, call_id: &str) -> i64 {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(round_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(turn_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(call_id.as_bytes());
    let digest = hasher.finalize();
    // 48 bits keeps the value comfortably inside i64 while making an accidental
    // collision within one session vanishingly unlikely.
    let mut bytes = [0u8; 8];
    bytes[..6].copy_from_slice(&digest[..6]);
    (u64::from_be_bytes(bytes) >> 16) as i64
}

#[cfg(test)]
mod residency_tests;
#[cfg(test)]
mod tests;

pub mod middleware;
pub mod reversal;

/// The harness's port to the durable journal.
///
/// Deliberately narrow and deliberately **async**, because every write here is
/// awaited on the tool-call path: a fire-and-forget append would let the tool run
/// before its pre-image was durable, which is precisely the crash window the
/// write-ahead row exists to close. A port (rather than a direct
/// `PersistenceHandle`) keeps the harness free of the storage crate's concrete
/// handle type and lets tests substitute an in-memory recorder.
///
/// Implementors own the CAS writes implied by `before_hash` / `after_hash`;
/// [`prepare`] and [`complete`] are the capture half and are called by the
/// middleware, not by the port.
#[async_trait::async_trait]
pub trait WorkspaceJournalPort: Send + Sync {
    /// Content-addressed storage for pre/post images.
    fn store(&self) -> &dyn PreImageStore;

    /// Append a **write-ahead** row, returning its journal id.
    ///
    /// Called *before* the mutation. The row is incomplete (`completed_at_ms` unset)
    /// until [`complete`](Self::complete) is called, and a failure here must abort the
    /// call.
    ///
    /// This method implies a lifecycle; it never infers one. An earlier revision let
    /// the port decide between write-ahead and already-complete by inspecting
    /// nullability (`before_hash.is_none() && irreversible_reason.is_some()`), which
    /// mis-classified a withheld pre-image — a row with no `before_hash` *because
    /// capture was refused*, not because the effect is a receipt — and stamped it
    /// complete before the tool ran. The phase is a property of intent, so it is
    /// declared by the caller, never reverse-engineered from which fields are null.
    async fn append(&self, row: JournalRow) -> Result<i64, String>;

    /// Append a row that needs **no recovery point**, already complete.
    ///
    /// For effects whose journal value is the receipt — what ran, when, what it left
    /// behind — rather than the ability to restore. There is no pre-image to protect,
    /// so there is no mid-call window to preserve and no [`complete`](Self::complete)
    /// to wait for.
    async fn append_receipt(&self, row: JournalRow, completed_at_ms: u64) -> Result<i64, String>;

    /// Complete a write-ahead row with its post-image. `Ok(false)` means another
    /// writer completed it first (a lost race, not an error).
    async fn complete(
        &self,
        effect_id: i64,
        after_hash: Option<String>,
        completed_at_ms: u64,
    ) -> Result<bool, String>;
}

/// One journal row, as the harness describes it before persistence.
///
/// A plain struct rather than the storage crate's borrowed/owned pair: the
/// harness should not need to know that the storage layer distinguishes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRow {
    pub thread_id: String,
    pub round_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub seq: i64,
    pub tool_name: String,
    pub path: Option<String>,
    pub op: JournalOpToken,
    pub effect_class: String,
    pub before_hash: Option<String>,
    pub irreversible_reason: Option<String>,
}

/// The journal's operation vocabulary, mirrored here so the harness does not have
/// to depend on the storage crate to name an operation.
///
/// Deliberately a mirror and not a re-export: the storage crate owns the durable
/// spelling, and a conversion boundary is where a rename should fail to compile
/// rather than silently write a token the reader cannot parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalOpToken {
    Create,
    Edit,
    Delete,
}

impl JournalOpToken {
    /// The durable token, matching `nuo_persistence::workspace_effects::JournalOp`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Edit => "edit",
            Self::Delete => "delete",
        }
    }
}
