use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use acp::{AgentAddress, AgentEnvelope, AgentManifest, Fabric, MessageIntent, SteerAction};
use nuo_wire::{MonitorAction, Tool};
use nuo_harness::{Agent, AgentIdentity};

use crate::registry::SessionRegistry;

/// The single Hypervisor station per server (staffed by an agent in Root posture).
///
/// Responsible for orchestrating sessions, tracking progress across projects,
/// joint debugging / cross-session coordination, and dispatching top-down
/// coordination instructions to session masters over the ACP fabric.
pub struct Hypervisor {
    agent: Arc<Agent>,
    #[allow(dead_code)]
    registry: SessionRegistry,
    #[allow(dead_code)]
    fabric: Fabric,
    address: AgentAddress,
    _mailbox: acp::Mailbox,
}

impl Hypervisor {
    /// Create the singleton hypervisor station for the server.
    pub async fn new(
        provider: Arc<dyn nuo_wire::Provider>,
        registry: SessionRegistry,
        fabric: Fabric,
    ) -> Self {
        let address = AgentAddress::parse("agent://local/hypervisor").expect("valid hypervisor address");

        let manifest = AgentManifest::new(
            address.clone(),
            "hypervisor",
            "the single server-level hypervisor for Nuo — orchestrating sessions and multi-session workflows",
        );
        let mailbox = fabric.join(manifest, 64).await;

        let list_sessions_tool = Arc::new(HypervisorListSessionsTool::new(registry.clone()));
        let inspect_session_tool = Arc::new(HypervisorInspectSessionTool::new(registry.clone()));
        let instruct_session_tool = Arc::new(HypervisorInstructSessionTool::new(
            fabric.clone(),
            address.clone(),
        ));
        let coordinate_tool = Arc::new(HypervisorCoordinateDebugTool::new(
            registry.clone(),
            fabric.clone(),
            address.clone(),
        ));

        let tools: Vec<Arc<dyn Tool>> = vec![
            list_sessions_tool,
            inspect_session_tool,
            instruct_session_tool,
            coordinate_tool,
        ];

        let identity = AgentIdentity::new(
            "hypervisor",
            "the single server-level hypervisor for Nuo — orchestrating sessions, tracking progress across projects, and coordinating joint debugging and multi-session workflows",
        );

        let agent = Arc::new(Agent::new(provider, tools, identity));
        agent.set_kind(nuo_wire::AgentKind::Root);

        Self {
            agent,
            registry,
            fabric,
            address,
            _mailbox: mailbox,
        }
    }

    pub fn agent(&self) -> Arc<Agent> {
        self.agent.clone()
    }

    pub fn address(&self) -> &AgentAddress {
        &self.address
    }
}

/// Tool for Hypervisor to list and monitor all hosted sessions across the server.
pub struct HypervisorListSessionsTool {
    registry: SessionRegistry,
}

impl HypervisorListSessionsTool {
    pub fn new(registry: SessionRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for HypervisorListSessionsTool {
    fn name(&self) -> &str {
        "hypervisor_list_sessions"
    }

    fn description(&self) -> &str {
        "List all active and hosted sessions in the server with their statuses and message counts."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "include_idle": {
                    "type": "boolean",
                    "description": "Whether to include idle/sleeping sessions (default: true)"
                }
            },
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let include_idle = args["include_idle"].as_bool().unwrap_or(true);

        let snapshot = self
            .registry
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle,
            })
            .await;

        let sessions: Vec<serde_json::Value> = snapshot
            .sessions
            .iter()
            .map(|s| {
                json!({
                    "session_id": s.id,
                    "status": format!("{:?}", s.status),
                    "message_count": s.message_count,
                    "created_at": s.created_at,
                    "overview": s.overview
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&json!({
            "total": sessions.len(),
            "sessions": sessions
        }))
        .unwrap_or_default())
    }
}

/// Tool for Hypervisor to inspect a session's detailed state and messages.
pub struct HypervisorInspectSessionTool {
    registry: SessionRegistry,
}

impl HypervisorInspectSessionTool {
    pub fn new(registry: SessionRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for HypervisorInspectSessionTool {
    fn name(&self) -> &str {
        "hypervisor_inspect_session"
    }

    fn description(&self) -> &str {
        "Inspect the detailed state and transcript history of a specific hosted session."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The ID of the session to inspect"
                }
            },
            "required": ["session_id"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;

        let session_id = args["session_id"]
            .as_str()
            .ok_or("Missing 'session_id' argument")?;

        let host = self
            .registry
            .get(session_id)
            .await
            .ok_or_else(|| format!("Session '{session_id}' not found in registry"))?;

        let project_root = host.workspace_root.as_ref().map(|p| p.to_string_lossy().to_string());
        let turns = host.session.commands().await.len();

        let details = json!({
            "session_id": session_id,
            "project_root": project_root,
            "turn_count": turns,
        });

        Ok(serde_json::to_string_pretty(&details).unwrap_or_default())
    }
}

/// Tool for Hypervisor to send top-down instructions to a session root agent over ACP.
pub struct HypervisorInstructSessionTool {
    fabric: Fabric,
    hypervisor_address: AgentAddress,
}

impl HypervisorInstructSessionTool {
    pub fn new(fabric: Fabric, hypervisor_address: AgentAddress) -> Self {
        Self {
            fabric,
            hypervisor_address,
        }
    }
}

#[async_trait]
impl Tool for HypervisorInstructSessionTool {
    fn name(&self) -> &str {
        "hypervisor_instruct_session"
    }

    fn description(&self) -> &str {
        "Send top-down instructions or steering guidance from the Hypervisor to a session agent over the ACP fabric."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The ID of the target session"
                },
                "instruction": {
                    "type": "string",
                    "description": "The directive or guidance for the session agent"
                }
            },
            "required": ["session_id", "instruction"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;

        let session_id = args["session_id"]
            .as_str()
            .ok_or("Missing 'session_id' argument")?;
        let instruction = args["instruction"]
            .as_str()
            .ok_or("Missing 'instruction' argument")?;

        let recipient = AgentAddress::parse(&format!("agent://local/session/{session_id}"))
            .map_err(|e| format!("Invalid recipient address: {e}"))?;

        let intent = MessageIntent::steer(instruction, SteerAction::Note);
        let envelope = AgentEnvelope::new(self.hypervisor_address.clone(), recipient, intent);

        let msg_id = envelope.id.to_string();
        self.fabric
            .dispatch(envelope)
            .await
            .map_err(|e| format!("Failed to dispatch instruction via ACP fabric: {e}"))?;

        Ok(json!({
            "status": "delivered",
            "message_id": msg_id,
            "target_session": session_id
        })
        .to_string())
    }
}

/// Tool for Hypervisor to coordinate joint debugging across multiple sessions.
pub struct HypervisorCoordinateDebugTool {
    registry: SessionRegistry,
    fabric: Fabric,
    hypervisor_address: AgentAddress,
}

impl HypervisorCoordinateDebugTool {
    pub fn new(
        registry: SessionRegistry,
        fabric: Fabric,
        hypervisor_address: AgentAddress,
    ) -> Self {
        Self {
            registry,
            fabric,
            hypervisor_address,
        }
    }
}

#[async_trait]
impl Tool for HypervisorCoordinateDebugTool {
    fn name(&self) -> &str {
        "hypervisor_coordinate_debug"
    }

    fn description(&self) -> &str {
        "Coordinate joint debugging (联调) between multiple sessions by linking their context and dispatching shared objectives."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_a": {
                    "type": "string",
                    "description": "First session ID (e.g. backend/server)"
                },
                "session_b": {
                    "type": "string",
                    "description": "Second session ID (e.g. frontend/client)"
                },
                "objective": {
                    "type": "string",
                    "description": "The joint debugging objective or contract to verify"
                }
            },
            "required": ["session_a", "session_b", "objective"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;

        let session_a = args["session_a"]
            .as_str()
            .ok_or("Missing 'session_a'")?;
        let session_b = args["session_b"]
            .as_str()
            .ok_or("Missing 'session_b'")?;
        let objective = args["objective"]
            .as_str()
            .ok_or("Missing 'objective'")?;

        if self.registry.get(session_a).await.is_none() {
            return Err(format!("Session A '{session_a}' not found"));
        }
        if self.registry.get(session_b).await.is_none() {
            return Err(format!("Session B '{session_b}' not found"));
        }

        // Notify both sessions over ACP fabric
        for session_id in &[session_a, session_b] {
            let recipient = AgentAddress::parse(&format!("agent://local/session/{session_id}"))
                .map_err(|e| format!("Invalid recipient address: {e}"))?;

            let intent = MessageIntent::steer(
                format!("[Joint Debugging Coordination]: You are paired with sibling session for: {objective}"),
                SteerAction::Note,
            );
            let envelope = AgentEnvelope::new(self.hypervisor_address.clone(), recipient, intent);
            let _ = self.fabric.dispatch(envelope).await;
        }

        Ok(json!({
            "status": "coordinated",
            "paired": [session_a, session_b],
            "objective": objective
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hypervisor_creation_and_tools() {
        let provider = Arc::new(nuo_harness::NoProvider);
        let registry = SessionRegistry::prehost_only();
        let fabric = acp::Fabric::new("test-fabric");

        let hypervisor = Hypervisor::new(provider, registry.clone(), fabric).await;
        assert_eq!(hypervisor.address().as_str(), "agent://local/hypervisor");

        let list_tool = HypervisorListSessionsTool::new(registry.clone());
        let res = list_tool.call("{}").await.unwrap();
        assert!(res.contains("total"));
    }
}
