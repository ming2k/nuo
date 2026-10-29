//! Workspace effect journal: the durable record of what each tool call changed
//! outside the conversation (ADR-0125).
//!
//! This module is **storage and nothing else**. It appends observed effects and
//! reads them back; it never decides whether an effect *should* have happened,
//! whether a reversal is safe now, or what the user should be told. Those are
//! policy questions owned by the harness (`WorkspaceJournalMiddleware`, which
//! captures pre-images) and the server (`/diff`, `/undo`, which project and act
//! on the rows). Keeping the split strict is what lets the reversal rules be
//! tested without a filesystem and the storage be tested without an agent.
//!
//! ## Why a separate store rather than columns on the transcript
//!
//! The transcript is the *conversation* (`ADR-0117`): what the model said and
//! what the user said. A workspace effect is a fact about the *world* that
//! correlates with a conversation by `(thread_id, round_id, turn_id)` but is not
//! dialogue. Hanging pre-image hashes off a `Tool`-role message would mean a
//! transcript edit (`/unsend`, compaction, projection pruning) could strand or
//! silently mutate the undo record — a compaction that prunes an old tool result
//! would take the ability to undo that result's file write with it.
//!
//! ## Storage discipline
//!
//! - **Write-ahead** (`INV-EFFECT-03`): a file mutation's row is appended after
//!   the pre-image is durable but *before* the tool runs, then completed once it
//!   returns. A crash mid-write therefore leaves changed bytes with a record
//!   instead of silently unrecorded. See [`complete_effect`].
//! - **Immutable observations** (`INV-EFFECT-01`): every column but the
//!   completion and reversal stamps is write-once, enforced by trigger. `/undo`
//!   reads `before_hash` as its sole authority on prior content.
//! - **Monotonic completion and reversal** (`INV-EFFECT-02`, `INV-EFFECT-03`):
//!   each stamp is set at most once and never cleared, so replaying `/undo`
//!   cannot write stale bytes back over content a later round produced.
//! - **No early reversal** (`INV-EFFECT-04`): an unconfirmed effect is not
//!   something the journal may act on — reversal refuses it rather than guessing
//!   whether this call was the thing that changed the file.
//! - **No delete** (`INV-EFFECT-01`): retention is a policy question (bounded
//!   history), so reclamation goes through an explicit retention path rather
//!   than an ad-hoc `DELETE`, exactly as `facts` does.
//!
//! ## Content residency
//!
//! `before_hash` / `after_hash` are CAS hashes into the shared [`BlobStore`].
//! The bytes are written by the caller (the harness middleware) *before* the row
//! is appended, so a row never references content that was not durably stored.
//! Registering those hashes in `blob_refs` for GC liveness is the caller's
//! responsibility because only the caller knows the owning thread.
//!
//! [`BlobStore`]: crate::blobs::BlobStore

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::db::PersistenceError;

/// What a journaled operation did to its path.
///
/// Closed rather than a free string: reversal behaviour differs per variant
/// (`Create` reverses by deleting, `Edit` by writing the pre-image back), and a
/// consumer that cannot see every case would silently mishandle one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalOp {
    /// The call brought the path into existence. Reversal removes it.
    Create,
    /// The call rewrote existing content. Reversal restores the pre-image.
    Edit,
    /// The call removed the path. Reversal recreates it from the pre-image.
    ///
    /// No built-in tool produces this today (`PatchOp::Delete` is declared but
    /// unwritten); the variant exists so the journal's own vocabulary is
    /// complete and a future `delete_file` cannot be represented as an `Edit`
    /// with empty content — which would reverse by writing an empty file
    /// instead of removing it.
    Delete,
}

impl JournalOp {
    /// The durable token.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Edit => "edit",
            Self::Delete => "delete",
        }
    }

    /// Parse the durable token. `None` for an unknown value; callers must treat
    /// that as a load error rather than guessing a reversal behaviour.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "create" => Some(Self::Create),
            "edit" => Some(Self::Edit),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

/// One appended effect, as read back from the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEffect {
    /// Journal identity, monotonic within the database.
    pub id: i64,
    /// Owning thread.
    pub thread_id: String,
    /// Round the effect happened in.
    pub round_id: String,
    /// ReAct turn within that round.
    pub turn_id: String,
    /// The dispatch call id that produced the effect.
    pub call_id: String,
    /// Journal-local observation order.
    pub seq: i64,
    /// Canonical tool name.
    pub tool_name: String,
    /// Absolute normalized path, when the effect touched exactly one file.
    pub path: Option<String>,
    /// What happened to the path.
    pub op: JournalOp,
    /// Reversibility classification, as a durable token. Consumers parse it
    /// through [`EffectClass::from_str_token`] and must treat an unknown token
    /// as irreversible.
    ///
    /// [`EffectClass::from_str_token`]: nuo_tool::EffectClass::from_str_token
    pub effect_class: String,
    /// CAS hash of the pre-image, `None` for a creation.
    pub before_hash: Option<String>,
    /// CAS hash of the post-image, `None` until the call completes (and always
    /// `None` for a deletion).
    pub after_hash: Option<String>,
    /// Why the effect is not reversible, copied at write time.
    pub irreversible_reason: Option<String>,
    /// When the call returned and the post-image was observed. `None` means the
    /// mutation may or may not have run; reversal refuses such a row.
    pub completed_at_ms: Option<u64>,
    /// When the pre-image was written back, if it ever was.
    pub reversed_at_ms: Option<u64>,
    /// What the reversal actually did — a partial reversal, or the reason it was
    /// refused at the moment it ran.
    pub reversal_note: Option<String>,
    /// When the pre-image aged out of the retention window. The row survives (it
    /// is evidence of the change); its bytes stop being live for the collector.
    pub expired_at_ms: Option<u64>,
    /// Observation timestamp.
    pub created_at_ms: u64,
}

impl WorkspaceEffect {
    /// Whether this row is a confirmed observation: the call returned and its
    /// post-image was recorded.
    ///
    /// An incomplete row is *true* — its pre-image is what was there before the
    /// call — but it is not evidence that the mutation ran, so nothing may act
    /// on it.
    pub fn is_completed(&self) -> bool {
        self.completed_at_ms.is_some()
    }

    /// When the call returned and its post-image was observed, if it did.
    ///
    /// The instant the *caller* declared at completion, recorded verbatim. An
    /// accessor rather than a field read so a consumer that needs it (a test asserting
    /// the declared instant survived the round trip, an inspection surface) does not
    /// depend on the row's representation.
    pub fn finished_at(&self) -> Option<u64> {
        self.completed_at_ms
    }

    /// Whether this row can be reversed: reversible class, confirmed
    /// observation, and no prior reversal.
    ///
    /// Every clause is load-bearing. A class that is not reversible has no
    /// pre-image to restore; an unconfirmed row cannot tell us whether this call
    /// was the thing that changed the file; an already-reversed row must not be
    /// reversed twice (which would write stale bytes over newer content).
    pub fn is_undoable(&self) -> bool {
        self.effect_class == "file_bytes"
            && self.irreversible_reason.is_none()
            && self.is_completed()
            && self.reversed_at_ms.is_none()
            // A row past its retention window still *describes* the change, but
            // its bytes have been reclaimed, so offering it for undo would promise
            // a restore that cannot read what it needs.
            && self.expired_at_ms.is_none()
    }
}

/// One effect to append.
///
/// Deliberately a plain data struct with no builders: the capture site
/// (the harness middleware) has exactly one shape of effect to record, and a
/// builder would only add a way to forget a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWorkspaceEffect<'a> {
    /// Owning thread.
    pub thread_id: &'a str,
    /// Round the effect happened in.
    pub round_id: &'a str,
    /// ReAct turn within that round.
    pub turn_id: &'a str,
    /// The dispatch call id.
    pub call_id: &'a str,
    /// Journal-local observation order within the thread.
    pub seq: i64,
    /// Canonical tool name.
    pub tool_name: &'a str,
    /// Absolute normalized path, when the effect touched exactly one file.
    pub path: Option<&'a str>,
    /// What happened to the path.
    pub op: JournalOp,
    /// Reversibility classification token.
    pub effect_class: &'a str,
    /// CAS hash of the pre-image.
    pub before_hash: Option<&'a str>,
    /// CAS hash of the post-image, when the caller already has it (the
    /// completed-receipt shape).
    pub after_hash: Option<&'a str>,
    /// Why the effect is not reversible, when it is not.
    pub irreversible_reason: Option<&'a str>,
    /// Observation timestamp.
    pub created_at_ms: u64,
}

/// An owned [`NewWorkspaceEffect`], for message passing through the
/// single-writer actor.
///
/// The borrowed form keeps the storage API allocation-free at the call site; the
/// owned form is what crosses the channel. `as_borrowed` is the only bridge, so
/// the field set cannot drift between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOwnedEffect {
    pub thread_id: String,
    pub round_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub seq: i64,
    pub tool_name: String,
    pub path: Option<String>,
    pub op: JournalOp,
    pub effect_class: String,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub irreversible_reason: Option<String>,
    pub created_at_ms: u64,
}

impl NewOwnedEffect {
    /// Borrow as a [`NewWorkspaceEffect`] for the storage call.
    pub fn as_borrowed(&self) -> NewWorkspaceEffect<'_> {
        NewWorkspaceEffect {
            thread_id: &self.thread_id,
            round_id: &self.round_id,
            turn_id: &self.turn_id,
            call_id: &self.call_id,
            seq: self.seq,
            tool_name: &self.tool_name,
            path: self.path.as_deref(),
            op: self.op,
            effect_class: &self.effect_class,
            before_hash: self.before_hash.as_deref(),
            after_hash: self.after_hash.as_deref(),
            irreversible_reason: self.irreversible_reason.as_deref(),
            created_at_ms: self.created_at_ms,
        }
    }
}

/// The journal stamp a reversal attempt produces.
///
/// Lives beside the store rather than in the harness because it is a shape of the
/// schema: `reversed_at_ms` plus the one note that explains what happened. The
/// harness decides *whether* to reverse; this type is how the decision is
/// recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEffectReversal {
    /// The row being stamped.
    pub effect_id: i64,
    /// Whether the pre-image was written back.
    pub restored: bool,
    /// What happened, in the form the user will read on a later `/diff`.
    pub note: String,
}

impl WorkspaceEffectReversal {
    /// A reversal that wrote the pre-image back.
    pub fn restored(effect_id: i64) -> Self {
        Self {
            effect_id,
            restored: true,
            note: "restored".to_string(),
        }
    }

    /// A reversal that deliberately left the file alone, with the reason.
    pub fn refused(effect_id: i64, reason: impl Into<String>) -> Self {
        Self {
            effect_id,
            restored: false,
            note: reason.into(),
        }
    }
}

/// Append one write-ahead effect and return its journal id.
///
/// The only write path, and the **first** of the three phases: call it after the
/// pre-image is durably stored and *before* the tool mutates the path, so a
/// crash mid-mutation leaves a record. The returned id is what
/// [`complete_effect`] completes.
///
/// A non-reversible effect (shell, network, process) is appended *after* the
/// call returns with its completion stamp already set, because there is nothing
/// to protect against a mid-call crash: the journal records it as a receipt, not
/// as a recovery point. [`append_completed_effect`] is the entry point for that
/// shape.
pub fn append_effect(
    conn: &Connection,
    effect: &NewWorkspaceEffect<'_>,
) -> Result<i64, PersistenceError> {
    conn.execute(
        "INSERT INTO workspace_effects (
            thread_id, round_id, turn_id, call_id, seq, tool_name, path, op,
            effect_class, before_hash, irreversible_reason, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            effect.thread_id,
            effect.round_id,
            effect.turn_id,
            effect.call_id,
            effect.seq,
            effect.tool_name,
            effect.path,
            effect.op.as_str(),
            effect.effect_class,
            effect.before_hash,
            effect.irreversible_reason,
            effect.created_at_ms as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Append an effect that needs no recovery point, already completed.
///
/// For classes where the journal's value is the *receipt* (what ran, when, with
/// which post-image) rather than the ability to restore: shell, network, and
/// process effects. There is no pre-image to protect, so the row arrives in its
/// final observation state rather than going through [`complete_effect`].
pub fn append_completed_effect(
    conn: &Connection,
    effect: &NewWorkspaceEffect<'_>,
    completed_at_ms: u64,
) -> Result<i64, PersistenceError> {
    conn.execute(
        "INSERT INTO workspace_effects (
            thread_id, round_id, turn_id, call_id, seq, tool_name, path, op,
            effect_class, before_hash, after_hash, irreversible_reason,
            completed_at_ms, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            effect.thread_id,
            effect.round_id,
            effect.turn_id,
            effect.call_id,
            effect.seq,
            effect.tool_name,
            effect.path,
            effect.op.as_str(),
            effect.effect_class,
            effect.before_hash,
            effect.after_hash,
            effect.irreversible_reason,
            completed_at_ms as i64,
            effect.created_at_ms as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Complete a write-ahead effect with its post-image. Returns `false` when the
/// row was already completed or does not exist.
///
/// The **second** phase. Idempotency is deliberately *not* provided by silent
/// re-application: a caller that believes it is completing a row it did not
/// append has a bug, and the boolean makes that visible. The trigger
/// `trg_workspace_effects_completion_monotonic` is the backstop.
pub fn complete_effect(
    conn: &Connection,
    effect_id: i64,
    after_hash: Option<&str>,
    completed_at_ms: u64,
) -> Result<bool, PersistenceError> {
    let changed = conn.execute(
        "UPDATE workspace_effects SET after_hash = ?1, completed_at_ms = ?2 \
         WHERE id = ?3 AND completed_at_ms IS NULL",
        params![after_hash, completed_at_ms as i64, effect_id],
    )?;
    Ok(changed == 1)
}

const SELECT_COLUMNS: &str = "id, thread_id, round_id, turn_id, call_id, seq, tool_name, \
     path, op, effect_class, before_hash, after_hash, irreversible_reason, \
     completed_at_ms, reversed_at_ms, reversal_note, expired_at_ms, created_at_ms";

fn row_to_effect(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceEffect> {
    let op_token: String = row.get(8)?;
    // An unreadable op is a schema this build cannot interpret: surface it
    // loudly rather than defaulting to `Edit` and reversing with the wrong
    // semantics (writing content back over a file that should be removed).
    let op = JournalOp::parse(&op_token).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "unknown workspace journal op '{op_token}'"
            ))),
        )
    })?;
    Ok(WorkspaceEffect {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        round_id: row.get(2)?,
        turn_id: row.get(3)?,
        call_id: row.get(4)?,
        seq: row.get(5)?,
        tool_name: row.get(6)?,
        path: row.get(7)?,
        op,
        effect_class: row.get(9)?,
        before_hash: row.get(10)?,
        after_hash: row.get(11)?,
        irreversible_reason: row.get(12)?,
        completed_at_ms: row.get::<_, Option<i64>>(13)?.map(|ms| ms as u64),
        reversed_at_ms: row.get::<_, Option<i64>>(14)?.map(|ms| ms as u64),
        reversal_note: row.get(15)?,
        expired_at_ms: row.get::<_, Option<i64>>(16)?.map(|ms| ms as u64),
        created_at_ms: row.get::<_, i64>(17)? as u64,
    })
}

/// Every effect of one thread, in observation order — the `/diff` projection.
pub fn list_effects(
    conn: &Connection,
    thread_id: &str,
) -> Result<Vec<WorkspaceEffect>, PersistenceError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM workspace_effects \
         WHERE thread_id = ?1 ORDER BY seq ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![thread_id], row_to_effect)?;
    let mut effects = Vec::new();
    for row in rows {
        effects.push(row?);
    }
    Ok(effects)
}

/// Effects of one thread from one round onward, in observation order.
///
/// The granular unit `/undo` and `/diff` operate on is a round: it is the
/// boundary the user perceives ("what did that turn do?") and the boundary the
/// causal graph already uses (`round_id` on `facts`).
pub fn list_effects_from_round(
    conn: &Connection,
    thread_id: &str,
    round_id: &str,
) -> Result<Vec<WorkspaceEffect>, PersistenceError> {
    // `round_id` is text and the harness permits non-numeric numbering on
    // resume, so the boundary is resolved by position, not by numeric compare:
    // find the lowest `seq` in that round, then take everything at or above it.
    let boundary: Option<i64> = conn
        .query_row(
            "SELECT MIN(seq) FROM workspace_effects WHERE thread_id = ?1 AND round_id = ?2",
            params![thread_id, round_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(boundary) = boundary else {
        return Ok(Vec::new());
    };
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM workspace_effects \
         WHERE thread_id = ?1 AND seq >= ?2 ORDER BY seq ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![thread_id, boundary], row_to_effect)?;
    let mut effects = Vec::new();
    for row in rows {
        effects.push(row?);
    }
    Ok(effects)
}

/// The newest `limit` confirmed, not-yet-reversed, **unexpired** reversible
/// effects, newest first.
///
/// This is the `/undo` candidate set: one round's worth of undoable work, in the
/// order it must be reversed (latest first, so overlapping writes to one path
/// unwind back through their own history rather than jumping to the oldest
/// pre-image).
///
/// Every clause is part of the predicate rather than a filter applied afterwards,
/// because a candidate the caller then has to refuse is a `/undo` that reports work
/// it did not do. `completed_at_ms IS NOT NULL` excludes an unconfirmed effect (the
/// journal cannot say whether it changed anything); `expired_at_ms IS NULL` excludes
/// one whose bytes the retention sweep has reclaimed (it cannot be restored).
pub fn undoable_effects(
    conn: &Connection,
    thread_id: &str,
    limit: usize,
) -> Result<Vec<WorkspaceEffect>, PersistenceError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM workspace_effects \
         WHERE thread_id = ?1 AND effect_class = 'file_bytes' \
           AND reversed_at_ms IS NULL AND completed_at_ms IS NOT NULL \
           AND expired_at_ms IS NULL \
           AND irreversible_reason IS NULL \
         ORDER BY id DESC LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![thread_id, limit as i64], row_to_effect)?;
    let mut effects = Vec::new();
    for row in rows {
        effects.push(row?);
    }
    Ok(effects)
}

/// Stamp a row as reversed. Returns `false` when the row was already reversed
/// or does not exist, which the caller must treat as "someone else got here
/// first" rather than as success.
///
/// The trigger `trg_workspace_effects_reversal_monotonic` is the backstop: even
/// a caller that ignores this return value cannot clear or re-stamp the column.
pub fn mark_reversed(
    conn: &Connection,
    effect_id: i64,
    reversed_at_ms: u64,
    note: Option<&str>,
) -> Result<bool, PersistenceError> {
    let changed = conn.execute(
        "UPDATE workspace_effects SET reversed_at_ms = ?1, reversal_note = ?2 \
         WHERE id = ?3 AND reversed_at_ms IS NULL",
        params![reversed_at_ms as i64, note, effect_id],
    )?;
    Ok(changed == 1)
}

/// The journal's newest `seq` for a thread, so a caller can append densely
/// without a read-modify-write race on the whole table.
pub fn latest_seq(conn: &Connection, thread_id: &str) -> Result<i64, PersistenceError> {
    let seq: Option<i64> = conn.query_row(
        "SELECT MAX(seq) FROM workspace_effects WHERE thread_id = ?1",
        params![thread_id],
        |row| row.get(0),
    )?;
    Ok(seq.unwrap_or(0))
}

#[cfg(test)]
mod retention_tests;
#[cfg(test)]
mod tests;

/// Default retention window for pre-image bytes: thirty days.
///
/// Chosen as a *policy default*, not a technical limit: a pre-image's value decays
/// with time (the further back an edit is, the less likely the user wants it back,
/// and the more likely the file has moved on so that reversal would refuse
/// anyway), while its storage cost does not. Thirty days is long enough that any
/// realistic "oops" is recoverable and short enough that the CAS stays proportional
/// to recent work rather than to lifetime history.
pub const DEFAULT_PREIMAGE_RETENTION_DAYS: u64 = 30;

/// Expire journal rows whose pre-images are past the retention window.
///
/// Returns the number of rows newly expired. Expiry is a **non-destructive**
/// transition: the row keeps its observation (what path, what op, which hashes,
/// when) and gains `expired_at_ms`; only the *bytes* leave the live set, because
/// `DbReader::live_blob_hashes` stops
/// counting them. That split is deliberate — the journal is evidence, and a change
/// that happened does not stop having happened because its bytes were reclaimed.
///
/// Only completed, not-yet-reversed rows expire. An unconfirmed row keeps whatever
/// it has, or a post-crash inspection could not tell what it recorded; a reversed
/// row's pre-image has already served its purpose.
pub fn expire_preimages(
    conn: &Connection,
    older_than_ms: u64,
    now_ms: u64,
) -> Result<usize, PersistenceError> {
    let expired = conn.execute(
        "UPDATE workspace_effects
            SET expired_at_ms = ?1
          WHERE expired_at_ms IS NULL
            AND completed_at_ms IS NOT NULL
            AND reversed_at_ms IS NULL
            AND created_at_ms < ?2",
        params![now_ms as i64, older_than_ms as i64],
    )?;
    Ok(expired)
}

/// How many pre-images are live, and how many are past the window but not yet
/// expired. Reporting only; the collector acts on the derived live set.
pub fn preimage_posture(
    conn: &Connection,
    older_than_ms: u64,
) -> Result<(usize, usize), PersistenceError> {
    let live: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workspace_effects
          WHERE effect_class = 'file_bytes' AND expired_at_ms IS NULL
            AND before_hash IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    let stale: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workspace_effects
          WHERE effect_class = 'file_bytes' AND expired_at_ms IS NULL
            AND before_hash IS NOT NULL AND created_at_ms < ?1",
        params![older_than_ms as i64],
        |row| row.get(0),
    )?;
    Ok((live as usize, stale as usize))
}
