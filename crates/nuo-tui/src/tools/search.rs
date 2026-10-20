//! Presenters for file discovery, text search, and shallow directory listing.

use super::{ResultKind, ToolPresenter, ToolView, truncate};
use crate::components::inline_layout::SemanticLine;
use crate::components::path::PathView;

pub struct SearchTextPresenter;

impl ToolPresenter for SearchTextPresenter {
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        let query = view.str("query").unwrap_or("...");
        let path = view.str("path").unwrap_or(".");
        let formatted_query = format!("\"{}\"", query);
        if path == "." {
            SemanticLine::new()
                .push_fixed("Search ")
                .push_flexible(formatted_query)
                .push_fixed(" in .")
        } else {
            SemanticLine::new()
                .push_fixed("Search ")
                .push_flexible(formatted_query)
                .push_fixed(" in ")
                .push_path(PathView::from_str(path).maybe_base_dir(view.workspace_root))
        }
    }

    fn summary(&self, view: &ToolView) -> String {
        self.render_summary(view).to_plain_text()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Matches
    }
}

pub struct FindFilesPresenter;

impl ToolPresenter for FindFilesPresenter {
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        let raw_patterns: Vec<String> = match view
            .args
            .get("pattern")
            .or_else(|| view.args.get("name"))
            .or_else(|| view.args.get("glob"))
            .or_else(|| view.args.get("query"))
        {
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => vec![s.clone()],
            _ => match view
                .args
                .get("patterns")
                .or_else(|| view.args.get("include"))
                .or_else(|| view.args.get("globs"))
            {
                Some(serde_json::Value::String(s)) if !s.trim().is_empty() => vec![s.clone()],
                Some(serde_json::Value::Array(patterns)) => patterns
                    .iter()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .filter(|s| !s.trim().is_empty())
                    .collect(),
                _ => Vec::new(),
            },
        };

        let is_all_files = raw_patterns.is_empty()
            || raw_patterns
                .iter()
                .all(|p| p == "*" || p == "**/*" || p == "**");

        let selection = if is_all_files {
            "all files".to_string()
        } else {
            match raw_patterns.as_slice() {
                [] => "all files".to_string(),
                [pattern] => truncate(pattern, 48),
                [first, rest @ ..] => {
                    format!("{} +{}", truncate(first, 36), rest.len())
                }
            }
        };

        let raw_path = view.str("path").unwrap_or(".");
        let path = if raw_path.is_empty() { "." } else { raw_path };
        if path == "." {
            SemanticLine::new()
                .push_fixed("Find ")
                .push_flexible(selection)
                .push_fixed(" in .")
        } else {
            SemanticLine::new()
                .push_fixed("Find ")
                .push_flexible(selection)
                .push_fixed(" in ")
                .push_path(PathView::from_str(path).maybe_base_dir(view.workspace_root))
        }
    }

    fn summary(&self, view: &ToolView) -> String {
        self.render_summary(view).to_plain_text()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Listing
    }
}

pub struct ListDirPresenter;

impl ToolPresenter for ListDirPresenter {
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        if let Some(path) = view.str("path") {
            SemanticLine::new()
                .push_fixed("List ")
                .push_path(PathView::from_str(path).maybe_base_dir(view.workspace_root))
        } else {
            SemanticLine::plain("List directory")
        }
    }

    fn summary(&self, view: &ToolView) -> String {
        self.render_summary(view).to_plain_text()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Listing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_view<'a>(name: &'a str, args: &'a serde_json::Value) -> ToolView<'a> {
        ToolView {
            name,
            args: args.as_object().expect("object"),
            profile: None,
            workspace_root: None,
        }
    }

    #[test]
    fn find_files_presenter_renders_clear_target_and_scope() {
        let presenter = FindFilesPresenter;

        // 1. Array of patterns in current directory
        let args = json!({"patterns": ["*.rs"], "path": "."});
        let view = tool_view("find_files", &args);
        assert_eq!(presenter.summary(&view), "Find *.rs in .");

        // 2. Singular pattern in subfolder
        let args = json!({"pattern": "*.rs", "path": "crates/core"});
        let view = tool_view("find_files", &args);
        assert_eq!(presenter.summary(&view), "Find *.rs in crates/core");

        // 3. Glob alias in subfolder
        let args = json!({"glob": "Cargo.toml", "path": "crates"});
        let view = tool_view("find_files", &args);
        assert_eq!(presenter.summary(&view), "Find Cargo.toml in crates");

        // 4. All files in current directory (empty patterns or omitted)
        let args = json!({"path": "."});
        let view = tool_view("find_files", &args);
        assert_eq!(presenter.summary(&view), "Find all files in .");

        let args = json!({"patterns": ["*"], "path": "."});
        let view = tool_view("find_files", &args);
        assert_eq!(presenter.summary(&view), "Find all files in .");

        // 5. All files in subfolder
        let args = json!({"path": "src"});
        let view = tool_view("find_files", &args);
        assert_eq!(presenter.summary(&view), "Find all files in src");

        // 6. Multiple patterns
        let args = json!({"patterns": ["*.rs", "*.toml", "*.md"], "path": "."});
        let view = tool_view("find_files", &args);
        assert_eq!(presenter.summary(&view), "Find *.rs +2 in .");
    }
}
