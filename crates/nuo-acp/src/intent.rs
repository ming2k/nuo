use crate::address::AgentAddress;
use crate::channel::id::ChannelId;
use crate::channel::message::NotifyReason;
use serde::{Deserialize, Serialize};

/// High-level communicative intent of an agent envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum MessageIntent {
    /// Peer-to-peer connection and identity handshake request
    Handshake(HandshakePayload),

    /// Handshake acknowledgment establishing confirmed pairing association
    HandshakeAck(HandshakeAckPayload),

    /// Task delegation: Assigns a task in natural language to a target agent
    Delegate(DelegatePayload),

    /// Immediate receipt confirmation when a task is accepted for execution
    TaskAck(TaskAckPayload),

    /// Interim execution progress report
    Progress(ProgressPayload),

    /// Task resolution: Successfully completed task with final outcome
    Resolve(ResolvePayload),

    /// Task rejection: Inability to execute or task failure
    Reject(RejectPayload),

    /// Conversational query
    Query(QueryPayload),

    /// Conversational informational reply
    Inform(InformPayload),

    /// Notification that a channel has new traffic.
    ///
    /// Sent to subscribers whose policy opts them in, with the reason they were
    /// selected. It carries no payload beyond the pointer, because the log — not
    /// the envelope — is the source of truth: a dropped notification costs
    /// latency, never content.
    ChannelNotify(ChannelNotifyPayload),

    /// Out-of-band guidance for work already in flight.
    Steer(SteerPayload),

    /// Replicated channel event for cross-host room synchronization.
    ChannelSync(ChannelSyncPayload),

    /// Protocol and lifecycle signals (e.g. Cancel, Ping, Ack)
    Signal(SignalPayload),
}

/// Explicit session negotiation intent when delegating tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionIntent {
    /// Requests opening a fresh, isolated conversation session.
    New,
    /// Requests continuing an ongoing session by thread / session ID.
    Continue(String),
}

/// Initial handshake request sent to establish mutual identity and capability pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakePayload {
    /// Logical agent address of the initiator.
    pub address: AgentAddress,
    /// Physical process / node instance unique identifier.
    pub instance_id: uuid::Uuid,
    /// Cryptographic public key fingerprint (e.g. Ed25519/HMAC key identifier).
    pub public_key: String,
    /// Existing association ID if attempting to reconnect/resume a previous pairing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association_id: Option<String>,
}

/// Handshake acknowledgment returning receiver identity and confirmed association ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandshakeAckPayload {
    /// Physical process / node instance unique identifier of the responder.
    pub instance_id: uuid::Uuid,
    /// Cryptographic public key fingerprint of the responder.
    pub public_key: String,
    /// Confirmed unique association/pairing ID established between the two agents.
    pub association_id: String,
    /// Full self-describing capability manifest of the responder.
    pub manifest: crate::manifest::AgentManifest,
}

/// Immediate receipt confirmation emitted when a delegated task is accepted into execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAckPayload {
    /// Unique task ID matching the envelope ID.
    pub task_id: uuid::Uuid,
    /// Bound session ID assigned to this task (either fresh or continued).
    pub session_id: String,
    /// Confirmed association ID for sticky routing.
    pub association_id: String,
}

/// Resource and performance metrics captured during delegation execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExecutionMetrics {
    /// Number of cognitive inference rounds spent.
    pub rounds: u32,
    /// Total tokens estimated or reported during execution.
    pub total_tokens: usize,
    /// Wall-clock execution duration in milliseconds.
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatePayload {
    /// Natural language task description / instructions
    pub task: String,
    /// Optional thread/session handle to continue a specific conversation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// Explicit session negotiation intent (New vs Continue)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_intent: Option<SessionIntent>,
    /// Optional contextual data or environment payload
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// Task priority (0 = lowest, 9 = critical)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressPayload {
    /// Current step description
    pub step: String,
    /// Completion percentage (0.0 to 1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentage: Option<f32>,
    /// Session identifier of the executing task
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Intermediate step output or partial finding
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intermediate_output: Option<String>,
    /// Additional detailed diagnostics or logs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvePayload {
    /// Final natural language response / summary of task result
    pub output: String,
    /// The session identifier bound to this conversation (for follow-up continuation)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Structured data or generated artifacts
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<serde_json::Value>,
    /// Execution and audit metrics
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<ExecutionMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectPayload {
    /// Natural language explanation of why task failed or was rejected
    pub reason: String,
    /// Standardized error classification code
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Whether the sender can retry this request
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPayload {
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InformPayload {
    pub reply: String,
}

/// Payload for steering in-flight work.
///
/// # Why this is not just another `Delegate`
///
/// Inference is atomic: once a prompt is dispatched, it cannot be amended. So
/// steering has exactly one honest meaning — *consume at the next round
/// boundary of the turn already running*. Two consequences follow, and they are
/// the reason the request/steer split is structural rather than cosmetic:
///
/// - **Steering affects the current turn.** Use it to correct course, add a
///   constraint, or cancel, where waiting for the current turn to finish would
///   make the guidance useless.
/// - **A new task is a new turn.** Queue it instead. Injecting unrelated work
///   mid-turn would contaminate the running task's reasoning.
///
/// Choosing wrongly is a real failure mode in both directions, and only the
/// sender knows which it means, so the sender picks the intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerPayload {
    /// The instruction to apply.
    pub instruction: String,
    /// Requested effect on the running turn.
    pub action: SteerAction,
}

/// What a steering instruction asks the receiver to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteerAction {
    /// Append the instruction as a note before the next inference round.
    ///
    /// The mildest action: the model sees the correction and continues. Use for
    /// clarification and course correction.
    Note,
    /// Discard the planned tool calls for this round and re-plan with the
    /// instruction applied.
    ///
    /// Use when the turn is heading the wrong way and continuing would waste
    /// the round.
    Redirect,
    /// Stop the turn and settle with whatever result exists so far.
    ///
    /// The receiver answers with `Resolve` (partial result) rather than
    /// abandoning the request, so the counterpart is never left waiting.
    Cancel,
}

impl SteerAction {
    /// Whether this action stops the turn.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Cancel)
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Note => "applied as a note before the next round",
            Self::Redirect => "re-plans the current round with the instruction",
            Self::Cancel => "stops the turn and settles with the partial result",
        }
    }
}

/// A steering instruction queued for a running turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerInstruction {
    /// Who sent it, for attribution in the model's context.
    pub from: AgentAddress,
    /// The instruction text.
    pub instruction: String,
    /// The requested action.
    pub action: SteerAction,
}

impl SteerInstruction {
    /// Renders the instruction for injection into a running session.
    pub fn to_context_line(&self) -> String {
        format!(
            "[steering from {} ({})] {}",
            self.from,
            self.action.describe(),
            self.instruction
        )
    }
}

/// Payload for a channel notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelNotifyPayload {
    /// Channel that received traffic.
    pub channel: ChannelId,
    /// Sequence of the newest message.
    pub seq: u64,
    /// Author of that message.
    pub from: AgentAddress,
    /// Why this agent was woken.
    ///
    /// Computed once by the room at publication time, under the same lock that
    /// selected the recipients. Re-deriving it at the receiver would be unsound:
    /// retention may already have evicted the message, and any scan window over
    /// the log is a bound that silently drops mentions beyond it.
    pub reason: NotifyReason,
    /// Short preview, for logging and UI only.
    ///
    /// Deliberately not authoritative: consumers read the channel log for the
    /// real content.
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSyncPayload {
    pub channel: ChannelId,
    pub message: crate::channel::ChannelMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalPayload {
    pub kind: SignalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Ping,
    Pong,
    Ack,
    Cancel,
    Heartbeat,
}

/// Normalized outcome of awaiting a delegation request.
///
/// Produced by correlating a `Delegate` request with the peer's terminal
/// `Resolve` / `Reject` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DelegationOutcome {
    /// The peer completed the task successfully.
    Resolved {
        output: String,
        session_id: Option<String>,
        artifacts: Option<serde_json::Value>,
        metrics: Option<ExecutionMetrics>,
    },
    /// The peer refused or failed the task.
    Rejected {
        reason: String,
        error_code: Option<String>,
        retryable: bool,
    },
}

impl DelegationOutcome {
    pub fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved { .. })
    }

    pub fn output(&self) -> Option<&str> {
        match self {
            Self::Resolved { output, .. } => Some(output.as_str()),
            _ => None,
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Resolved { session_id, .. } => session_id.as_deref(),
            _ => None,
        }
    }

    pub fn artifacts(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Resolved { artifacts, .. } => artifacts.as_ref(),
            _ => None,
        }
    }

    pub fn metrics(&self) -> Option<&ExecutionMetrics> {
        match self {
            Self::Resolved { metrics, .. } => metrics.as_ref(),
            _ => None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Rejected { retryable, .. } => *retryable,
            _ => false,
        }
    }

    /// Renders the outcome as natural language text for reasoning contexts.
    pub fn to_natural_language(&self) -> String {
        match self {
            Self::Resolved { output, .. } => output.clone(),
            Self::Rejected {
                reason, error_code, ..
            } => match error_code {
                Some(code) => {
                    format!("The peer agent could not complete the task ({code}): {reason}")
                }
                None => format!("The peer agent could not complete the task: {reason}"),
            },
        }
    }
}

impl MessageIntent {
    pub fn handshake(
        address: AgentAddress,
        instance_id: uuid::Uuid,
        public_key: impl Into<String>,
        association_id: Option<String>,
    ) -> Self {
        Self::Handshake(HandshakePayload {
            address,
            instance_id,
            public_key: public_key.into(),
            association_id,
        })
    }

    pub fn handshake_ack(
        instance_id: uuid::Uuid,
        public_key: impl Into<String>,
        association_id: impl Into<String>,
        manifest: crate::manifest::AgentManifest,
    ) -> Self {
        Self::HandshakeAck(HandshakeAckPayload {
            instance_id,
            public_key: public_key.into(),
            association_id: association_id.into(),
            manifest,
        })
    }

    pub fn task_ack(
        task_id: uuid::Uuid,
        session_id: impl Into<String>,
        association_id: impl Into<String>,
    ) -> Self {
        Self::TaskAck(TaskAckPayload {
            task_id,
            session_id: session_id.into(),
            association_id: association_id.into(),
        })
    }

    pub fn delegate(task: impl Into<String>) -> Self {
        Self::Delegate(DelegatePayload {
            task: task.into(),
            thread: None,
            session_intent: Some(SessionIntent::New),
            context: None,
            priority: None,
        })
    }

    pub fn delegate_with_context(task: impl Into<String>, context: serde_json::Value) -> Self {
        Self::Delegate(DelegatePayload {
            task: task.into(),
            thread: None,
            session_intent: Some(SessionIntent::New),
            context: Some(context),
            priority: None,
        })
    }

    pub fn delegate_with_thread(task: impl Into<String>, thread: impl Into<String>) -> Self {
        let t_str = thread.into();
        Self::Delegate(DelegatePayload {
            task: task.into(),
            thread: Some(t_str.clone()),
            session_intent: Some(SessionIntent::Continue(t_str)),
            context: None,
            priority: None,
        })
    }

    pub fn progress(step: impl Into<String>) -> Self {
        Self::Progress(ProgressPayload {
            step: step.into(),
            percentage: None,
            session_id: None,
            intermediate_output: None,
            details: None,
        })
    }

    pub fn resolve(output: impl Into<String>) -> Self {
        Self::Resolve(ResolvePayload {
            output: output.into(),
            session_id: None,
            artifacts: None,
            metrics: None,
        })
    }

    pub fn resolve_with_session(output: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self::Resolve(ResolvePayload {
            output: output.into(),
            session_id: Some(session_id.into()),
            artifacts: None,
            metrics: None,
        })
    }

    pub fn reject(reason: impl Into<String>) -> Self {
        Self::Reject(RejectPayload {
            reason: reason.into(),
            error_code: None,
            retryable: false,
        })
    }

    pub fn reject_with_details(
        reason: impl Into<String>,
        error_code: Option<String>,
        retryable: bool,
    ) -> Self {
        Self::Reject(RejectPayload {
            reason: reason.into(),
            error_code,
            retryable,
        })
    }

    pub fn query(prompt: impl Into<String>) -> Self {
        Self::Query(QueryPayload {
            prompt: prompt.into(),
        })
    }

    pub fn inform(reply: impl Into<String>) -> Self {
        Self::Inform(InformPayload {
            reply: reply.into(),
        })
    }

    pub fn channel_notify(
        channel: ChannelId,
        seq: u64,
        from: AgentAddress,
        reason: NotifyReason,
        preview: impl Into<String>,
    ) -> Self {
        Self::ChannelNotify(ChannelNotifyPayload {
            channel,
            seq,
            from,
            reason,
            preview: preview.into(),
        })
    }

    pub fn steer(instruction: impl Into<String>, action: SteerAction) -> Self {
        Self::Steer(SteerPayload {
            instruction: instruction.into(),
            action,
        })
    }

    pub fn channel_sync(channel: ChannelId, message: crate::channel::ChannelMessage) -> Self {
        Self::ChannelSync(ChannelSyncPayload { channel, message })
    }

    pub fn signal(kind: SignalKind) -> Self {
        Self::Signal(SignalPayload {
            kind,
            payload: None,
        })
    }

    /// Whether this intent represents a task completion (success or rejection).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Resolve(_) | Self::Reject(_))
    }

    /// Whether this intent settles an in-flight correlated request.
    ///
    /// `Resolve` / `Reject` settle delegations; `Inform` settles queries.
    /// Envelopes carrying these intents are routed to the pending-request
    /// registry rather than the unsolicited inbox.
    pub fn settles_correlated_request(&self) -> bool {
        matches!(
            self,
            Self::Resolve(_)
                | Self::Reject(_)
                | Self::Inform(_)
                | Self::HandshakeAck(_)
                | Self::TaskAck(_)
                | Self::Signal(SignalPayload {
                    kind: SignalKind::Pong | SignalKind::Ack,
                    ..
                })
        )
    }

    /// Short human-readable summary of this intent, used for diagnostics,
    /// in-flight request tracking, and tracing.
    pub fn summary(&self) -> String {
        match self {
            Self::Handshake(p) => format!("handshake inst:{}", p.instance_id),
            Self::HandshakeAck(p) => format!("handshake_ack pair:{}", p.association_id),
            Self::TaskAck(p) => format!("task_ack task:{} in sid:{}", p.task_id, p.session_id),
            Self::Delegate(p) => p.task.clone(),
            Self::Progress(p) => p.step.clone(),
            Self::Resolve(p) => p.output.clone(),
            Self::Reject(p) => p.reason.clone(),
            Self::Query(p) => p.prompt.clone(),
            Self::Inform(p) => p.reply.clone(),
            Self::ChannelNotify(p) => {
                format!("{} to #{} at seq {}", p.reason.describe(), p.channel, p.seq)
            }
            Self::Steer(p) => format!("{:?}: {}", p.action, p.instruction),
            Self::ChannelSync(p) => format!("sync #{}: seq {}", p.channel, p.message.seq),
            Self::Signal(p) => format!("{:?}", p.kind),
        }
    }

    /// Converts a terminal reply intent into a [`DelegationOutcome`].
    pub fn to_outcome(&self) -> Option<DelegationOutcome> {
        match self {
            Self::Resolve(payload) => Some(DelegationOutcome::Resolved {
                output: payload.output.clone(),
                session_id: payload.session_id.clone(),
                artifacts: payload.artifacts.clone(),
                metrics: payload.metrics,
            }),
            Self::Reject(payload) => Some(DelegationOutcome::Rejected {
                reason: payload.reason.clone(),
                error_code: payload.error_code.clone(),
                retryable: payload.retryable,
            }),
            _ => None,
        }
    }
}
