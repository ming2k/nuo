//! Daemon-observability wire contracts (ADR-0093): the [`MonitorAction`]
//! handshake selector and the [`MonitorEvent`] stream a daemon
//! publishes about every session it hosts.
//!
//! These types are the read-only control-plane counterpart of the
//! session-scoped `AgentRequest`/`AgentResponse` protocol: a control panel (or
//! any other observer) connects, selects `Monitor`, receives one
//! [`MonitorEvent::Snapshot`], and then follows `MonitorEvent::Diff`s. They
//! carry **no conversation content** — only ids, titles/previews, status, and
//! accounting — so a dashboard never deserializes a transcript.
//!
//! The types are pure contracts (ADR-0057): no I/O, no derivations. The
//! session status machine that produces [`SessionStatus`] values from the
//! `AgentResponse` stream lives in `muta_runtime::monitor`.

use serde::{Deserialize, Serialize};

use crate::events::SessionForkKind;

/// Handshake action selecting a daemon-observability stream instead of a
/// session attach (ADR-0093 §2). Sent as the first frame:
/// `{"type":"Select","action":{"monitor":{"watch":…,"include_idle":…}}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct MonitorAction {
    /// Keep the connection open and stream `MonitorEvent::Diff`s after the
    /// initial snapshot (`muta status --watch`, live control apps). When
    /// `false` the server sends the snapshot and closes the connection.
    #[serde(default)]
    pub watch: bool,
    /// Include live sessions that are simply idle (no round running and
    /// nothing blocked). Defaults to `false` so a busy dashboard stays a
    /// zero-statement surface: an all-idle daemon reports an empty list.
    #[serde(default)]
    pub include_idle: bool,
}

/// How the session behind a [`MonitoredSession`] row is hosted. Under
/// ADR-0096's unified ownership every session is daemon-held, so this is
/// always [`Hosted`](Self::Hosted); the field is kept on the wire (with its
/// serde default) so rows produced before the distinction was removed still
/// deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum SessionHosting {
    /// The session's driver lives inside the serving host process (an
    /// `attach`-created or lazily resumed session). The host owns its
    /// lifecycle and can serve full `Attach` clients for it.
    #[default]
    Hosted,
}

impl SessionHosting {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hosted => "hosted",
        }
    }
}

impl std::fmt::Display for SessionHosting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stream frame about the daemon as a whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum MonitorEvent {
    /// The full current state, sent exactly once as the first frame after the
    /// monitor handshake. Sessions are sorted by `updated_at`, newest first.
    Snapshot(MonitorSnapshot),
    /// One hosted session was created or re-hosted (lazy resume). Carries its
    /// complete row so a consumer needs no back-reference.
    SessionAdded(MonitoredSession),
    /// One hosted session's row changed in place.
    SessionUpdated(MonitoredSession),
    /// A hosted session shut down. Consumers drop the row. (Session teardown
    /// is not yet emitted by the host — hosted sessions live for the daemon's
    /// lifetime — but the variant is part of the contract so panels written
    /// against it handle teardown when it lands.)
    SessionRemoved { session_id: String },
    /// The daemon as a whole began its graceful shutdown (ADR-0101): no new
    /// attaches will be served, live connections are being closed with a
    /// WebSocket `GoingAway`, and every hosted session's teardown (including
    /// `SessionEnd` hooks) is in flight. Watch clients should treat the
    /// stream as terminal — the process exits after a bounded grace budget —
    /// and surface a "daemon stopping" notice rather than attempting
    /// reconnects against it. Emitted exactly once, before the individual
    /// `SessionRemoved` diffs of the same shutdown.
    DaemonDraining,
    /// A daemon-level task (ADR-0190) changed in place — spawned, progressed
    /// to `Ready`, or settled. Carries the complete row; consumers upsert
    /// by id. Session-scoped tasks do not stream here.
    TaskUpdated(MonitoredTask),
    /// A daemon-level task left the snapshot entirely (pruned / aborted).
    TaskRemoved { task_id: String },
    /// The daemon's durable-storage writer changed state (ADR-0196): healthy
    /// again after a degradation, degraded further, or recovering. A
    /// `Healthy` transition clears any retained degradation banner.
    PersistenceHealth(PersistenceHealth),
}

/// User-visible durability health of the daemon's single-writer persistence
/// actor (ADR-0196 D4). While not `Healthy`, durability is degraded: every
/// frontend should retain a visible banner until the next `Healthy` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum PersistenceHealth {
    /// Serving normally. Clears a previously shown degradation banner.
    Healthy,
    /// The writer died (or failed to open the database) and the supervisor
    /// is respawning it with backoff. Writes in this window fail with a
    /// retryable "writer down" error.
    Recovering {
        attempt: u32,
        since_ms: u64,
        error: String,
    },
    /// Respawn attempts exceeded the recovering budget; retries continue
    /// forever, but durability is effectively unavailable and must be
    /// surfaced to the user, not just logged.
    Down {
        attempt: u32,
        since_ms: u64,
        error: String,
    },
}

impl PersistenceHealth {
    /// Whether durability is currently expected to work.
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// The degradation banner line: severity already implied by the variant.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Healthy => None,
            Self::Recovering { error, .. } | Self::Down { error, .. } => Some(error),
        }
    }
}

/// The daemon-level snapshot: who is serving and what is happening right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct MonitorSnapshot {
    pub project_root: String,
    /// Unix seconds when the daemon process started (from the discovery
    /// record; `0` when the registry was not created by a daemon, e.g. an
    /// in-TUI `/serve` prehost).
    pub daemon_started_at: u64,
    pub sessions: Vec<MonitoredSession>,
    /// Daemon-level task fabric rows (ADR-0190): rehosted services and any
    /// other task with no owning session. Session-scoped tasks stay in
    /// their session's own fabric; this is the human-side view of what the
    /// daemon itself is running on the operator's behalf. Empty for
    /// producers that predate the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<MonitoredTask>,
    /// Latest durability-health state (ADR-0196 D4), folded from the
    /// supervisor's transitions. `None` for producers that predate the
    /// field or when the writer has never degraded (healthy by omission).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence_health: Option<PersistenceHealth>,
}

/// One row of the daemon-level task tree (ADR-0190 D6): identity, spec
/// label, lifecycle state, and ownership. Content-free — the transcript
/// stays in the session, the full log stays on disk (path included).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct MonitoredTask {
    pub id: String,
    /// Human label (job label, or the command's first word).
    pub label: String,
    /// Spec summary line (command preview / timer descriptor).
    pub spec: String,
    pub state: crate::job::JobState,
    /// Owning session id; `None` = daemon-level (rehosted services).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session: Option<String>,
    /// Unix-epoch ms the task was created.
    pub created_at_ms: u64,
    /// Unix-epoch ms of settle, when terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    /// Latest output line, for the at-a-glance tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_output: Option<String>,
    /// On-disk full log path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
}

/// One row of the control panel: a hosted session's identity, status, and
/// accounting. Deliberately a superset of nothing — every field is cheap and
/// content-free (see module docs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct MonitoredSession {
    pub id: String,
    /// Stored AI/manual title, falling back to the first-prompt preview.
    pub overview: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    /// Who owns the session's driver. Defaults to `Hosted` — the only value
    /// since ADR-0096 — so producers written against ADR-0093 stay valid.
    #[serde(default)]
    pub hosting: SessionHosting,
    /// Derived lifecycle status (ADR-0093 §3): the panel's primary sort key.
    pub status: SessionStatus,
    /// 1-based index of the current (or most recently completed) user round.
    pub round: u64,
    /// 0-based index of the model request within the current round, when one
    /// has started.
    pub turn: Option<usize>,
    /// Output tokens generated by the current/most-recent round so far.
    pub output_tokens: u64,
    /// Wall-clock milliseconds since the current round started (frozen at the
    /// final duration once the round terminates).
    pub elapsed_ms: u64,
    /// Currently executing tool, if any.
    pub current_tool: Option<String>,
    /// Latest one-line activity string (e.g. "waiting for model").
    pub activity: Option<String>,
    /// Current AI-visible context size, when reported.
    pub context_tokens: Option<usize>,
    /// One-line error/notice text for `Failed` / `NeedsApproval` / `NeedsInput`.
    pub note: Option<String>,
    /// Absolute project workspace path this session belongs to (ADR-0096's
    /// two-level indexing projected down to the row). Empty for producers
    /// that predate the field (e.g. `/serve` prehosts) — display code must
    /// tolerate it. Content-free in the monitor sense: it is addressing
    /// metadata, not conversation.
    #[serde(default)]
    pub project_root: String,
    /// Lineage (ADR-0103 fork surfacing): the parent session this one was
    /// forked from, when it is a branch. `None` on a trunk. The dashboard
    /// groups by trunk: one main card per conversation, its branches
    /// badged beneath — the main line is always exactly one.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// How this session came to exist: trunk root, `/fork` branch, or
    /// `/btw` aside. Defaults to `Trunk` for producers that predate the
    /// field.
    #[serde(default)]
    pub fork_kind: SessionForkKind,
    /// Structured digest (intent + history checklist), if
    /// the session has generated one.
    #[serde(default)]
    pub digest: Option<crate::cognitive::SessionDigest>,
}

impl MonitoredSession {
    /// A zeroed row for one session id — the seed a tracker starts from
    /// before any event has been folded in.
    pub fn empty(id: String) -> Self {
        Self {
            id,
            overview: String::new(),
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            hosting: SessionHosting::Hosted,
            status: SessionStatus::Idle,
            round: 0,
            turn: None,
            output_tokens: 0,
            elapsed_ms: 0,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
            project_root: String::new(),
            parent_id: None,
            fork_kind: SessionForkKind::default(),
            digest: None,
        }
    }
}

/// Display-level lifecycle status of a hosted session, derived from its
/// response stream. This is the multi-session analogue of the single-session
/// [`ParentStatus`](crate::ParentStatus) badge (ADR-0017): a coarse,
/// panel-facing classification, not the protocol state — the round lifecycle
/// itself stays binary (`RoundLifecycle`, ADR-0078).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum SessionStatus {
    /// No round running, nothing waiting on a human.
    Idle,
    /// A round is actively producing model output or running tools.
    Running,
    /// Blocked on a tool-permission decision.
    NeedsApproval,
    /// Blocked on an `ask_user` question or interactive-command input.
    NeedsInput,
    /// The current round ended via interruption (Esc); the prompt may resume.
    Interrupted,
    /// The current round ended with a turn-level error.
    Failed,
}

impl SessionStatus {
    /// Whether a panel row in this status describes ongoing or blocked work —
    /// the default (non-`include_idle`) filter for monitor snapshots.
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// The wire string, also used directly by the `muta status` table.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::NeedsApproval => "needs-approval",
            Self::NeedsInput => "needs-input",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_action_defaults_are_off() {
        let action: MonitorAction = serde_json::from_str("{}").unwrap();
        assert!(!action.watch);
        assert!(!action.include_idle);
    }

    #[test]
    fn monitor_action_roundtrips() {
        let action = MonitorAction {
            watch: true,
            include_idle: true,
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: MonitorAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back, action);
    }

    #[test]
    fn session_status_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&SessionStatus::NeedsApproval).unwrap(),
            "\"needs_approval\""
        );
        assert_eq!(SessionStatus::NeedsApproval.as_str(), "needs-approval");
        assert_eq!(SessionStatus::NeedsInput.to_string(), "needs-input");
    }

    #[test]
    fn session_status_is_active_gates_idle_only() {
        assert!(!SessionStatus::Idle.is_active());
        for status in [
            SessionStatus::Running,
            SessionStatus::NeedsApproval,
            SessionStatus::NeedsInput,
            SessionStatus::Interrupted,
            SessionStatus::Failed,
        ] {
            assert!(status.is_active(), "{status} should be active");
        }
    }

    #[test]
    fn monitor_event_uses_kind_tag() {
        let event = MonitorEvent::SessionRemoved {
            session_id: "s-1".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, r#"{"kind":"session_removed","session_id":"s-1"}"#);
        let back: MonitorEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn daemon_draining_serializes_as_unit_tag() {
        let json = serde_json::to_string(&MonitorEvent::DaemonDraining).unwrap();
        assert_eq!(json, r#"{"kind":"daemon_draining"}"#);
        let back: MonitorEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MonitorEvent::DaemonDraining);
    }

    #[test]
    fn hosting_defaults_to_hosted_for_older_producers() {
        let json = r#"{"id":"s","overview":"","created_at":0,"updated_at":0,"message_count":0,"status":"idle","round":0,"turn":null,"output_tokens":0,"elapsed_ms":0,"current_tool":null,"activity":null,"context_tokens":null,"note":null}"#;
        let row: MonitoredSession = serde_json::from_str(json).unwrap();
        assert_eq!(row.hosting, SessionHosting::Hosted);
    }

    #[test]
    fn snapshot_roundtrips_with_a_full_row() {
        let snapshot = MonitorSnapshot {
            project_root: "/tmp/proj".into(),
            daemon_started_at: 1_700_000_000,
            tasks: Vec::new(),
            persistence_health: None,
            sessions: vec![MonitoredSession {
                id: "s-1".into(),
                overview: "fix the flaky test".into(),
                created_at: 1,
                updated_at: 2,
                message_count: 7,
                hosting: SessionHosting::Hosted,
                status: SessionStatus::Running,
                round: 3,
                turn: Some(1),
                output_tokens: 512,
                elapsed_ms: 9_000,
                current_tool: Some("execute_command".into()),
                activity: Some("running bash".into()),
                context_tokens: Some(48_000),
                note: None,
                project_root: "/tmp/proj".into(),
                parent_id: None,
                fork_kind: SessionForkKind::Trunk,
                digest: None,
            }],
        };
        let json = serde_json::to_string(&MonitorEvent::Snapshot(snapshot.clone())).unwrap();
        let back: MonitorEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MonitorEvent::Snapshot(snapshot));
    }
}
