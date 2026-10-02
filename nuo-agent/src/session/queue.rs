use crate::error::{AgentError, Result};
use acp::{AgentAddress, ChannelId};

/// Identifies the conversation a turn belongs to.
///
/// A session key is the unit of *context continuity* and of *serialization*.
/// Choosing the key correctly is what makes the concurrency story coherent:
///
/// - Keying per **agent** serializes everything, so one long task blocks all
///   others and unrelated work contaminates shared context.
/// - Keying per **request** means no continuity at all: a follow-up has no
///   memory of the previous turn.
/// - Keying per **conversation surface** gives each conversation its own ordered
///   history, with unrelated conversations running concurrently.
///
/// # Multi-party conversations
///
/// An agent has *many* sessions, never one, and the key names the **surface** the
/// conversation happens on rather than assuming a single counterpart:
///
/// | Surface | Key | Used for |
/// |---------|-----|----------|
/// | 1:1 with an agent | [`SessionKey::Peer`] | Task delegation, queries |
/// | Threaded 1:1 | [`SessionKey::Thread`] | Independent continuity with one peer |
/// | Multi-party | [`SessionKey::Channel`] | A shared channel with N participants |
///
/// A channel is a conversation surface in its own right: when A, B and C discuss
/// something in `#ops`, that discussion has its own continuity, and B's reasoning
/// about it does not belong in B's private 1:1 history with A. Collapsing the two
/// would either fragment the shared conversation into pairwise copies, or force
/// every 1:1 to carry unrelated group chatter.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SessionKey {
    /// Conversation with a specific peer.
    Peer(AgentAddress),
    /// Conversation with a peer, scoped to an explicit thread.
    Thread { peer: AgentAddress, thread: String },
    /// Multi-party conversation on a channel.
    Channel(ChannelId),
}

impl SessionKey {
    /// Conversation key for a peer, unthreaded.
    pub fn for_peer(peer: &AgentAddress) -> Self {
        Self::Peer(peer.clone())
    }

    /// Conversation key for a peer within an explicit thread.
    ///
    /// An empty thread is treated as absent, so a caller cannot accidentally
    /// partition a conversation by passing `""`.
    pub fn for_thread(peer: &AgentAddress, thread: impl Into<String>) -> Self {
        let thread = thread.into();
        if thread.is_empty() {
            Self::Peer(peer.clone())
        } else {
            Self::Thread {
                peer: peer.clone(),
                thread,
            }
        }
    }

    /// Conversation key for a multi-party channel.
    pub fn for_channel(channel: ChannelId) -> Self {
        Self::Channel(channel)
    }

    /// The peer this conversation is with, if it is a 1:1 conversation.
    ///
    /// A channel conversation has no single counterpart, which is precisely why
    /// it needs its own key rather than pretending to be a peer.
    pub fn peer(&self) -> Option<&AgentAddress> {
        match self {
            Self::Peer(peer) | Self::Thread { peer, .. } => Some(peer),
            Self::Channel(_) => None,
        }
    }

    /// The channel this conversation is on, if any.
    pub fn channel(&self) -> Option<&ChannelId> {
        match self {
            Self::Channel(id) => Some(id),
            _ => None,
        }
    }

    /// Human-readable description, used in prompts and diagnostics.
    pub fn describe(&self) -> String {
        match self {
            Self::Peer(peer) => format!("direct conversation with {peer}"),
            Self::Thread { peer, thread } => {
                format!("conversation with {peer} in thread `{thread}`")
            }
            Self::Channel(id) => format!("multi-party conversation on #{id}"),
        }
    }

    /// Stable identifier, used as the persisted session id.
    pub fn as_session_id(&self) -> String {
        match self {
            Self::Peer(peer) => format!("peer:{peer}"),
            Self::Thread { peer, thread } => format!("thread:{peer}#{thread}"),
            Self::Channel(id) => format!("channel:{id}"),
        }
    }
}

/// How inbound work relates to a turn already running.
///
/// This is the distinction that matters and that only the *sender* can make:
///
/// | Variant | Meaning | Consumed |
/// |---------|---------|----------|
/// | [`Inbound::Turn`] | New work: becomes its own turn | After the current turn finishes (FIFO) |
/// | [`Inbound::Steer`] | Guidance for work in flight | At the next round boundary |
///
/// Inference is atomic, so "inject immediately" cannot mean mid-generation. It
/// means *at the next round boundary*, which is exactly what `Steer` does. New
/// work must not take that path: injecting an unrelated task mid-turn would
/// contaminate the running task's reasoning.
#[derive(Debug, Clone)]
pub enum Inbound {
    /// A new turn, queued behind work already in this session.
    Turn {
        /// Who sent the work.
        from: AgentAddress,
        /// The task text.
        prompt: String,
        /// Envelope id to correlate the answer with.
        correlation: uuid::Uuid,
    },
    /// Guidance for the turn currently running.
    Steer {
        /// Who sent the guidance.
        from: AgentAddress,
        /// Which in-flight request it applies to.
        correlation: uuid::Uuid,
        /// The instruction.
        instruction: String,
        /// What the receiver should do with it.
        action: acp::SteerAction,
    },
}

impl Inbound {
    /// Whether this is new work rather than guidance.
    pub fn is_turn(&self) -> bool {
        matches!(self, Self::Turn { .. })
    }

    /// Whether this is guidance.
    pub fn is_steer(&self) -> bool {
        matches!(self, Self::Steer { .. })
    }
}

/// Bounded FIFO queue for one session.
///
/// `Session` is the single writer of its own history, so a queue per session is
/// what makes that guarantee real: turns in one conversation are processed in
/// arrival order, with no interleaving.
#[derive(Debug)]
pub struct TurnQueue {
    key: SessionKey,
    pending: std::collections::VecDeque<Inbound>,
    capacity: usize,
}

impl TurnQueue {
    /// Creates a queue with the given bound.
    pub fn new(key: SessionKey, capacity: usize) -> Self {
        Self {
            key,
            pending: std::collections::VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    pub fn key(&self) -> &SessionKey {
        &self.key
    }

    /// Number of turns waiting.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Whether the queue is at capacity.
    pub fn is_full(&self) -> bool {
        self.pending.len() >= self.capacity
    }

    /// Enqueues a turn.
    ///
    /// Returns [`AgentError::SessionBusy`] when full rather than blocking or
    /// dropping: the sender learns immediately and can decide whether to retry
    /// or delegate elsewhere, which is strictly better than a silent loss or an
    /// unbounded queue.
    pub fn push_turn(&mut self, item: Inbound) -> Result<()> {
        if self.is_full() {
            return Err(AgentError::SessionBusy(format!(
                "session `{}` queue is full ({} pending)",
                self.key.as_session_id(),
                self.pending.len()
            )));
        }
        self.pending.push_back(item);
        Ok(())
    }

    /// Applies a steering instruction to this queue.
    ///
    /// Steering does **not** consume a queue slot: it modifies work already
    /// accepted, so a saturated queue must not prevent a cancellation or a
    /// correction from arriving.
    ///
    /// A `Cancel` also drops queued turns for the same request, since continuing
    /// to process turns for cancelled work would be wasted effort.
    pub fn push_steer(&mut self, item: Inbound) -> Result<()> {
        let Inbound::Steer { correlation, .. } = &item else {
            return Err(AgentError::Session(format!(
                "`push_steer` requires a steering item for session `{}`",
                self.key.as_session_id()
            )));
        };
        let correlation = *correlation;

        // A cancel supersedes any guidance queued for the same request: applying
        // a note after a cancellation would resurrect work the sender stopped.
        if let Inbound::Steer { action, .. } = &item
            && action.is_terminal()
        {
            self.pending.retain(|queued| {
                !matches!(queued, Inbound::Steer { correlation: c, .. } if *c == correlation)
            });
        } else {
            // A later note supersedes an earlier one for the same request, so a
            // corrected instruction replaces the stale one instead of stacking.
            self.pending.retain(|queued| {
                !matches!(queued, Inbound::Steer { correlation: c, action, .. }
                    if *c == correlation && !action.is_terminal())
            });
        }

        self.pending.push_back(item);
        Ok(())
    }

    /// Removes and returns the next item, oldest first.
    pub fn pop(&mut self) -> Option<Inbound> {
        self.pending.pop_front()
    }

    /// Removes queued turns for `correlation`, keeping anything else.
    ///
    /// Used when a request is cancelled or times out so abandoned work is not
    /// executed later.
    pub fn discard_correlated(&mut self, correlation: uuid::Uuid) -> usize {
        let before = self.pending.len();
        self.pending.retain(
            |queued| !matches!(queued, Inbound::Turn { correlation: c, .. } if *c == correlation),
        );
        before - self.pending.len()
    }

    /// Queued items, oldest first, for inspection.
    pub fn pending(&self) -> Vec<&Inbound> {
        self.pending.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(uri: &str) -> AgentAddress {
        AgentAddress::parse(uri).unwrap()
    }

    fn turn(from: &str, text: &str) -> Inbound {
        Inbound::Turn {
            from: peer(from),
            prompt: text.into(),
            correlation: uuid::Uuid::new_v4(),
        }
    }

    #[test]
    fn session_key_defaults_to_peer_and_threads_explicitly() {
        let p = peer("agent://local/peer");

        assert_eq!(SessionKey::for_peer(&p), SessionKey::Peer(p.clone()));
        // An empty thread must not silently create a separate conversation.
        assert_eq!(SessionKey::for_thread(&p, ""), SessionKey::Peer(p.clone()));

        let threaded = SessionKey::for_thread(&p, "release-1");
        assert_ne!(threaded, SessionKey::for_peer(&p));
        assert_eq!(threaded.peer(), Some(&p));
        assert_eq!(threaded.channel(), None);
        assert_eq!(
            threaded.as_session_id(),
            "thread:agent://local/peer#release-1"
        );
    }

    #[test]
    fn a_channel_key_is_multi_party_and_has_no_single_peer() {
        let channel = ChannelId::new("ops").unwrap();
        let key = SessionKey::for_channel(channel.clone());

        // Absence of a peer is exactly what distinguishes a shared discussion
        // from a private conversation.
        assert_eq!(key.peer(), None);
        assert_eq!(key.channel(), Some(&channel));
        assert_eq!(key.as_session_id(), "channel:ops");
        assert!(key.describe().contains("multi-party"));
    }

    #[test]
    fn the_three_surfaces_never_share_a_session_id() {
        let p = peer("agent://local/alice");
        let ids = [
            SessionKey::for_peer(&p).as_session_id(),
            SessionKey::for_thread(&p, "t1").as_session_id(),
            SessionKey::for_channel(ChannelId::new("ops").unwrap()).as_session_id(),
        ];
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "session ids must be unambiguous");
    }

    #[test]
    fn queue_is_fifo() {
        let mut q = TurnQueue::new(SessionKey::for_peer(&peer("agent://local/p")), 8);
        q.push_turn(turn("agent://local/a", "first")).unwrap();
        q.push_turn(turn("agent://local/a", "second")).unwrap();

        let first = q.pop().unwrap();
        let second = q.pop().unwrap();
        assert!(matches!(first, Inbound::Turn { prompt, .. } if prompt == "first"));
        assert!(matches!(second, Inbound::Turn { prompt, .. } if prompt == "second"));
        assert!(q.pop().is_none());
    }

    #[test]
    fn a_full_queue_rejects_rather_than_blocking_or_dropping() {
        let mut q = TurnQueue::new(SessionKey::for_peer(&peer("agent://local/p")), 2);
        q.push_turn(turn("agent://local/a", "one")).unwrap();
        q.push_turn(turn("agent://local/a", "two")).unwrap();

        let err = q.push_turn(turn("agent://local/a", "three")).unwrap_err();
        assert!(matches!(err, AgentError::SessionBusy(_)), "got {err:?}");

        // The rejected item must not have displaced queued work.
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn steering_is_accepted_even_when_the_queue_is_full() {
        let mut q = TurnQueue::new(SessionKey::for_peer(&peer("agent://local/p")), 1);
        q.push_turn(turn("agent://local/a", "busy")).unwrap();
        assert!(q.is_full());

        // A cancellation must still get through: refusing it would mean a stuck
        // queue cannot be stopped.
        let correlation = uuid::Uuid::new_v4();
        q.push_steer(Inbound::Steer {
            from: peer("agent://local/a"),
            correlation,
            instruction: "stop".into(),
            action: acp::SteerAction::Cancel,
        })
        .unwrap();
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn a_later_note_replaces_an_earlier_one_for_the_same_request() {
        let mut q = TurnQueue::new(SessionKey::for_peer(&peer("agent://local/p")), 8);
        let correlation = uuid::Uuid::new_v4();

        for instruction in ["first guess", "corrected instruction"] {
            q.push_steer(Inbound::Steer {
                from: peer("agent://local/a"),
                correlation,
                instruction: instruction.into(),
                action: acp::SteerAction::Note,
            })
            .unwrap();
        }

        let notes: Vec<String> = q
            .pending()
            .into_iter()
            .filter_map(|i| match i {
                Inbound::Steer { instruction, .. } => Some(instruction.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes,
            vec!["corrected instruction"],
            "stale note must not stack"
        );
    }

    #[test]
    fn a_cancel_discards_queued_correlated_work() {
        let mut q = TurnQueue::new(SessionKey::for_peer(&peer("agent://local/p")), 8);
        let correlation = uuid::Uuid::new_v4();

        q.push_turn(Inbound::Turn {
            from: peer("agent://local/a"),
            prompt: "abandoned task".into(),
            correlation,
        })
        .unwrap();
        q.push_turn(turn("agent://local/a", "unrelated")).unwrap();

        let dropped = q.discard_correlated(correlation);
        assert_eq!(dropped, 1, "cancelled work must not run later");

        let remaining: Vec<String> = q
            .pending()
            .into_iter()
            .filter_map(|i| match i {
                Inbound::Turn { prompt, .. } => Some(prompt.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(remaining, vec!["unrelated"], "unrelated work must survive");
    }
}
