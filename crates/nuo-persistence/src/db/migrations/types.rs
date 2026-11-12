//! Constants, record types, and column lists for SQLite migrations.
use serde::{Deserialize, Serialize};

use crate::db::SessionMessageView;

/// SQLite schema version tracking. Fresh databases jump straight to the latest version.
pub const CURRENT_DB_VERSION: u32 = 30;

/// SHA-256 fingerprint of the migration catalog (version + SQL of every
/// entry). Locked by `migration_catalog_fingerprint_is_stable`; see that test
/// for the discipline this enforces.
#[cfg(test)]
pub const MIGRATION_CATALOG_FINGERPRINT: &str =
    "ce1b8736db061125eb7d7a799fd09665e8a533de99589f06ba435f734962585d";

/// Payload size threshold (4 KB) beyond which text content is offloaded to CAS BlobStore.
pub const CAS_THRESHOLD_BYTES: usize = 4096;

/// Hard cap on retained request-projection records per session. The
/// archive is forensic, not authoritative: a bounded ring keeps a long session
/// from growing it without limit.
pub const MAX_RETAINED_REQUEST_PROJECTIONS: usize = 64;

/// A structured database migration step.
pub struct Migration {
    pub(crate) version: u32,
    pub(crate) sql: &'static str,
}

/// Working-state columns the final `sessions` rebuild must carry,
/// with their DDL. Shared by the migration-6 repair and the startup schema
/// guard so the two cannot drift.
pub const SESSIONS_WORKING_STATE_COLUMNS: &[(&str, &str)] = &[
    ("provider_connection", "TEXT"),
    ("round_counter", "INTEGER NOT NULL DEFAULT 0"),
    ("unattended", "INTEGER NOT NULL DEFAULT 0"),
    ("disabled_tools", "TEXT NOT NULL DEFAULT '[]'"),
    ("commands", "TEXT NOT NULL DEFAULT '[]'"),
    ("round_interrupts", "TEXT NOT NULL DEFAULT '[]'"),
    // `retry_resolutions` is owned by migration 9, not this conditional
    // repair list: migration 6's repair runs before migration 9 on an
    // interim-v5 database and would add the column first, making migration
    // 9's unconditional `ADD COLUMN` fail with "duplicate column name".
    ("retry_pending", "TEXT"),
    ("request_usage_records", "TEXT NOT NULL DEFAULT '[]'"),
    ("checksum", "INTEGER"),
    ("schema_version", "INTEGER NOT NULL DEFAULT 13"),
    ("role_manifest", "TEXT"),
];

/// Columns persistence v2 added to `sessions`. Guard-only:
/// migration 7 owns their creation, so the migration-6 repair must not add
/// them (it runs first and its additions would collide with migration 7's).
pub const SESSIONS_V2_COLUMNS: &[(&str, &str)] = &[
    ("digest_anchor", "INTEGER"),
    ("tree", "TEXT NOT NULL DEFAULT '{}'"),
    ("transcript_generation", "TEXT"),
];

/// Identity columns every `sessions` incarnation must carry. Guard-only:
/// migrations own their creation.
pub const SESSIONS_IDENTITY_COLUMNS: &[&str] = &[
    "id",
    "parent_id",
    "fork_kind",
    "title",
    "created_at_s",
    "updated_at_s",
    "workspace_root",
    "persona",
    "additional_roots",
    "msg_count",
    "last_user_prompt",
    "digest",
];

/// Columns the interim version-5 rebuild inherited from the retired
/// cron/repeat scheduling tranche; removed by the migration-6 repair.
pub const SESSIONS_RETIRED_COLUMNS: &[&str] = &["scheduled_jobs", "data", "title_manual"];

/// Session record representation in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub fork_kind: String,
    /// Fork provenance (ADR-0120): the address the branch diverged at, or
    /// `None` on a trunk. One canonical type rather than the two raw columns,
    /// so an invalid pairing (a turn with no round) cannot be carried around.
    #[serde(default)]
    pub fork_point: Option<nuo_protocol::ForkPoint>,
    pub title: Option<String>,
    pub created_at_s: i64,
    pub updated_at_s: i64,
    pub workspace_root: Option<String>,
    pub persona: Option<String>,
    #[serde(default)]
    pub msg_count: i64,
    #[serde(default)]
    pub last_user_prompt: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
    /// Durable thread status (`idle`, `running`, `suspended`, …). A listing
    /// uses this for rows that have no live driver.
    #[serde(default = "default_thread_status")]
    pub status: String,
    /// Durable user-round counter (the current or most recently completed
    /// round). Live monitor rows overlay the live counter when hosted.
    #[serde(default)]
    pub round_counter: u64,
}

fn default_thread_status() -> String {
    "idle".to_string()
}

/// One full-text search hit over transcript entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistorySearchResult {
    pub entry_id: String,
    pub session_id: String,
    pub workspace_root: Option<String>,
    /// Stored AI/manual title of the owning session, when one exists
    /// (joined from `sessions.title`).
    pub session_title: Option<String>,
    pub role: String,
    pub snippet: String,
    pub score: f64,
}

/// One persisted thread's projected transcript tail plus metadata — the
/// public, field-private shape out-of-crate readers consume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadTranscriptView {
    pub id: String,
    pub title: Option<String>,
    pub digest: Option<nuo_protocol::ThreadDigest>,
    pub workspace_root: Option<String>,
    pub message_count: usize,
    pub messages: Vec<SessionMessageView>,
}
