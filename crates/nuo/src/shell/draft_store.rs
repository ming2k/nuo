//! The parked-draft ledger: one unsent composer draft per thread, durable.
//!
//! # Why this exists
//!
//! A tab is a *view*. The text a user is typing belongs to the **thread** they
//! are typing it into. The pre-ADR-0247 model stored the draft on the tab
//! (`TabContextState::input`), which had three consequences, each of them a
//! reported data loss:
//!
//! 1. a **cross-thread** tab switch tears the whole TUI down and re-attaches
//!    (the server hosts one session per client), so the departing tab's struct
//!    was carried into a *new* `App` whose first frame then ran the
//!    viewed-session change hook and blanked the composer — the draft survived
//!    the handoff and died immediately afterwards;
//! 2. a tab **replaced** in place (`/threads` picking a thread over an empty
//!    one) inherited the arriving tab's `Default` context, silently overwriting
//!    whatever the user had typed;
//! 3. nothing was durable, so a crash, a `kill`, or a re-attach lost it.
//!
//! Keying the draft by thread id and writing it under the state directory fixes
//! all three: the identity that owns the text survives every view transition
//! *and* the process, and two views of the same thread can never disagree
//! because there is no second copy to disagree with.
//!
//! # Shape
//!
//! One JSON file per thread under `$XDG_STATE_HOME/nuo/drafts/`. Not one map for
//! everything: two `nuo` instances in different projects never rewrite each
//! other's entries, deleting a draft is an `unlink` rather than a merge rule,
//! and a truncated file costs exactly one draft instead of all of them.
//!
//! The ledger is a **write-through mirror**, never the sole owner of the live
//! draft: the composer keeps the focused thread's draft in its own buffers and
//! the ledger holds the last parked image of every thread, including that one.
//! The asymmetry is deliberate — a crash must lose at most the keystrokes since
//! the last autosave, never the whole draft.
//!
//! # Bounds
//!
//! Attachment payloads are base64 images, so residency is capped by bytes with
//! LRU eviction; an evicted draft is still on disk and is re-read on demand. The
//! bound is on *memory*, never on what may be parked.
//!
//! # Failure policy
//!
//! A draft is unsent working state, not durable history. A read that fails
//! yields "no draft" and a warning; a write that fails keeps the entry dirty and
//! retries after a backoff. Neither may block the UI or fail a frame.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nuo_tool::ImagePart;
use nuo_view::composer_draft::ComposerDraft;

/// Resident-draft memory budget. Every resident draft is also on disk, so
/// eviction is free; the cap exists only so a session that visits twenty
/// image-bearing tabs does not pin a hundred megabytes of base64.
const RESIDENT_BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// How long a failed write waits before it is retried. Short enough that a
/// transient `ENOSPC`/`EACCES` heals within one editing session, long enough
/// that a persistent failure does not turn every frame into a syscall.
const WRITE_RETRY_BACKOFF: Duration = Duration::from_secs(15);

/// A borrowed view of the composer's live buffers.
///
/// The ledger takes this rather than an owned [`ComposerDraft`] so an autosave
/// can decide "nothing changed" **without cloning the attachments**: a staged
/// base64 screenshot is megabytes, and copying one per heartbeat to discover it
/// is identical would make the durability feature the most expensive thing in
/// the loop.
pub struct LiveDraft<'a> {
    pub text: &'a str,
    /// Caret as a byte offset into `text`.
    pub cursor_position: usize,
    pub pending_images: &'a [ImagePart],
    pub pending_text_pastes: &'a [String],
    pub input_scroll: usize,
}

impl LiveDraft<'_> {
    /// A cheap content signal: everything that can change without a copy, with
    /// the text hashed instead of held.
    pub fn signal(&self) -> DraftSignal {
        DraftSignal::of(self)
    }

    /// Materialize an owned draft (only called when the signal says it differs).
    pub fn to_owned(&self) -> ComposerDraft {
        ComposerDraft::capture(
            self.text.to_string(),
            self.cursor_position,
            self.pending_images.to_vec(),
            self.pending_text_pastes.to_vec(),
            self.input_scroll,
        )
    }
}

/// Cheap identity of a draft's content.
///
/// Two drafts with equal signals are treated as the same draft. The text is
/// hashed (so an edit is always noticed), while attachments are compared by
/// `(count, total bytes)`: the composer never mutates a staged payload in place
/// — a chip is atomic, and editing one drops the payload rather than rewriting
/// it — so the pair is a sound key and costs O(attachments) instead of O(bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftSignal {
    text_hash: u64,
    text_len: usize,
    cursor_position: usize,
    input_scroll: usize,
    image_count: usize,
    image_bytes: usize,
    paste_count: usize,
    paste_bytes: usize,
}

impl DraftSignal {
    fn of(live: &LiveDraft<'_>) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        live.text.hash(&mut hasher);
        Self {
            text_hash: hasher.finish(),
            text_len: live.text.len(),
            cursor_position: live.cursor_position,
            input_scroll: live.input_scroll,
            image_count: live.pending_images.len(),
            image_bytes: live
                .pending_images
                .iter()
                .map(|image| image.mime.len() + image.data.len())
                .sum(),
            paste_count: live.pending_text_pastes.len(),
            paste_bytes: live.pending_text_pastes.iter().map(String::len).sum(),
        }
    }
}

/// The on-disk envelope. `thread_id` is inside the file because the file name is
/// a sanitized derivative of it — the directory is readable and collision-free
/// without trusting a sanitizer to also be reversible.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StoredDraft {
    thread_id: String,
    draft: ComposerDraft,
}

/// One thread's parked draft plus its residency bookkeeping.
#[derive(Debug, Clone)]
struct Entry {
    draft: ComposerDraft,
    signal: DraftSignal,
    bytes: usize,
}

/// Parked composer drafts, keyed by thread id.
pub struct DraftLedger {
    root: PathBuf,
    /// Resident drafts. An absent key means "not resident", *not* "no draft":
    /// the file may still exist and is re-read by [`Self::parked`].
    entries: BTreeMap<String, Entry>,
    /// Least-recently-used order of resident keys, for the memory cap.
    recency: VecDeque<String>,
    /// Threads whose file no longer matches their resident draft.
    dirty: BTreeSet<String>,
    /// Threads whose draft became empty: the file must be unlinked.
    removed: BTreeSet<String>,
    /// Every thread this ledger knows holds a draft — resident or evicted but
    /// still on disk. Lets [`Self::has_stored`] answer without a disk read, and
    /// keeps an evicted entry from being mistaken for "no draft" by the
    /// thread-emptiness predicate.
    known: BTreeSet<String>,
    /// Set while a failed write is in backoff.
    retry_at: Option<Instant>,
    /// Whether the ledger reaches the filesystem at all. `false` in unit tests,
    /// which construct an `App` directly and must never touch — let alone
    /// truncate — the developer's real draft directory.
    persist: bool,
}

impl std::fmt::Debug for DraftLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftLedger")
            .field("root", &self.root)
            .field("resident", &self.entries.len())
            .field("dirty", &self.dirty.len())
            .field("removed", &self.removed.len())
            .field("persist", &self.persist)
            .finish_non_exhaustive()
    }
}

impl DraftLedger {
    /// A ledger that never touches the filesystem.
    ///
    /// Test-only by construction: production always goes through
    /// [`Self::load`], the one place the directory is read.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self::empty(PathBuf::new(), false)
    }

    /// Load the ledger from `root`, tolerating a missing directory and any
    /// individually unreadable file.
    ///
    /// Unreadable files are *left in place*: a file this version cannot parse
    /// may be the only copy of the user's text, and deleting it to tidy up is
    /// exactly the irreversible action a tolerant loader must not take.
    pub fn load(root: PathBuf) -> Self {
        let mut ledger = Self::empty(root, true);
        ledger.refresh_from_disk();
        ledger
    }

    /// Load a server-scoped draft ledger under `<base_dir>/<scope-dir>/`
    /// (ADR-0250 `[INV-DRAFT-06]`). Partitioning by server identity prevents
    /// cross-server ID collisions and makes catalog reconciliation sound.
    pub fn load_for_scope(base_dir: PathBuf, server_scope: &str) -> Self {
        Self::load(base_dir.join(scope_dir_name(server_scope)))
    }

    /// All thread IDs that currently hold a parked draft in this server scope
    /// (resident or evicted-on-disk), for batch catalog reconciliation (ADR-0250).
    pub fn known_thread_ids(&self) -> Vec<String> {
        self.known.iter().cloned().collect()
    }

    /// Authoritatively unlink drafts whose threads the server confirmed do not
    /// exist in its catalog (ADR-0250 `[INV-DRAFT-07]`). Never touches
    /// `active_thread_id` (which may be a fresh unpersisted thread).
    pub fn reconcile_missing(
        &mut self,
        missing_thread_ids: &[String],
        active_thread_id: Option<&str>,
    ) -> usize {
        let mut removed = 0usize;
        for id in missing_thread_ids {
            if active_thread_id.is_some_and(|active| active == id.as_str()) {
                continue;
            }
            if self.known.contains(id) {
                self.forget(id);
                removed += 1;
            }
        }
        if removed > 0 {
            self.flush();
        }
        removed
    }

    fn empty(root: PathBuf, persist: bool) -> Self {
        Self {
            root,
            entries: BTreeMap::new(),
            recency: VecDeque::new(),
            dirty: BTreeSet::new(),
            removed: BTreeSet::new(),
            known: BTreeSet::new(),
            retry_at: None,
            persist,
        }
    }

    /// Read every stored draft, keeping what fits in the resident budget.
    fn refresh_from_disk(&mut self) {
        let Ok(directory) = std::fs::read_dir(&self.root) else {
            return;
        };
        let mut paths: Vec<PathBuf> = directory
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        // Directory order is arbitrary; sorting makes the resident set
        // reproducible. Nothing here depends on *which* drafts stay resident.
        paths.sort();
        for path in paths {
            let Some(stored) = read_stored(&path) else {
                continue;
            };
            self.insert_resident(stored.thread_id, stored.draft);
        }
    }

    /// The parked draft for `thread_id`, if any.
    ///
    /// A cache miss reads that thread's file. A draft that is empty by
    /// [`ComposerDraft::is_empty`] is reported as absent: the ledger stores
    /// *work*, and an empty entry has none.
    pub fn parked(&mut self, thread_id: &str) -> Option<&ComposerDraft> {
        if !self.entries.contains_key(thread_id) {
            self.load_one(thread_id);
        }
        self.touch(thread_id);
        self.entries
            .get(thread_id)
            .map(|entry| &entry.draft)
            .filter(|draft| !draft.is_empty())
    }

    /// Park the live composer draft as `thread_id`'s draft.
    ///
    /// Returns whether anything was written to memory, so a caller can report
    /// "the draft moved" and a caller in a loop can stay invisible when it did
    /// not. An unchanged draft is not copied at all.
    ///
    /// An empty draft *removes* the entry rather than storing a blank: sending
    /// the message, or clearing the composer, is precisely the moment the user
    /// made the draft stop existing, and a blank tombstone would make the next
    /// restore indistinguishable from a real empty buffer.
    pub fn park_live(&mut self, thread_id: &str, live: LiveDraft<'_>) -> bool {
        if thread_id.is_empty() {
            // No thread identity yet (a pre-attach client): there is nowhere to
            // park it and nothing to restore it into. Dropping it is the honest
            // answer, and it cannot happen once the session id is known.
            return false;
        }
        let signal = live.signal();
        if live.text.is_empty()
            && live.pending_images.is_empty()
            && live.pending_text_pastes.is_empty()
        {
            if self.known.contains(thread_id) {
                self.forget(thread_id);
                return true;
            }
            return false;
        }
        if self
            .entries
            .get(thread_id)
            .is_some_and(|entry| entry.signal == signal)
        {
            // Unchanged: no copy, no write, no dirty mark. This is what makes
            // the autosave heartbeat free while the user is idle.
            self.touch(thread_id);
            return false;
        }
        let draft = live.to_owned();
        self.insert(thread_id.to_string(), draft, signal);
        if self.persist {
            self.removed.remove(thread_id);
            self.dirty.insert(thread_id.to_string());
        }
        true
    }

    /// Park an owned draft (the boot/restore path, where one already exists).
    ///
    /// A draft that is empty by [`ComposerDraft::is_empty`] removes the entry:
    /// this is the deletion path, not a "store a blank" path. `dirty` is inserted
    /// even when the signal matches a resident entry, because the caller
    /// (`seal_drafts`, after a recall borrow) is asserting what the *file* must
    /// say, not merely what memory happens to hold.
    pub fn park(&mut self, thread_id: &str, draft: ComposerDraft) {
        if thread_id.is_empty() {
            return;
        }
        if draft.is_empty() {
            self.forget(thread_id);
            return;
        }
        let signal = DraftSignal::of(&LiveDraft {
            text: &draft.text,
            cursor_position: draft.cursor_position,
            pending_images: &draft.pending_images,
            pending_text_pastes: &draft.pending_text_pastes,
            input_scroll: draft.input_scroll,
        });
        if self
            .entries
            .get(thread_id)
            .is_some_and(|entry| entry.signal == signal)
        {
            self.touch(thread_id);
        } else {
            self.insert(thread_id.to_string(), draft, signal);
        }
        if self.persist {
            self.removed.remove(thread_id);
            self.dirty.insert(thread_id.to_string());
        }
    }

    /// Whether `thread_id` has a parked draft, resident or on disk.
    ///
    /// Immutable, and never reads the filesystem: the ledger records every
    /// thread it knows about, so an entry evicted by the memory cap still counts.
    /// This is what an "is this thread empty?" predicate consults — a thread
    /// with unsent text is not empty, whether or not that text happens to be
    /// resident at the moment of asking.
    pub fn has_stored(&self, thread_id: &str) -> bool {
        self.known.contains(thread_id)
    }

    /// Drop `thread_id`'s parked draft.
    pub fn forget(&mut self, thread_id: &str) {
        self.entries.remove(thread_id);
        self.recency.retain(|id| id != thread_id);
        self.known.remove(thread_id);
        if self.persist {
            self.dirty.remove(thread_id);
            self.removed.insert(thread_id.to_string());
        }
    }

    /// Whether anything is parked in memory.
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    /// Whether a write is waiting to happen (dirty or removed).
    fn is_pending(&self) -> bool {
        !self.dirty.is_empty() || !self.removed.is_empty()
    }

    /// Write every dirty draft and unlink every removed one.
    ///
    /// Best effort by contract: a failure is logged, the entry stays dirty, and
    /// the pass is retried after a 15-second backoff rather than on the next
    /// frame.
    pub fn flush(&mut self) {
        if !self.persist || !self.is_pending() {
            return;
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let mut failed = false;
        for thread_id in std::mem::take(&mut self.removed) {
            let path = self.path_for(&thread_id);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "could not remove parked draft");
                    failed = true;
                    self.removed.insert(thread_id);
                }
            }
        }
        for thread_id in std::mem::take(&mut self.dirty) {
            let Some(entry) = self.entries.get(&thread_id) else {
                continue;
            };
            let stored = StoredDraft {
                thread_id: thread_id.clone(),
                draft: entry.draft.clone(),
            };
            let path = self.path_for(&thread_id);
            match nuo_host::fsutil::atomic_write_json(&path, &stored) {
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "could not park draft");
                    failed = true;
                    self.dirty.insert(thread_id);
                }
            }
        }
        self.retry_at = failed.then(|| Instant::now() + WRITE_RETRY_BACKOFF);
        // Any dirty entries that were pinned over budget during editing are now
        // safely on disk and eligible for LRU eviction.
        self.evict_over_budget();
    }

    /// The file a thread's draft is stored in.
    fn path_for(&self, thread_id: &str) -> PathBuf {
        self.root.join(draft_file_name(thread_id))
    }

    /// Read one thread's file into residency.
    fn load_one(&mut self, thread_id: &str) {
        if !self.persist {
            return;
        }
        let path = self.path_for(thread_id);
        let Some(stored) = read_stored(&path) else {
            return;
        };
        if stored.thread_id != thread_id {
            // The file name is a derived, sanitized form; a payload naming a
            // different thread means the file was moved or hand-edited. Trusting
            // the name over the payload would graft one thread's draft onto
            // another, so the entry is ignored rather than adopted.
            tracing::warn!(
                path = %path.display(),
                expected = thread_id,
                found = %stored.thread_id,
                "ignoring parked draft whose payload names a different thread"
            );
            return;
        }
        self.insert_resident(stored.thread_id, stored.draft);
    }

    fn insert(&mut self, thread_id: String, draft: ComposerDraft, signal: DraftSignal) {
        if draft.is_empty() {
            return;
        }
        let bytes = draft.size_bytes();
        self.recency.retain(|id| id != &thread_id);
        self.recency.push_back(thread_id.clone());
        self.known.insert(thread_id.clone());
        self.entries.insert(
            thread_id,
            Entry {
                draft,
                signal,
                bytes,
            },
        );
        self.evict_over_budget();
    }

    fn insert_resident(&mut self, thread_id: String, draft: ComposerDraft) {
        let signal = DraftSignal::of(&LiveDraft {
            text: &draft.text,
            cursor_position: draft.cursor_position,
            pending_images: &draft.pending_images,
            pending_text_pastes: &draft.pending_text_pastes,
            input_scroll: draft.input_scroll,
        });
        self.insert(thread_id, draft, signal);
    }

    /// Mark `thread_id` as most recently used.
    fn touch(&mut self, thread_id: &str) {
        if !self.entries.contains_key(thread_id) {
            return;
        }
        if self.recency.back().is_some_and(|id| id == thread_id) {
            return;
        }
        self.recency.retain(|id| id != thread_id);
        self.recency.push_back(thread_id.to_string());
    }

    /// Drop least-recently-used **clean** drafts until the resident set fits
    /// the budget.
    ///
    /// Dirty drafts (not yet flushed to disk) are never evicted: dropping a
    /// dirty entry from memory before `flush()` runs would silently lose unsent
    /// user input. Clean entries are already durable on disk and re-read on
    /// demand; `flush()` re-runs eviction once dirty entries land on disk.
    ///
    /// The entry just parked is the newest in `recency`, so it can never evict
    /// itself; a single draft larger than the whole budget therefore stays
    /// resident (it is the one the user is working in) and the others go.
    fn evict_over_budget(&mut self) {
        let mut total: usize = self.entries.values().map(|entry| entry.bytes).sum();
        if total <= RESIDENT_BUDGET_BYTES || self.entries.len() <= 1 {
            return;
        }
        let newest = self.recency.back().cloned();
        let mut idx = 0;
        while total > RESIDENT_BUDGET_BYTES && self.entries.len() > 1 && idx < self.recency.len() {
            let candidate = self.recency[idx].clone();
            if newest.as_deref() == Some(candidate.as_str()) {
                break;
            }
            if self.dirty.contains(&candidate) {
                idx += 1;
                continue;
            }
            let Some(entry) = self.entries.remove(&candidate) else {
                self.recency.remove(idx);
                continue;
            };
            total = total.saturating_sub(entry.bytes);
            self.recency.remove(idx);
        }
    }
}

/// Read and parse one stored draft, warning (never failing) on a bad file.
fn read_stored(path: &Path) -> Option<StoredDraft> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read parked draft");
            return None;
        }
    };
    match serde_json::from_slice::<StoredDraft>(&bytes) {
        Ok(stored) => Some(stored),
        Err(error) => {
            // Left in place on purpose: it may be the only copy of the text, and
            // a future version may be able to read it.
            tracing::warn!(%error, path = %path.display(), "ignoring unreadable parked draft");
            None
        }
    }
}

/// Map a server identity to a filesystem-safe, injective directory name (ADR-0250).
fn scope_dir_name(server_scope: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut readable: String = server_scope
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    if readable.is_empty() {
        readable.push_str("local");
    }
    let mut hasher = Sha256::new();
    hasher.update(server_scope.as_bytes());
    let digest = hasher.finalize();
    let suffix: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{readable}-{suffix}")
}

/// Map a thread id to a filesystem-safe, injective file name.
///
/// The readable part keeps debugging sane; the digest makes the mapping
/// injective for every id, including the ones sanitization would otherwise fold
/// together (`a/b` and `a:b` both sanitize to `a_b`).
fn draft_file_name(thread_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut readable: String = thread_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    if readable.is_empty() {
        readable.push('_');
    }
    let mut hasher = Sha256::new();
    hasher.update(thread_id.as_bytes());
    let digest = hasher.finalize();
    let suffix: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{readable}-{suffix}.json")
}

#[cfg(test)]
mod tests;
