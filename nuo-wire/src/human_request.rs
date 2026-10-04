//! The human-request control protocol (ADR-0141).
//!
//! Three protocols used to park an agent mid-round on a human decision —
//! permission approval, `ask_user` questions, and interactive stdin — each
//! maintained its own oneshot lifecycle while sharing the same settlement
//! rule (`requested/parked --> replied | cancelled`, see
//! `docs/reference/state-model.md`). This module is the shared vocabulary
//! that lets one broker (`muta_agent::human_broker`) own all three.
//!
//! The axis the old protocol lacked is *provenance*: who actually settled a
//! request. A non-TTY headless client used to answer `ask_user` with the
//! first option and the model received "User answered the question(s)" — a
//! fabricated human decision. Under this protocol a request only resolves
//! as [`ReplyProvenance::User`] when a human saw it; everything else is
//! labeled policy.

use serde::{Deserialize, Serialize};

/// Which of the three parked protocols a request belongs to. Carried on
/// every parked request so cancellation, hooks, and metrics can be uniform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HumanRequestKind {
    /// A write/execute the user must approve (permission broker), or a
    /// dangerous-command confirmation.
    Permission,
    /// A structured multiple-choice question (`ask_user`).
    Question,
    /// A line of stdin for an interactive `bash` command.
    #[serde(alias = "Input")]
    Stdin,
}

/// The interactivity posture a client declares in its attach `Select` frame.
///
/// The session's effective channel is the OR over all attached clients: one
/// interactive watcher is enough. Subagent children inherit their parent's
/// posture, so a question that would flow up to a nonexistent human fails
/// fast in the child instead of parking forever.
///
/// Legacy clients that predate the field default to `Interactive`,
/// preserving their behavior exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HumanChannelPosture {
    /// A human is watching this session right now and can answer parked
    /// requests. The default for TUI and Web clients.
    #[default]
    Interactive,
    /// No human is reachable — headless runs without a TTY, CI, cron
    /// wrappers. The agent must not park on the user: requests resolve per
    /// the configured [`AutonomousFallbackPolicy`], and every resolution is
    /// labeled with its true source.
    Autonomous,
}

/// What happens to an `ask_user` question when the session's human channel
/// is [`HumanChannelPosture::Autonomous`].
///
/// Permission requests and interactive input never take the
/// `RecommendedLabeled` branch: permissions fail closed (a missing human
/// cannot grant authority) and interactive commands run with closed stdin
/// exactly as if the operator had dismissed the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomousFallbackPolicy {
    /// Refuse the question: the tool returns an "unavailable" result and the
    /// model must resolve the ambiguity itself (pick the safest option and
    /// say so in prose). The default — a missing human is an error, not an
    /// opinion.
    #[default]
    FailClosed,
    /// Answer with each question's first option — the convention the
    /// `ask_user` schema establishes as "recommended" — with an explicit
    /// `[answered by policy, not by user]` label in the tool result. The
    /// model knows it holds a recommendation, not a decision.
    RecommendedLabeled,
}

/// Who actually settled a parked human request. The anti-fabrication
/// invariant: only a reply that crossed a client connection resolves as
/// [`ReplyProvenance::User`]. Policy settlements are generated agent-side and are labeled
/// so the model can never mistake them for human intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplyProvenance {
    /// A human saw the request and decided. Only wire-originated replies
    /// carry this.
    User,
    /// Policy decided because no human channel existed. The payload names
    /// the policy so metrics can distinguish fail-closed refusals from
    /// labeled recommendations.
    Policy { policy: AutonomousFallbackPolicy },
}

/// The settlement payload for any parked request, unified so one broker map
/// can hold all three kinds. Internal to the harness — it never crosses the
/// wire (wire replies are by construction [`ReplyProvenance::User`]; policy
/// settlements are generated where they are consumed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HumanReply {
    Permission(crate::PermissionDecision),
    Question(Option<crate::UserQuestionReply>),
    Stdin(Option<crate::StdinReply>),
}

impl HumanReply {
    /// The provenance of a settlement that arrived through a client
    /// connection — always [`ReplyProvenance::User`] by construction.
    pub fn wire_provenance(&self) -> ReplyProvenance {
        ReplyProvenance::User
    }
}

/// Session-level OR-accounting of attached clients' postures
/// (ADR-0141). One [`HumanChannelPosture::Interactive`] client keeps the
/// session interactive; the session is autonomous only while zero
/// interactive clients are attached. Shared by the WS attach layer (attach
/// / detach) and the harness (posture gate) — the harness reads, the WS
/// layer writes, `AtomicUsize` makes each side lock-free against the other.
///
/// Attach/detach bookkeeping is per-connection and monotonic: a client that
/// declared `Autonomous` still increments the connection count (so a later
/// interactive attach is detectable) but never the interactive count.
#[derive(Debug, Default)]
pub struct HumanChannelAccountant {
    interactive: std::sync::atomic::AtomicUsize,
    connections: std::sync::atomic::AtomicUsize,
}

impl HumanChannelAccountant {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an attaching client's declaration. Returns the effective
    /// posture after the attach.
    pub fn attach(&self, posture: HumanChannelPosture) -> HumanChannelPosture {
        self.connections
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if posture == HumanChannelPosture::Interactive {
            self.interactive
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        self.effective()
    }

    /// Record a client disconnect. Returns the effective posture after the
    /// detach (may drop to Autonomous when the last interactive watcher
    /// leaves — parked requests then resolve by labeled policy).
    ///
    /// The disconnecting client's own declaration is unknown here (the WS
    /// layer calls this from its connection-drop path), so the conservative
    /// move is to clamp the interactive count to the remaining connection
    /// count: a disconnect can only remove interactivity, never add it.
    pub fn detach(&self) -> HumanChannelPosture {
        let remaining = self
            .connections
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
            .saturating_sub(1);
        let interactive = self.interactive_count();
        if interactive > remaining {
            self.interactive
                .store(remaining, std::sync::atomic::Ordering::Release);
        }
        self.effective()
    }

    /// The current effective posture.
    pub fn effective(&self) -> HumanChannelPosture {
        if self.interactive.load(std::sync::atomic::Ordering::Acquire) > 0 {
            HumanChannelPosture::Interactive
        } else {
            HumanChannelPosture::Autonomous
        }
    }

    /// Interactive attachments right now.
    pub fn interactive_count(&self) -> usize {
        self.interactive.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Total attachments right now.
    pub fn connection_count(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::Acquire)
    }
}
