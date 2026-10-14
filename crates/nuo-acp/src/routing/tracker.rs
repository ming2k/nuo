//! Distributed agent presence tracking and discovery ledger (Tracker).

use crate::address::AgentAddress;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Record of an active agent registered with the presence tracker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPresence {
    pub address: AgentAddress,
    pub instance_id: Option<Uuid>,
    pub metadata: HashMap<String, serde_json::Value>,
    pub registered_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub ttl_secs: u64,
}

impl AgentPresence {
    pub fn is_alive(&self, now: DateTime<Utc>) -> bool {
        let elapsed = now.signed_duration_since(self.last_heartbeat);
        elapsed.num_seconds() <= self.ttl_secs as i64
    }
}

/// Tracker ledger maintaining agent presence, heartbeat expiration, and discovery.
#[derive(Clone, Default)]
pub struct PresenceTracker {
    entries: Arc<RwLock<HashMap<AgentAddress, AgentPresence>>>,
}

impl PresenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or updates an agent in the tracker.
    pub async fn register(
        &self,
        address: AgentAddress,
        ttl_secs: u64,
        metadata: HashMap<String, serde_json::Value>,
    ) -> AgentPresence {
        let now = Utc::now();
        let presence = AgentPresence {
            address: address.clone(),
            instance_id: None,
            metadata,
            registered_at: now,
            last_heartbeat: now,
            ttl_secs,
        };
        let mut guard = self.entries.write().await;
        guard.insert(address, presence.clone());
        presence
    }

    /// Records a heartbeat for an existing agent, refreshing its TTL.
    pub async fn heartbeat(&self, address: &AgentAddress) -> bool {
        let mut guard = self.entries.write().await;
        if let Some(presence) = guard.get_mut(address) {
            presence.last_heartbeat = Utc::now();
            true
        } else {
            false
        }
    }

    /// Queries all currently alive agents, purging expired ones.
    pub async fn active_agents(&self) -> Vec<AgentPresence> {
        let now = Utc::now();
        let mut guard = self.entries.write().await;
        guard.retain(|_, p| p.is_alive(now));
        guard.values().cloned().collect()
    }

    /// Retrieves presence info for a specific agent if alive.
    pub async fn get(&self, address: &AgentAddress) -> Option<AgentPresence> {
        let now = Utc::now();
        let guard = self.entries.read().await;
        guard.get(address).filter(|p| p.is_alive(now)).cloned()
    }

    /// Deregisters an agent immediately.
    pub async fn deregister(&self, address: &AgentAddress) -> Option<AgentPresence> {
        let mut guard = self.entries.write().await;
        guard.remove(address)
    }
}
