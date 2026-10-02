use crate::address::AgentAddress;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The natural language card describing an agent's capability, role, and identity.
///
/// This is used both for discovery across hosts/nodes and for cognitive reasoning
/// by LLMs to determine which peer agent to delegate tasks to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Canonical address of this agent
    pub address: AgentAddress,
    /// Human-readable display name
    pub name: String,
    /// Natural language description of what this agent does, its domain, and when to delegate to it
    pub description: String,
    /// High-level capability tags or skills
    #[serde(default)]
    pub skills: Vec<String>,
    /// Optional structured parameters description or instructions
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl AgentManifest {
    pub fn new(
        address: AgentAddress,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            address,
            name: name.into(),
            description: description.into(),
            skills: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_skill(mut self, skill: impl Into<String>) -> Self {
        self.skills.push(skill.into());
        self
    }

    pub fn with_skills(mut self, skills: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.skills.extend(skills.into_iter().map(Into::into));
        self
    }

    pub fn with_meta(mut self, key: impl Into<String>, val: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), val);
        self
    }

    /// Formats a concise summary of this agent suitable for inclusion in a prompt
    pub fn to_prompt_summary(&self) -> String {
        format!(
            "- `{}` ({}): {}",
            self.name,
            self.address.as_str(),
            self.description
        )
    }
}
