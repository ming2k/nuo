//! Long-term memory subsystem: cross-session knowledge retention,
//! scope-isolated fact recall, hybrid semantic/lexical RRF search, and background observation.

use crate::error::Result;
use crate::session::SessionKey;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use acp::{AgentAddress, ChannelId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Visibility and access boundary for a stored memory fact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryScope {
    /// Accessible across all conversation surfaces.
    Public,
    /// Accessible only within a specific channel discussion.
    Channel(ChannelId),
    /// Accessible only within direct 1:1 conversations with a specific peer.
    Peer(AgentAddress),
}

impl MemoryScope {
    /// Determines whether this scope is accessible from the given session key.
    pub fn is_accessible_from(&self, key: &SessionKey) -> bool {
        match self {
            Self::Public => true,
            Self::Channel(expected) => key.channel() == Some(expected),
            Self::Peer(expected) => key.peer() == Some(expected),
        }
    }

    /// Derives the appropriate memory scope from a session key.
    pub fn from_session_key(key: &SessionKey) -> Self {
        if let Some(ch) = key.channel() {
            Self::Channel(ch.clone())
        } else if let Some(peer) = key.peer() {
            Self::Peer(peer.clone())
        } else {
            Self::Public
        }
    }
}

/// A discrete unit of long-term memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    /// Unique identifier of this fact.
    pub id: Uuid,
    /// Natural language representation of the memory.
    pub content: String,
    /// Access control boundary.
    pub scope: MemoryScope,
    /// Relevance score (0.0 to 1.0) when returned from a query.
    pub score: f32,
    /// Arbitrary structured metadata (e.g. entities, tags, category).
    pub metadata: serde_json::Value,
    /// Optional embedding vector for dense semantic retrieval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// When this fact was recorded.
    pub timestamp: DateTime<Utc>,
}

impl MemoryFact {
    pub fn new(content: impl Into<String>, scope: MemoryScope) -> Self {
        Self {
            id: Uuid::new_v4(),
            content: content.into(),
            scope,
            score: 1.0,
            metadata: serde_json::Value::Null,
            embedding: None,
            timestamp: Utc::now(),
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }
}

/// Query parameters for memory retrieval.
#[derive(Debug, Clone)]
pub struct MemoryQuery {
    /// Text to search for (keyword or semantic topic).
    pub text: String,
    /// Conversation key initiating the query, used to enforce scope isolation.
    pub session_key: SessionKey,
    /// Maximum number of facts to return.
    pub limit: usize,
    /// Minimum relevance threshold (0.0 to 1.0).
    pub min_score: f32,
}

impl MemoryQuery {
    pub fn new(text: impl Into<String>, session_key: SessionKey) -> Self {
        Self {
            text: text.into(),
            session_key,
            limit: 5,
            min_score: 0.05,
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_min_score(mut self, min_score: f32) -> Self {
        self.min_score = min_score;
        self
    }
}

/// Information describing a completed conversation turn for memory observation.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub session_key: SessionKey,
    pub correlation_id: Uuid,
    pub user_prompt: String,
    pub agent_response: String,
    pub tool_calls: Vec<(String, String)>,
}

/// Interface for generating semantic text embeddings.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generates embedding vector for `text`.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// Computes cosine similarity between two float vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        0.0
    } else {
        (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
    }
}

/// Reciprocal Rank Fusion (RRF) combining sparse lexical and dense semantic rankings.
pub fn reciprocal_rank_fusion(
    lexical_rankings: &[(Uuid, usize)],
    semantic_rankings: &[(Uuid, usize)],
    k: f32,
) -> HashMap<Uuid, f32> {
    let mut scores: HashMap<Uuid, f32> = HashMap::new();
    for (id, rank) in lexical_rankings {
        let rrf = 1.0 / (k + (*rank as f32) + 1.0);
        *scores.entry(*id).or_default() += rrf;
    }
    for (id, rank) in semantic_rankings {
        let rrf = 1.0 / (k + (*rank as f32) + 1.0);
        *scores.entry(*id).or_default() += rrf;
    }
    scores
}

/// Long-term memory provider interface.
#[async_trait]
pub trait Memory: Send + Sync {
    /// Retrieves relevant facts that match `query` and are accessible to its session key.
    async fn recall(&self, query: &MemoryQuery) -> Result<Vec<MemoryFact>>;

    /// Records or updates an explicit memory fact.
    async fn record(&self, fact: MemoryFact) -> Result<()>;

    /// Observes a completed conversation turn to extract and consolidate facts.
    async fn observe(&self, outcome: &TurnOutcome) -> Result<()>;
}

/// In-memory implementation of [`Memory`] with hybrid lexical + semantic RRF scoring and scope isolation.
#[derive(Clone, Default)]
pub struct InMemoryMemory {
    facts: Arc<RwLock<Vec<MemoryFact>>>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
}

impl InMemoryMemory {
    pub fn new() -> Self {
        Self {
            facts: Arc::new(RwLock::new(Vec::new())),
            embedding_provider: None,
        }
    }

    /// Attaches an embedding provider for hybrid semantic retrieval.
    pub fn with_embedding_provider(mut self, provider: impl EmbeddingProvider + 'static) -> Self {
        self.embedding_provider = Some(Arc::new(provider));
        self
    }

    /// Attaches a shared reference to an embedding provider.
    pub fn with_embedding_provider_arc(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Helper to directly seed a fact.
    pub async fn add_fact(&self, content: impl Into<String>, scope: MemoryScope) -> MemoryFact {
        let content_str = content.into();
        let mut fact = MemoryFact::new(content_str.clone(), scope);
        if let Some(ep) = &self.embedding_provider
            && let Ok(vec) = ep.embed(&content_str).await
        {
            fact.embedding = Some(vec);
        }
        self.facts.write().await.push(fact.clone());
        fact
    }

    /// Total count of facts currently stored.
    pub async fn count(&self) -> usize {
        self.facts.read().await.len()
    }
}

#[async_trait]
impl Memory for InMemoryMemory {
    async fn recall(&self, query: &MemoryQuery) -> Result<Vec<MemoryFact>> {
        let guard = self.facts.read().await;

        let query_tokens: HashSet<String> = query
            .text
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // 1. Filter candidates accessible to this session scope
        let accessible: Vec<&MemoryFact> = guard
            .iter()
            .filter(|f| f.scope.is_accessible_from(&query.session_key))
            .collect();

        if accessible.is_empty() {
            return Ok(Vec::new());
        }

        // 2. Lexical scoring
        let mut lexical_scored: Vec<(Uuid, f32)> = accessible
            .iter()
            .map(|f| {
                let fact_lower = f.content.to_lowercase();
                let score = if query_tokens.is_empty() {
                    0.5
                } else {
                    let matches = query_tokens
                        .iter()
                        .filter(|token| fact_lower.contains(token.as_str()))
                        .count();
                    matches as f32 / query_tokens.len() as f32
                };
                (f.id, score)
            })
            .collect();

        lexical_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let lexical_rankings: Vec<(Uuid, usize)> = lexical_scored
            .iter()
            .enumerate()
            .map(|(rank, (id, _))| (*id, rank))
            .collect();

        // 3. Dense semantic scoring (if embedding provider configured)
        let query_embedding = if let Some(ep) = &self.embedding_provider {
            ep.embed(&query.text).await.ok()
        } else {
            None
        };

        let mut semantic_rankings = Vec::new();
        if let Some(q_vec) = &query_embedding {
            let mut semantic_scored: Vec<(Uuid, f32)> = accessible
                .iter()
                .filter_map(|f| {
                    f.embedding
                        .as_ref()
                        .map(|f_vec| (f.id, cosine_similarity(q_vec, f_vec)))
                })
                .collect();
            semantic_scored
                .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            semantic_rankings = semantic_scored
                .iter()
                .enumerate()
                .map(|(rank, (id, _))| (*id, rank))
                .collect();
        }

        // 4. Combine with RRF or fallback to lexical
        let combined_scores = if !semantic_rankings.is_empty() {
            reciprocal_rank_fusion(&lexical_rankings, &semantic_rankings, 60.0)
        } else {
            lexical_scored.into_iter().collect::<HashMap<Uuid, f32>>()
        };

        // 5. Build final scored list
        let mut final_facts: Vec<MemoryFact> = accessible
            .into_iter()
            .filter_map(|f| {
                let score = combined_scores.get(&f.id).copied().unwrap_or(0.0);
                if score >= query.min_score {
                    let mut scored_fact = f.clone();
                    scored_fact.score = score;
                    Some(scored_fact)
                } else {
                    None
                }
            })
            .collect();

        final_facts.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.timestamp.cmp(&a.timestamp))
        });

        final_facts.truncate(query.limit);
        Ok(final_facts)
    }

    async fn record(&self, mut fact: MemoryFact) -> Result<()> {
        if fact.embedding.is_none()
            && let Some(ep) = &self.embedding_provider
            && let Ok(vec) = ep.embed(&fact.content).await
        {
            fact.embedding = Some(vec);
        }
        let mut guard = self.facts.write().await;
        guard.push(fact);
        Ok(())
    }

    async fn observe(&self, outcome: &TurnOutcome) -> Result<()> {
        // 1. Automatically extract key facts from user prompts that declare state or decisions
        let prompt_lower = outcome.user_prompt.to_lowercase();
        let is_decision_or_fact = prompt_lower.contains("remember")
            || prompt_lower.contains("decided")
            || prompt_lower.contains("note that")
            || prompt_lower.contains("decision:")
            || prompt_lower.contains("preference:");

        if is_decision_or_fact {
            let scope = MemoryScope::from_session_key(&outcome.session_key);
            let fact = MemoryFact::new(outcome.user_prompt.clone(), scope).with_metadata(
                serde_json::json!({
                    "auto_extracted": true,
                    "correlation_id": outcome.correlation_id.to_string(),
                }),
            );
            self.record(fact).await?;
        }

        // 2. Consolidate autobiographical self-action memory so the agent retains
        // cross-session awareness of what it performed across different surfaces.
        let surface_name = outcome.session_key.as_session_id();
        let prompt_summary = if outcome.user_prompt.len() > 100 {
            format!("{}...", &outcome.user_prompt[..100])
        } else {
            outcome.user_prompt.clone()
        };
        let response_summary = if outcome.agent_response.len() > 150 {
            format!("{}...", &outcome.agent_response[..150])
        } else {
            outcome.agent_response.clone()
        };

        let action_fact = MemoryFact::new(
            format!("[Self-Action in {surface_name}]: Task: \"{prompt_summary}\" -> Result: \"{response_summary}\""),
            MemoryScope::Public,
        )
        .with_metadata(serde_json::json!({
            "autobiographical": true,
            "session_key": outcome.session_key.as_session_id(),
            "correlation_id": outcome.correlation_id.to_string(),
        }));

        self.record(action_fact).await?;

        Ok(())
    }
}
