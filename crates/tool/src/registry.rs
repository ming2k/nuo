use crate::Tool;
use crate::context::ToolContext;
use crate::error::{Result, ToolError};
use crate::output::ToolOutput;
use crate::risk::RiskProfile;
use std::collections::HashMap;
use std::sync::Arc;

/// Central registry managing all tools available to an agent runtime.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

/// The canonical OpenAI-family function envelope for one tool.
///
/// Single definition on purpose: this shape was previously re-spelled in three
/// places in this crate (twice in [`ToolRegistry`], once on the `Tool` trait),
/// which is exactly how copies drift — one gains a field and the others keep
/// emitting the old contract while every caller keeps compiling. OpenAI-shaped
/// *providers* still own their own envelope because they add provider-specific
/// fields (`strict`, cache markers); the base shape belongs here, once.
pub(crate) fn function_envelope(
    name: &str,
    description: &str,
    parameters: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    })
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Registers a tool into the registry.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
    }

    /// Registers an Arc-wrapped tool.
    pub fn register_arc(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
    }

    /// Looks up a registered tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Verifies whether a tool is allowed within the active scopes.
    pub fn is_allowed_in_scope(
        &self,
        name: &str,
        active_scopes: Option<&[crate::scope::ToolScope]>,
    ) -> bool {
        let Some(scopes) = active_scopes else {
            return self.tools.contains_key(name);
        };
        self.tools.get(name).is_some_and(|tool| {
            let declared = tool.scopes();
            declared.iter().any(|s| scopes.contains(s))
        })
    }

    /// Registered tool names, sorted for deterministic assertions and logs.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    /// Checks the risk profile for a named tool.
    pub fn risk_profile(&self, name: &str) -> Option<RiskProfile> {
        self.tools.get(name).map(|t| t.risk_profile())
    }

    /// Checks whether execution of `name` with `arguments` requires approval under `ctx`.
    pub fn requires_approval(
        &self,
        ctx: &ToolContext,
        name: &str,
        arguments: &serde_json::Value,
    ) -> bool {
        self.tools
            .get(name)
            .is_some_and(|tool| tool.requires_approval(ctx, arguments))
    }

    /// Dispatches execution of a registered tool with full cancellation check and context propagation.
    pub async fn execute(
        &self,
        ctx: &ToolContext,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput> {
        ctx.check_cancelled(name)?;

        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::not_found(name))?;

        tool.execute(ctx, arguments).await
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The envelope shape is pinned as a literal, not compared against itself.
    ///
    /// This is the guard that makes the de-duplication safe to keep: the three
    /// copies this replaced could each drift independently, and a test that
    /// compared `function_envelope` to `function_envelope` would happily agree
    /// with a wrong shape. Pinning the wire bytes is what a provider consuming
    /// this actually depends on.
    #[test]
    fn function_envelope_is_the_openai_function_contract() {
        let envelope = function_envelope(
            "find_files",
            "Find files by glob",
            serde_json::json!({"type": "object"}),
        );
        assert_eq!(
            envelope,
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "find_files",
                    "description": "Find files by glob",
                    "parameters": {"type": "object"},
                }
            })
        );
    }
}
