//! Shared path-scope and ignore semantics for file discovery and text search.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use nuo_protocol::ExecutionEnvironment;

pub(crate) const DEFAULT_SEARCH_LIMIT: usize = 200;
pub(crate) const MAX_SEARCH_LIMIT: usize = 1000;

/// Resolve a model-supplied workspace-relative search root against the execution environment.
pub(crate) fn resolve_search_root(
    env: &dyn ExecutionEnvironment,
    path: &str,
) -> Result<PathBuf, String> {
    env.resolve_path(path).map_err(|err| match err {
        nuo_protocol::execution::FsError::PermissionDenied(_) => format!(
            "Search path is outside the admitted workspace roots (admitted: {})",
            admitted_roots_summary(env.workspace_root(), &env.additional_roots())
        ),
        other => other.to_string(),
    })
}

/// Human-readable admitted set for denial messages — names every root so a
/// cross-project path miss is diagnosable instead of a bare refusal. The
/// implicit temp admission is summarized as one `$TMPDIR` token rather than
/// spelling out platform-specific paths.
pub(crate) fn admitted_roots_summary(workspace: &Path, additional_roots: &[PathBuf]) -> String {
    std::iter::once(workspace.display().to_string())
        .chain(additional_roots.iter().map(|r| r.display().to_string()))
        .chain(std::iter::once("$TMPDIR".to_string()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Clamp a caller-selected result limit to the public tool contract.
pub(crate) fn search_limit(requested: Option<u64>) -> Result<usize, String> {
    let requested = requested.unwrap_or(DEFAULT_SEARCH_LIMIT as u64);
    if requested == 0 {
        return Err("Search limit must be at least 1".to_string());
    }
    Ok(requested.min(MAX_SEARCH_LIMIT as u64) as usize)
}

/// Extract the common search root argument for scheduler access declarations.
pub(crate) fn search_path_argument(arguments: &serde_json::Value) -> String {
    arguments
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| ".".to_string())
}

/// Build a ripgrep-style walker using the shared [`nuo_tool_fs::walker`] engine.
pub(crate) fn build_file_walker(
    root: &Path,
    exclude: &[String],
    max_depth: Option<usize>,
) -> Result<WalkBuilder, String> {
    nuo_tool_fs::walker::build_file_walker(root, exclude, max_depth, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_limits_and_scoped_paths() {
        assert_eq!(search_limit(None).unwrap(), DEFAULT_SEARCH_LIMIT);
        assert_eq!(search_limit(Some(50_000)).unwrap(), MAX_SEARCH_LIMIT);
        assert!(search_limit(Some(0)).is_err());

        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let sibling = tmp.path().join("sibling");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let env = crate::helpers::SimpleExecutionEnvironment::new(workspace.clone());
        assert!(resolve_search_root(&env, "src").is_ok());
        assert_eq!(
            search_path_argument(&serde_json::json!({"path":"src"})),
            "src"
        );
        assert_eq!(search_path_argument(&serde_json::json!({})), ".");
    }

    #[test]
    fn validates_override_patterns_before_traversal() {
        assert!(
            nuo_tool_fs::walker::PathFilter::compile(Path::new("."), &["*.rs".into()], &[]).is_ok()
        );
        assert!(
            nuo_tool_fs::walker::PathFilter::compile(Path::new("."), &["[broken".into()], &[])
                .is_err()
        );
        assert!(
            nuo_tool_fs::walker::PathFilter::compile(Path::new("."), &["!*.rs".into()], &[])
                .is_err()
        );
        assert!(
            nuo_tool_fs::walker::PathFilter::compile(Path::new("."), &["../*.rs".into()], &[])
                .is_err()
        );
    }
}
