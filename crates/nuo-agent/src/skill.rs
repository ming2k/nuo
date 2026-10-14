//! Ephemeral Skill Stack and Procedural Cognitive Sandbox primitives.
//!
//! Implements ADR-0008: first-class procedural knowledge injection, JIT prompt mounting,
//! deterministic prompt caching breakpoints, and ephemeral capability sandboxing (`ToolScope`).

use nuo_tool::ToolScope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(feature = "wire")]
pub use nuo_model_codec::CacheControl;

#[cfg(not(feature = "wire"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheControl {
    Ephemeral,
}

/// A procedural capability package composed of domain instructions (SOP),
/// minimal-privilege tool scopes, and prompt caching directives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub scopes: Vec<ToolScope>,
    pub cache_control: Option<CacheControl>,
}

impl Skill {
    pub fn builder(id: impl Into<String>) -> SkillBuilder {
        SkillBuilder::new(id)
    }

    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            instructions: instructions.into(),
            scopes: Vec::new(),
            cache_control: Some(CacheControl::Ephemeral),
        }
    }

    /// Parses a skill from markdown text containing YAML frontmatter and a procedural SOP body.
    pub fn from_markdown(id: impl Into<String>, markdown: &str) -> Self {
        let id_str = id.into();
        let (frontmatter, body) = split_frontmatter(markdown);
        let mut name = id_str.clone();
        let mut description = String::new();
        let mut scopes = Vec::new();
        let mut cache_control = Some(CacheControl::Ephemeral);

        let mut current_list_key: Option<&str> = None;

        for line in frontmatter.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(item) = line.strip_prefix('-') {
                let item = item.trim().trim_matches(|c| c == '\'' || c == '"');
                if let Some("scopes") = current_list_key {
                    let scope = match item.to_lowercase().as_str() {
                        "read_only" | "readonly" => ToolScope::ReadOnly,
                        "workspace" => ToolScope::Workspace,
                        "execution" => ToolScope::Execution,
                        "network" => ToolScope::Network,
                        "collaboration" => ToolScope::Collaboration,
                        other => ToolScope::Custom(other.to_string()),
                    };
                    scopes.push(scope);
                }
                continue;
            }

            current_list_key = None;

            if let Some((key, val)) = line.split_once(':') {
                let key = key.trim();
                let val = val.trim().trim_matches(|c| c == '\'' || c == '"');
                match key {
                    "name" => {
                        if !val.is_empty() {
                            name = val.to_string();
                        }
                    }
                    "description" => {
                        description = val.to_string();
                    }
                    "cache_control" => {
                        cache_control = match val.to_lowercase().as_str() {
                            "ephemeral" => Some(CacheControl::Ephemeral),
                            "none" | "disabled" => None,
                            _ => Some(CacheControl::Ephemeral),
                        };
                    }
                    "scopes" => {
                        if !val.is_empty() {
                            let trimmed_val = val.trim_matches(|c| c == '[' || c == ']');
                            for item in trimmed_val.split(',') {
                                let item = item.trim().trim_matches(|c| c == '\'' || c == '"');
                                if !item.is_empty() {
                                    let scope = match item.to_lowercase().as_str() {
                                        "read_only" | "readonly" => ToolScope::ReadOnly,
                                        "workspace" => ToolScope::Workspace,
                                        "execution" => ToolScope::Execution,
                                        "network" => ToolScope::Network,
                                        "collaboration" => ToolScope::Collaboration,
                                        other => ToolScope::Custom(other.to_string()),
                                    };
                                    scopes.push(scope);
                                }
                            }
                        } else {
                            current_list_key = Some("scopes");
                        }
                    }
                    _ => {}
                }
            }
        }

        Self {
            id: id_str,
            name,
            description,
            instructions: body.trim().to_string(),
            scopes,
            cache_control,
        }
    }
}

/// Splits markdown frontmatter from body text. Returns `("", body)` when no frontmatter exists.
fn split_frontmatter(text: &str) -> (&str, &str) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return ("", trimmed);
    }
    let after_open = &trimmed[3..];
    let Some(close_idx) = after_open.find("---") else {
        return ("", trimmed);
    };
    let frontmatter = after_open[..close_idx].trim();
    let body = &after_open[close_idx + 3..];
    (frontmatter, body)
}

/// Builder for constructing [`Skill`] entities with ergonomic defaults.
#[derive(Debug, Clone)]
pub struct SkillBuilder {
    id: String,
    name: Option<String>,
    description: String,
    instructions: String,
    scopes: Vec<ToolScope>,
    cache_control: Option<CacheControl>,
}

impl SkillBuilder {
    pub fn new(id: impl Into<String>) -> Self {
        let id_str = id.into();
        Self {
            id: id_str.clone(),
            name: None,
            description: String::new(),
            instructions: String::new(),
            scopes: Vec::new(),
            cache_control: Some(CacheControl::Ephemeral),
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = instructions.into();
        self
    }

    pub fn scope(mut self, scope: impl Into<ToolScope>) -> Self {
        self.scopes.push(scope.into());
        self
    }

    pub fn scopes(mut self, scopes: impl IntoIterator<Item = impl Into<ToolScope>>) -> Self {
        self.scopes.extend(scopes.into_iter().map(Into::into));
        self
    }

    pub fn cache_control(mut self, control: Option<CacheControl>) -> Self {
        self.cache_control = control;
        self
    }

    pub fn build(self) -> Skill {
        let name = self.name.unwrap_or_else(|| self.id.clone());
        Skill {
            id: self.id,
            name,
            description: self.description,
            instructions: self.instructions,
            scopes: self.scopes,
            cache_control: self.cache_control,
        }
    }
}

/// Registry of skills available to an agent runtime.
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, skill: Skill) {
        self.skills.insert(skill.id.clone(), skill);
    }

    pub fn get(&self, id: &str) -> Option<&Skill> {
        self.skills.get(id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.skills.contains_key(id)
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn list(&self) -> Vec<&Skill> {
        let mut list: Vec<&Skill> = self.skills.values().collect();
        list.sort_by_key(|s| &s.id);
        list
    }

    /// Emits a compact, zero-noise catalog summary for inactive skill discovery (`[INV-SKILL-01]`).
    pub fn catalog_summary(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut lines = vec![
            "Available procedural skills (call `mount_skill` with id to enter specialized mode):"
                .to_string(),
        ];
        let skills = self.list();
        for s in skills {
            lines.push(format!("- {}: {}", s.id, s.description));
        }
        lines.join("\n")
    }
}

/// In-flight activation frame in the skill stack.
#[derive(Debug, Clone)]
pub struct SkillFrame {
    pub skill: Skill,
    pub previous_scopes: Option<Vec<ToolScope>>,
}

/// Stateful runtime stack managing active skill frames and dynamic sandboxes.
#[derive(Debug, Clone, Default)]
pub struct SkillStack {
    frames: Vec<SkillFrame>,
}

impl SkillStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pushes a new skill frame onto the stack, saving prior scopes for reversible unmounting.
    pub fn push(&mut self, skill: Skill, current_scopes: Option<Vec<ToolScope>>) -> &SkillFrame {
        self.frames.push(SkillFrame {
            skill,
            previous_scopes: current_scopes,
        });
        #[allow(clippy::expect_used)]
        self.frames.last().expect("frame was just pushed")
    }

    /// Pops the active skill frame, returning previous tool scopes.
    pub fn pop(&mut self) -> Option<SkillFrame> {
        self.frames.pop()
    }

    /// Returns the currently active skill at the top of the stack.
    pub fn active_skill(&self) -> Option<&Skill> {
        self.frames.last().map(|f| &f.skill)
    }

    /// Whether any skill is currently mounted.
    pub fn is_active(&self) -> bool {
        !self.frames.is_empty()
    }

    /// Current depth of the skill stack.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Resolves the effective tool scopes governed by the active skill frame (`[INV-SKILL-02]`).
    pub fn effective_scopes<'a>(
        &'a self,
        base_scopes: Option<&'a Vec<ToolScope>>,
    ) -> Option<Vec<ToolScope>> {
        if let Some(frame) = self.frames.last()
            && !frame.skill.scopes.is_empty()
        {
            return Some(frame.skill.scopes.clone());
        }
        base_scopes.cloned()
    }

    /// Formats all active procedural SOP instructions for cognitive prompt injection.
    pub fn active_instructions(&self) -> Vec<(String, Option<CacheControl>)> {
        self.frames
            .iter()
            .map(|f| (f.skill.instructions.clone(), f.skill.cache_control))
            .collect()
    }
}

impl From<&str> for Skill {
    fn from(id: &str) -> Self {
        Skill::new(id, id, format!("Procedural capability tag: {id}"), "")
    }
}

impl From<String> for Skill {
    fn from(id: String) -> Self {
        Skill::new(id.clone(), id, "", "")
    }
}

/// Tool allowing the agent to autonomously mount a specialized procedural skill.
pub struct MountSkillTool {
    registry: SkillRegistry,
    stack: std::sync::Arc<std::sync::Mutex<SkillStack>>,
}

impl MountSkillTool {
    pub fn new(
        registry: SkillRegistry,
        stack: std::sync::Arc<std::sync::Mutex<SkillStack>>,
    ) -> Self {
        Self { registry, stack }
    }
}

#[async_trait::async_trait]
impl nuo_tool::Tool for MountSkillTool {
    fn name(&self) -> &str {
        "mount_skill"
    }

    fn description(&self) -> &str {
        "Mount a specialized procedural skill by ID to enter restricted scope and load expert instructions."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_id": {
                    "type": "string",
                    "description": "Identifier of the skill to mount"
                }
            },
            "required": ["skill_id"]
        })
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![
            ToolScope::ReadOnly,
            ToolScope::Workspace,
            ToolScope::Execution,
            ToolScope::Network,
            ToolScope::Collaboration,
            ToolScope::custom("skill_management"),
        ]
    }

    async fn execute(
        &self,
        _ctx: &nuo_tool::ToolContext,
        arguments: serde_json::Value,
    ) -> nuo_tool::Result<nuo_tool::ToolOutput> {
        let skill_id = arguments
            .get("skill_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if skill_id.is_empty() {
            return Ok(nuo_tool::ToolOutput::error(
                "Missing required argument `skill_id`",
            ));
        }

        let Some(skill) = self.registry.get(skill_id) else {
            return Ok(nuo_tool::ToolOutput::error(format!(
                "Skill `{skill_id}` not found in registry. Call catalog for available skills."
            )));
        };

        let mut stack = self.stack.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev_scopes = stack.effective_scopes(None);
        stack.push(skill.clone(), prev_scopes);

        let scope_info = if skill.scopes.is_empty() {
            "unrestricted".to_string()
        } else {
            format!("{:?}", skill.scopes)
        };

        Ok(nuo_tool::ToolOutput::success(format!(
            "Mounted skill `{}`: {}. Active tools scoped to: {}. Procedural instructions activated.",
            skill.id, skill.name, scope_info
        )))
    }
}

/// Tool allowing the agent to unmount the currently active procedural skill.
pub struct UnmountSkillTool {
    stack: std::sync::Arc<std::sync::Mutex<SkillStack>>,
}

impl UnmountSkillTool {
    pub fn new(stack: std::sync::Arc<std::sync::Mutex<SkillStack>>) -> Self {
        Self { stack }
    }
}

#[async_trait::async_trait]
impl nuo_tool::Tool for UnmountSkillTool {
    fn name(&self) -> &str {
        "unmount_skill"
    }

    fn description(&self) -> &str {
        "Unmount the active specialized skill and restore default capabilities and scopes."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![
            ToolScope::ReadOnly,
            ToolScope::Workspace,
            ToolScope::Execution,
            ToolScope::Network,
            ToolScope::Collaboration,
            ToolScope::custom("skill_management"),
        ]
    }

    async fn execute(
        &self,
        _ctx: &nuo_tool::ToolContext,
        _arguments: serde_json::Value,
    ) -> nuo_tool::Result<nuo_tool::ToolOutput> {
        let mut stack = self.stack.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(frame) = stack.pop() {
            Ok(nuo_tool::ToolOutput::success(format!(
                "Unmounted skill `{}`. Capabilities restored to baseline.",
                frame.skill.id
            )))
        } else {
            Ok(nuo_tool::ToolOutput::success(
                "No skill was active. Baseline capabilities preserved.",
            ))
        }
    }
}

/// Installs the autonomous skill mounting and unmounting tools into a tool registry.
pub fn install_skill_tools(
    tools: &mut nuo_tool::ToolRegistry,
    stack: std::sync::Arc<std::sync::Mutex<SkillStack>>,
    registry: SkillRegistry,
) {
    tools.register(MountSkillTool::new(registry, stack.clone()));
    tools.register(UnmountSkillTool::new(stack));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_from_markdown_with_yaml_frontmatter() {
        let md = r#"---
name: rust-engineer
description: Rust software development guidelines
scopes:
  - read_only
  - workspace
cache_control: ephemeral
---
# Rust SOP
Always run `cargo check` and follow idiomatic Rust.
"#;
        let skill = Skill::from_markdown("rust-eng", md);
        assert_eq!(skill.id, "rust-eng");
        assert_eq!(skill.name, "rust-engineer");
        assert_eq!(skill.description, "Rust software development guidelines");
        assert_eq!(skill.scopes, vec![ToolScope::ReadOnly, ToolScope::Workspace]);
        assert_eq!(skill.cache_control, Some(CacheControl::Ephemeral));
        assert!(skill.instructions.contains("Always run `cargo check`"));
    }

    #[test]
    fn test_skill_from_markdown_inline_scopes() {
        let md = r#"---
name: code-reviewer
scopes: [read_only, network]
---
Review pull requests thoroughly.
"#;
        let skill = Skill::from_markdown("reviewer", md);
        assert_eq!(skill.id, "reviewer");
        assert_eq!(skill.name, "code-reviewer");
        assert_eq!(skill.scopes, vec![ToolScope::ReadOnly, ToolScope::Network]);
        assert_eq!(skill.instructions, "Review pull requests thoroughly.");
    }

    #[test]
    fn test_skill_from_markdown_without_frontmatter() {
        let md = "# Simple Instructions\nJust do the work.";
        let skill = Skill::from_markdown("simple", md);
        assert_eq!(skill.id, "simple");
        assert_eq!(skill.name, "simple");
        assert!(skill.scopes.is_empty());
        assert_eq!(skill.instructions, "# Simple Instructions\nJust do the work.");
    }
}
