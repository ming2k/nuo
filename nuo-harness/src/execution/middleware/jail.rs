//! Workspace jail middleware to ensure file operations stay confined within the workspace root.

use async_trait::async_trait;
use nuo_contracts::execution::{ExecutionEnvironment, ToolMiddleware};
use serde_json::Value;

/// Reject path arguments outside the primary and explicitly admitted workspace roots.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceJailMiddleware;

#[async_trait]
impl ToolMiddleware for WorkspaceJailMiddleware {
    async fn pre_execute(
        &self,
        tool: &str,
        arguments: &Value,
        env: &dyn ExecutionEnvironment,
    ) -> Result<(), String> {
        let path_str = match tool {
            "edit_text" | "find_files" | "list_dir" | "read_text" | "read_text_terse"
            | "search_text" | "write_file" | "read_image" => {
                arguments.get("path").and_then(|v| v.as_str())
            }
            _ => None,
        };

        if let Some(raw_path) = path_str
            && let Err(err) = env.resolve_path(raw_path)
        {
            return Err(format!(
                "Security Denial: access to '{raw_path}' is outside the admitted workspace roots ({err})."
            ));
        }

        Ok(())
    }
}
