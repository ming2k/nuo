//! Persisted session state: the [`SessionStore`] single-transcript model and
//! its split submodules (ADR-0186).
//!
//! The module root keeps the data model (`SessionData`), checksum/blob
//! plumbing, the free helper functions, and the pure compaction pipeline; the
//! `impl SessionStore` surface is split by concern:
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
//! both stored authoritatively in SQLite (ADR-0168 / ADR-0186). Every
//! consumer-facing window is a pure derive; nothing derived is persisted.

use crate::blobs::BlobStore;
use crate::paths;
use nuo_wire::{
    EntryPayload, InjectionKind, InjectionOrigin, Message, Provider, Role, SessionDetail,
    estimate_tokens,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// C2 (ADR-0022): added `title` and `title_manual`. C4 (ADR-0034): added `Message::origin` (`Option<InjectionOrigin>`)
/// for structured injection provenance. Both are structural no-ops for
/// legacy snapshots, which load with the new fields at their `#[serde(default)]`
/// values (`None` / `false`). C6 (per-session provider/model): added
/// `provider_selection`. A session that has run `/models` pins its own
/// provider + model here so the live selection does not leak into the global
/// `config.toml` or affect other concurrent sessions.
/// C11 added `round_interrupts` (durable round-interrupt records): a
/// structural no-op — legacy snapshots load with an empty list.
/// v13 (ADR-0186): clean break — the transcript is entries + directives;
/// legacy pre-transcript snapshots are not migrated and load as empty.
/// v14 (ADR-0187): persistence v2 — the row checksum covers the session row
/// (working state) instead of the transcript, and is verified on load.
/// v15 (ADR-0236): the row checksum no longer folds the per-session
/// `request_usage_records` mirror — usage lives in its own key-addressed table
/// (its integrity is the table's PK), so including it made every ordinary turn
/// checksum scale with the session's total retained attempts (invariant #3).
pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 15;

/// A session-scoped connection + model pin (C6 / ADR-0186). `connection`
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

pub use nuo_wire::{ContextProjectionCheckpoint, ContextProjectionKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionData {
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    /// How this session came to exist relative to its lineage: a root trunk,
    /// an explicit `/fork` branch, or a `/btw` aside forked off the trunk.
    #[serde(default)]
    pub(crate) fork_kind: nuo_wire::SessionForkKind,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    /// The single durable transcript (ADR-0186): immutable entries plus the
    /// projection decision history. The model window and every other read
    /// model derive from it via `Transcript::project`.
    #[serde(default)]
    pub(crate) transcript: nuo_wire::Transcript,
    /// Stats of the most recent model-context projection (prune or compaction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_projection: Option<ContextProjectionCheckpoint>,
    pub(crate) workspace: Option<nuo_wire::WorkspaceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<String>,
    /// Immutable role manifest snapshot captured at session creation (ADR-0245, ADR-0246).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role_manifest: Option<nuo_wire::SessionRoleManifest>,
    /// Schema version of this session. Migrations are no longer applied —
    /// ADR-0186 is a clean break and legacy snapshots load as empty.
    pub(crate) schema_version: u32,
    /// CRC32C checksum of the canonical JSON payload (excluding this field).
    #[serde(default)]
    pub(crate) checksum: Option<u32>,
    /// AI-generated session title. Non-`NULL` is terminal: AI generation only
    /// fills `None` (ADR-0186 dropped `title_manual`).
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// AI-generated session digest — the resume-time working-memory
    /// projection shown by the session picker's detail view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) digest: Option<nuo_wire::SessionDigest>,
    /// Transcript char count when `digest` was generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) digest_anchor: Option<u64>,
    /// Identity of the transcript lineage (ADR-0187). Incremental saves are
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
    pub(crate) request_usage_records: Vec<nuo_wire::RequestUsageRecord>,
    /// Durable command ledger (ADR-0091).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) commands: Vec<nuo_wire::CommandRecord>,
    /// Durable round-interrupt records (C11): projection state, never part of
    /// the transcript (ADR-0186 §3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) round_interrupts: Vec<nuo_wire::RoundInterrupt>,
    /// Durable retry-resolution records: the success-side mirror of
    /// `round_interrupts` — one per round that recovered from transient
    /// provider faults via the harness retry loop. Projection state, never
    /// part of the transcript (ADR-0186 §3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) retry_resolutions: Vec<nuo_wire::RetryResolution>,
    /// The durable `/retry` resume point (C12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) retry_pending: Option<nuo_wire::RetryPoint>,
    /// Session-scoped unattended posture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) unattended: bool,
    /// Native DAG session tree (Schema v12).
    #[serde(default)]
    pub(crate) tree: nuo_wire::SessionTree,
    /// Transcript entries this binary cannot decode, preserved verbatim
    /// (ADR-0187): raw rows ride in memory and round-trip through every
    /// save, so a newer binary's data survives an older binary untouched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) unknown_entries: Vec<UnknownEntryRow>,
    /// Projection directives this binary cannot decode, preserved verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) unknown_directives: Vec<UnknownDirectiveRow>,
}

impl SessionData {
    /// Borrow the underlying transcript entries and directives (ADR-0186, ADR-0262).
    pub fn transcript(&self) -> &nuo_wire::Transcript {
        &self.transcript
    }

    /// Clone working state and metadata while omitting the factual transcript history.
    ///
    /// Hot-path incremental turn commits use this to send only new delta rows to
    /// SQLite, completely eliminating O(N) memory allocations inside the session mutex.
    pub(crate) fn clone_metadata_without_history(&self) -> Self {
        Self {
            id: self.id.clone(),
            parent_id: self.parent_id.clone(),
            fork_kind: self.fork_kind,
            created_at: self.created_at,
            updated_at: self.updated_at,
            transcript: nuo_wire::Transcript::default(),
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
            // ADR-0236 invariant #3: the delta carries only new transcript rows
            // and the changed usage upserts (passed separately to the save), so
            // the per-session usage mirror is *not* cloned here. Cloning it made
            // every turn O(total retained attempts).
            request_usage_records: Vec::new(),
            commands: self.commands.clone(),
            round_interrupts: self.round_interrupts.clone(),
            retry_resolutions: self.retry_resolutions.clone(),
            retry_pending: self.retry_pending.clone(),
            unknown_entries: Vec::new(),
            unknown_directives: Vec::new(),
            tree: self.tree.clone(),
            unattended: self.unattended,
        }
    }
}

/// A transcript entry preserved verbatim because its payload kind is unknown
/// to this binary (ADR-0187). The envelope columns are stored raw and are
/// written back unchanged; the payload JSON is opaque.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct UnknownEntryRow {
    pub id: String,
    pub seq: u64,
    /// Envelope kind string as stored (`message` / `state` — or whatever a
    /// newer schema wrote; the table CHECK decides what may round-trip).
    pub kind: String,
    pub role: Option<String>,
    pub content: Option<String>,
    pub origin: Option<String>,
    pub hidden: bool,
    pub created_at_ms: u64,
    pub payload_json: String,
}

/// A projection directive preserved verbatim because its payload kind is
/// unknown to this binary (ADR-0187).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct UnknownDirectiveRow {
    pub seq: u64,
    pub kind: String,
    pub up_to_seq: u64,
    pub payload_json: String,
}

impl Default for SessionData {
    fn default() -> Self {
        let now = unix_timestamp();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            fork_kind: nuo_wire::SessionForkKind::Trunk,
            created_at: now,
            updated_at: now,
            transcript: nuo_wire::Transcript::new(),
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
            retry_resolutions: Vec::new(),
            retry_pending: None,
            unattended: false,
            tree: nuo_wire::SessionTree::default(),
            unknown_entries: Vec::new(),
            unknown_directives: Vec::new(),
        }
    }
}

impl SessionData {
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
    /// [`SessionStore::should_skip_persist`]) instead of re-deriving the
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

/// Serde default for [`SessionData`]'s workspace root. Resolves to the current
/// process cwd so legacy snapshots (which predate the field) load with the
/// closest-to-correct project binding on first deserialisation.
fn default_project_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn default_workspace() -> Option<nuo_wire::WorkspaceBinding> {
    Some(nuo_wire::WorkspaceBinding::new(default_project_root()))
}

/// ADR-0186 is a clean break: legacy snapshots (pre-transcript `SessionData`
/// JSON) are **not migrated**. serde's missing-field defaults load them as an
/// empty transcript — the session content is retired, the identity remains.
/// `schema_version` is stamped to the current version on load.
fn migrate_session_data(mut data: SessionData) -> SessionData {
    data.schema_version = CURRENT_SCHEMA_VERSION;
    data
}

#[derive(Serialize)]
struct SessionRowChecksumView<'a> {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commands: &'a Vec<nuo_wire::CommandRecord>,
    created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<&'a nuo_wire::SessionDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest_anchor: Option<u64>,
    disabled_tools: &'a std::collections::HashSet<String>,
    fork_kind: nuo_wire::SessionForkKind,
    generation: &'a str,
    id: &'a str,
    parent_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<&'a nuo_wire::WorkspaceBinding>,
    provider_selection: Option<&'a ProviderSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_pending: Option<&'a nuo_wire::RetryPoint>,
    round_counter: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    round_interrupts: &'a Vec<nuo_wire::RoundInterrupt>,
    schema_version: u32,
    title: Option<&'a str>,
    tree: &'a nuo_wire::SessionTree,
    unattended: bool,
    updated_at: u64,
}

impl<'a> From<&'a SessionData> for SessionRowChecksumView<'a> {
    fn from(data: &'a SessionData) -> Self {
        Self {
            commands: &data.commands,
            created_at: data.created_at,
            digest: data.digest.as_ref(),
            digest_anchor: data.digest_anchor,
            disabled_tools: &data.disabled_tools,
            fork_kind: data.fork_kind,
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
            tree: &data.tree,
            unattended: data.unattended,
            updated_at: data.updated_at,
        }
    }
}

/// Compute the CRC32C checksum over the session **row** — the working state
/// (ADR-0187). The transcript and the verbatim unknown rows are excluded:
/// they live in their own append-only tables with primary-key integrity, so
/// the row checksum covers exactly the data a single UPSERT writes. Returns
/// `Err` on serialization failure rather than a sentinel `0`.
///
/// Implemented via a zero-copy row view [`SessionRowChecksumView`] so hot-path
/// turn commits never serialize or clone the multi-megabyte `transcript`.
fn compute_checksum(data: &SessionData) -> Result<u32, String> {
    let view = SessionRowChecksumView::from(data);
    let bytes = serde_json::to_vec(&view).map_err(|e| e.to_string())?;
    Ok(crc32c::crc32c(&bytes))
}

/// Verify the stored row checksum on load (ADR-0187). Rows stamped by an
/// older schema (whose checksum covered the transcript) are exempt via
/// `schema_version`. A mismatch is reported loudly but is not fatal: the
/// durable per-row data remains authoritative and the next save restamps.
pub(crate) fn verify_checksum(data: &SessionData, session_id: &str) {
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
fn load_session_blobs(data: &mut SessionData, blob_store: &BlobStore) -> Result<(), String> {
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
pub struct SessionSummary {
    pub id: String,
    pub parent_id: Option<String>,
    /// How this session came to exist: trunk root, explicit `/fork`
    /// branch, or `/btw` aside. Drives the dashboard's lineage grouping.
    pub fork_kind: nuo_wire::SessionForkKind,
    pub message_count: usize,
    pub updated_at: u64,
    pub created_at: u64,
    /// Short description of what the session is about (first user message),
    /// already truncated for display.
    pub overview: String,
    pub active: bool,
    /// The Chronicler's structured digest (intent + history checklist), if present.
    pub digest: Option<nuo_wire::SessionDigest>,
}

/// The mutable bits a [`SessionStore`] pins to one session: the snapshot path
/// alias and the in-memory session data. Grouped under a single
/// [`tokio::sync::Mutex`] so repointing the store (reset / fork / open) is
/// atomic with respect to every
/// reader and writer. There is no second lock to deadlock against.
pub(crate) struct SessionState {
    /// Absolute path of this session's snapshot: `<sessions_dir>/<id>.json`.
    pub(crate) path: PathBuf,
    /// In-memory session, authoritative between writes.
    pub(crate) data: SessionData,
    /// `true` only for a **fresh** primary session (`pin_fresh`): defer the
    /// first durable write until the session gains user-facing content, so
    /// starting and exiting without a round leaves no empty-file litter
    /// (ADR-0018). `false` for an explicitly pinned path (`for_path`, and any
    /// store loaded from an existing snapshot): there the caller has already
    /// materialised the session, so every write persists eagerly.
    pub(crate) defer_persist: bool,
    /// In-memory cache of projected messages, avoiding repetitive full-history
    /// projections on ReAct turn hot paths.
    pub(crate) projected_cache: Option<Vec<nuo_wire::Message>>,
}

impl SessionState {
    pub(crate) fn new(path: PathBuf, data: SessionData, defer_persist: bool) -> Self {
        Self {
            path,
            data,
            defer_persist,
            projected_cache: None,
        }
    }

    #[inline]
    pub(crate) fn get_or_project_messages(&mut self) -> &[nuo_wire::Message] {
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
    pub(crate) fn append_to_projection_cache(&mut self, new_messages: &[nuo_wire::Message]) {
        if let Some(ref mut cache) = self.projected_cache {
            cache.extend_from_slice(new_messages);
        } else {
            self.projected_cache = Some(self.data.transcript.project_messages());
        }
    }
}

pub struct SessionStore {
    workspace: std::sync::RwLock<Option<nuo_wire::WorkspaceBinding>>,
    role: std::sync::RwLock<Option<String>>,
    /// Directory holding every session file for this project (or, for
    /// [`SessionStore::for_path`], the parent of the pinned snapshot). All
    /// `reset` / `fork` / `open` targets live here, so the store never writes
    /// outside it.
    sessions_dir: PathBuf,
    pub(crate) db_path: PathBuf,
    blob_store: BlobStore,
    pub(crate) writer: crate::db::PersistenceHandle,
    state: Mutex<SessionState>,
    /// FIFO commit gate for snapshot writes.
    persist_gate: Mutex<()>,
}

/// Write `data` authoritatively through the single-writer actor (ADR-0231).
///
/// There is no path where a session write opens its own connection: the caller
/// passes the handle that owns this database — the process-wide one in
/// production, the store-private one for a path-pinned instance.
fn persist_to(
    writer: &crate::db::PersistenceHandle,
    data: &SessionData,
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
/// schema) loads as a brand-new empty session — ADR-0186 retires legacy
/// content instead of migrating it.
fn load_or_seed(
    reader: Option<&crate::db::DbReader>,
    writer: Option<&crate::db::PersistenceHandle>,
    session_id: &str,
    blob_store: &BlobStore,
    workspace: Option<&nuo_wire::WorkspaceBinding>,
    persona: Option<&str>,
    legacy_file: Option<&Path>,
) -> SessionData {
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
    SessionData {
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

pub(crate) fn last_effective_prompt_from_data(data: &SessionData) -> Option<String> {
    // Reverse scan without materializing the full projection view.
    // Respect compaction watermark so archived head entries are elided.
    let compact_watermark = data
        .transcript
        .directives
        .iter()
        .filter_map(|d| {
            if matches!(d.payload, nuo_wire::DirectivePayload::Compact { .. }) {
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

pub struct ContextProjectionResult {
    pub model_window: Vec<Message>,
    pub archived_originals: Vec<Message>,
    pub checkpoint: ContextProjectionCheckpoint,
}

/// Header prepended to every compaction checkpoint message. Doubles as the
/// classifier that excludes checkpoints from the user-round count and lets a
/// later compaction extract the previous summary for incremental updates.
const CHECKPOINT_HEADER: &str = "[Conversation checkpoint]\n\
     Earlier complete rounds were compacted. Treat this as durable context, not a new user request.\n\n";

/// Per-message excerpt cap used by the deterministic excerpt fallback.
const EXCERPT_CAP_TOKENS: usize = 375;

pub struct CompactionSelection {
    /// Older complete rounds moved out of the model-visible window.
    pub archived: Vec<Message>,
    /// Recent rounds preserved verbatim after the checkpoint.
    pub tail: Vec<Message>,
    /// Body of a prior checkpoint message, when present, fed forward as the
    /// anchored summary so each compaction updates rather than restarts.
    pub previous_summary: Option<String>,
}

/// Split a message list into the archived head and the verbatim tail. Returns
/// `None` when there are not enough complete user rounds to compact.
pub fn select_compaction(
    messages: &[Message],
    preserve_rounds: usize,
) -> Option<CompactionSelection> {
    let user_indices = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message.role == Role::User
                && !message.content.starts_with("[Conversation checkpoint]")
                // Non-driving command echoes are recorded as Role::User for
                // resume/audit faithfulness but are not real rounds; exclude
                // them so they don't inflate the round count and skew which
                // rounds compaction preserves (ADR-0050).
                && !message.is_command_echo()
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if user_indices.len() <= preserve_rounds {
        return None;
    }

    let keep_from = user_indices[user_indices.len() - preserve_rounds];
    let archived = messages[..keep_from]
        .iter()
        .filter(|message| message.role != Role::System)
        .cloned()
        .collect::<Vec<_>>();
    if archived.is_empty() {
        return None;
    }
    let tail = messages[keep_from..].to_vec();

    // A prior checkpoint message (hidden user, `[Conversation checkpoint]`
    // prefix) carries the previous summary; surface it for incremental updates.
    let previous_summary = messages.iter().rev().find_map(|message| {
        if message.role == Role::User
            && message.hidden
            && message.content.starts_with("[Conversation checkpoint]")
        {
            message
                .content
                .strip_prefix(CHECKPOINT_HEADER)
                .map(|body| body.trim().to_string())
                .filter(|body| !body.is_empty())
        } else {
            None
        }
    });

    Some(CompactionSelection {
        archived,
        tail,
        previous_summary,
    })
}

/// Choose the deepest coherent compaction that leaves room for the checkpoint
/// summary inside the configured working-memory target. The configured number
/// of preserved rounds is still preferred, but on a large-context model it must
/// not make the absolute active-window ceiling ineffective.
fn select_compaction_for_target(
    messages: &[Message],
    preserve_rounds: usize,
    target_tokens: usize,
) -> Option<CompactionSelection> {
    let complete_rounds = messages
        .iter()
        .filter(|message| {
            message.role == Role::User
                && !message.content.starts_with("[Conversation checkpoint]")
                && !message.is_command_echo()
        })
        .count();
    // Keep the current/latest real round verbatim. If that one round alone is
    // enormous it can exceed a soft target, but the projection never silently
    // truncates the user's current request.
    let maximum = preserve_rounds
        .min(complete_rounds.saturating_sub(1))
        .max(1);
    let tail_budget = target_tokens.saturating_mul(3) / 4;
    let mut fallback = None;
    for rounds in (1..=maximum).rev() {
        let selection = select_compaction(messages, rounds)?;
        if estimate_tokens(&selection.tail) <= tail_budget {
            return Some(selection);
        }
        fallback = Some(selection);
    }
    fallback
}

/// Allocate the remaining working-memory budget to the checkpoint after the
/// verbatim tail is accounted for. A small floor preserves a useful task state
/// even when a recent tail is unusually large.
fn summary_token_budget(target_tokens: usize, tail: &[Message]) -> usize {
    target_tokens
        .saturating_sub(estimate_tokens(tail))
        .max(2_000)
}

/// Token budget for the compaction summary, derived from the post-compaction
/// token target (ADR-0120: token-native; the old `target × 4` char budget was
/// then binary-searched back into tokens — a pure-loss round trip). Bounded
/// so huge windows do not produce enormous summaries and tiny windows still
/// get a useful digest.
fn summary_token_budget_clamped(target_tokens: usize) -> usize {
    target_tokens.clamp(2_000, 24_000)
}

fn label_for(role: Role) -> Option<&'static str> {
    match role {
        Role::User => Some("User"),
        Role::Assistant => Some("Assistant"),
        Role::Tool => Some("Tool"),
        Role::System => None,
    }
}

/// Build a checkpoint message wrapping `summary` with the durable header.
pub fn checkpoint_message(summary: &str) -> Message {
    Message::injected(
        Role::User,
        format!("{CHECKPOINT_HEADER}{summary}"),
        InjectionOrigin::new(InjectionKind::CompactionCheckpoint),
    )
}

/// Assemble the final [`ContextProjectionResult`] from a selection and a summary.
pub fn build_compaction_result(
    window_tokens_before: usize,
    selection: CompactionSelection,
    summary: String,
) -> ContextProjectionResult {
    let CompactionSelection { archived, tail, .. } = selection;
    let mut model_window = Vec::with_capacity(tail.len() + 1);
    model_window.push(checkpoint_message(&summary));
    model_window.extend(tail);
    let window_tokens_after = estimate_tokens(&model_window);
    ContextProjectionResult {
        checkpoint: ContextProjectionCheckpoint {
            operation: ContextProjectionKind::Compact,
            archived_messages: archived.len(),
            active_messages: model_window.len(),
            window_tokens_before,
            window_tokens_after,
            summary: Some(summary),
            tracked_files: Vec::new(),
        },
        model_window,
        archived_originals: archived,
    }
}

/// Deterministic excerpt fallback used when no provider is available or the
/// LLM summarization call fails. Budget is allocated **newest-first** so recent
/// context is never crowded out by older verbose messages; selected excerpts
/// are then emitted in chronological order for readability. When a previous
/// summary exists it is carried forward as anchored context. The budget and
/// per-message caps are **tokens** (ADR-0120); every cut lands on an exact
/// token boundary.
pub fn build_excerpt_summary(
    archived: &[Message],
    max_tokens: usize,
    previous_summary: Option<&str>,
) -> String {
    // Pass 1 (newest-first): pick which messages fit the remaining budget.
    let mut used = 0usize;
    let mut chosen: Vec<usize> = Vec::new();
    for (index, message) in archived.iter().enumerate().rev() {
        let Some(label) = label_for(message.role) else {
            continue;
        };
        let content = message.content.trim();
        if content.is_empty() {
            continue;
        }
        let remaining = max_tokens.saturating_sub(used);
        if remaining < 16 {
            break;
        }
        let cost = nuo_wire::tokenizer::count_tokens(content).min(EXCERPT_CAP_TOKENS)
            + nuo_wire::tokenizer::count_tokens(label)
            + 2;
        used += cost;
        chosen.push(index);
    }
    chosen.reverse(); // chronological

    // Pass 2: render the chosen messages in order, hard-truncating each.
    let mut output = String::new();
    for index in chosen {
        let message = &archived[index];
        // Skip roles without a render label (e.g. System). Pass 1 above already
        // filters these out of `chosen`, so this is defensive — but it keeps the
        // two passes consistent and avoids a panic if the selection ever diverges.
        let Some(label) = label_for(message.role) else {
            continue;
        };
        let content = message.content.trim();
        let remaining = max_tokens.saturating_sub(nuo_wire::tokenizer::count_tokens(&output));
        if remaining < 16 {
            break;
        }
        let excerpt = nuo_wire::tokenizer::truncate_str_to_tokens(
            content,
            remaining.min(EXCERPT_CAP_TOKENS),
        );
        output.push_str(label);
        output.push_str(": ");
        output.push_str(excerpt);
        output.push_str("\n\n");
    }
    let history = output.trim_end().to_string();

    if let Some(previous) = previous_summary.map(str::trim).filter(|s| !s.is_empty()) {
        let previous_budget = (max_tokens / 4).clamp(125, 1_000);
        let previous_excerpt =
            nuo_wire::tokenizer::truncate_str_to_tokens(previous, previous_budget);
        format!("[Previous summary]\n{previous_excerpt}\n\n[Recent history]\n{history}")
    } else {
        history
    }
}

/// Pure, provider-less compaction using the deterministic excerpt fallback.
/// Kept as a testable building block and as the ultimate fallback when LLM
/// summarization is disabled or unavailable.
pub fn compact_messages(
    messages: &[Message],
    target_tokens: usize,
    preserve_rounds: usize,
) -> Option<ContextProjectionResult> {
    let window_tokens_before = estimate_tokens(messages);
    let selection = select_compaction_for_target(messages, preserve_rounds, target_tokens)?;
    let summary_tokens = summary_token_budget(target_tokens, &selection.tail);
    let excerpt_budget = summary_token_budget_clamped(summary_tokens);
    let summary = truncate_summary_to_token_budget(
        build_excerpt_summary(
            &selection.archived,
            excerpt_budget,
            selection.previous_summary.as_deref(),
        ),
        summary_tokens,
    );
    Some(build_compaction_result(
        window_tokens_before,
        selection,
        summary,
    ))
}

// LLM-based summarization

const SUMMARIZATION_SYSTEM_PROMPT: &str = "\
You are an anchored context summarization assistant for coding sessions.\n\
Summarize only the conversation history you are given. The newest rounds may be \
kept verbatim outside your summary, so focus on the older context that still \
matters for continuing the work.\n\
If a <previous-summary> block is included, treat it as the current anchored \
summary: preserve still-true details, remove stale details, and merge in new \
facts.\n\
Always follow the exact output structure requested. Keep every section, \
preserve exact file paths and identifiers when known, and prefer terse bullets \
over paragraphs.\n\
Do not answer the conversation itself. Do not mention that you are summarizing \
or compacting. Respond in the same language as the conversation.";

const SUMMARY_TEMPLATE: &str = "\
Output exactly the Markdown structure shown inside <template> and keep the \
section order unchanged. Do not include the <template> tags in your response.\n\
<template>\n\
## Objective\n\
- [single-sentence task summary]\n\
\n\
## Constraints & Preferences\n\
- [user constraints, preferences, specs, or \"(none)\"]\n\
\n\
## Progress\n\
### Done\n\
- [completed work or \"(none)\"]\n\
\n\
### In Progress\n\
- [current work or \"(none)\"]\n\
\n\
### Blocked\n\
- [blockers or \"(none)\"]\n\
\n\
## Key Decisions\n\
- [decision and why, or \"(none)\"]\n\
\n\
## Next Steps\n\
- [ordered next actions or \"(none)\"]\n\
\n\
## Critical Context\n\
- [important technical facts, errors, open questions, or \"(none)\"]\n\
\n\
## Relevant Files\n\
- [file or directory path: why it matters, or \"(none)\"]\n\
</template>\n\
\n\
Rules:\n\
- Keep every section, even when empty.\n\
- Use terse bullets, not prose paragraphs.\n\
- Preserve exact file paths, commands, error strings, and identifiers when known.\n\
- Do not mention the summary process or that context was compacted.";

/// Token cap applied to each tool-result when serializing history for the
/// summarizer (ADR-0120).
const SUMMARY_TOOL_OUTPUT_CAP_TOKENS: usize = 375;

/// Render `archived` as a readable transcript for the summarizer, capping tool
/// outputs and dropping the oldest messages when the result exceeds `budget`.
pub fn serialize_for_summary(archived: &[Message], budget: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for message in archived {
        let Some(label) = label_for(message.role) else {
            continue;
        };
        let mut body = message.content.trim().to_string();
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                body.push_str(&format!("\n[tool call: {}({})]", call.name, call.arguments));
            }
        }
        if message.role == Role::Tool {
            body = nuo_wire::tokenizer::truncate_str_to_tokens(
                body.trim(),
                SUMMARY_TOOL_OUTPUT_CAP_TOKENS,
            )
            .to_string();
        }
        // Subagent transcripts: render a bounded view of the nested work so
        // the summarizer can capture what each `task` call actually did
        // (otherwise the LLM only sees "[task result]:\n<final text>" and
        // cannot decide whether the subagent's tool usage is worth mentioning
        // in the anchored summary). The nested view is hard-capped to avoid
        // blowing the budget on a single subagent that ran for 30 turns.
        if let Some(children) = &message.children
            && !children.is_empty()
        {
            let nested =
                serialize_subagent_transcript_for_summary(children, SUMMARY_SUBAGENT_CAP_TOKENS);
            if !nested.is_empty() {
                body.push_str("\n[subagent transcript]\n");
                body.push_str(&nested);
            }
        }
        if body.trim().is_empty() {
            continue;
        }
        lines.push(format!("{label}: {body}"));
    }

    let joined = lines.join("\n\n");
    if nuo_wire::tokenizer::count_tokens(&joined) <= budget {
        return joined;
    }

    // Over budget: keep the most recent lines that fit (token budgets).
    let mut kept: Vec<&String> = Vec::new();
    let mut total = 0usize;
    for line in lines.iter().rev() {
        let cost = nuo_wire::tokenizer::count_tokens(line) + 2;
        if total + cost > budget {
            break;
        }
        total += cost;
        kept.push(line);
    }
    kept.reverse();
    let kept_str: Vec<&str> = kept.iter().map(|s| s.as_str()).collect();
    format!(
        "...[earlier history omitted]...\n\n{}",
        kept_str.join("\n\n")
    )
}

/// Per-subagent token cap when rendering the nested transcript into the
/// summarizer prompt (ADR-0120). Large enough to surface the subagent's task,
/// its key tool calls, and its conclusion; small enough that a turn with
/// five subagents cannot crowd out the rest of the conversation.
const SUMMARY_SUBAGENT_CAP_TOKENS: usize = 500;

/// Render a subagent's nested transcript as a compact summarizer-facing view.
/// Recursive: a subagent's own `task` results (sub-subagents) are rendered
/// one level deeper with an even smaller cap. Depth is bounded in practice by
/// the `SubagentTool` excluding itself from the sub-toolset.
fn serialize_subagent_transcript_for_summary(children: &[Message], budget: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for message in children {
        let Some(label) = label_for(message.role) else {
            continue;
        };
        let mut body = message.content.trim().to_string();
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                body.push_str(&format!("\n[tool call: {}({})]", call.name, call.arguments));
            }
        }
        if message.role == Role::Tool {
            body = nuo_wire::tokenizer::truncate_str_to_tokens(
                body.trim(),
                SUMMARY_TOOL_OUTPUT_CAP_TOKENS,
            )
            .to_string();
        }
        // One level deeper, with a much smaller cap, so we never spend more
        // than ~25% of the parent subagent's budget on a single sub-subagent.
        if let Some(nested) = &message.children
            && !nested.is_empty()
        {
            let inner = serialize_subagent_transcript_for_summary(nested, (budget / 4).max(125));
            if !inner.is_empty() {
                body.push_str("\n[sub-subagent transcript]\n");
                body.push_str(&inner);
            }
        }
        if body.trim().is_empty() {
            continue;
        }
        lines.push(format!("  {label}: {body}"));
    }
    let joined = lines.join("\n");
    if nuo_wire::tokenizer::count_tokens(&joined) <= budget {
        joined
    } else {
        format!(
            "{}...[truncated]",
            nuo_wire::tokenizer::truncate_str_to_tokens(&joined, budget)
        )
    }
}

fn build_summarization_user_prompt(
    transcript: &str,
    previous_summary: Option<&str>,
    extra_context: &[String],
) -> String {
    let mut parts = Vec::new();
    match previous_summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(previous) => parts.push(format!(
            "Update the anchored summary below using the conversation history that \
             follows. Preserve still-true details, remove stale details, and merge in \
             new facts.\n<previous-summary>\n{previous}\n</previous-summary>"
        )),
        None => parts
            .push("Create a new anchored summary from the conversation history below.".to_string()),
    }
    parts.push(SUMMARY_TEMPLATE.to_string());
    for context in extra_context {
        let context = context.trim();
        if !context.is_empty() {
            parts.push(context.to_string());
        }
    }
    parts.push(format!("Conversation history:\n{transcript}"));
    parts.join("\n\n")
}

/// Ask `provider` to summarize `archived`. Returns the summary text, or an
/// error that the caller maps to the deterministic excerpt fallback.
pub async fn summarize_with_provider(
    provider: &Arc<dyn Provider>,
    archived: &[Message],
    previous_summary: Option<&str>,
    extra_context: &[String],
    budget: usize,
) -> Result<String, String> {
    let transcript = serialize_for_summary(archived, budget);
    let user_prompt = build_summarization_user_prompt(&transcript, previous_summary, extra_context);
    let instructions = nuo_wire::InstructionBundle::from_single(
        "compaction.summarization",
        nuo_wire::InstructionTier::Task,
        SUMMARIZATION_SYSTEM_PROMPT,
    );
    let messages = vec![Message::new(Role::User, user_prompt)];
    // Bound the summarization call so a stalled or overloaded provider
    // triggers the excerpt fallback instead of hanging the turn (and the
    // entire frontend) forever. Two minutes is generous for a single
    // summarization response.
    const SUMMARIZATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
    let response = match tokio::time::timeout(
        SUMMARIZATION_TIMEOUT,
        provider.chat(
            nuo_wire::ModelRequest::ephemeral(messages).with_instructions(instructions),
        ),
    )
    .await
    {
        Ok(result) => result.map_err(|e| e.to_string())?,
        Err(_elapsed) => {
            return Err(format!(
                "Summarization timed out after {} seconds; using excerpt fallback.",
                SUMMARIZATION_TIMEOUT.as_secs()
            ));
        }
    };
    let summary = response.message.content.trim().to_string();
    if summary.is_empty() {
        return Err("Summarization returned an empty summary.".to_string());
    }
    Ok(summary)
}

// Compaction orchestrator

/// Run a compaction over `history` in place.
///
/// When `provider` is `Some`, an LLM produces an anchored structured summary
/// (with the previous summary carried forward for incremental updates); on any
/// failure it falls back to the deterministic excerpt summary. When `provider`
/// is `None`, the excerpt summary is used directly.
pub async fn run_compaction(
    history: &mut Vec<Message>,
    target_tokens: usize,
    preserve_rounds: usize,
    provider: Option<Arc<dyn Provider>>,
    extra_context: Vec<String>,
) -> Result<Option<ContextProjectionResult>, String> {
    let window_tokens_before = estimate_tokens(history);
    let Some(selection) = select_compaction_for_target(history, preserve_rounds, target_tokens)
    else {
        return Ok(None);
    };

    let summary_tokens = summary_token_budget(target_tokens, &selection.tail);
    let transcript_budget = summary_token_budget_clamped(summary_tokens);
    let summary = match provider.as_ref() {
        Some(provider) => {
            match summarize_with_provider(
                provider,
                &selection.archived,
                selection.previous_summary.as_deref(),
                &extra_context,
                transcript_budget,
            )
            .await
            {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "LLM summarization failed; falling back to excerpt compaction"
                    );
                    build_excerpt_summary(
                        &selection.archived,
                        transcript_budget,
                        selection.previous_summary.as_deref(),
                    )
                }
            }
        }
        None => build_excerpt_summary(
            &selection.archived,
            transcript_budget,
            selection.previous_summary.as_deref(),
        ),
    };

    let summary = truncate_summary_to_token_budget(summary, summary_tokens);
    let result = build_compaction_result(window_tokens_before, selection, summary);
    tracing::debug!(
        window_tokens_before,
        window_tokens_after = result.checkpoint.window_tokens_after,
        "compaction complete"
    );
    let model_window = result.model_window.clone();
    *history = model_window;
    Ok(Some(result))
}

/// Enforce the allocated checkpoint budget even when a summarizing provider
/// ignores its requested length. Now a thin wrapper over the exact
/// token-boundary cut ([`nuo_wire::tokenizer::truncate_to_tokens`]);
/// the old binary search existed only because the budget round-tripped
/// through characters (ADR-0120 removed that).
fn truncate_summary_to_token_budget(text: String, max_tokens: usize) -> String {
    let (prefix, _) = nuo_wire::tokenizer::truncate_to_tokens(&text, max_tokens);
    prefix.trim_end().to_string()
}

/// Diagnostic scan of stored sessions. SQLite is the only store; every row is
/// checked for presence of the authoritative tables (ADR-0186). Prints one
/// line per session and a summary.
pub async fn run_doctor(project_root: Option<&std::path::Path>) -> Result<(), String> {
    let db_path = paths::get().db_file();
    let reader = crate::db::get_persistence_handle()
        .reader()
        .map_err(|e| format!("cannot open {}: {e}", db_path.display()))?;
    let mut examined = 0usize;
    let mut corrupt = 0usize;
    let filter = project_root.map(|p| nuo_wire::WorkspaceFilter::Path(p.to_path_buf()));
    for session in reader
        .list_sessions(filter.as_ref())
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

mod fields;
mod history;

pub use history::CommitTurn;
#[cfg(test)]
pub(crate) use history::rebuild_transcript_from_messages as rebuild_for_test;
mod store;
#[cfg(test)]
mod tests;
