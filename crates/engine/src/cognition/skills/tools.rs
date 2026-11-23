//! The two model-facing skill tools.
//!
//! Both are **deferred**: registered so `tool_search` and an `Overflow::Index`
//! resume can reach them, but never declared in every prompt. The skill catalog
//! is already in the instruction region; putting a second copy of the same
//! capability in the tool schemas would pay for it twice (ADR-0230).
use super::{SkillRegistry, SkillScope};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tool::{Tool, ToolExposure};

/// Load a skill's body into the conversation context.
///
/// The body arrives as a `<skill>` envelope with the same shape an `@name`
/// mention produces: one renderer, two doors, one format (ADR-0226).
pub struct UseSkillTool {
    pub registry: Arc<SkillRegistry>,
}

#[async_trait]
impl Tool for UseSkillTool {
    fn name(&self) -> &str {
        "use_skill"
    }

    fn description(&self) -> &str {
        "Load a skill's body into the conversation context. Prefer an `@name` mention in a \
         request; call this when the catalog is elided and you need the body now."
    }

    fn builtin(&self) -> Option<tool::BuiltinTool> {
        Some(tool::BuiltinTool::UseSkill)
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Deferred
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name to load" }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        _ctx: &::tool::ToolContext,
        args: serde_json::Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        let name = args["name"].as_str().ok_or("Missing 'name'")?;

        // Snapshot only the metadata we need under the read lock,
        // then release it before reading the body — keeps lock scope tight.
        let (scope, quarantined) = {
            let registry = self.registry.lock();
            let Some(skill) = registry.get(name) else {
                return Err(format!(
                    "Skill '{}' not found. Check the skill catalog to discover available skills.",
                    name
                )
                .into());
            };
            (skill.scope, skill.quarantined)
        };

        if quarantined {
            return Err(format!(
                "Skill '{name}' is quarantined (workspace skills domain is untrusted). Review it and run `/trust skills` to enable.",
            )
            .into());
        }

        // A repo skill may have changed since the last discovery scan. Check
        // the live domain digest before reading any filenames or body bytes;
        // on mismatch, rescan so the stale entry disappears from the registry.
        if scope == SkillScope::Repo {
            let project_root = self.registry.project_root().ok_or_else(|| {
                "Project skill has no workspace root for trust attestation.".to_string()
            })?;
            let state = persistence::workspace_security::WorkspaceSecurityStore::load()
                .snapshot(&project_root)
                .skills;
            if !state.is_trusted() {
                self.registry.reload().await;
                return Err(format!(
                    "Project skill '{name}' is {} because the skills-domain content changed. Review it and run `/trust skills`.",
                    state.as_str()
                )
                .into());
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

        Ok(::tool::ToolOutput::text(
            super::render::render_skill_envelope(&skill, &content),
        ))
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
        "List every declared skill, including the ones the catalog elided."
    }

    fn builtin(&self) -> Option<tool::BuiltinTool> {
        Some(tool::BuiltinTool::ListSkills)
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Deferred
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        _ctx: &::tool::ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        let registry = self.registry.lock();
        Ok(::tool::ToolOutput::text(
            super::render::render_skill_catalog(&registry.list()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkillRegistry;

    #[tokio::test]
    async fn use_skill_returns_not_found_for_missing_skill() {
        let registry = Arc::new(SkillRegistry::empty());
        let tool = UseSkillTool { registry };
        let result = tool
            .execute_text(&::tool::ToolContext::default(), r#"{"name":"missing"}"#)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn list_skills_reports_empty_registry() {
        let registry = Arc::new(SkillRegistry::empty());
        let tool = ListSkillsTool { registry };
        let result = tool
            .execute_text(&::tool::ToolContext::default(), "{}")
            .await
            .unwrap();
        assert!(result.contains("Available skills"));
    }
}
