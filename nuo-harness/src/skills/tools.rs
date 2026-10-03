//! Tools for interacting with the skill registry.

use super::{SkillRegistry, SkillScope};
use async_trait::async_trait;
use nuo_wire::Tool;
use serde_json::json;
use std::sync::Arc;

/// Load a skill into the conversation context.
pub struct UseSkillTool {
    pub registry: Arc<SkillRegistry>,
}

#[async_trait]
impl Tool for UseSkillTool {
    fn name(&self) -> &str {
        "use_skill"
    }

    fn description(&self) -> &str {
        "Load a skill into the conversation context. Skills provide domain-specific expertise. \
         Call this when the task matches a skill's description."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The skill name to load" }
            },
            "required": ["name"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let name = args["name"].as_str().ok_or("Missing 'name'")?;

        // Snapshot only the metadata we need under the read lock,
        // then release it before reading the body — keeps lock scope tight.
        let (scope, quarantined) = {
            let registry = self.registry.lock();
            let Some(skill) = registry.get(name) else {
                return Err(format!(
                    "Skill '{}' not found. Use the list_skills tool to discover available skills.",
                    name
                ));
            };
            (skill.scope, skill.quarantined)
        };

        if quarantined {
            return Err(format!(
                "Skill '{name}' is quarantined (workspace skills domain is untrusted). Review it and run `/trust skills` to enable.",
            ));
        }

        // A repo skill may have changed since the last discovery scan. Check
        // the live domain digest before reading any filenames or body bytes;
        // on mismatch, rescan so the stale entry disappears from the registry.
        if scope == SkillScope::Repo {
            let project_root = self.registry.project_root().ok_or_else(|| {
                "Project skill has no workspace root for trust attestation.".to_string()
            })?;
            let state = nuo_persistence::workspace_security::WorkspaceSecurityStore::load()
                .snapshot(&project_root)
                .skills;
            if !state.is_trusted() {
                self.registry.reload().await;
                return Err(format!(
                    "Project skill '{name}' is {} because the skills-domain content changed. Review it and run `/trust skills`.",
                    state.as_str()
                ));
            }
        }
        // Body is loaded lazily (and cached) on first use of this skill.
        let content = self
            .registry
            .body_for(name)
            .ok_or_else(|| format!("Skill '{}' not found.", name))??;

        let skill = self
            .registry
            .lock()
            .get(name)
            .ok_or_else(|| format!("Skill '{}' not found.", name))?;

        Ok(super::render::format_skill_injection(&skill, &content))
    }
}

/// List all available skills.
pub struct ListSkillsTool {
    pub registry: Arc<SkillRegistry>,
}

#[async_trait]
impl Tool for ListSkillsTool {
    fn name(&self) -> &str {
        "list_skills"
    }

    fn description(&self) -> &str {
        "List all available skills with their scope, description, and enabled state."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        let registry = self.registry.lock();
        Ok(super::render::format_skill_list(&registry.list()))
    }
}

// Tools available for programmatic invocation / tests
//
// Not admitted to any agent toolset: `Agent::with_skills` attaches the registry
// only, and no code path installs `UseSkillTool`/`ListSkillsTool`. The intended
// design is progressive disclosure — skill metadata in the request with bodies
// loaded on demand — and that is not yet implemented either, so today the model
// reaches skills through mention injection and the `skill` subagent role, and
// reaches skill files with the ordinary file tools. See ADR-0237.
//
// Do not describe these tools as available to the model until a call site
// installs them.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkillRegistry;

    #[tokio::test]
    async fn use_skill_returns_not_found_for_missing_skill() {
        let registry = Arc::new(SkillRegistry::empty());
        let tool = UseSkillTool { registry };
        let result = tool.call(r#"{"name":"missing"}"#).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn list_skills_reports_empty_registry() {
        let registry = Arc::new(SkillRegistry::empty());
        let tool = ListSkillsTool { registry };
        let result = tool.call("{}").await.unwrap();
        assert!(result.contains("Available skills"));
    }
}
