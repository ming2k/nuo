use crate::address::AgentAddress;
use crate::error::{ProtocolError, Result};
use crate::intent::{DelegationOutcome, MessageIntent};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use uuid::Uuid;

/// Default maximum number of delegation hops before a task is refused.
pub const DEFAULT_MAX_HOPS: u32 = 8;

/// Execution and resource constraints passed along with a delegation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationBudget {
    /// Maximum cognitive rounds allowed for the delegate agent
    pub max_rounds: Option<u32>,
    /// Maximum execution timeout in milliseconds
    pub timeout_ms: Option<u64>,
    /// Maximum tokens budget allowed for this task
    pub max_tokens: Option<u64>,
    /// Remaining delegation hop allowance for the receiving agent.
    ///
    /// Decremented on each hand-off; reaching zero forbids further
    /// re-delegation, which structurally prevents infinite agent ping-pong.
    pub remaining_hops: u32,
}

impl Default for DelegationBudget {
    fn default() -> Self {
        Self {
            max_rounds: None,
            timeout_ms: None,
            max_tokens: None,
            remaining_hops: DEFAULT_MAX_HOPS,
        }
    }
}

impl DelegationBudget {
    /// Decrements the hop allowance, erroring when exhausted.
    pub fn descend(&self) -> Result<Self> {
        if self.remaining_hops == 0 {
            return Err(ProtocolError::HopLimitExceeded(DEFAULT_MAX_HOPS));
        }
        Ok(Self {
            remaining_hops: self.remaining_hops - 1,
            ..*self
        })
    }
}

/// The fundamental message container exchanged between agents.
///
/// An envelope always has exactly one addressee. There is deliberately no
/// "channel" or "broadcast" recipient variant: channel traffic is not an envelope
/// addressed to a channel but a *publication* recorded in a log, which the room
/// then points individual subscribers at. Modelling it as an envelope target
/// would misrepresent fan-out as per-message addressing — the router cannot
/// resolve channel subscribers, and it would have to lie about delivering one
/// copy to each of them while carrying a single `target`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEnvelope {
    /// Unique identifier for this envelope
    pub id: Uuid,

    /// Correlation identifier linking responses/progress to the initiating request
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,

    /// Optional session/thread identifier binding this envelope to a specific conversation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Originating agent address
    pub source: AgentAddress,

    /// The single agent this envelope is addressed to.
    pub target: AgentAddress,

    /// Message intent and payload
    pub intent: MessageIntent,

    /// Message creation timestamp in UTC
    pub created_at: DateTime<Utc>,

    /// Optional execution budget and constraints
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<DelegationBudget>,

    /// Extensible metadata headers
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,

    /// Originating process / node unique physical instance identifier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<Uuid>,

    /// Point-to-point peer association / pairing identifier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association_id: Option<String>,

    /// Optional parent envelope identifier for task tree lineage and causal tracking
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,

    /// Optional supervisor agent address holding authority over this task
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<AgentAddress>,

    /// Optional cryptographic signature for zero-trust message validation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<crate::signature::EnvelopeSignature>,
}

impl AgentEnvelope {
    /// Creates a new envelope with a freshly generated UUID and current timestamp.
    pub fn new(source: AgentAddress, target: AgentAddress, intent: MessageIntent) -> Self {
        Self {
            id: Uuid::new_v4(),
            correlation_id: None,
            session_id: None,
            source,
            target,
            intent,
            created_at: Utc::now(),
            budget: None,
            metadata: HashMap::new(),
            instance_id: None,
            association_id: None,
            parent_id: None,
            supervisor: None,
            signature: None,
        }
    }

    pub fn with_parent_id(mut self, parent_id: Uuid) -> Self {
        self.parent_id = Some(parent_id);
        self
    }

    pub fn with_supervisor(mut self, supervisor: AgentAddress) -> Self {
        self.supervisor = Some(supervisor);
        self
    }

    pub fn with_instance_id(mut self, instance_id: Uuid) -> Self {
        self.instance_id = Some(instance_id);
        self
    }

    pub fn with_association_id(mut self, association_id: impl Into<String>) -> Self {
        self.association_id = Some(association_id.into());
        self
    }

    pub fn with_signature(mut self, signature: crate::signature::EnvelopeSignature) -> Self {
        self.signature = Some(signature);
        self
    }

    /// Binds this envelope to a specific conversation session identifier.
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Sets correlation ID to link with a previous envelope.
    pub fn with_correlation(mut self, correlation_id: Uuid) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    /// Sets delegation budget constraint.
    pub fn with_budget(mut self, budget: DelegationBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Attaches a metadata entry.
    pub fn with_meta(mut self, key: impl Into<String>, val: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), val);
        self
    }

    /// Creates a response envelope addressing the original sender with matching correlation ID.
    pub fn reply(&self, source: AgentAddress, intent: MessageIntent) -> Self {
        Self {
            id: Uuid::new_v4(),
            correlation_id: Some(self.id),
            session_id: self.session_id.clone(),
            source,
            target: self.source.clone(),
            intent,
            created_at: Utc::now(),
            // Hop allowance is carried through so descendant workflows
            // stay within the originating request's depth budget.
            budget: self.budget,
            metadata: HashMap::new(),
            instance_id: None,
            association_id: self.association_id.clone(),
            parent_id: Some(self.id),
            supervisor: self.supervisor.clone(),
            signature: None,
        }
    }

    /// Whether this envelope settles a correlated in-flight request.
    pub fn settles_request(&self) -> bool {
        self.correlation_id.is_some() && self.intent.settles_correlated_request()
    }

    /// Extracts the terminal outcome if this envelope settles a delegation.
    pub fn outcome(&self) -> Option<DelegationOutcome> {
        self.intent.to_outcome()
    }
}

/// Correlates a `Delegate` request with its terminal response.
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// Envelope id of the originating request.
    pub request_id: Uuid,
    /// Address of the peer the request was sent to.
    pub target: AgentAddress,
    /// Natural language task sent, retained for diagnostics.
    pub task: String,
    /// Time the request was issued.
    pub issued_at: DateTime<Utc>,
    /// Hop allowance granted to this request.
    pub remaining_hops: u32,
}

impl fmt::Display for PendingRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "request {} -> {} ({})",
            self.request_id, self.target, self.task
        )
    }
}
