//! Record types and runtime constants shared by the persistence layer.
use serde::{Deserialize, Serialize};

use crate::db::ThreadMessageView;

/// Payload size threshold (4 KB) beyond which text content is offloaded to CAS BlobStore.
pub const CAS_THRESHOLD_BYTES: usize = 4096;

/// Hard cap on retained request-projection records per session. The archive is
/// forensic, not authoritative: a bounded ring keeps a long session from
/// growing it without limit.
pub const MAX_RETAINED_REQUEST_PROJECTIONS: usize = 64;

/// Thread record representation in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub origin: String,
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
    /// (joined from `threads.title`).
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
    pub messages: Vec<ThreadMessageView>,
}
