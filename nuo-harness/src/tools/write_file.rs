use async_trait::async_trait;
use nuo_wire::Tool;
use nuo_tool::ToolSchema;
use serde::Deserialize;

use crate::tools::helpers::{
    WorkspaceBase, check_expected_version, env_from_root, execution_environment, json_string,
    read_optional, resolve_workspace_path, workspace_base,
};

#[derive(ToolSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    #[tool(
        desc = "Path to the file to create or overwrite; relative paths use the primary workspace"
    )]
    path: String,
    #[tool(desc = "The complete file content to write")]
    content: String,
    #[tool(
        desc = "Optional content version the caller last saw for this file (as reported by read_text or code_query). The write is rejected if the file has changed or no longer exists since."
    )]
    expected_version: Option<String>,
}

/// Write content to a file (overwrites).
///
/// Relative paths resolve against the session's workspace root (captured at
/// factory time), not the daemon process's cwd — under the unified daemon
/// (ADR-0096) those differ whenever the daemon was first spawned from another
/// project, and a write is exactly where that divergence does damage.
pub struct WriteFileTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>>,
}

impl WriteFileTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self { root, env: None }
    }

    pub fn with_env(env: std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Create a new file or completely overwrite an existing file with the given content."
    }
    fn parameters(&self) -> serde_json::Value {
        WriteFileArgs::parameters_schema()
    }
    fn scope_target(&self, arguments: &str) -> nuo_wire::ScopeTarget {
        nuo_wire::ScopeTarget::Path(std::path::PathBuf::from(json_string(arguments, "path")))
    }
    fn hazard_level(&self) -> nuo_wire::HazardLevel {
        nuo_wire::HazardLevel::FileModification
    }
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<nuo_wire::ToolPermissionSubmission> {
        let path = json_string(arguments, "path");
        Some(nuo_wire::ToolPermissionSubmission {
            hazard_level: nuo_wire::HazardLevel::FileModification,
            label: format!("Write file `{path}`"),
            description: format!("Creates or overwrites file `{path}` with new content."),
            scope: path.clone(),
            payload: nuo_wire::ToolPermissionPayload::FileEdit {
                paths: vec![path],
                operation: "write_file".to_string(),
            },
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<nuo_wire::ToolOutput, String> {
        let args: WriteFileArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let path = &args.path;
        let content = &args.content;

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        let resolved = resolve_workspace_path(&self.root, path);

        // Freshness precondition (ADR-0237): a full overwrite has no
        // `old_string` anchor, so a supplied version is the only thing standing
        // between an out-of-date model and silently lost content. Checked
        // before the write, and fail-closed when the file vanished.
        if args.expected_version.is_some() {
            let current = read_optional(env.as_ref(), &resolved)
                .await
                .map_err(|error| format!("Failed to read '{path}' for version check: {error}"))?;
            check_expected_version(path, args.expected_version.as_deref(), current.as_deref())?;
        }

        // Write atomically (temp file + fsync + rename) so an interrupted write
        // never leaves a half-written, corrupt file in place of the original.
        env.fs()
            .write(&resolved, content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write '{}': {}", path, e))?;

        Ok(super::syntax_guard::mutation_output(
            &resolved,
            content,
            nuo_wire::ToolOutput::Patch {
                path: path.to_string(),
                op: nuo_wire::PatchOp::Create,
                old: String::new(),
                new: content.to_string(),
                start_line: 0,
                warnings: Vec::new(),
            },
        ))
    }
}
nuo_wire::register_tool!(WriteFileFactory => |ctx| WriteFileTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::helpers::content_version;

    /// A full overwrite has no `old_string` anchor, so `expected_version` is
    /// the only thing standing between an out-of-date model and silently lost
    /// content (ADR-0237).
    #[tokio::test]
    async fn expected_version_blocks_an_overwrite_of_unseen_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.rs");
        std::fs::write(&file, "const A: u8 = 1;\n").unwrap();
        let tool = WriteFileTool::new(Some(dir.path().to_path_buf()));

        let stale = content_version(b"const A: u8 = 0;\n");
        let error = tool
            .call(
                &serde_json::json!({
                    "path": "config.rs",
                    "content": "const A: u8 = 2;\n",
                    "expected_version": stale,
                })
                .to_string(),
            )
            .await
            .unwrap_err();
        assert!(error.contains("has changed since version"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "const A: u8 = 1;\n",
            "the rejected overwrite must not have landed"
        );
    }

    /// A file that vanished since it was read is *not* recreated silently: the
    /// precondition fails closed rather than resurrecting deleted content.
    #[tokio::test]
    async fn expected_version_fails_closed_when_the_file_vanished() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(Some(dir.path().to_path_buf()));

        let error = tool
            .call(
                &serde_json::json!({
                    "path": "gone.rs",
                    "content": "const A: u8 = 2;\n",
                    "expected_version": "deadbeef0000",
                })
                .to_string(),
            )
            .await
            .unwrap_err();
        assert!(error.contains("no longer exists"), "{error}");
        assert!(!dir.path().join("gone.rs").exists());
    }

    /// Creating a new file and overwriting without a precondition keep working
    /// exactly as before.
    #[tokio::test]
    async fn creates_and_overwrites_without_a_precondition() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(Some(dir.path().to_path_buf()));

        tool.call(r#"{"path":"new.rs","content":"const A: u8 = 1;\n"}"#)
            .await
            .expect("create without precondition");
        tool.call(r#"{"path":"new.rs","content":"const A: u8 = 3;\n"}"#)
            .await
            .expect("overwrite without precondition");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.rs")).unwrap(),
            "const A: u8 = 3;\n"
        );
    }

    /// The version a structural query reports is the value this parameter
    /// accepts — the two halves of the freshness contract must agree.
    #[tokio::test]
    async fn accepts_the_version_a_code_query_reports() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, "pub fn alpha() {}\n").unwrap();

        let query = crate::tools::CodeQueryTool::new(Some(dir.path().to_path_buf()));
        let outline = query
            .call(r#"{"mode":"outline","path":"lib.rs"}"#)
            .await
            .unwrap();
        let version = outline
            .split("(version ")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .expect("outline reports a version")
            .to_string();

        let write = WriteFileTool::new(Some(dir.path().to_path_buf()));
        write
            .call(
                &serde_json::json!({
                    "path": "lib.rs",
                    "content": "pub fn beta() {}\n",
                    "expected_version": version,
                })
                .to_string(),
            )
            .await
            .expect("the version a code_query reported must be accepted by a write");
        assert!(std::fs::read_to_string(&file).unwrap().contains("beta"));
    }
}
