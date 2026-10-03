//! Shared path-scope and ignore semantics for file discovery and text search.

use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;
use ignore::overrides::{Override as OverrideMatcher, OverrideBuilder};
use nuo_wire::ExecutionEnvironment;

use crate::tools::helpers::IGNORED_DIRS;

pub(crate) const DEFAULT_SEARCH_LIMIT: usize = 200;
pub(crate) const MAX_SEARCH_LIMIT: usize = 1000;

/// Resolve a model-supplied workspace-relative search root against the execution environment.
pub(crate) fn resolve_search_root(
    env: &dyn ExecutionEnvironment,
    path: &str,
) -> Result<PathBuf, String> {
    env.resolve_path(path).map_err(|err| match err {
        nuo_wire::execution::FsError::PermissionDenied(_) => format!(
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
pub(crate) fn search_path_argument(arguments: &str) -> String {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| value.get("path")?.as_str().map(str::to_string))
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| ".".to_string())
}

/// Compile include globs into a matcher with gitignore semantics: slashless
/// globs match basenames at any depth, a leading slash anchors to `root`,
/// and later exclusions win. `None` matches everything.
pub(crate) fn build_include_matcher(
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<Option<OverrideMatcher>, String> {
    if include.is_empty() && exclude.is_empty() {
        return Ok(None);
    }
    let mut matcher = OverrideBuilder::new(root);
    for pattern in include {
        validate_override_pattern(pattern)?;
        matcher
            .add(pattern)
            .map_err(|error| format!("Invalid include glob '{pattern}': {error}"))?;
    }
    for pattern in exclude {
        validate_override_pattern(pattern)?;
        matcher
            .add(&format!("!{pattern}"))
            .map_err(|error| format!("Invalid exclude glob '{pattern}': {error}"))?;
    }
    matcher
        .build()
        .map(Some)
        .map_err(|error| format!("Invalid file glob: {error}"))
}

/// Does `path` (relative to the walker root) pass the include/exclude globs?
pub(crate) fn include_allows(
    matcher: &Option<OverrideMatcher>,
    has_includes: bool,
    root: &Path,
    path: &Path,
) -> bool {
    match matcher {
        None => true,
        Some(matcher) => {
            let matched = matcher.matched(path.strip_prefix(root).unwrap_or(path), path.is_dir());
            matched.is_whitelist() || (!has_includes && !matched.is_ignore())
        }
    }
}

/// Build a ripgrep-style walker that applies project ignore rules
/// (`.gitignore`, `.ignore`, `IGNORED_DIRS`) as the *first* filter. Include /
/// exclude globs are evaluated separately by the caller against each yielded
/// entry (see [`build_include_matcher`] / [`include_allows`]) so that a
/// whitelisting include glob cannot resurrect an ignored file — the walker
/// prunes it before the include pattern ever runs.
pub(crate) fn build_file_walker(
    root: &Path,
    exclude: &[String],
    max_depth: Option<usize>,
) -> Result<WalkBuilder, String> {
    // Validate globs eagerly so a bad pattern fails before any traversal,
    // keeping error behavior identical to the previous override-based path.
    let _ = build_include_matcher(root, &[], exclude)?;

    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(true)
        // Configuration commonly lives in explicit hidden paths such as
        // `.agents`; hard exclusions below still prune metadata/build trees.
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right))
        .filter_entry(|entry| {
            if entry.depth() == 0 || !entry.file_type().is_some_and(|kind| kind.is_dir()) {
                return true;
            }
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !IGNORED_DIRS.contains(&name))
        });
    if let Some(depth) = max_depth {
        builder.max_depth(Some(depth));
    }
    Ok(builder)
}

fn validate_override_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("File glob must not be empty".to_string());
    }
    if pattern.starts_with('!') {
        return Err(format!(
            "File glob '{pattern}' must not start with '!'; use the exclude field"
        ));
    }

    // A single leading slash is a supported search-root anchor, not a
    // filesystem root. Parent components are never meaningful in a scoped
    // file selector and tend to signal an accidental workspace escape.
    let relative = pattern.strip_prefix('/').unwrap_or(pattern);
    if Path::new(relative)
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(format!(
            "File glob '{pattern}' must stay relative to the search root"
        ));
    }
    Ok(())
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

        let env = crate::execution::WorkspaceExecutionEnvironment::with_additional_roots(
            workspace.clone(),
            vec![sibling.clone()],
        );
        assert!(resolve_search_root(&env, "src").is_ok());
        assert!(resolve_search_root(&env, sibling.to_str().unwrap()).is_ok());
        assert!(resolve_search_root(&env, "/opt/secret_root_outside").is_err());
        assert_eq!(search_path_argument(r#"{"path":"src"}"#), "src");
        assert_eq!(search_path_argument("{}"), ".");

        // When confinement is disabled, external paths are admitted
        env.shared_confinement().set_confined(false);
        assert!(resolve_search_root(&env, "/opt/secret_root_outside").is_ok());
    }

    #[test]
    fn validates_override_patterns_before_traversal() {
        assert!(build_include_matcher(Path::new("."), &["*.rs".into()], &[]).is_ok());
        assert!(build_include_matcher(Path::new("."), &["[broken".into()], &[]).is_err());
        assert!(build_include_matcher(Path::new("."), &["!*.rs".into()], &[]).is_err());
        assert!(build_include_matcher(Path::new("."), &["../*.rs".into()], &[]).is_err());
    }

    #[test]
    fn exclude_only_matcher_allows_unmatched_files() {
        let root = Path::new("/workspace");
        let matcher = build_include_matcher(root, &[], &["*.log".into()]).unwrap();
        assert!(include_allows(
            &matcher,
            false,
            root,
            Path::new("/workspace/src/main.rs")
        ));
        assert!(!include_allows(
            &matcher,
            false,
            root,
            Path::new("/workspace/debug.log")
        ));
    }
}
