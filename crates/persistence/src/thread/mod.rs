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
//! entries + projection directives) plus working state on the session row,
//! both stored authoritatively in SQLite. Every
//! consumer-facing window is a pure derive; nothing derived is persisted.
use crate::blobs::BlobStore;
use host::paths;
use protocol::{EntryPayload, ThreadDetail, estimate_tokens};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tool::{InjectionKind, Message, Role};

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
        Self {
            id: self.id.clone(),
            active_branch: self.active_branch.clone(),
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
            commands: self.commands.clone(),
            round_interrupts: self.round_interrupts.clone(),
            retry_pending: self.retry_pending.clone(),
            unattended: self.unattended,
        }
    }
}

impl Default for ThreadData {
    fn default() -> Self {
        let now = unix_timestamp();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            active_branch: protocol::BranchId::main(),
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
}

impl ThreadSessionState {
    pub(crate) fn new(path: PathBuf, data: ThreadData, defer_persist: bool) -> Self {
        Self {
            path,
            data,
            defer_persist,
            projected_cache: None,
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

/// Write `data` authoritatively through the single-writer actor.
///
/// There is no path where a session write opens its own connection: the caller
/// passes the handle that owns this database — the process-wide one in
/// production, the store-private one for a path-pinned instance.
fn persist_to(
    writer: &crate::db::PersistenceHandle,
    data: &ThreadData,
    _blob_store: &BlobStore,
) -> Result<(), String> {
    let mut data = data.clone();
    data.checksum = Some(compute_checksum(&data)?);
    writer
        .save_session_blocking(data)
        .map_err(|e| format!("failed to save session to sqlite: {e}"))
}

/// Load the session for `session_id` directly from SQLite (SSOT).
/// A session not present in SQLite (or one stored under the pre-transcript
/// schema) loads as a brand-new empty session — retires legacy
/// content instead of migrating it.
fn load_or_seed(
    reader: Option<&crate::db::DbReader>,
    writer: Option<&crate::db::PersistenceHandle>,
    session_id: &str,
    blob_store: &BlobStore,
    workspace: Option<&protocol::WorkspaceBinding>,
    persona: Option<&str>,
    legacy_file: Option<&Path>,
) -> ThreadData {
    // Path alias in kv_store maps a legacy snapshot path to its session id.
    let mapped_id = if let (Some(path), Some(reader)) = (legacy_file, reader) {
        reader
            .get_kv(&format!("path:{}", path.display()))
            .ok()
            .flatten()
    } else {
        None
    };

    let target_id = mapped_id.as_deref().unwrap_or(session_id);

    // Primary path: load directly from SQLite (SSOT).
    if let Some(reader) = reader
        && let Ok(Some(mut data)) = reader.load_session_full(target_id)
    {
        if let Err(error) = load_session_blobs(&mut data, blob_store) {
            tracing::warn!(error = %error, "could not load session blobs from sqlite");
        }
        data = migrate_session_data(data);
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
        // Durably awaited: the alias is what keeps a legacy path resolving to
        // the same session id across reloads, so it must be visible before
        // this constructor returns. Safe to block here because the actor
        // always owns a thread or worker of its own (see
        // `PersistenceHandle::spawn`).
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
        {
            let is_echo = m
                .origin
                .as_ref()
                .is_some_and(|o| o.kind == InjectionKind::CommandEcho);
            if !m.hidden && !is_echo {
                return Some(m.content);
            }
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

/// Diagnostic scan of stored sessions. SQLite is the only store; every row is
/// checked for presence of the authoritative tables. Prints one
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
    Ok(existed)
}

mod fields;
mod history;
pub(crate) use history::{thread_from_session_data, transcript_from_thread};

pub use history::{CommitBasis, CommitTurn, ForkTarget};
mod store;
#[cfg(test)]
mod tests;
