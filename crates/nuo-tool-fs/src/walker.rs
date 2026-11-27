//! Shared workspace traversal and gitignore-style glob matching for
//! filesystem discovery (`find_files`), lexical search (`search_text`), and
//! structural queries (`code_query`).

use std::path::{Component, Path};

use ignore::overrides::{Override as OverrideMatcher, OverrideBuilder};
use ignore::{Match, WalkBuilder};

/// Directories that are almost never interesting to search or list and can be
/// enormous: VCS metadata, dependency trees, and build output.
pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "target",
    "__pycache__",
    ".next",
    ".nuxt",
    ".turbo",
    "dist",
    "build",
    ".venv",
    "venv",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    "coverage",
    ".gradle",
    ".idea",
    ".vscode",
    "proc",
    "sys",
    "dev",
    "run",
];

/// OS metadata files pruned by default during workspace walks.
pub const IGNORED_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

/// Compiled include/exclude glob filter rooted at a search directory.
#[derive(Clone)]
pub struct PathFilter {
    matcher: OverrideMatcher,
    has_includes: bool,
}

impl PathFilter {
    /// Compile `include` and `exclude` patterns into a gitignore-semantics filter.
    ///
    /// Returns `Ok(None)` when both lists are empty (everything matches).
    pub fn compile(
        root: &Path,
        include: &[String],
        exclude: &[String],
    ) -> Result<Option<Self>, String> {
        if include.is_empty() && exclude.is_empty() {
            return Ok(None);
        }
        let mut builder = OverrideBuilder::new(root);
        for pattern in include {
            validate_override_pattern(pattern)?;
            for expanded in expand_braces(pattern)? {
                builder
                    .add(&expanded)
                    .map_err(|error| format!("Invalid include glob '{pattern}': {error}"))?;
            }
        }
        for pattern in exclude {
            validate_override_pattern(pattern)?;
            for expanded in expand_braces(pattern)? {
                builder
                    .add(&format!("!{expanded}"))
                    .map_err(|error| format!("Invalid exclude glob '{pattern}': {error}"))?;
            }
        }
        let matcher = builder
            .build()
            .map_err(|error| format!("Invalid file glob: {error}"))?;
        Ok(Some(Self {
            matcher,
            has_includes: !include.is_empty(),
        }))
    }

    /// Whether a search-root-relative path passes the include and exclude rules.
    pub fn matches(&self, relative: &Path, is_dir: bool) -> bool {
        match self.matcher.matched(relative, is_dir) {
            Match::Whitelist(_) => true,
            Match::Ignore(_) => false,
            Match::None => !self.has_includes,
        }
    }
}

/// Build an `ignore::WalkBuilder` rooted at `root`.
///
/// When `include_ignored` is `false`, `.gitignore`/`.ignore` filters and
/// [`IGNORED_DIRS`]/[`IGNORED_FILES`] pruning apply. When `include_ignored` is
/// `true`, user ignore files and default build/dependency exclusions are lifted
/// while still skipping nested `.git` object trees unless `root` itself names
/// `.git`.
pub fn build_file_walker(
    root: &Path,
    exclude: &[String],
    max_depth: Option<usize>,
    include_ignored: bool,
) -> Result<WalkBuilder, String> {
    // Eagerly validate exclude globs so invalid syntax fails before traversal.
    let _ = PathFilter::compile(root, &[], exclude)?;

    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(!include_ignored)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right))
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let Some(name) = entry.file_name().to_str() else {
                return true;
            };
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
            if include_ignored {
                return !(is_dir && name == ".git");
            }
            if is_dir {
                !IGNORED_DIRS.contains(&name)
            } else {
                !IGNORED_FILES.contains(&name)
            }
        });
    if let Some(depth) = max_depth {
        builder.max_depth(Some(depth));
    }
    Ok(builder)
}

/// Expand `{a,b}` brace alternations in a glob pattern into flat patterns.
///
/// Supports nested braces (e.g. `src/{a,{b,c}}/*.rs`). Unbalanced braces fail
/// cleanly rather than silently mis-matching.
pub fn expand_braces(pattern: &str) -> Result<Vec<String>, String> {
    let mut depth = 0usize;
    let mut open_idx = None;
    let mut comma_positions = Vec::new();
    let mut close_idx = None;

    for (idx, ch) in pattern.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    open_idx = Some(idx);
                }
                depth += 1;
            }
            ',' if depth == 1 => {
                comma_positions.push(idx);
            }
            '}' => {
                if depth == 0 {
                    return Err(format!("Unmatched '}}' in glob pattern '{pattern}'"));
                }
                depth -= 1;
                if depth == 0 {
                    close_idx = Some(idx);
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(format!("Unclosed '{{' in glob pattern '{pattern}'"));
    }

    let (Some(start), Some(end)) = (open_idx, close_idx) else {
        return Ok(vec![pattern.to_string()]);
    };

    let prefix = &pattern[..start];
    let suffix = &pattern[end + 1..];
    let inner = &pattern[start + 1..end];

    let mut branches = Vec::new();
    let mut branch_start = 0usize;
    for comma in comma_positions {
        let rel = comma - (start + 1);
        branches.push(&inner[branch_start..rel]);
        branch_start = rel + 1;
    }
    branches.push(&inner[branch_start..]);

    let mut expanded = Vec::new();
    for branch in branches {
        let combined = format!("{prefix}{branch}{suffix}");
        expanded.extend(expand_braces(&combined)?);
    }
    Ok(expanded)
}

/// Validate a single glob pattern against workspace-escape or invalid negation syntax.
pub fn validate_override_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("File glob must not be empty".to_string());
    }
    if pattern.starts_with('!') {
        return Err(format!(
            "File glob '{pattern}' must not start with '!'; use the exclude field"
        ));
    }
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
    fn brace_expansion_handles_simple_and_nested_groups() {
        assert_eq!(
            expand_braces("**/*.{ts,tsx}").unwrap(),
            vec!["**/*.ts", "**/*.tsx"]
        );
        assert_eq!(
            expand_braces("crates/{a,{b,c}}/*.rs").unwrap(),
            vec!["crates/a/*.rs", "crates/b/*.rs", "crates/c/*.rs"]
        );
        assert!(expand_braces("*.{ts,tsx").is_err());
        assert!(expand_braces("*.ts}").is_err());
    }

    #[test]
    fn path_filter_supports_braces_and_exclusions() {
        let filter = PathFilter::compile(
            Path::new("."),
            &["*.{rs,toml}".into()],
            &["**/fixtures/**".into()],
        )
        .unwrap()
        .unwrap();

        assert!(filter.matches(Path::new("src/lib.rs"), false));
        assert!(filter.matches(Path::new("Cargo.toml"), false));
        assert!(!filter.matches(Path::new("README.md"), false));
        assert!(!filter.matches(Path::new("src/fixtures/sample.rs"), false));
    }

    #[test]
    fn path_filter_exclude_only_allows_everything_else() {
        let filter = PathFilter::compile(Path::new("."), &[], &["**/fixtures/**".into()])
            .unwrap()
            .unwrap();

        assert!(filter.matches(Path::new("src/lib.rs"), false));
        assert!(!filter.matches(Path::new("src/fixtures/sample.rs"), false));
    }

    #[test]
    fn validates_invalid_override_patterns() {
        assert!(PathFilter::compile(Path::new("."), &["".into()], &[]).is_err());
        assert!(PathFilter::compile(Path::new("."), &["!*.rs".into()], &[]).is_err());
        assert!(PathFilter::compile(Path::new("."), &["../*.rs".into()], &[]).is_err());
        assert!(PathFilter::compile(Path::new("."), &["[broken".into()], &[]).is_err());
    }
}
