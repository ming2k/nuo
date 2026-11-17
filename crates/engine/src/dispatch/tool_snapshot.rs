//! The immutable tool set advertised for one model request.
//!
//! Dispatch must not consult the live registry: a tool added or removed while a
//! request is in flight (MCP connect/disconnect, `/reload`, a scoped hook) must
//! not change what an already-advertised call can reach. The snapshot also
//! compiles every input/output schema once, so each call in the request
//! validates against the same compiled validator.

use std::collections::HashMap;
use std::sync::Arc;

use tool::{CompiledSchema, Tool};

/// One advertised tool plus its compiled schemas.
pub(crate) struct ToolSnapshotEntry {
    pub tool: Arc<dyn Tool>,
    /// `None` when the tool has no declared input schema (never for a valid
    /// tool) or when compilation failed.
    pub input: Option<Arc<CompiledSchema>>,
    pub input_error: Option<String>,
    pub output: Option<Arc<CompiledSchema>>,
    pub output_error: Option<String>,
}

/// An immutable request-scoped catalog.
pub(crate) struct ToolSnapshot {
    entries: Vec<Arc<ToolSnapshotEntry>>,
    by_name: HashMap<String, Arc<ToolSnapshotEntry>>,
}

impl ToolSnapshot {
    /// Compile and index one declaration set.
    pub(crate) fn build(tools: Vec<Arc<dyn Tool>>) -> Self {
        let mut entries = Vec::with_capacity(tools.len());
        let mut by_name = HashMap::new();
        for tool in tools {
            let (input, input_error) = match tool::compile_schema_cached(&tool.parameters_schema())
            {
                Ok(schema) => (Some(schema), None),
                Err(error) => (None, Some(error)),
            };
            let (output, output_error) = match tool.output_schema() {
                Some(schema) => match tool::compile_schema_cached(&schema) {
                    Ok(schema) => (Some(schema), None),
                    Err(error) => (None, Some(error)),
                },
                None => (None, None),
            };
            let entry = Arc::new(ToolSnapshotEntry {
                tool,
                input,
                input_error,
                output,
                output_error,
            });
            by_name.insert(entry.tool.name().to_string(), Arc::clone(&entry));
            for alias in entry.tool.aliases() {
                by_name
                    .entry((*alias).to_string())
                    .or_insert_with(|| Arc::clone(&entry));
            }
            entries.push(entry);
        }
        Self { entries, by_name }
    }

    /// Resolve a model-supplied name (canonical or alias) within this snapshot.
    pub(crate) fn find(&self, name: &str) -> Option<Arc<ToolSnapshotEntry>> {
        self.by_name.get(name).cloned()
    }

    /// The tools in deterministic declaration order.
    pub(crate) fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.entries
            .iter()
            .map(|entry| Arc::clone(&entry.tool))
            .collect()
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.tool.name().to_string())
            .collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    struct AliasTool;

    #[async_trait]
    impl Tool for AliasTool {
        fn name(&self) -> &str {
            "canonical"
        }
        fn description(&self) -> &str {
            "canonical"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["alias"]
        }
        async fn execute(
            &self,
            _ctx: &tool::ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<tool::ToolOutput, tool::ToolError> {
            Ok(tool::ToolOutput::text("ok"))
        }
    }

    #[test]
    fn a_snapshot_resolves_canonical_names_and_aliases() {
        let snapshot = ToolSnapshot::build(vec![Arc::new(AliasTool)]);
        assert!(snapshot.find("canonical").is_some());
        assert!(snapshot.find("alias").is_some());
        assert!(snapshot.find("not_advertised").is_none());
        assert_eq!(snapshot.len(), 1);
    }

    #[test]
    fn a_broken_schema_is_retained_as_an_error_not_a_silent_drop() {
        struct Broken;
        #[async_trait]
        impl Tool for Broken {
            fn name(&self) -> &str {
                "broken"
            }
            fn description(&self) -> &str {
                "broken"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                json!({"type": "object", "properties": 3})
            }
            async fn execute(
                &self,
                _ctx: &tool::ToolContext,
                _arguments: serde_json::Value,
            ) -> Result<tool::ToolOutput, tool::ToolError> {
                Ok(tool::ToolOutput::text("never"))
            }
        }
        let snapshot = ToolSnapshot::build(vec![Arc::new(Broken)]);
        let entry = snapshot.find("broken").expect("still advertised");
        assert!(entry.input.is_none());
        assert!(entry.input_error.is_some());
    }
}
