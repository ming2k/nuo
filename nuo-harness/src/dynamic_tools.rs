//! Agent-owned registry for tools published by dynamic external sources.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

use nuo_wire::{DynamicToolSink, Tool};

/// A deterministic snapshot entry carrying provenance beside the tool.
pub(crate) struct DynamicToolEntry {
    pub source: String,
    pub tool: Arc<dyn Tool>,
}

/// Thread-safe implementation of the core dynamic-tool publication port.
///
/// Sources are ordered lexicographically and each tool name appears at most
/// once in a snapshot. A source publishes a complete replacement, making
/// removal and reconnect behavior atomic from the agent's perspective.
#[derive(Default)]
pub(crate) struct DynamicToolRegistry {
    sources: RwLock<BTreeMap<String, Vec<Arc<dyn Tool>>>>,
}

impl DynamicToolRegistry {
    pub fn snapshot(&self) -> Vec<DynamicToolEntry> {
        let sources = self.sources.read().unwrap_or_else(|e| e.into_inner());
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for (source, tools) in sources.iter() {
            for tool in tools {
                if seen.insert(tool.name().to_string()) {
                    entries.push(DynamicToolEntry {
                        source: source.clone(),
                        tool: Arc::clone(tool),
                    });
                }
            }
        }
        entries
    }

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let sources = self.sources.read().unwrap_or_else(|e| e.into_inner());
        for tools in sources.values() {
            for tool in tools {
                if tool.name() == name {
                    return Some(Arc::clone(tool));
                }
            }
        }
        None
    }

    pub fn contains(&self, name: &str) -> bool {
        let sources = self.sources.read().unwrap_or_else(|e| e.into_inner());
        for tools in sources.values() {
            for tool in tools {
                if tool.name() == name {
                    return true;
                }
            }
        }
        false
    }

    /// Register tools conforming to the canonical substrate [`nuo_tool::Tool`] standard.
    #[allow(dead_code)]
    pub fn replace_substrate_tools(&self, source: &str, tools: Vec<Arc<dyn nuo_tool::Tool>>) {
        let bridged: Vec<Arc<dyn Tool>> = tools
            .into_iter()
            .map(|t| Arc::new(crate::NousToolBridge::new(t)) as Arc<dyn Tool>)
            .collect();
        self.replace(source, bridged);
    }
}

impl DynamicToolSink for DynamicToolRegistry {
    fn replace(&self, source: &str, tools: Vec<Arc<dyn Tool>>) {
        if source.trim().is_empty() {
            tracing::warn!("ignored dynamic tools with an empty source id");
            return;
        }
        self.sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(source.to_string(), tools);
    }

    fn remove(&self, source: &str) {
        self.sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct NamedTool(&'static str);

    #[async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            self.0
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok(self.0.to_string())
        }
    }

    #[test]
    fn replacement_and_removal_are_scoped_by_source() {
        let registry = DynamicToolRegistry::default();
        registry.replace("mcp:a", vec![Arc::new(NamedTool("first"))]);
        registry.replace("mcp:b", vec![Arc::new(NamedTool("second"))]);
        registry.replace("mcp:a", vec![Arc::new(NamedTool("replacement"))]);

        assert!(registry.find("first").is_none());
        assert!(registry.find("replacement").is_some());
        assert!(registry.find("second").is_some());

        registry.remove("mcp:a");
        assert!(registry.find("replacement").is_none());
        assert!(registry.find("second").is_some());
    }

    #[test]
    fn source_order_resolves_duplicate_names_deterministically() {
        let registry = DynamicToolRegistry::default();
        registry.replace("source:z", vec![Arc::new(NamedTool("same"))]);
        registry.replace("source:a", vec![Arc::new(NamedTool("same"))]);

        let entries = registry.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "source:a");
    }

    #[test]
    fn registers_substrate_nuo_tools() {
        struct MockNousTool;
        #[async_trait::async_trait]
        impl nuo_tool::Tool for MockNousTool {
            fn name(&self) -> &str {
                "substrate_calc"
            }
            fn description(&self) -> &str {
                "Substrate calculator"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _ctx: &nuo_tool::ToolContext,
                _args: serde_json::Value,
            ) -> Result<nuo_tool::ToolOutput, nuo_tool::ToolError> {
                Ok(nuo_tool::ToolOutput::success("42"))
            }
        }
        let registry = DynamicToolRegistry::default();
        registry.replace_substrate_tools("mcp-server-1", vec![Arc::new(MockNousTool)]);
        assert!(registry.contains("substrate_calc"));
        let tool = registry.find("substrate_calc").unwrap();
        assert_eq!(tool.name(), "substrate_calc");
        assert_eq!(tool.hazard_level(), nuo_wire::HazardLevel::Safe);
    }
}
