use super::state::Session;
use crate::error::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Abstraction for storing and recovering agent session snapshots.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn save(&self, session: &Session) -> Result<()>;
    async fn load(&self, id: &str) -> Result<Option<Session>>;
    async fn delete(&self, id: &str) -> Result<()>;
}

/// In-memory session persistence.
#[derive(Default, Clone)]
pub struct InMemorySessionStore {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn save(&self, session: &Session) -> Result<()> {
        let mut guard = self.sessions.write().await;
        guard.insert(session.id.clone(), session.clone());
        Ok(())
    }

    async fn load(&self, id: &str) -> Result<Option<Session>> {
        let guard = self.sessions.read().await;
        Ok(guard.get(id).cloned())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let mut guard = self.sessions.write().await;
        guard.remove(id);
        Ok(())
    }
}
