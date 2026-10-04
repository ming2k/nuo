//! Per-session monitor state machines (ADR-0093 §3).
//!
//! A [`MonitorTracker`] taps one hosted session's [`AgentResponse`] broadcast
//! and maintains the [`SessionStatus`] + accounting row a control panel
//! renders. The tap lives in the session's broadcast-tap task
//! ([`crate::registry`]), which calls [`MonitorTracker::observe`] for every
//! response; the registry owns publishing the resulting
//! [`nuo_wire::MonitorEvent`] diffs to the host-level topic.
//!
//! The tracker is deliberately *derived state, not protocol state*: the round
//! lifecycle itself stays binary (ADR-0078) — `Idle`/`Running` here are a
//! display badge, and the `Needs*` states are overlays on a still-running
//! round, exactly like the single-session `ParentStatus` (ADR-0017).

use nuo_wire::{AgentResponse, MonitoredSession, RoundEvent, SessionStatus};

/// Tracks one hosted session. `base` is the cheap header row (from the same
/// deferred parse that feeds the sessions picker), re-seeded whenever a
/// `SessionsOverview` snapshot flows by (a rename/delete pushes one); every
/// other field is folded from the live event stream, starting at
/// [`Self::bootstrap`].
pub struct MonitorTracker {
    base: MonitoredSession,
    status: SessionStatus,
    round: u64,
    turn: Option<usize>,
    output_tokens: u64,
    round_started: Option<std::time::Instant>,
    elapsed_ms: u64,
    current_tool: Option<String>,
    activity: Option<String>,
    context_tokens: Option<usize>,
    note: Option<String>,
}

impl MonitorTracker {
    /// Seed the tracker for a session that has just been (re-)hosted.
    ///
    /// `seed_status` is the registry's bootstrap verdict: [`SessionStatus::Running`]
    /// when the resumed transcript's tail shows an unfinished round (the
    /// driver will re-drive it), otherwise [`SessionStatus::Idle`].
    pub fn bootstrap(mut base: MonitoredSession, seed_status: SessionStatus) -> Self {
        base.status = seed_status;
        Self {
            base,
            status: seed_status,
            round: 0,
            turn: None,
            output_tokens: 0,
            round_started: if seed_status == SessionStatus::Running {
                Some(std::time::Instant::now())
            } else {
                None
            },
            elapsed_ms: 0,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
        }
    }

    /// The current panel row. `updated_at` advances with every observed event
    /// so a consumer can sort by liveness without the session store.
    pub fn row(&self) -> MonitoredSession {
        let mut row = self.base.clone();
        row.updated_at = now_secs();
        row.status = self.status;
        row.round = self.round;
        row.turn = self.turn;
        row.output_tokens = self.output_tokens;
        row.elapsed_ms = self.elapsed_ms();
        row.current_tool = self.current_tool.clone();
        row.activity = self.activity.clone();
        row.context_tokens = self.context_tokens;
        row.note = self.note.clone();
        row
    }

    /// Fold one broadcast response into the tracked state.
    pub fn observe(&mut self, response: &AgentResponse) {
        // A sessions-overview snapshot (pushed after a rename or delete)
        // carries the store-authoritative picker header for every session;
        // fold ours in so a `RenameSession` repaints the row. The live event
        // stream never carries the title — the base header was seeded once at
        // bootstrap — so without this the row would show the stale overview
        // forever. `updated_at` is deliberately left alone: [`Self::row`]
        // re-stamps it on every projection.
        if let AgentResponse::SessionsOverview(items) = response {
            if let Some(item) = items.iter().find(|item| item.id == self.base.id) {
                self.base.overview.clone_from(&item.overview);
                self.base.message_count = item.message_count;
                self.base.created_at = item.created_at;
                // Lineage (fork surfacing): the overview is the authoritative
                // source — it reads the persisted snapshot, where `parent_id`
                // and `fork_kind` are stamped at fork time.
                self.base.parent_id.clone_from(&item.parent_id);
                self.base.fork_kind = item.fork_kind;
                self.base.digest.clone_from(&item.digest);
            }
            return;
        }
        let AgentResponse::Round { event, .. } = response else {
            return;
        };
        match event {
            RoundEvent::TurnStarted { round, turn } => {
                if self.round_started.is_none() {
                    // A fresh round begins with its first turn; subsequent
                    // turns of the same round arrive while a start time is
                    // already recorded and only update the position.
                    self.round_started = Some(std::time::Instant::now());
                    self.output_tokens = 0;
                    self.current_tool = None;
                }
                self.round = *round;
                self.turn = Some(*turn);
                self.note = None;
                self.status = SessionStatus::Running;
            }
            RoundEvent::ToolCall { name, .. } => {
                self.current_tool = Some(name.clone());
                self.status = SessionStatus::Running;
                self.note = None;
            }
            RoundEvent::ToolResult { .. } | RoundEvent::ToolCancelled { .. } => {
                self.current_tool = None;
            }
            RoundEvent::PermissionRequest(request) => {
                self.status = SessionStatus::NeedsApproval;
                self.note = Some(format!("permission: {}", request.tool));
            }
            RoundEvent::UserQuestionRequest(request) => {
                self.status = SessionStatus::NeedsInput;
                self.note = Some(
                    request
                        .questions
                        .first()
                        .map(|q| truncate(&q.question, 80))
                        .unwrap_or_else(|| "question pending".to_string()),
                );
            }
            RoundEvent::StdinRequest(request) => {
                self.status = SessionStatus::NeedsInput;
                self.note = Some(format!("stdin: {}", truncate(&request.prompt, 80)));
            }
            RoundEvent::StreamStart | RoundEvent::StreamDelta(_) | RoundEvent::StreamEnd(_) => {
                // Model output is flowing again: any human-decision overlay
                // has been resolved.
                if matches!(
                    self.status,
                    SessionStatus::NeedsApproval | SessionStatus::NeedsInput
                ) {
                    self.status = SessionStatus::Running;
                    self.note = None;
                }
            }
            RoundEvent::RoundCompleted(summary) => {
                self.finish_round(SessionStatus::Idle);
                self.round = summary.round;
                self.output_tokens = summary.output_tokens;
                self.elapsed_ms = summary.duration_ms;
            }
            RoundEvent::Error(message) => {
                self.finish_round(SessionStatus::Failed);
                self.note = Some(truncate(message, 120));
            }
            RoundEvent::UnsentInput { .. } => {
                self.finish_round(SessionStatus::Interrupted);
            }
            RoundEvent::RoundInterrupted(record) => {
                // Every interrupted round now carries an explicit record
                // (C11) — including the previously invisible supersede and
                // phase-2/3 interrupt paths. Fold it as the row's terminal
                // state with the reason as the note.
                self.finish_round(SessionStatus::Interrupted);
                self.note = Some(truncate(record.label(), 120));
            }
            RoundEvent::Activity(text) => {
                self.activity = Some(truncate(text, 120));
            }
            RoundEvent::ContextTokens(snapshot) => {
                self.context_tokens = Some(snapshot.tokens);
            }
            _ => {}
        }
    }

    /// A `Chat`/`ChatToSession` was admitted for a fresh round: pre-mark the
    /// session running so the panel reflects it before the first `TurnStarted`
    /// arrives. Idempotent within a round. (Currently the round boundary is
    /// learned from the event stream itself — see `observe`'s `TurnStarted`
    /// arm — so this is a public seam for the control plane, not yet called.)
    #[allow(dead_code)]
    pub fn note_new_round(&mut self) {
        if self.round_started.is_none() {
            self.round += 1;
            self.round_started = Some(std::time::Instant::now());
            self.turn = None;
            self.output_tokens = 0;
            self.current_tool = None;
            self.note = None;
            self.status = SessionStatus::Running;
        }
    }

    fn elapsed_ms(&self) -> u64 {
        match self.round_started {
            Some(start) => start.elapsed().as_millis() as u64,
            None => self.elapsed_ms,
        }
    }

    fn finish_round(&mut self, status: SessionStatus) {
        self.elapsed_ms = self.elapsed_ms();
        self.round_started = None;
        self.current_tool = None;
        self.status = status;
        if status != SessionStatus::Failed {
            self.note = None;
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::{PermissionRequest, RoundSummary};

    fn round_event(event: RoundEvent) -> AgentResponse {
        AgentResponse::Round {
            session_id: "s".into(),
            event,
        }
    }

    fn tracker() -> MonitorTracker {
        MonitorTracker::bootstrap(
            MonitoredSession {
                id: "s".into(),
                overview: "task".into(),
                created_at: 1,
                updated_at: 1,
                message_count: 2,
                status: SessionStatus::Idle,
                hosting: nuo_wire::SessionHosting::Hosted,
                round: 0,
                turn: None,
                output_tokens: 0,
                elapsed_ms: 0,
                current_tool: None,
                activity: None,
                context_tokens: None,
                note: None,
                project_root: "/tmp/proj".into(),
                parent_id: None,
                fork_kind: nuo_wire::SessionForkKind::Trunk,
                digest: None,
            },
            SessionStatus::Idle,
        )
    }

    #[test]
    fn sessions_overview_refreshes_the_base_header() {
        // A rename pushes a sessions-overview snapshot through the broadcast;
        // the tracker must re-seed its base header from it so the republished
        // row shows the new title (the overview derives from the stored
        // title, which the live event stream never carries).
        let mut t = tracker();
        t.observe(&AgentResponse::SessionsOverview(vec![
            nuo_wire::SessionOverview {
                id: "s".into(),
                overview: "renamed title".into(),
                created_at: 7,
                updated_at: 8,
                message_count: 9,
                active: true,
                parent_id: None,
                fork_kind: nuo_wire::SessionForkKind::Trunk,
                digest: None,
            },
            // Another session's row must not leak into ours.
            nuo_wire::SessionOverview {
                id: "other".into(),
                overview: "someone else".into(),
                created_at: 1,
                updated_at: 1,
                message_count: 1,
                active: false,
                parent_id: None,
                fork_kind: nuo_wire::SessionForkKind::Trunk,
                digest: None,
            },
        ]));
        let row = t.row();
        assert_eq!(row.overview, "renamed title");
        assert_eq!(row.message_count, 9);
        assert_eq!(row.created_at, 7);
    }

    #[test]
    fn sessions_overview_without_our_id_leaves_the_header_alone() {
        // After a delete (or before first persist) the snapshot may not carry
        // our session at all; the seeded header must survive untouched.
        let mut t = tracker();
        t.observe(&AgentResponse::SessionsOverview(vec![
            nuo_wire::SessionOverview {
                id: "other".into(),
                overview: "someone else".into(),
                created_at: 1,
                updated_at: 1,
                message_count: 1,
                active: false,
                parent_id: None,
                fork_kind: nuo_wire::SessionForkKind::Trunk,
                digest: None,
            },
        ]));
        let row = t.row();
        assert_eq!(row.overview, "task");
        assert_eq!(row.message_count, 2);
    }

    #[test]
    fn fresh_round_flows_running_to_idle_with_summary() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        let row = t.row();
        assert_eq!(row.status, SessionStatus::Running);
        assert_eq!(row.round, 1);
        assert_eq!(row.turn, Some(0));
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 1 }));
        assert_eq!(t.row().turn, Some(1), "a second turn keeps the round open");
        t.observe(&round_event(RoundEvent::ToolCall {
            id: "c1".into(),
            name: "execute_command".into(),
            arguments: "{}".into(),
        }));
        assert_eq!(t.row().current_tool.as_deref(), Some("execute_command"));
        t.observe(&round_event(RoundEvent::RoundCompleted(RoundSummary {
            round: 1,
            output_tokens: 300,
            duration_ms: 5_000,
            paused_ms: 0,
            generation_ms: 2_000,
            ..Default::default()
        })));
        let row = t.row();
        assert_eq!(row.status, SessionStatus::Idle);
        assert_eq!(row.round, 1);
        assert_eq!(row.output_tokens, 300);
        assert_eq!(row.elapsed_ms, 5_000);
        assert!(row.current_tool.is_none());
    }

    #[test]
    fn permission_overlay_clears_on_stream_resume() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::PermissionRequest(
            PermissionRequest {
                id: "r".into(),
                tool: "write_file".into(),
                label: "Write file".into(),
                description: String::new(),
                arguments: "{}".into(),
                scope: String::new(),
                elevation: false,
                one_off: false,
                origin: None,
                ..Default::default()
            },
        )));
        assert_eq!(t.row().status, SessionStatus::NeedsApproval);
        assert!(t.row().note.unwrap().contains("write_file"));
        t.observe(&round_event(RoundEvent::StreamDelta("…".into())));
        assert_eq!(t.row().status, SessionStatus::Running);
        assert!(t.row().note.is_none());
    }

    #[test]
    fn error_round_reports_failed_and_keeps_note() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::Error("provider blew up".into())));
        let row = t.row();
        assert_eq!(row.status, SessionStatus::Failed);
        assert_eq!(row.note.as_deref(), Some("provider blew up"));
    }

    #[test]
    fn interrupted_round_reports_interrupted() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::UnsentInput {
            prompt: "redo".into(),
            images: Vec::new(),
        }));
        assert_eq!(t.row().status, SessionStatus::Interrupted);
    }

    #[test]
    fn round_interrupted_event_reports_interrupted_with_reason_note() {
        // C11: the explicit record covers every interrupt phase (including
        // supersede and phase-2/3, which previously left the row stuck on
        // Running) and folds the reason into the note.
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::RoundInterrupted(
            nuo_wire::RoundInterrupt {
                reason: nuo_wire::RoundInterruptReason::User,
                at_ms: 1_700_000_000_000,
                round: Some(1),
                detail: None,
            },
        )));
        let row = t.row();
        assert_eq!(row.status, SessionStatus::Interrupted);
        assert_eq!(row.note.as_deref(), Some("Esc Esc"));
    }

    #[test]
    fn repeated_turn_starts_keep_one_open_round() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 1 }));
        let row = t.row();
        assert_eq!(row.round, 1, "turns of one round must not double-count");
        assert_eq!(row.turn, Some(1));
        assert_eq!(row.status, SessionStatus::Running);
    }
}
