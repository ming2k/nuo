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

    /// Formats all registered tools into model-facing function calling specifications.
    pub fn model_specs(&self) -> Vec<serde_json::Value> {
        self.tools
            .values()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters_schema(),
                    }
                })
            })
            .collect()
    }

    /// Returns model specs filtered by active operational scopes.
    /// If `active_scopes` is None, all registered tools are returned.
    pub fn model_specs_scoped(
        &self,
        active_scopes: Option<&[crate::scope::ToolScope]>,
    ) -> Vec<serde_json::Value> {
        let Some(scopes) = active_scopes else {
            return self.model_specs();
        };

        self.tools
            .values()
            .filter(|t| {
                let declared = t.scopes();
                declared.iter().any(|s| scopes.contains(s))
            })
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters_schema(),
                    }
                })
            })
            .collect()
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
