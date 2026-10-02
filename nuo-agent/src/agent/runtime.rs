use crate::collaboration::{Collaboration, DelegationContext, install_collaboration_tools};
use crate::error::{AgentError, Result};
use crate::provider::Provider;
use crate::session::{Session, SessionEvent, SessionKey, SessionStore, SteeringHandle};
use crate::token::{Compactor, TokenBudget};
use crate::tools::{Tool, ToolRegistry};
use acp::{
    AgentAddress, AgentEnvelope, AgentManifest, DelegationBudget, Fabric, Mailbox, MessageIntent,
    NotifyReason,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::r#loop::CognitiveLoop;

/// Default bound on a single delegation round trip.
const DEFAULT_DELEGATION_TIMEOUT: Duration = Duration::from_secs(120);

/// How many unread messages per channel are surfaced in a system prompt.
const CHANNEL_UNREAD_LIMIT: usize = 20;

/// Why a channel turn is running.
///
/// Deliberately distinct from [`NotifyReason`], which is a *protocol* fact about
/// why a notification was delivered. This is a *runtime* cause, and it has one
/// case the protocol cannot express: a turn the agent decided to run itself,
/// without any notification at all. Folding that into `NotifyReason` would force
/// callers to claim a notification they never received.
///
/// The cause is what shapes the turn: being addressed calls for an answer,
/// keeping up does not, and a turn the agent chose to run is its own business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelWake {
    /// A notification arrived because the agent's policy is
    /// [`SubscriptionMode::All`](acp::SubscriptionMode::All).
    Subscribed,
    /// A notification arrived because the message names this agent.
    Mentioned,
    /// An unaddressed message matched the agent's subscription filter.
    MatchedFilter,
    /// The agent or its host started the turn directly. Nobody addressed it and
    /// no subscription policy was involved.
    Requested,
}

impl ChannelWake {
    /// Whether the agent owes the channel a reply.
    ///
    /// This is the gate on *automatic* posting. A subscriber that was merely
    /// keeping up decides for itself whether to contribute; a mentioned agent has
    /// been asked for an answer.
    pub fn expects_attention(self) -> bool {
        matches!(
            self,
            Self::Mentioned | Self::Requested | Self::MatchedFilter
        )
    }

    /// Instruction given to the model, derived from the cause.
    fn guidance(self) -> &'static str {
        match self {
            Self::Mentioned => "You were mentioned by name, so the message is addressed to you.",
            Self::Requested => "You were asked to look at this discussion.",
            Self::MatchedFilter => {
                "This unaddressed message matched your expertise or filter, so you were selected to handle it."
            }
            Self::Subscribed => {
                "You are subscribed to this channel, so you are seeing this on your own \
                 initiative rather than because anyone addressed you."
            }
        }
    }
}

impl From<NotifyReason> for ChannelWake {
    fn from(reason: NotifyReason) -> Self {
        match reason {
            NotifyReason::Subscribed => Self::Subscribed,
            NotifyReason::Mentioned => Self::Mentioned,
            NotifyReason::MatchedFilter => Self::MatchedFilter,
        }
    }
}

/// An autonomous agent: a model, a tool set, and a session lifecycle.
///
/// An agent is *not* inherently networked. Collaboration capabilities exist only
/// when the agent is built into a [`Room`]; otherwise the agent is an isolated
/// cognitive worker with no protocol surface.
#[derive(Clone)]
pub struct Agent {
    manifest: AgentManifest,
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    budget: TokenBudget,
    compactor: Compactor,
    collaboration: Option<Collaboration>,
    /// Inbox retained for [`Agent::serve`]. Shared across clones so that exactly
    /// one serve loop can ever claim it, which is asserted at runtime.
    inbox: Option<Arc<StdMutex<Option<Mailbox>>>>,
    /// Steering instructions waiting for in-flight turns.
    steering: SteeringHandle,
    /// Requests currently executing, used to decide whether guidance is
    /// actionable or has arrived too late.
    in_flight: Arc<tokio::sync::Mutex<HashMap<Uuid, SessionKey>>>,
    /// Per-session serialization: each session processes one turn at a time.
    ///
    /// This is what makes "single writer per session" real. Without it, two
    /// concurrent turns on one session would interleave their appends and
    /// produce a history that reflects neither conversation.
    session_locks: Arc<tokio::sync::Mutex<HashMap<SessionKey, Arc<tokio::sync::Mutex<()>>>>>,
    /// Optional persistence for session continuity across turns.
    store: Option<Arc<dyn SessionStore>>,
    /// Optional long-term memory for cross-session knowledge retention.
    memory: Option<Arc<dyn crate::memory::Memory>>,
    /// Optional tool approval handler for human-in-the-loop policies.
    approval_handler: Option<Arc<dyn crate::tools::ApprovalHandler>>,
    /// Anti-abuse tool execution and quota policy.
    policy: nuo_tool::ToolPolicy,
    /// Active operational scopes restricting model-visible tools.
    active_scopes: Option<Vec<nuo_tool::ToolScope>>,
    /// Unique physical instance identifier for this agent node.
    instance_id: Uuid,
    /// Active peer pairings established via P2P handshakes.
    peer_pairings: Arc<tokio::sync::Mutex<HashMap<AgentAddress, acp::HandshakeAckPayload>>>,
    /// Optional cryptographic signature verifier for zero-trust envelope verification.
    signature_verifier: Option<Arc<dyn acp::signature::EnvelopeVerifier>>,
    /// Security enforcement policy for zero-trust envelope verification.
    signature_enforcement: acp::signature::SignatureEnforcement,
    /// Procedural skills registered on this agent node.
    skills: crate::skill::SkillRegistry,
    /// In-flight stack of active procedural skill frames.
    skill_stack: Arc<std::sync::Mutex<crate::skill::SkillStack>>,
}

impl Agent {
    /// Starts building an agent at the given `agent://` address.
    pub fn builder(address: impl Into<String>) -> AgentBuilder {
        AgentBuilder::new(address)
    }

    /// This agent's capability card.
    pub fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }

    /// This agent's canonical address.
    pub fn address(&self) -> &AgentAddress {
        &self.manifest.address
    }

    /// This agent's unique physical node instance identifier.
    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    /// Performs an active P2P identity handshake with a peer agent, returning the confirmed pairing acknowledgment.
    pub async fn handshake_peer(
        &self,
        peer: &AgentAddress,
        timeout: Duration,
    ) -> Result<acp::HandshakeAckPayload> {
        let Some(collab) = &self.collaboration else {
            return Err(AgentError::Session(
                "agent has no collaboration binding configured".into(),
            ));
        };

        let public_key = self.manifest.address.to_string();
        let envelope = acp::AgentEnvelope::new(
            self.manifest.address.clone(),
            peer.clone(),
            acp::MessageIntent::handshake(
                self.manifest.address.clone(),
                self.instance_id,
                public_key,
                None,
            ),
        )
        .with_instance_id(self.instance_id);

        let reply = collab
            .mailbox()
            .request(envelope, timeout)
            .await
            .map_err(|e| AgentError::Session(format!("handshake to `{peer}` failed: {e}")))?;

        match reply.intent {
            acp::MessageIntent::HandshakeAck(payload) => {
                let mut guard = self.peer_pairings.lock().await;
                guard.insert(peer.clone(), payload.clone());
                Ok(payload)
            }
            other => Err(AgentError::Session(format!(
                "expected HandshakeAck from `{peer}`, got `{}`",
                other.summary()
            ))),
        }
    }

    /// Collaboration binding, present only when the agent is in a room.
    pub fn collaboration(&self) -> Option<&Collaboration> {
        self.collaboration.as_ref()
    }

    /// Long-term memory provider, if configured.
    pub fn memory(&self) -> Option<&Arc<dyn crate::memory::Memory>> {
        self.memory.as_ref()
    }

    /// Names of every tool registered on this agent.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.names()
    }

    /// Tool specifications as advertised to the model provider, respecting active scopes.
    pub fn model_specs(&self) -> Vec<serde_json::Value> {
        self.tools.model_specs_scoped(self.active_scopes.as_deref())
    }

    /// Tool specifications explicitly filtered by operational scopes.
    pub fn model_specs_scoped(
        &self,
        scopes: Option<&[nuo_tool::ToolScope]>,
    ) -> Vec<serde_json::Value> {
        self.tools.model_specs_scoped(scopes)
    }

    /// Names of the tools actually advertised to the provider, sorted.
    ///
    /// Kept separate from [`Agent::tool_names`] so a test can assert that
    /// advertisement and executability never drift apart.
    pub fn advertised_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .model_specs()
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str().map(str::to_string))
            .collect();
        names.sort();
        names
    }

    /// System prompt for a session, including the peer directory when peers
    /// exist. No directory section is produced for a lone agent.
    pub async fn system_prompt(&self) -> String {
        self.system_prompt_for(&SessionKey::for_peer(&self.manifest.address))
            .await
    }

    /// System prompt scoped to a specific conversation.
    ///
    /// Scoping matters because context must not leak across surfaces: a 1:1
    /// conversation should not carry a group's channel traffic, and a channel
    /// conversation should see that channel's discussion rather than one peer's
    /// private history.
    pub async fn system_prompt_for(&self, key: &SessionKey) -> String {
        let mut prompt = format!(
            "You are {}, {}.\nYour address: {}\nThis conversation: {}",
            self.manifest.name,
            self.manifest.description,
            self.manifest.address,
            key.describe()
        );

        if !self.manifest.skills.is_empty() {
            prompt.push_str(&format!(
                "\nYour skills: {}",
                self.manifest.skills.join(", ")
            ));
        }

        if !self.skills.is_empty() {
            let summary = self.skills.catalog_summary();
            if !summary.is_empty() {
                prompt.push_str("\n\n");
                prompt.push_str(&summary);
            }
        }

        if let Some(collaboration) = &self.collaboration
            && let Some(directory) = collaboration.peer_directory().await
        {
            prompt.push_str(&format!(
                "\n\nOther agents you can delegate to:\n\
                 {directory}\n\
                 Use `list_peers` to refresh this list, and `delegate_to_peer` to hand off work \
                 that matches a peer's specialty."
            ));
        }

        if let Some(memory) = &self.memory {
            let query = crate::memory::MemoryQuery::new("", key.clone()).with_limit(5);
            if let Ok(facts) = memory.recall(&query).await
                && !facts.is_empty()
            {
                prompt.push_str("\n\nRelevant long-term memories:\n");
                for fact in facts {
                    prompt.push_str(&format!("- {}\n", fact.content.trim()));
                }
            }
        }

        prompt
    }

    /// Answers a user prompt in a fresh session.
    pub async fn prompt(&self, text: impl Into<String>) -> Result<String> {
        let mut session = Session::new()
            .with_budget(self.budget)
            .with_system_prompt(self.system_prompt().await);
        session.add_user_message(text);

        self.run_session(&mut session, None).await
    }

    /// Answers a user prompt, streaming lifecycle events.
    pub async fn prompt_streaming(
        &self,
        text: impl Into<String>,
        events: mpsc::Sender<SessionEvent>,
    ) -> Result<String> {
        let mut session = Session::new()
            .with_budget(self.budget)
            .with_system_prompt(self.system_prompt().await);
        session.add_user_message(text);

        self.run_session(&mut session, Some(events)).await
    }

    /// Continues an existing session, with a fresh correlation id.
    ///
    /// Prefer [`Agent::run_session_correlated`] when inbound steering must be
    /// able to target this turn.
    pub async fn run_session(
        &self,
        session: &mut Session,
        events: Option<mpsc::Sender<SessionEvent>>,
    ) -> Result<String> {
        self.run_session_correlated(session, Uuid::new_v4(), events)
            .await
    }

    /// Continues an existing session, tied to a specific request id.
    ///
    /// The correlation id is what steering addresses, so a turn started here can
    /// be corrected or cancelled while it runs.
    pub async fn run_session_correlated(
        &self,
        session: &mut Session,
        correlation: Uuid,
        events: Option<mpsc::Sender<SessionEvent>>,
    ) -> Result<String> {
        let loop_ = CognitiveLoop::new(
            self.provider.clone(),
            self.tools.clone(),
            self.compactor.clone(),
            self.steering.clone(),
            self.approval_handler.clone(),
            self.policy.clone(),
            self.active_scopes.clone(),
            self.skill_stack.clone(),
        );
        loop_.run(session, correlation, events).await
    }

    /// Steering handle, for injecting guidance into in-flight turns.
    pub fn steering(&self) -> &SteeringHandle {
        &self.steering
    }

    /// Programmatically mounts a procedural skill onto this agent's active skill stack.
    pub fn mount_skill(&self, skill_id: &str) -> Result<()> {
        let Some(skill) = self.skills.get(skill_id) else {
            return Err(AgentError::Session(format!("Skill `{skill_id}` not found in registry")));
        };
        let mut stack = self.skill_stack.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev_scopes = stack.effective_scopes(self.active_scopes.as_ref());
        stack.push(skill.clone(), prev_scopes);
        Ok(())
    }

    /// Programmatically unmounts the active skill, restoring baseline capabilities.
    pub fn unmount_skill(&self) -> Option<crate::skill::Skill> {
        let mut stack = self.skill_stack.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        stack.pop().map(|frame| frame.skill)
    }

    /// Returns the currently active skill, if any.
    pub fn active_skill(&self) -> Option<crate::skill::Skill> {
        self.skill_stack.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active_skill().cloned()
    }

    /// Applies an inbound steering instruction to the matching in-flight turn.
    ///
    /// Returns whether a turn was actually running for that request. A `false`
    /// means the guidance arrived after the turn settled, which the caller may
    /// treat as "too late" rather than as an error.
    pub async fn steer(
        &self,
        correlation: Uuid,
        from: &AgentAddress,
        instruction: impl Into<String>,
        action: acp::SteerAction,
    ) -> bool {
        let in_flight = self.in_flight.lock().await.contains_key(&correlation);
        if in_flight {
            self.steering
                .push(correlation, from.clone(), instruction, action)
                .await;
        }
        in_flight
    }

    /// Number of turns currently executing for this agent.
    pub async fn active_turns(&self) -> usize {
        self.in_flight.lock().await.len()
    }

    /// Pings a peer agent to verify liveness and measure round-trip latency.
    pub async fn ping_peer(&self, peer: &AgentAddress, timeout: Duration) -> Result<Duration> {
        let Some(collab) = &self.collaboration else {
            return Err(AgentError::Session(
                "agent has no collaboration binding configured".into(),
            ));
        };
        let start = std::time::Instant::now();
        let envelope = acp::AgentEnvelope::new(
            self.manifest.address.clone(),
            peer.clone(),
            acp::MessageIntent::signal(acp::SignalKind::Ping),
        );
        let reply = collab
            .mailbox()
            .request(envelope, timeout)
            .await
            .map_err(|e| AgentError::Session(format!("ping to `{peer}` failed: {e}")))?;

        if matches!(
            reply.intent,
            acp::MessageIntent::Signal(ref s)
                if s.kind == acp::SignalKind::Ack || s.kind == acp::SignalKind::Pong
        ) {
            Ok(start.elapsed())
        } else {
            Err(AgentError::Session(format!(
                "expected Pong reply from `{peer}`, got `{}`",
                reply.intent.summary()
            )))
        }
    }

    /// Programmatically delegates a task to a peer agent and awaits its structured outcome.
    pub async fn delegate_to(
        &self,
        peer: &AgentAddress,
        task: impl Into<String>,
    ) -> Result<acp::DelegationOutcome> {
        self.delegate_with_thread(peer, task, None).await
    }

    /// Programmatically delegates a task to a peer agent, specifying an optional thread for session continuation.
    pub async fn delegate_with_thread(
        &self,
        peer: &AgentAddress,
        task: impl Into<String>,
        thread: Option<&str>,
    ) -> Result<acp::DelegationOutcome> {
        let Some(collab) = &self.collaboration else {
            return Err(AgentError::Session(
                "agent has no collaboration binding configured".into(),
            ));
        };
        let mut intent = acp::MessageIntent::delegate(task);
        if let (Some(t), acp::MessageIntent::Delegate(p)) = (thread, &mut intent) {
            let t_str = t.to_string();
            p.thread = Some(t_str.clone());
            p.session_intent = Some(acp::SessionIntent::Continue(t_str));
        }

        let envelope =
            acp::AgentEnvelope::new(self.manifest.address.clone(), peer.clone(), intent);

        let reply = collab
            .mailbox()
            .request(envelope, Duration::from_secs(120))
            .await
            .map_err(|e| AgentError::Delegation(e.to_string()))?;

        reply.outcome().ok_or_else(|| {
            AgentError::Delegation(format!(
                "peer replied with non-terminal intent `{}`",
                reply.intent.summary()
            ))
        })
    }

    /// Loads or creates the session for `key`, restoring persisted history.
    ///
    /// Continuity is per conversation surface, not per agent and not per request:
    /// each conversation keeps its own ordered history, so a follow-up has memory
    /// while unrelated conversations stay isolated.
    ///
    /// The system prompt is **rebuilt on every load** rather than persisted with
    /// the history. Room membership and channel traffic change over time, so a
    /// prompt baked in at session creation would freeze that conversation's view
    /// of the world permanently — it would never see a peer join or a message
    /// published after the session began.
    pub async fn session_for(&self, key: &SessionKey) -> Result<Session> {
        let id = key.as_session_id();
        let prompt = self.system_prompt_for(key).await;

        if let Some(store) = &self.store
            && let Some(mut existing) = store.load(&id).await?
        {
            existing.refresh_system_prompt(prompt);
            if let Some(collaboration) = &self.collaboration
                && let Some(ch_id) = key.channel()
                && let Some(unread) = collaboration
                    .unread_channels_for(key, CHANNEL_UNREAD_LIMIT)
                    .await
                && !unread.is_empty()
            {
                existing
                    .add_user_message(format!("Unread channel messages in #{ch_id}:\n{unread}"));
            }
            return Ok(existing);
        }

        let mut session = Session::with_id(&id)
            .with_budget(self.budget)
            .with_system_prompt(prompt);

        // Project channel traffic monotonically at the tail without dirtying system prompt
        if let Some(collaboration) = &self.collaboration
            && let Some(ch_id) = key.channel()
            && let Some(unread) = collaboration
                .unread_channels_for(key, CHANNEL_UNREAD_LIMIT)
                .await
            && !unread.is_empty()
        {
            session.add_user_message(format!("Unread channel messages in #{ch_id}:\n{unread}"));
        }

        session.add_metadata("session_key", serde_json::json!(id));
        session.add_metadata("scope", serde_json::json!(key.describe()));
        if let Some(peer) = key.peer() {
            session.add_metadata("peer", serde_json::json!(peer.to_string()));
        }
        if let Some(channel) = key.channel() {
            session.add_metadata("channel", serde_json::json!(channel.to_string()));
        }
        Ok(session)
    }

    /// Persists a session, when a store is configured.
    pub async fn persist(&self, session: &Session) -> Result<()> {
        if let Some(store) = &self.store {
            store.save(session).await?;
        }
        Ok(())
    }

    /// Acquires the serialization lock for a session, ensuring one writer.
    async fn lock_session(&self, key: &SessionKey) -> Arc<tokio::sync::Mutex<()>> {
        let mut guard = self.session_locks.lock().await;
        guard
            .entry(key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Number of per-session lock entries currently retained.
    ///
    /// Exposed so the runtime can be asserted not to accumulate state for
    /// conversations that have ended.
    pub async fn retained_session_locks(&self) -> usize {
        self.session_locks.lock().await.len()
    }

    /// Drops the serialization lock for `key` once no turn is using it.
    ///
    /// Callers must hold no guard on `lock` when calling this, and must call it
    /// after every turn — including turns that returned early or errored — since
    /// a retained entry is a permanent leak for a conversation that will never
    /// run again (see `tests/probe_lock_leak_test.rs`).
    ///
    /// Eviction is guarded rather than unconditional: the map entry is removed
    /// only while no other turn holds or awaits this mutex. `strong_count` is 2
    /// — this frame's reference plus the map's — exactly when nobody else is
    /// waiting, and the check happens under the map lock so a turn entering
    /// `lock_session` concurrently cannot slip past it. Removing unconditionally
    /// would let the next turn install a *second* mutex for the same session and
    /// break the single-writer guarantee.
    async fn release_session_lock(&self, key: &SessionKey, lock: &Arc<tokio::sync::Mutex<()>>) {
        let mut guard = self.session_locks.lock().await;
        let is_sole_owner = Arc::strong_count(lock) == 2;
        if is_sole_owner && guard.get(key).is_some_and(|held| Arc::ptr_eq(held, lock)) {
            guard.remove(key);
        }
    }

    /// Marks a request as in-flight for its duration.
    async fn begin_turn(&self, correlation: Uuid, key: &SessionKey) {
        self.in_flight.lock().await.insert(correlation, key.clone());
    }

    /// Clears in-flight state and any steering that never got applied.
    async fn end_turn(&self, correlation: Uuid) {
        self.in_flight.lock().await.remove(&correlation);
        self.steering.clear(correlation).await;
    }

    /// Runs one queued turn against the session for `key`.
    ///
    /// This is the single entry point for inbound work, so both guarantees live
    /// in one place:
    ///
    /// - **Serialization** — the per-session lock means turns in one
    ///   conversation never interleave. Different sessions still run
    ///   concurrently.
    /// - **Continuity** — the session is loaded, extended, and persisted, so a
    ///   follow-up sees what came before.
    pub async fn run_turn(
        &self,
        key: &SessionKey,
        correlation: Uuid,
        prompt: impl Into<String>,
    ) -> Result<String> {
        let prompt_text = prompt.into();
        let lock = self.lock_session(key).await;
        let prompt_for_run = prompt_text.clone();
        let result = async {
            let _guard = lock.lock().await;

            self.begin_turn(correlation, key).await;
            let outcome = async {
                let mut session = self.session_for(key).await?;
                session.add_user_message(prompt_for_run);
                let answer = self
                    .run_session_correlated(&mut session, correlation, None)
                    .await?;
                self.persist(&session).await?;
                Ok(answer)
            }
            .await;

            self.end_turn(correlation).await;
            outcome
        }
        .await;

        // Released only after the guard has dropped, so there is no window in
        // which a later turn could install a second mutex for this session.
        self.release_session_lock(key, &lock).await;

        if let Ok(answer) = &result
            && let Some(mem) = &self.memory
        {
            let outcome = crate::memory::TurnOutcome {
                session_key: key.clone(),
                correlation_id: correlation,
                user_prompt: prompt_text,
                agent_response: answer.clone(),
                tool_calls: Vec::new(),
            };
            let mem = mem.clone();
            tokio::spawn(async move {
                let _ = mem.observe(&outcome).await;
            });
        }

        result
    }

    /// Runs a turn in a channel conversation, consuming the unread backlog.
    ///
    /// A multi-party conversation is not a sequence of 1:1 messages: the agent
    /// needs to see what *everyone* said, in order, to respond usefully. So the
    /// turn is seeded with the channel's unread messages and the cursor advances,
    /// making the discussion part of this session's memory rather than of each
    /// sender's private history.
    ///
    /// `wake` is the cause of this turn, and it shapes what the agent is told to
    /// do — being addressed calls for an answer, whereas merely keeping up does
    /// not — as well as whether its output is posted back to the channel.
    ///
    /// Returns the agent's contribution, or `None` when there was nothing new to
    /// react to.
    pub async fn run_channel_turn(
        &self,
        channel_id: &acp::ChannelId,
        correlation: Uuid,
        wake: ChannelWake,
    ) -> Result<Option<String>> {
        let Some(collaboration) = &self.collaboration else {
            return Err(AgentError::Session(
                "agent is not in a room, so it has no channels".into(),
            ));
        };

        let Some(fabric) = collaboration.fabric() else {
            return Err(AgentError::Session(
                "agent is not connected to a fabric, so it has no channels".into(),
            ));
        };

        let channel = fabric
            .channel(channel_id)
            .await
            .ok_or_else(|| AgentError::Session(format!("channel `{channel_id}` does not exist")))?;

        if !channel.is_subscribed(collaboration.address()).await {
            return Err(AgentError::Session(format!(
                "agent is not subscribed to channel `{channel_id}`"
            )));
        }

        let key = SessionKey::for_channel(channel_id.clone());
        let lock = self.lock_session(&key).await;
        let result = async {
            let _guard = lock.lock().await;

            // Drain and advance: reading marks the discussion as considered, so a
            // later turn does not re-reason about the same messages.
            let unread = channel
                .drain_for(collaboration.address(), CHANNEL_UNREAD_LIMIT)
                .await?;
            if unread.is_empty() {
                return Ok(None);
            }

            let transcript: Vec<String> = unread.iter().map(|m| m.to_prompt_line()).collect();

            // A channel turn reads a bounded window, so the backlog may be deeper
            // than what is shown. Saying so matters: presented as a complete
            // transcript, a truncated view invites the agent to answer a partial
            // discussion as though it were the whole of it. The unread remainder
            // stays unread, so it is not lost — only deferred.
            let truncated = unread.len() >= CHANNEL_UNREAD_LIMIT;
            let scope = if truncated {
                format!(
                    "The channel backlog is deeper than the {CHANNEL_UNREAD_LIMIT} most recent \
                     unread messages shown here; older unread messages remain and will follow."
                )
            } else {
                String::new()
            };

            // The instruction depends on why this turn is running, and stating
            // the cause lets the agent judge relevance itself rather than guess
            // why it was woken.
            let expects_reply = if wake.expects_attention() {
                "Post your reply with `publish_to_channel`."
            } else {
                "Contribute only if you have something material to add; otherwise there is \
                 nothing to post."
            };
            let prompt = format!(
                "New messages in #{channel_id}:\n{}\n\n{scope} {} Respond as a participant in \
                 this discussion. {expects_reply}",
                transcript.join("\n"),
                wake.guidance()
            );

            self.begin_turn(correlation, &key).await;
            let outcome = async {
                let mut session = self.session_for(&key).await?;
                session.add_user_message(prompt);
                let answer = self
                    .run_session_correlated(&mut session, correlation, None)
                    .await?;
                self.persist(&session).await?;
                Ok(Some(answer))
            }
            .await;

            self.end_turn(correlation).await;
            outcome
        }
        .await;

        self.release_session_lock(&key, &lock).await;

        if let Ok(Some(answer)) = &result
            && let Some(mem) = &self.memory
        {
            let outcome = crate::memory::TurnOutcome {
                session_key: key.clone(),
                correlation_id: correlation,
                user_prompt: format!("Channel discussion in #{channel_id}"),
                agent_response: answer.clone(),
                tool_calls: Vec::new(),
            };
            let mem = mem.clone();
            tokio::spawn(async move {
                let _ = mem.observe(&outcome).await;
            });
        }

        result
    }

    /// Executes one inbound envelope, producing the reply to send back.
    ///
    /// Only `Delegate` and `Query` are actionable; other intents are surfaced as
    /// errors so the serve loop can decide whether to answer. Delegation hop
    /// budget from the envelope is honoured and propagated to nested calls.
    pub async fn handle_envelope(&self, envelope: &AgentEnvelope) -> Result<AgentEnvelope> {
        if let Some(verifier) = &self.signature_verifier {
            let max_skew = Some(std::time::Duration::from_secs(300));
            match envelope.verify_with(verifier.as_ref(), max_skew).await {
                Ok(true) => {}
                Ok(false) => {
                    if self.signature_enforcement == acp::SignatureEnforcement::Strict
                        || envelope.signature.is_some()
                    {
                        return Err(AgentError::Protocol(
                            acp::ProtocolError::RoutingError(format!(
                                "envelope from `{}` rejected by zero-trust security policy: invalid signature",
                                envelope.source
                            )),
                        ));
                    }
                }
                Err(err) => return Err(AgentError::Protocol(err)),
            }
        } else if self.signature_enforcement == acp::SignatureEnforcement::Strict {
            return Err(AgentError::Protocol(
                acp::ProtocolError::RoutingError(
                    "strict zero-trust policy active but no verifier configured".into(),
                ),
            ));
        }

        match &envelope.intent {
            MessageIntent::Handshake(payload) => {
                let association_id = payload.association_id.clone().unwrap_or_else(|| {
                    format!(
                        "pair_{}_{}",
                        self.manifest.address.path(),
                        payload.instance_id
                    )
                });
                let ack = acp::HandshakeAckPayload {
                    instance_id: self.instance_id,
                    public_key: self.manifest.address.to_string(),
                    association_id: association_id.clone(),
                    manifest: self.manifest.clone(),
                };
                let mut reply = envelope.reply(
                    self.manifest.address.clone(),
                    acp::MessageIntent::HandshakeAck(ack),
                );
                reply.association_id = Some(association_id);
                reply.instance_id = Some(self.instance_id);
                Ok(reply)
            }

            MessageIntent::Delegate(payload) => {
                // The budget arrives already decremented by the sender's
                // context; re-deriving it here keeps nested delegation bounded.
                let budget = envelope
                    .budget
                    .unwrap_or_else(|| DelegationBudget::default().descend().unwrap_or_default());

                let delegation_ctx = DelegationContext::new(envelope.source.clone(), budget);

                let task = payload.task.clone();
                let context_note = payload
                    .context
                    .as_ref()
                    .map(|ctx| {
                        format!(
                            "\n\nContext supplied by the requesting agent:\n{}",
                            serde_json::to_string_pretty(ctx).unwrap_or_else(|_| ctx.to_string())
                        )
                    })
                    .unwrap_or_default();

                let prompt = format!(
                    "{task}{context_note}\n\n\
                     Answer the requesting agent directly with the result. \
                     Do not ask them for information you can determine yourself."
                );

                // Continuity is per conversation: successive tasks from the same
                // peer extend one history, unless the sender asked for a
                // separate thread or explicit session intent.
                let key = if let Some(thread) = &payload.thread {
                    SessionKey::for_thread(&envelope.source, thread)
                } else if let Some(acp::SessionIntent::Continue(sid)) =
                    &payload.session_intent
                {
                    SessionKey::for_thread(&envelope.source, sid)
                } else if let Some(acp::SessionIntent::New) = &payload.session_intent {
                    SessionKey::for_thread(&envelope.source, format!("sess_{}", Uuid::new_v4()))
                } else if let Some(serde_json::Value::String(thread)) =
                    payload.context.as_ref().and_then(|c| c.get("thread"))
                {
                    SessionKey::for_thread(&envelope.source, thread)
                } else {
                    SessionKey::for_peer(&envelope.source)
                };

                let session_id_str = key.as_session_id();
                delegation_ctx
                    .scope(self.run_turn(&key, envelope.id, prompt))
                    .await
                    .map(|output| {
                        let mut rep = envelope.reply(
                            self.manifest.address.clone(),
                            MessageIntent::resolve_with_session(output, session_id_str),
                        );
                        rep.association_id = envelope.association_id.clone();
                        rep.instance_id = Some(self.instance_id);
                        rep
                    })
                    .or_else(|err| {
                        let mut rep = envelope.reply(
                            self.manifest.address.clone(),
                            MessageIntent::reject(err.to_string()),
                        );
                        rep.association_id = envelope.association_id.clone();
                        rep.instance_id = Some(self.instance_id);
                        Ok(rep)
                    })
            }

            MessageIntent::Query(payload) => {
                // A query is a conversation with its own continuity, keyed the
                // same way as a delegation from that peer.
                let key = SessionKey::for_peer(&envelope.source);
                match self
                    .run_turn(&key, envelope.id, payload.prompt.clone())
                    .await
                {
                    Ok(reply) => {
                        Ok(envelope
                            .reply(self.manifest.address.clone(), MessageIntent::inform(reply)))
                    }
                    Err(err) => Ok(envelope.reply(
                        self.manifest.address.clone(),
                        MessageIntent::reject(err.to_string()),
                    )),
                }
            }

            // Steering modifies work already accepted, so it is never queued as
            // a turn and never answered with a reply envelope: the turn it
            // steers will settle normally.
            MessageIntent::Steer(payload) => {
                let correlation = envelope.correlation_id.ok_or_else(|| {
                    AgentError::Protocol(acp::ProtocolError::RoutingError(
                        "steering envelope must carry a correlation_id identifying the turn".into(),
                    ))
                })?;

                let applied = self
                    .steer(
                        correlation,
                        &envelope.source,
                        payload.instruction.clone(),
                        payload.action,
                    )
                    .await;

                if applied {
                    // Acknowledge so the sender knows the guidance landed; the
                    // turn itself replies separately.
                    Ok(envelope.reply(
                        self.manifest.address.clone(),
                        MessageIntent::signal(acp::SignalKind::Ack),
                    ))
                } else {
                    Ok(envelope.reply(
                        self.manifest.address.clone(),
                        MessageIntent::reject(
                            "the request is no longer running, so the guidance was not applied"
                                .to_string(),
                        ),
                    ))
                }
            }

            MessageIntent::Signal(payload) if payload.kind == acp::SignalKind::Ping => {
                Ok(envelope.reply(
                    self.manifest.address.clone(),
                    MessageIntent::signal(acp::SignalKind::Pong),
                ))
            }

            other => Err(AgentError::Protocol(
                acp::ProtocolError::RoutingError(format!(
                    "agent cannot act on intent `{}`",
                    other.summary()
                )),
            )),
        }
    }

    /// Serves this agent's inbox until it closes.
    ///
    /// Delegations and queries are answered with correlated replies, each on its
    /// own task so a slow peer task cannot stall the inbox. Cancellable intents
    /// and anything else are logged and skipped.
    ///
    /// Takes the inbox by value from the builder rather than claiming it from
    /// `self`, so ownership is checked statically instead of at runtime.
    pub async fn serve(self, mut mailbox: Mailbox) -> Result<()> {
        let address = self.manifest.address.clone();
        tracing::info!(%address, "agent serve loop started");

        while let Some(envelope) = mailbox.recv().await {
            // A channel notification is a hint, not a payload: the log is the
            // source of truth, so the decision to act on one is made here.
            //
            // `All` means "wake me on every message", so such a subscriber does
            // get a turn — that is what it asked for, and its turn reads the whole
            // backlog rather than just this message. But an automatic reply is
            // reserved for *mentions*: with two `All` subscribers, auto-replying
            // to each post would have them answer each other without bound. A
            // subscriber that was merely keeping up decides for itself whether to
            // contribute, and normally does so on its next turn.
            if let MessageIntent::ChannelNotify(payload) = &envelope.intent {
                let wake = ChannelWake::from(payload.reason);
                let channel_id = payload.channel.clone();

                // No attempt is made to collapse a burst into one turn. It is
                // unnecessary: `Channel::drain_for` advances the cursor, so a
                // concurrent turn that has already consumed the backlog finds
                // nothing unread and spends no inference. Suppressing turns
                // instead would trade that cheap no-op for a lost wakeup — a
                // mention arriving while a turn runs could be dropped and never
                // trigger one.
                let agent = self.clone();
                tokio::spawn(async move {
                    let correlation = Uuid::new_v4();
                    let outcome = agent.run_channel_turn(&channel_id, correlation, wake).await;

                    match outcome {
                        // Reply *on the channel*, so the discussion stays in one
                        // place rather than fragmenting into whispers. A direct
                        // `Inform` to the sender would also be dropped: it carries
                        // no correlation id, so it lands in the peer's
                        // unsolicited inbox, and a serve loop acts only on
                        // Delegate/Query/Steer.
                        Ok(Some(answer)) if wake.expects_attention() => {
                            if let Some(collaboration) = &agent.collaboration
                                && let Some(fabric) = collaboration.fabric()
                                && let Err(err) = fabric
                                    .publish(
                                        collaboration.address(),
                                        &channel_id,
                                        answer,
                                        Vec::new(),
                                    )
                                    .await
                            {
                                tracing::warn!(%err, "failed to answer channel mention");
                            }
                        }
                        // Subscribed-but-not-mentioned: the turn already recorded
                        // the discussion in the session's memory, which is the
                        // whole point of being woken. Its output is not posted,
                        // because nobody asked for it.
                        Ok(Some(_)) | Ok(None) => {}
                        Err(err) => {
                            tracing::warn!(%err, "channel turn failed");
                        }
                    }
                });
                continue;
            }

            if !matches!(
                envelope.intent,
                MessageIntent::Handshake(_)
                    | MessageIntent::Delegate(_)
                    | MessageIntent::Query(_)
                    | MessageIntent::Steer(_)
                    | MessageIntent::Signal(_)
            ) {
                tracing::debug!(
                    from = %envelope.source,
                    intent = %envelope.intent.summary(),
                    "ignoring non-actionable envelope"
                );
                continue;
            }

            let agent = self.clone();
            let source = envelope.source.clone();
            // One handle per task; the handle cannot consume the inbox, so
            // concurrent replies cannot interfere with each other.
            let handle = mailbox.handle();

            tokio::spawn(async move {
                match agent.handle_envelope(&envelope).await {
                    Ok(reply) => {
                        if let Err(err) = handle.send(reply).await {
                            tracing::warn!(%err, "failed to send delegation reply");
                        }
                    }
                    Err(err) => {
                        tracing::warn!(from = %source, %err, "failed to handle envelope");
                    }
                }
            });
        }

        tracing::info!(%address, "agent serve loop ended");
        Ok(())
    }

    /// Convenience constructor pairing an agent with its inbox.
    ///
    /// Preferred over [`Agent::serve`] when the caller does not need to
    /// interleave other work on the mailbox: it makes the ownership pairing
    /// impossible to get wrong.
    pub fn into_serving(self) -> Result<(Agent, Mailbox)> {
        let inbox = self
            .inbox
            .as_ref()
            .and_then(|slot| slot.lock().ok().and_then(|mut guard| guard.take()))
            .ok_or_else(|| {
                AgentError::Session(
                    "agent has no inbox: connect it with `connect_to` to receive tasks".into(),
                )
            })?;
        Ok((self, inbox))
    }
}

/// Fluent builder for [`Agent`].
pub struct AgentBuilder {
    address: String,
    name: Option<String>,
    description: Option<String>,
    skills: crate::skill::SkillRegistry,
    provider: Option<Arc<dyn Provider>>,
    tools: ToolRegistry,
    budget: TokenBudget,
    compactor: Compactor,
    fabric: Option<acp::Fabric>,
    delegation_timeout: Duration,
    store: Option<Arc<dyn SessionStore>>,
    memory: Option<Arc<dyn crate::memory::Memory>>,
    approval_handler: Option<Arc<dyn crate::tools::ApprovalHandler>>,
    compaction_policy: crate::token::CompactionPolicy,
    observation_store: Option<Arc<dyn crate::token::ObservationStore>>,
    policy: nuo_tool::ToolPolicy,
    active_scopes: Option<Vec<nuo_tool::ToolScope>>,
    instance_id: Option<Uuid>,
    p2p_only: bool,
    signature_verifier: Option<Arc<dyn acp::signature::EnvelopeVerifier>>,
    signature_enforcement: acp::signature::SignatureEnforcement,
}

impl AgentBuilder {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            name: None,
            description: None,
            skills: crate::skill::SkillRegistry::new(),
            provider: None,
            tools: ToolRegistry::new(),
            budget: TokenBudget::default(),
            compactor: Compactor::default(),
            fabric: None,
            delegation_timeout: DEFAULT_DELEGATION_TIMEOUT,
            store: None,
            memory: None,
            approval_handler: None,
            compaction_policy: crate::token::CompactionPolicy::default(),
            observation_store: None,
            policy: nuo_tool::ToolPolicy::default(),
            active_scopes: None,
            instance_id: None,
            p2p_only: false,
            signature_verifier: None,
            signature_enforcement: acp::signature::SignatureEnforcement::Permissive,
        }
    }

    /// Sets an explicit physical node instance identifier. Defaults to a fresh v4 UUID.
    pub fn instance_id(mut self, id: Uuid) -> Self {
        self.instance_id = Some(id);
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Describes what this agent does. The text is shown to peers, so it is the
    /// primary signal other agents use to decide whether to delegate here.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn skill(mut self, skill: impl Into<crate::skill::Skill>) -> Self {
        self.skills.register(skill.into());
        self
    }

    pub fn skills(
        mut self,
        skills: impl IntoIterator<Item = impl Into<crate::skill::Skill>>,
    ) -> Self {
        for s in skills {
            self.skills.register(s.into());
        }
        self
    }

    pub fn provider(mut self, provider: impl Provider + 'static) -> Self {
        self.provider = Some(Arc::new(provider));
        self
    }

    pub fn provider_arc(mut self, provider: Arc<dyn Provider>) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn tool(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.register(tool);
        self
    }

    pub fn tool_arc(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.register_arc(tool);
        self
    }

    /// Discovers and registers all tools from an existing MCP client.
    #[cfg(feature = "mcp")]
    pub async fn with_mcp_client(
        mut self,
        client: Arc<nuo_mcp::McpClient>,
    ) -> crate::error::Result<Self> {
        let tools = client
            .into_tools()
            .await
            .map_err(|err| AgentError::Tool("mcp".into(), err.to_string()))?;
        for tool in tools {
            self.tools.register_arc(tool);
        }
        Ok(self)
    }

    /// Spawns a local MCP server subprocess over stdio, performs the handshake, and registers all discovered tools.
    #[cfg(feature = "mcp")]
    pub async fn with_mcp_stdio(self, program: &str, args: &[&str]) -> crate::error::Result<Self> {
        let client = nuo_mcp::McpClient::connect_stdio(program, args)
            .await
            .map_err(|err| AgentError::Tool("mcp".into(), err.to_string()))?;
        self.with_mcp_client(client).await
    }

    pub fn budget(mut self, budget: TokenBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn compactor(mut self, compactor: Compactor) -> Self {
        self.compactor = compactor;
        self
    }

    /// Connects this agent to a unified communication fabric.
    pub fn connect_to(mut self, fabric: &Fabric) -> Self {
        self.fabric = Some(fabric.clone());
        self
    }

    /// Configures 1:1 P2P direct delegation mode, cleanly isolating the model prompt from channel tools.
    pub fn with_p2p_delegation(mut self) -> Self {
        self.p2p_only = true;
        self
    }

    /// Enables full channel collaboration tools for group discussions.
    pub fn with_channel_collaboration(mut self) -> Self {
        self.p2p_only = false;
        self
    }

    /// Enables session persistence, giving conversations continuity across turns.
    pub fn with_store(mut self, store: impl SessionStore + 'static) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    /// Enables session persistence with a shared store.
    pub fn with_store_arc(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Enables long-term memory for cross-session knowledge retention.
    pub fn with_memory(mut self, memory: impl crate::memory::Memory + 'static) -> Self {
        self.memory = Some(Arc::new(memory));
        self
    }

    /// Enables long-term memory with a shared instance.
    pub fn with_memory_arc(mut self, memory: Arc<dyn crate::memory::Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Enables human-in-the-loop tool execution approval.
    pub fn with_approval_handler(
        mut self,
        handler: impl crate::tools::ApprovalHandler + 'static,
    ) -> Self {
        self.approval_handler = Some(Arc::new(handler));
        self
    }

    /// Enables human-in-the-loop tool execution approval with a shared handler.
    pub fn with_approval_handler_arc(
        mut self,
        handler: Arc<dyn crate::tools::ApprovalHandler>,
    ) -> Self {
        self.approval_handler = Some(handler);
        self
    }

    /// Bounds a single delegation round trip.
    pub fn delegation_timeout(mut self, timeout: Duration) -> Self {
        self.delegation_timeout = timeout;
        self
    }

    /// Sets the declarative two-tier context pressure policy (Prune & Compact).
    pub fn compaction_policy(mut self, policy: crate::token::CompactionPolicy) -> Self {
        self.compaction_policy = policy;
        self
    }

    /// Disables all automatic offloading and compaction.
    pub fn without_compaction(mut self) -> Self {
        self.compaction_policy = crate::token::CompactionPolicy::none();
        self
    }

    /// Configures an observation backing store for claim-check invoices.
    /// Automatically registers the `inspect` tool.
    pub fn with_observation_store(
        mut self,
        store: impl crate::token::ObservationStore + 'static,
    ) -> Self {
        self.observation_store = Some(Arc::new(store));
        self
    }

    /// Configures an observation backing store with a shared Arc reference.
    /// Automatically registers the `inspect` tool.
    pub fn with_observation_store_arc(
        mut self,
        store: Arc<dyn crate::token::ObservationStore>,
    ) -> Self {
        self.observation_store = Some(store);
        self
    }

    /// Sets the anti-abuse execution policy, quotas, and circuit breakers.
    pub fn tool_policy(mut self, policy: nuo_tool::ToolPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Sets active operational scopes restricting which tools are model-visible.
    pub fn with_active_scopes(mut self, scopes: Vec<nuo_tool::ToolScope>) -> Self {
        self.active_scopes = Some(scopes);
        self
    }

    /// Enforces zero-trust cryptographic signature verification on incoming envelopes.
    pub fn with_zero_trust(
        mut self,
        verifier: Arc<dyn acp::signature::EnvelopeVerifier>,
        enforcement: acp::signature::SignatureEnforcement,
    ) -> Self {
        self.signature_verifier = Some(verifier);
        self.signature_enforcement = enforcement;
        self
    }

    pub async fn build(mut self) -> Result<Agent> {
        let address = AgentAddress::parse(&self.address).map_err(|err| {
            AgentError::Session(format!("invalid agent address `{}`: {err}", self.address))
        })?;

        let name = self.name.unwrap_or_else(|| address.path().to_string());
        let description = self
            .description
            .unwrap_or_else(|| "General-purpose autonomous agent".to_string());

        let manifest_skills: Vec<String> =
            self.skills.list().iter().map(|s| s.id.clone()).collect();
        let manifest =
            AgentManifest::new(address.clone(), name, description).with_skills(manifest_skills);

        let skill_stack = Arc::new(std::sync::Mutex::new(crate::skill::SkillStack::new()));
        if !self.skills.is_empty() {
            crate::skill::install_skill_tools(
                &mut self.tools,
                skill_stack.clone(),
                self.skills.clone(),
            );
        }

        let provider = self
            .provider
            .ok_or_else(|| AgentError::Provider("no provider configured for agent".into()))?;

        // Collaboration is installed when connected to a unified fabric.
        let (collaboration, inbox) = if let Some(fabric) = &self.fabric {
            let mailbox = fabric.join(manifest.clone(), 64).await;
            let handle = mailbox.handle();
            let collaboration = if self.p2p_only {
                let peers = fabric.peers_of(&address).await;
                crate::collaboration::install_direct_delegation_tools(
                    &mut self.tools,
                    address,
                    handle,
                    peers,
                    self.delegation_timeout,
                )
            } else {
                install_collaboration_tools(
                    &mut self.tools,
                    address,
                    fabric.clone(),
                    handle,
                    self.delegation_timeout,
                )
            };
            (Some(collaboration), Some(mailbox))
        } else {
            (None, None)
        };

        let mut compactor = self.compactor;
        compactor.policy = self.compaction_policy;

        if let Some(obs_store) = self.observation_store {
            self.tools
                .register(crate::tools::InspectTool::new(obs_store.clone()));
            compactor = compactor.with_observation_store(obs_store);
        }

        Ok(Agent {
            manifest,
            provider,
            tools: self.tools,
            budget: self.budget,
            compactor,
            collaboration,
            inbox: inbox.map(|mailbox| Arc::new(StdMutex::new(Some(mailbox)))),
            steering: SteeringHandle::new(),
            in_flight: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            session_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            store: self.store,
            memory: self.memory,
            approval_handler: self.approval_handler,
            policy: self.policy,
            active_scopes: self.active_scopes,
            instance_id: self.instance_id.unwrap_or_else(Uuid::new_v4),
            peer_pairings: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            signature_verifier: self.signature_verifier,
            signature_enforcement: self.signature_enforcement,
            skills: self.skills,
            skill_stack,
        })
    }
}
