#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use nuo_agent::memory::{
    EmbeddingProvider, InMemoryMemory, Memory, MemoryFact, MemoryQuery, MemoryScope,
};
use nuo_agent::session::SessionKey;
use acp::AgentAddress;

struct MockEmbeddingProvider;

#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, text: &str) -> nuo_agent::Result<Vec<f32>> {
        // Deterministic mock embedding based on semantic topics
        let lower = text.to_lowercase();
        let mut vec = vec![0.0; 4];
        if lower.contains("database") || lower.contains("postgres") || lower.contains("sql") {
            vec[0] = 1.0;
        }
        if lower.contains("neural") || lower.contains("ai") || lower.contains("model") {
            vec[1] = 1.0;
        }
        if lower.contains("security") || lower.contains("auth") || lower.contains("token") {
            vec[2] = 1.0;
        }
        if lower.contains("frontend") || lower.contains("ui") || lower.contains("css") {
            vec[3] = 1.0;
        }
        Ok(vec)
    }
}

#[tokio::test]
async fn test_hybrid_rrf_memory_retrieval() {
    let memory = InMemoryMemory::new().with_embedding_provider(MockEmbeddingProvider);

    // Record facts with semantic topics
    memory
        .record(MemoryFact::new(
            "Production PostgreSQL database runs on port 5432 with replication enabled",
            MemoryScope::Public,
        ))
        .await
        .unwrap();

    memory
        .record(MemoryFact::new(
            "JWT token authentication middleware validates user roles on API gateway",
            MemoryScope::Public,
        ))
        .await
        .unwrap();

    let session_key = SessionKey::for_peer(&AgentAddress::parse("agent://local/dev").unwrap());

    // Query using semantic term "SQL storage engine" - contains neither 'postgres' nor exact words,
    // but MockEmbeddingProvider triggers database vector component vec[0]
    let query = MemoryQuery::new("SQL storage engine", session_key)
        .with_limit(2)
        .with_min_score(0.01);

    let recalled = memory.recall(&query).await.unwrap();
    assert!(
        !recalled.is_empty(),
        "Hybrid recall should return semantic matches"
    );
    assert!(
        recalled[0].content.contains("PostgreSQL"),
        "Top recalled fact should be PostgreSQL database"
    );
}
