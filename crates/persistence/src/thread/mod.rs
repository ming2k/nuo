//! Persisted session state: the [`ThreadSession`] single-transcript model and
//! its split submodules.
//!
//! The module root keeps the data model (`ThreadData`), checksum/blob
//! plumbing, and the free helper functions; the
//! `impl ThreadSession` surface is split by concern:
//!
//! - `fields`: typed read/write accessors over session fields.
//! - `history`: transcript append/replace, rounds, retry bookkeeping,
//!   fork/lineage queries, and the session tree.
//! - `store`: construction, load/persist, list/detail/active views, and
//!   offline scan tools.
//! - `tests`: embedded test suite.
//!
//! The durable state is the **single transcript** (`Transcript`: immutable
//! entries + projection directives) plus working state on the session row.
//! The per-session append-only journal is the source of truth (ADR-0236);
//! SQLite is a derived, queryable projection fed after the journal ack.
//! Every consumer-facing window is a pure derive; nothing derived is
//! persisted.
use crate::blobs::BlobStore;
use host::paths;
use protocol::{EntryPayload, ThreadDetail, estimate_tokens};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tool::{Message, Role};

/// C2 (ADR-0022): added `title` and `title_manual`. C4 (ADR-0034): added `Message::origin` (`Option<InjectionOrigin>`)
/// for structured injection provenance. Both are structural no-ops for
/// legacy snapshots, which load with the new fields at their `#[serde(default)]`
/// values (`None` / `false`). C6 (per-session provider/model): added
/// `provider_selection`. A session that has run `/models` pins its own
/// provider + model here so the live selection does not leak into the global
/// `config.toml` or affect other concurrent sessions.
/// C11 added `round_interrupts` (durable round-interrupt records): a
/// structural no-op — legacy snapshots load with an empty list.
/// v13: clean break — the transcript is entries + directives;
/// legacy pre-transcript snapshots are not migrated and load as empty.
/// v14: persistence v2 — the row checksum covers the session row
/// (working state) instead of the transcript, and is verified on load.
/// v15: the row checksum no longer folds the per-session
/// `request_usage_records` mirror — usage lives in its own key-addressed table
/// (its integrity is the table's PK), so including it made every ordinary turn
/// checksum scale with the session's total retained attempts (invariant #3).
/// v16 (ADR-0120): the fork point becomes part of the session row, so a
/// reloaded branch still knows where it diverged. Legacy rows load with `None`,
/// which is the truthful "unrecorded" value.
/// v17 (ADR-0140): the command ledger carries **acts only**. Rows written before
/// the split mixed acts with readings (a token count, a job list, a workspace
/// diff), and a reading replayed at its timestamp seam asserts a stale
/// measurement as history with no freshness signal. Because the two classes were
/// never distinguished at write time, no load-time filter can recover which row
/// was which from prose alone — so the ledger is dropped on load, the ADR-0059
/// clean-break precedent: the acts a thread took are reconstructible from its
/// state (posture, role, provider pin, trust grants), while a false measurement
/// is not recoverable by any later reader.
pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 17;

/// A session-scoped connection + model pin (C6). `connection`
/// carries the **connection id** (provider + account + endpoint, per
/// ADR-0066's dual-write selection); it overlays the global
/// `config.default_connection` / `config.default_model` for this session
/// only, so one session switching `/models` does not change what any other
/// session — or the next fresh session — sees. `None` means "follow the
/// global default". The serde alias keeps pre-rename snapshots
/// (`{"provider": ...}`) loading.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSelection {
    #[serde(alias = "provider")]
    pub connection: String,
    pub model: Option<String>,
}

/// What one projection produced, re-exported from the contract layer where it
/// was already declared (`protocol::context_projection`).
///
/// The session used to carry a verbatim second copy of this struct beside its
/// own compaction pipeline; with that pipeline retired there is exactly one
/// definition, and the store and the kernel name the same type.
pub use protocol::ContextProjectionResult;
pub use protocol::{ContextProjectionCheckpoint, ContextProjectionKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadData {
    pub(crate) id: String,
    /// Branch cursor this store view is projecting.
    #[serde(default = "default_branch")]
    pub(crate) active_branch: protocol::BranchId,
    /// Durable branch cursors. The transcript is the shared log; a branch is
    /// only a named head over it (ADR-0239). Journal frames carry this map, so
    /// a transcript switch and its cursor commit atomically.
    #[serde(default)]
    pub(crate) branches: std::collections::BTreeMap<protocol::BranchId, protocol::BranchState>,
    /// Full fact graph sidecar. Present on branch mutations and full rewrites;
    /// absent on ordinary turn deltas (which only extend the active
    /// transcript). The graph is what lets sibling branches survive a reload
    /// without SQLite (ADR-0239).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) graph: Option<protocol::Thread>,
    /// Materialised catalog summary: message entries in the transcript.
    #[serde(default)]
    pub(crate) msg_count: u64,
    /// Materialised catalog summary: the last effective user prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_user_prompt: Option<String>,
    pub(crate) parent_id: Option<String>,
    /// How this session came to exist relative to its lineage: a root trunk,
    /// or an explicit `/fork` branch (ADR-0058).
    #[serde(default)]
    pub(crate) origin: protocol::ThreadOrigin,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    /// The single durable transcript: immutable entries plus the
    /// projection decision history. The model window and every other read
    /// model derive from it via `Transcript::project`.
    #[serde(default)]
    pub(crate) transcript: protocol::Transcript,
    /// Stats of the most recent model-context projection (prune or compaction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_projection: Option<ContextProjectionCheckpoint>,
    pub(crate) workspace: Option<protocol::WorkspaceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<String>,
    /// Immutable role manifest snapshot captured at session creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role_manifest: Option<protocol::ThreadRoleManifest>,
    /// Schema version of this session. Migrations are no longer applied —
    /// Is a clean break and legacy snapshots load as empty.
    pub(crate) schema_version: u32,
    /// CRC32C checksum of the canonical JSON payload (excluding this field).
    #[serde(default)]
    pub(crate) checksum: Option<u32>,
    /// AI-generated session title. Non-`NULL` is terminal: AI generation only
    /// fills `None` (dropped `title_manual`).
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// AI-generated session digest — the resume-time working-memory
    /// projection shown by the session picker's detail view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) digest: Option<protocol::ThreadDigest>,
    /// Transcript char count when `digest` was generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) digest_anchor: Option<u64>,
    /// Identity of the transcript lineage. Incremental saves are
    /// appended only while this matches the store's generation; any rebuild
    /// mints a new one and the next save escalates to a full rewrite.
    #[serde(default)]
    pub(crate) generation: String,
    /// Session-scoped provider pin (C6). `None` means "follow the global
    /// default".
    #[serde(default)]
    pub(crate) provider_selection: Option<ProviderSelection>,
    /// Session-level disabled-tool mask (ADR-0048 Phase 2).
    #[serde(default)]
    pub(crate) disabled_tools: std::collections::HashSet<String>,
    /// Harness round counter (ADR-0048 Phase 2).
    #[serde(default)]
    pub(crate) round_counter: u64,
    /// Per-request token accounting for this session.
    #[serde(default)]
    pub(crate) request_usage_records: Vec<protocol::RequestUsageRecord>,
    /// Durable command ledger.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) commands: Vec<protocol::CommandRecord>,
    /// Durable round-interrupt records (C11): projection state, never part of
    /// the transcript.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) round_interrupts: Vec<protocol::RoundInterrupt>,
    /// The durable `/retry` resume point (C12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) retry_pending: Option<protocol::RetryPoint>,
    /// Session-scoped unattended posture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) unattended: bool,
}

impl ThreadData {
    /// Borrow the underlying transcript entries and directives.
    pub fn transcript(&self) -> &protocol::Transcript {
        &self.transcript
    }

    /// Clone working state and metadata while omitting the factual transcript history.
    ///
    /// Hot-path incremental turn commits use this to send only new delta rows to
    /// SQLite, completely eliminating O(N) memory allocations inside the session mutex.
    pub(crate) fn clone_metadata_without_history(&self) -> Self {
        let msg_count = self.transcript.message_count() as u64;
        let last_user_prompt = last_effective_prompt_from_data(self);
        Self {
            id: self.id.clone(),
            active_branch: self.active_branch.clone(),
            branches: self.branches.clone(),
            graph: None,
            msg_count,
            last_user_prompt,
            parent_id: self.parent_id.clone(),
            origin: self.origin,
            created_at: self.created_at,
            updated_at: self.updated_at,
            transcript: protocol::Transcript::default(),
            last_projection: self.last_projection.clone(),
            workspace: self.workspace.clone(),
            role: self.role.clone(),
            role_manifest: self.role_manifest.clone(),
            schema_version: self.schema_version,
            checksum: self.checksum,
            title: self.title.clone(),
            digest: self.digest.clone(),
            digest_anchor: self.digest_anchor,
            generation: self.generation.clone(),
            provider_selection: self.provider_selection.clone(),
            disabled_tools: self.disabled_tools.clone(),
            round_counter: self.round_counter,
            // Invariant #3: the delta carries only new transcript rows
            // and the changed usage upserts (passed separately to the save), so
            // the per-session usage mirror is *not* cloned here. Cloning it made
            // every turn O(total retained attempts).
            request_usage_records: Vec::new(),
            // ADR-0249: command acts are appended as O(1) journal frames and
            // normalized rows; ordinary turns never clone or rewrite the ledger.
            commands: Vec::new(),
            round_interrupts: self.round_interrupts.clone(),
            retry_pending: self.retry_pending.clone(),
            unattended: self.unattended,
        }
    }
}

impl ThreadData {
    /// Apply one journal frame to the in-memory state (ADR-0236).
    ///
    /// A journal frame carries the same shape the projection uses, so replay
    /// and live apply share one rule:
    ///
    /// - `FullRewrite` replaces the whole state;
    /// - `MetadataOnly` replaces the working state and keeps the transcript;
    /// - `Delta` replaces the working state and appends the frame's new
    ///   entries and directives.
    ///
    /// Fold the frame's usage upserts into the session mirror. The journal
    /// carries them beside the transcript delta so a reload reconstructs the
    /// same accounting the live state had.
    pub(crate) fn apply_usage_upserts(&mut self, records: &[protocol::RequestUsageRecord]) {
        for record in records {
            match self
                .request_usage_records
                .iter_mut()
                .find(|existing| existing.key == record.key)
            {
                Some(existing) => *existing = record.clone(),
                None => self.request_usage_records.push(record.clone()),
            }
        }
    }

    pub(crate) fn apply_journal_commit(&mut self, incoming: ThreadData, mode: crate::db::SaveMode) {
        // A delta/metadata frame omits the graph on purpose: it only extends
        // the active transcript. Carry the previously materialised graph
        // forward; a branch/full frame supplies its own and replaces it.
        let graph = self.graph.take();
        match mode {
            crate::db::SaveMode::FullRewrite => *self = incoming,
            crate::db::SaveMode::MetadataOnly => {
                let transcript = std::mem::take(&mut self.transcript);
                let commands = std::mem::take(&mut self.commands);
                *self = incoming;
                self.transcript = transcript;
                if self.commands.is_empty() {
                    self.commands = commands;
                }
            }
            crate::db::SaveMode::Delta => {
                let mut transcript = std::mem::take(&mut self.transcript);
                let commands = std::mem::take(&mut self.commands);
                *self = incoming;
                transcript.entries.append(&mut self.transcript.entries);
                transcript
                    .directives
                    .append(&mut self.transcript.directives);
                transcript.min_next_seq = transcript.min_next_seq.max(transcript.next_seq());
                transcript.min_next_directive_seq = transcript
                    .min_next_directive_seq
                    .max(transcript.next_directive_seq());
                self.transcript = transcript;
                if self.commands.is_empty() {
                    self.commands = commands;
                }
            }
        }
        // Only a delta/metadata frame inherits the previous graph. A full
        // rewrite states the whole truth, including "no graph" after a
        // wholesale transcript rebuild (ADR-0239).
        if !matches!(mode, crate::db::SaveMode::FullRewrite) && self.graph.is_none() {
            self.graph = graph;
        }
    }
}

impl Default for ThreadData {
    fn default() -> Self {
        let now = unix_timestamp();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            active_branch: protocol::BranchId::main(),
            branches: {
                let mut branches = std::collections::BTreeMap::new();
                let main = protocol::BranchId::main();
                branches.insert(
                    main.clone(),
                    protocol::BranchState::main(now.saturating_mul(1000)),
                );
                branches
            },
            graph: None,
            msg_count: 0,
            last_user_prompt: None,
            parent_id: None,
            origin: protocol::ThreadOrigin::Trunk,
            created_at: now,
            updated_at: now,
            transcript: protocol::Transcript::new(),
            last_projection: None,
            workspace: default_workspace(),
            role: None,
            role_manifest: None,
            schema_version: CURRENT_SCHEMA_VERSION,
            checksum: None,
            title: None,
            digest: None,
            digest_anchor: None,
            generation: uuid::Uuid::new_v4().to_string(),
            provider_selection: None,
            disabled_tools: std::collections::HashSet::new(),
            round_counter: 0,
            request_usage_records: Vec::new(),
            commands: Vec::new(),
            round_interrupts: Vec::new(),
            retry_pending: None,
            unattended: false,
        }
    }
}

impl ThreadData {
    /// The single authority for "this session has no substantive content yet"
    /// (ADR-0018). A session is empty while it carries neither dialogue
    /// (active `model_window` or `archived_transcript`), nor any *substantive*
    /// piece of session state — a non-empty todo list, at least one scheduled job,
    /// a non-empty disabled-tool mask, or a started round counter. Any one of
    /// those is a real user action worth durably recording, so it materialises
    /// the session.
    ///
    /// Auxiliary state deliberately does **not** count on their own, matching
    /// the lazy contract: the **title** (a title on an otherwise-empty session
    /// is still an empty record in the picker), the **provider selection**
    /// (pinning `/models` must not surface a never-used session), and the
    /// **commands ledger** (navigational / informational slash commands like
    /// `/sessions`, `/models`, `/dashboard`, `/help` executed before any dialogue
    /// must not materialize an empty session). All of these ride along once
    /// substantive dialogue or state makes the session real.
    ///
    /// Every guarded write path consults this (via
    /// [`ThreadSession::should_skip_persist`]) instead of re-deriving the
    /// condition inline, so the "what makes a session real" rule lives in
    /// exactly one place and cannot drift between setters.
    /// The user-facing-emptiness rule deliberately excludes `delegated`:
    /// toggling delegated mode on an otherwise-fresh session is a posture change
    /// on a session that has nothing to resume yet, not substantive work —
    /// it must not materialize an empty session file. Once the session gains
    /// dialogue or other substantive state, the flag rides along like every
    /// other session-scoped field.
    fn is_user_facing_empty(&self) -> bool {
        self.transcript.entries.is_empty()
            && self.transcript.derive_todos().is_none_or(|t| t.is_empty())
            && self.disabled_tools.is_empty()
            && self.round_counter == 0
    }
}

/// Serde default for [`ThreadData`]'s workspace root. Resolves to the current
/// process cwd so legacy snapshots (which predate the field) load with the
/// closest-to-correct project binding on first deserialisation.
fn default_branch() -> protocol::BranchId {
    protocol::BranchId::main()
}

fn default_project_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn default_workspace() -> Option<protocol::WorkspaceBinding> {
    Some(protocol::WorkspaceBinding::new(default_project_root()))
}

/// Is a clean break: legacy snapshots (pre-transcript `ThreadData`
/// JSON) are **not migrated**. serde's missing-field defaults load them as an
/// empty transcript — the session content is retired, the identity remains.
/// `schema_version` is stamped to the current version on load.
///
/// v17 (ADR-0140) retires the pre-split command ledger for the same reason,
/// applied to the one field whose *contents* became a claim the design no longer
/// makes: every row written before the split mixed acts with readings, and a
/// reading restored at its timestamp seam asserts a stale measurement as history.
/// No load-time filter can separate them after the fact — the class was never
/// recorded — so the ledger is dropped. Acts the thread took are still
/// reconstructible from its own state (posture, role, provider pin, trust
/// grants); a false measurement is not recoverable by any later reader. This is
/// the ADR-0059 precedent: retired content is retired, not quarantined.
fn migrate_session_data(mut data: ThreadData) -> ThreadData {
    if data.schema_version < CURRENT_SCHEMA_VERSION {
        data.commands.clear();
    }
    data.schema_version = CURRENT_SCHEMA_VERSION;
    data
}

#[derive(Serialize)]
struct SessionRowChecksumView<'a> {
    active_branch: &'a protocol::BranchId,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    branches: &'a std::collections::BTreeMap<protocol::BranchId, protocol::BranchState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commands: &'a Vec<protocol::CommandRecord>,
    created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<&'a protocol::ThreadDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest_anchor: Option<u64>,
    disabled_tools: &'a std::collections::HashSet<String>,
    origin: protocol::ThreadOrigin,
    generation: &'a str,
    id: &'a str,
    parent_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<&'a protocol::WorkspaceBinding>,
    provider_selection: Option<&'a ProviderSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_pending: Option<&'a protocol::RetryPoint>,
    round_counter: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    round_interrupts: &'a Vec<protocol::RoundInterrupt>,
    schema_version: u32,
    title: Option<&'a str>,
    unattended: bool,
    updated_at: u64,
}

impl<'a> From<&'a ThreadData> for SessionRowChecksumView<'a> {
    fn from(data: &'a ThreadData) -> Self {
        Self {
            active_branch: &data.active_branch,
            branches: &data.branches,
            commands: &data.commands,
            created_at: data.created_at,
            digest: data.digest.as_ref(),
            digest_anchor: data.digest_anchor,
            disabled_tools: &data.disabled_tools,
            origin: data.origin,
            generation: &data.generation,
            id: &data.id,
            parent_id: data.parent_id.as_deref(),
            persona: data.role.as_deref(),
            workspace: data.workspace.as_ref(),
            provider_selection: data.provider_selection.as_ref(),
            retry_pending: data.retry_pending.as_ref(),
            round_counter: data.round_counter,
            round_interrupts: &data.round_interrupts,
            schema_version: data.schema_version,
            title: data.title.as_deref(),
            unattended: data.unattended,
            updated_at: data.updated_at,
        }
    }
}

/// Compute the CRC32C checksum over the session **row** — the working state
/// The transcript and the verbatim unknown rows are excluded:
/// they live in their own append-only tables with primary-key integrity, so
/// the row checksum covers exactly the data a single UPSERT writes. Returns
/// `Err` on serialization failure rather than a sentinel `0`.
///
/// Implemented via a zero-copy row view [`SessionRowChecksumView`] so hot-path
/// turn commits never serialize or clone the multi-megabyte `transcript`.
fn compute_checksum(data: &ThreadData) -> Result<u32, String> {
    let view = SessionRowChecksumView::from(data);
    let bytes = serde_json::to_vec(&view).map_err(|e| e.to_string())?;
    Ok(crc32c::crc32c(&bytes))
}

/// Verify the stored row checksum on load. Rows stamped by an
/// older schema (whose checksum covered the transcript) are exempt via
/// `schema_version`. A mismatch is reported loudly but is not fatal: the
/// durable per-row data remains authoritative and the next save restamps.
pub(crate) fn verify_checksum(data: &ThreadData, session_id: &str) {
    let Some(stored) = data.checksum else {
        return;
    };
    if data.schema_version < CURRENT_SCHEMA_VERSION {
        return;
    }
    match compute_checksum(data) {
        Ok(expected) if expected == stored => {}
        Ok(expected) => tracing::error!(
            session = %session_id,
            stored = format!("{stored:#010x}"),
            computed = format!("{expected:#010x}"),
            "session row checksum mismatch: the working state was corrupted between saves"
        ),
        Err(error) => tracing::error!(
            session = %session_id,
            error,
            "session row checksum could not be recomputed"
        ),
    }
}

/// Characters above which a message content is moved to the blob store.
/// Rehydrate entry bodies from `content_blob` references after loading.
pub(crate) fn load_session_blobs(
    data: &mut ThreadData,
    blob_store: &BlobStore,
) -> Result<(), String> {
    for entry in data.transcript.entries.iter_mut() {
        let EntryPayload::Message(payload) = &mut entry.payload else {
            continue;
        };
        let Some(hash) = payload.content_blob.take() else {
            continue;
        };
        let bytes = blob_store
            .get(&hash)
            .ok_or_else(|| format!("missing content blob {hash}"))?;
        entry.content = Some(String::from_utf8(bytes).map_err(|e| e.to_string())?);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
    pub id: String,
    /// Authentic AI/manual thread title, when established (ADR-0050).
    pub title: Option<String>,
    pub parent_id: Option<String>,
    /// How this thread came to exist: a user thread or a subagent run.
    pub origin: protocol::ThreadOrigin,
    pub message_count: usize,
    pub updated_at: u64,
    pub created_at: u64,
    /// Short description of what the session is about (first user message),
    /// already truncated for display.
    pub overview: String,
    pub active: bool,
    /// The Chronicler's structured digest (intent + history checklist), if present.
    pub digest: Option<protocol::ThreadDigest>,
}

/// The mutable bits a [`ThreadSession`] pins to one session: the snapshot path
/// alias and the in-memory session data. Grouped under a single
/// [`tokio::sync::Mutex`] so repointing the store (reset / fork / open) is
/// atomic with respect to every
/// reader and writer. There is no second lock to deadlock against.
pub(crate) struct ThreadSessionState {
    /// Absolute path of this session's snapshot: `<sessions_dir>/<id>.json`.
    pub(crate) path: PathBuf,
    /// In-memory session, authoritative between writes.
    pub(crate) data: ThreadData,
    /// `true` only for a **fresh** primary session (`pin_fresh`): defer the
    /// first durable write until the session gains user-facing content, so
    /// starting and exiting without a round leaves no empty-file litter
    /// (ADR-0018). `false` for an explicitly pinned path (`for_path`, and any
    /// store loaded from an existing snapshot): there the caller has already
    /// materialised the session, so every write persists eagerly.
    pub(crate) defer_persist: bool,
    /// In-memory cache of projected messages, avoiding repetitive full-history
    /// projections on ReAct turn hot paths.
    pub(crate) projected_cache: Option<Vec<tool::Message>>,
    /// Durable per-session journal (ADR-0236). Swapped when this store opens
    /// or resets to another session.
    pub(crate) journal: Arc<crate::journal::SessionJournal>,
    /// Derived SQLite projection state (ADR-0236). Swapped with the journal.
    pub(crate) projection: Arc<ProjectionState>,
}

impl ThreadSessionState {
    pub(crate) fn new(
        path: PathBuf,
        data: ThreadData,
        defer_persist: bool,
        journal: Arc<crate::journal::SessionJournal>,
        projection: Arc<ProjectionState>,
    ) -> Self {
        Self {
            path,
            data,
            defer_persist,
            projected_cache: None,
            journal,
            projection,
        }
    }

    #[inline]
    pub(crate) fn get_or_project_messages(&mut self) -> &[tool::Message] {
        if self.projected_cache.is_none() {
            self.projected_cache = Some(self.data.transcript.project_messages());
        }
        self.projected_cache.as_deref().unwrap_or(&[])
    }

    #[inline]
    pub(crate) fn invalidate_projection_cache(&mut self) {
        self.projected_cache = None;
    }

    #[inline]
    pub(crate) fn append_to_projection_cache(&mut self, new_messages: &[tool::Message]) {
        if let Some(ref mut cache) = self.projected_cache {
            cache.extend_from_slice(new_messages);
        } else {
            self.projected_cache = Some(self.data.transcript.project_messages());
        }
    }
}

/// The pre-commit shape of everything a commit mutates in memory.
///
/// A commit has to mutate the session **before** its journal append: the frame
/// it persists is the delta that mutation produces. If the append then fails —
/// a full lane, a failed `fdatasync`, a closed season — the in-memory transcript
/// is already ahead of the journal. Leaving it there is silent data loss: the
/// next commit compares the caller's window against the *in-memory* projection,
/// sees the lost entries as durable, and persists a `MetadataOnly` frame that
/// never carries them. They are then gone from the only copy that matters.
///
/// So every mutating accessor captures one of these while it holds the lock and
/// hands it to the persist path, which restores it if (and only if) the commit
/// did not become durable.
///
/// The snapshot is O(1) for the appending case (lengths, not clones) and
/// O(history) only where the commit is already O(history) — a wholesale
/// transcript replacement.
pub(crate) struct SessionUndo {
    entries_len: usize,
    directives_len: usize,
    min_next_seq: u64,
    min_next_directive_seq: u64,
    updated_at: u64,
    round_counter: u64,
    retry_pending: Option<protocol::RetryPoint>,
    round_interrupts_len: usize,
    request_usage_records: Vec<protocol::RequestUsageRecord>,
    defer_persist: bool,
    /// Present iff the commit may replace the transcript wholesale: the exact
    /// previous transcript, graph sidecar, and generation identity.
    replaced: Option<(protocol::Transcript, Option<protocol::Thread>, String)>,
}

impl SessionUndo {
    /// Snapshot a commit that only appends entries/directives and mutates
    /// working fields. O(1).
    pub(crate) fn appending(state: &ThreadSessionState) -> Self {
        Self {
            entries_len: state.data.transcript.entries.len(),
            directives_len: state.data.transcript.directives.len(),
            min_next_seq: state.data.transcript.min_next_seq,
            min_next_directive_seq: state.data.transcript.min_next_directive_seq,
            updated_at: state.data.updated_at,
            round_counter: state.data.round_counter,
            retry_pending: state.data.retry_pending.clone(),
            round_interrupts_len: state.data.round_interrupts.len(),
            request_usage_records: state.data.request_usage_records.clone(),
            defer_persist: state.defer_persist,
            replaced: None,
        }
    }

    /// Snapshot a commit that may replace the transcript, its graph sidecar,
    /// and its generation identity (a rebuild, branch switch, or full rewrite).
    ///
    /// Must be captured **before** the mutation, while the old transcript is
    /// still in place.
    pub(crate) fn replacing(state: &ThreadSessionState) -> Self {
        let mut undo = Self::appending(state);
        undo.replaced = Some((
            state.data.transcript.clone(),
            state.data.graph.clone(),
            state.data.generation.clone(),
        ));
        undo
    }

    /// Restore the snapshotted fields after a commit failed to become durable.
    pub(crate) fn restore(self, state: &mut ThreadSessionState) {
        match self.replaced {
            Some((transcript, graph, generation)) => {
                state.data.transcript = transcript;
                state.data.graph = graph;
                state.data.generation = generation;
            }
            None => {
                state.data.transcript.entries.truncate(self.entries_len);
                state
                    .data
                    .transcript
                    .directives
                    .truncate(self.directives_len);
                state.data.transcript.min_next_seq = self.min_next_seq;
                state.data.transcript.min_next_directive_seq = self.min_next_directive_seq;
            }
        }
        state.data.updated_at = self.updated_at;
        state.data.round_counter = self.round_counter;
        state.data.retry_pending = self.retry_pending;
        state
            .data
            .round_interrupts
            .truncate(self.round_interrupts_len);
        state.data.request_usage_records = self.request_usage_records;
        state.defer_persist = self.defer_persist;
        // The projection cache mirrors whatever transcript was current; after a
        // rollback it is unknown, and a lazy reprojection is always correct.
        state.invalidate_projection_cache();
    }
}

pub struct ThreadSession {
    workspace: std::sync::RwLock<Option<protocol::WorkspaceBinding>>,
    role: std::sync::RwLock<Option<String>>,
    /// Directory holding every session file for this project (or, for
    /// [`ThreadSession::for_path`], the parent of the pinned snapshot). All
    /// `reset` / `fork` / `open` targets live here, so the store never writes
    /// outside it.
    sessions_dir: PathBuf,
    #[allow(dead_code)]
    pub(crate) db_path: PathBuf,
    blob_store: BlobStore,
    pub(crate) writer: crate::db::PersistenceHandle,
    state: Mutex<ThreadSessionState>,
    /// FIFO commit gate for snapshot writes.
    persist_gate: Mutex<()>,
}

/// Directory that owns per-session journals for `db_path`.
///
/// Colocating the journal with the database (rather than with a workspace's
/// legacy snapshot directory) keeps one stable root per store, including
/// path-pinned test stores.
pub(crate) fn journal_root(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .map(|parent| parent.join("journal"))
        .unwrap_or_else(|| PathBuf::from("journal"))
}

/// Durable path→session identity for legacy path-pinned stores.
///
/// The SQLite alias is derived state; deleting the database must not orphan a
/// journal. The alias lives in the journal plane so a path keeps resolving to
/// the same session even when the derived view is gone.
pub(crate) fn journal_alias_path(journal_root: &Path, legacy_path: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(legacy_path.to_string_lossy().as_bytes());
    journal_root
        .join("aliases")
        .join(format!("{:x}.txt", hasher.finalize()))
}

fn write_journal_alias(journal_root: &Path, legacy_path: &Path, session_id: &str) {
    let path = journal_alias_path(journal_root, legacy_path);
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let temp = path.with_extension("tmp");
    if fs::write(&temp, session_id).is_ok() {
        if let Ok(file) = fs::File::open(&temp) {
            let _ = file.sync_all();
        }
        if fs::rename(&temp, &path).is_ok()
            && let Some(parent) = path.parent()
            && let Ok(dir) = fs::File::open(parent)
        {
            let _ = dir.sync_all();
        }
    }
}

/// Open the shared journal for one session under this database.
pub(crate) fn open_journal(
    db_path: &Path,
    session_id: &str,
) -> Arc<crate::journal::SessionJournal> {
    crate::journal::SessionJournal::shared(&journal_root(db_path), session_id)
}

/// Seed a fresh session when the requested identity is terminally deleted.
fn seed_fresh_session(
    writer: Option<&crate::db::PersistenceHandle>,
    legacy_file: Option<&Path>,
    workspace: Option<&protocol::WorkspaceBinding>,
    persona: Option<&str>,
    target_id: &str,
) -> ThreadData {
    let id = if target_id.len() >= 32
        && target_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && writer.is_none()
    {
        target_id.to_string()
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    if let Some(path) = legacy_file
        && let Some(writer) = writer
        && !writer.is_lock_contention()
    {
        write_journal_alias(&journal_root(writer.db_path()), path, &id);
        let _ = writer.set_kv_blocking(format!("path:{}", path.display()), id.clone());
    }
    let role_manifest = Some(crate::roles::resolve_role_manifest(
        workspace.map(|w| w.root.as_path()),
        persona,
    ));
    ThreadData {
        id,
        workspace: workspace.cloned(),
        role: persona.map(str::to_string),
        role_manifest,
        ..Default::default()
    }
}

fn load_or_seed(
    reader: Option<&crate::db::DbReader>,
    writer: Option<&crate::db::PersistenceHandle>,
    session_id: &str,
    blob_store: &BlobStore,
    workspace: Option<&protocol::WorkspaceBinding>,
    persona: Option<&str>,
    legacy_file: Option<&Path>,
) -> ThreadData {
    // Path alias maps a legacy snapshot path to its session id. The journal
    // plane is authoritative: the SQLite KV is only a fallback for stores
    // created before the alias moved (ADR-0240).
    let mapped_id = if let Some(path) = legacy_file {
        let journal_alias = writer.and_then(|writer| {
            fs::read_to_string(journal_alias_path(&journal_root(writer.db_path()), path))
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        });
        journal_alias.or_else(|| {
            reader.and_then(|reader| {
                reader
                    .get_kv(&format!("path:{}", path.display()))
                    .ok()
                    .flatten()
            })
        })
    } else {
        None
    };

    let target_id = mapped_id.as_deref().unwrap_or(session_id);
    if mapped_id.is_some()
        && let (Some(path), Some(writer)) = (legacy_file, writer)
        && !writer.is_lock_contention()
    {
        write_journal_alias(&journal_root(writer.db_path()), path, target_id);
    }

    // Journal first (ADR-0236): it is the source of truth. The SQLite view is
    // derived and may lag, so it is only a fallback for a session whose
    // journal has not been materialised yet.
    if let Some(writer) = writer {
        let journal = open_journal(writer.db_path(), target_id);
        match journal.load() {
            Ok(Some(mut data)) => {
                if let Err(error) = load_session_blobs(&mut data, blob_store) {
                    tracing::warn!(error = %error, "could not load session blobs for journal state");
                }
                // The journal is authoritative; the derived row may be missing
                // or behind when the process died between the fsync and the
                // projection. Rebuild it once, on this cold open, so the picker
                // and recovery see the same session the journal does.
                let stale =
                    match reader.and_then(|reader| reader.get_session(target_id).ok().flatten()) {
                        Some(record) => record.updated_at_s < data.updated_at as i64,
                        None => true,
                    };
                if stale && !writer.is_lock_contention() {
                    let mut projection = data.clone();
                    match compute_checksum(&projection) {
                        Ok(checksum) => projection.checksum = Some(checksum),
                        Err(error) => {
                            tracing::warn!(%error, "could not checksum the journal state")
                        }
                    }
                    let seq = journal.durable_seq();
                    if let Err(error) = writer.save_session_blocking(projection, Some(seq)) {
                        tracing::warn!(%error, "could not rebuild the derived row from the journal");
                    }
                }
                return migrate_session_data(data);
            }
            Ok(None) => {
                if journal.is_deleted() {
                    // A terminal tombstone outranks a stale derived row: never
                    // resurrect a deleted session by falling back to SQLite.
                    return seed_fresh_session(
                        Some(writer),
                        legacy_file,
                        workspace,
                        persona,
                        target_id,
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    session = %target_id,
                    %error,
                    "could not read the session journal; falling back to the derived view"
                );
            }
        }
    }

    // Primary path: load directly from SQLite (SSOT).
    if let Some(reader) = reader
        && let Ok(Some(mut data)) = reader.load_session_full(target_id)
    {
        if let Err(error) = load_session_blobs(&mut data, blob_store) {
            tracing::warn!(error = %error, "could not load session blobs from sqlite");
        }
        data = migrate_session_data(data);
        // One-time materialisation: from this open onward the journal is the
        // source of truth for this session.
        if let Some(writer) = writer
            && !writer.is_lock_contention()
        {
            let journal = open_journal(writer.db_path(), &data.id);
            if let Err(error) = journal.import(&data) {
                tracing::warn!(
                    session = %data.id,
                    %error,
                    "could not import the existing session into its journal"
                );
            }
        }
        return data;
    }

    // Legacy flat-file snapshots are retired, not migrated: the path alias
    // pins one fresh identity so the caller's path keeps resolving to the
    // same (empty) session.
    let id =
        if target_id.len() >= 32 && target_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            target_id.to_string()
        } else {
            uuid::Uuid::new_v4().to_string()
        };
    if let Some(path) = legacy_file
        && let Some(writer) = writer
    {
        // The journal-plane alias is the durable one; the SQLite alias remains
        // for readers that only have the derived view.
        write_journal_alias(&journal_root(writer.db_path()), path, &id);
        let _ = writer.set_kv_blocking(format!("path:{}", path.display()), id.clone());
    }
    let role_manifest = Some(crate::roles::resolve_role_manifest(
        workspace.map(|w| w.root.as_path()),
        persona,
    ));
    ThreadData {
        id,
        workspace: workspace.cloned(),
        role: persona.map(str::to_string),
        role_manifest,
        ..Default::default()
    }
}

pub(crate) fn truncate_preview(text: &str, max: usize) -> String {
    // Flatten to one line: control chars (newlines, tabs, …) would otherwise
    // survive into the picker row, where the terminal paints a `\n`/`\r` as a
    // carriage return and spills the row out the left edge of the modal.
    let text: String = text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let head: String = chars.into_iter().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

pub(crate) fn last_effective_prompt_from_data(data: &ThreadData) -> Option<String> {
    // Reverse scan without materializing the full projection view.
    // Respect compaction watermark so archived head entries are elided.
    let compact_watermark = data
        .transcript
        .directives
        .iter()
        .filter_map(|d| {
            if matches!(d.payload, protocol::DirectivePayload::Compact { .. }) {
                Some(d.up_to_seq)
            } else {
                None
            }
        })
        .max();

    for entry in data.transcript.entries.iter().rev() {
        if entry.is_hidden_kind() || entry.hidden {
            continue;
        }
        if let Some(watermark) = compact_watermark
            && entry.seq <= watermark
        {
            break;
        }
        if entry.role == Some(Role::User)
            && let Some(m) = entry.to_message()
            && !m.hidden
        {
            return Some(m.content);
        }
    }
    None
}

pub(crate) fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Diagnostic scan of the derived session view. Every row is checked for the
/// tables the projection owns; a missing row is recoverable from its journal.
/// Prints one
/// line per session and a summary.
pub async fn run_doctor(project_root: Option<&std::path::Path>) -> Result<(), String> {
    let db_path = paths::get().db_file();
    let reader = crate::db::get_persistence_handle()
        .reader()
        .map_err(|e| format!("cannot open {}: {e}", db_path.display()))?;
    let mut examined = 0usize;
    let mut corrupt = 0usize;
    let filter = match project_root {
        Some(path) => protocol::ThreadFilter::workspace_only(protocol::WorkspaceFilter::Path(
            path.to_path_buf(),
        )),
        None => protocol::ThreadFilter::global(),
    };
    for session in reader
        .list_sessions(&filter, true)
        .map_err(|e| e.to_string())?
    {
        examined += 1;
        match reader.load_session_full(&session.id) {
            Ok(Some(data)) => println!(
                "ok       {} (schema {}, {} entries)",
                session.id,
                data.schema_version,
                data.transcript.entries.len()
            ),
            Ok(None) => {
                corrupt += 1;
                println!("corrupt  {} (session row without transcript)", session.id);
            }
            Err(error) => {
                corrupt += 1;
                println!("corrupt  {}: {}", session.id, error);
            }
        }
    }
    println!("---");
    println!("examined: {}, corrupt: {}", examined, corrupt);
    Ok(())
}

/// Permanently delete one durable thread and its snapshot/log artifacts.
///
/// This is the server-side purge primitive, not the in-session picker delete:
/// it has no active store to repoint and therefore only touches the durable
/// row, derived indexes (through the single writer), and the two per-thread
/// files. Callers must stop and await any resident driver first.
pub async fn delete_thread(id: &str, workspace_root: Option<&Path>) -> Result<bool, String> {
    projection::mark_deleted(id);
    let existed = crate::db::get_persistence_handle()
        .delete_session(id.to_string())
        .await
        .map_err(|e| e.to_string())?;
    let dirs = paths::get();
    let dir = match workspace_root.filter(|root| !root.as_os_str().is_empty()) {
        Some(root) => dirs.project_sessions_dir(root),
        None => dirs.bucket_sessions_dir("workspace-free"),
    };
    let snapshot = dir.join(format!("{id}.json"));
    let _ = fs::remove_file(&snapshot);
    let _ = fs::remove_file(snapshot.with_extension("jsonl"));
    let handle = crate::db::get_persistence_handle();
    if let Err(error) = open_journal(handle.db_path(), id).mark_deleted().await {
        tracing::warn!(session = %id, %error, "could not tombstone the session journal");
    }
    Ok(existed)
}

mod fields;
mod history;
mod projection;
pub(crate) use history::{thread_from_session_data, transcript_from_thread};
pub(crate) use projection::{ProjectionJob, ProjectionState};

pub use history::{CommitBasis, CommitTurn, ForkTarget};
mod store;
#[cfg(test)]
mod tests;

/// Rebuild any derived session whose watermark is behind its journal head.
///
/// This is the startup catch-up for a crash between the journal fsync and the
/// projection, or for a shard that was deleted. It is idempotent and safe to
/// run while the server is idle; the projector remains the steady-state path
/// (ADR-0242).
pub fn catch_up_projections_for(handle: &crate::db::PersistenceHandle) -> Result<usize, String> {
    let root = journal_root(handle.db_path());
    // Reclaim bodies left by a crash between the deletion marker and cleanup;
    // the marker already decided the outcome (ADR-0241).
    crate::journal::sweep_deleted(&root);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("could not scan the journal root: {error}")),
    };
    let mut repaired = 0usize;
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if id == "aliases" {
            continue;
        }
        let journal = crate::journal::SessionJournal::shared(&root, &id);
        if journal.is_deleted() {
            continue;
        }
        let durable = journal.durable_seq();
        if durable == 0 {
            continue;
        }
        let reader = handle.reader().ok();
        let present = reader
            .as_ref()
            .and_then(|reader| reader.get_session(&id).ok().flatten())
            .is_some();
        let watermark = reader
            .as_ref()
            .and_then(|reader| reader.projection_watermark(&id).ok().flatten())
            .unwrap_or(0);
        if present && watermark >= durable {
            continue;
        }
        let effects = journal.load_workspace_effects().unwrap_or_default();
        if let Some(mut data) = journal.load()? {
            if journal.is_deleted() {
                continue;
            }
            data.checksum = Some(compute_checksum(&data)?);
            match handle.save_session_blocking(data, Some(durable)) {
                Ok(()) => {
                    repaired = repaired.saturating_add(1);
                    tracing::info!(session = %id, seq = durable, "rebuilt a derived session from its journal");
                }
                Err(error) => {
                    tracing::warn!(session = %id, %error, "could not rebuild a derived session");
                }
            }
        }
        if !effects.is_empty() {
            if let Err(error) =
                handle.replay_workspace_effects_blocking(id.clone(), effects, Some(durable))
            {
                tracing::warn!(session = %id, %error, "could not replay workspace effects from journal");
            } else if !present {
                repaired = repaired.saturating_add(1);
            }
        }
    }
    Ok(repaired)
}

/// Process-wide catch-up: the first caller pays for the scan, later callers
/// are no-ops. Called once by the server bootstrap.
pub async fn catch_up_projections_once() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let outcome = tokio::task::spawn_blocking(|| {
            catch_up_projections_for(&crate::db::get_persistence_handle())
        })
        .await;
        match outcome {
            Ok(Ok(0)) => {}
            Ok(Ok(count)) => {
                tracing::info!(repaired = count, "projection catch-up repaired sessions");
            }
            Ok(Err(error)) => tracing::warn!(%error, "projection catch-up failed"),
            Err(error) => tracing::warn!(%error, "projection catch-up task failed"),
        }
    })
    .await;
}
