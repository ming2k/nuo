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
        let val = view
            .args
            .get("patterns")
            .or_else(|| view.args.get("include"));
        let selection = match val {
            Some(serde_json::Value::String(s)) => truncate(s, 48),
            Some(serde_json::Value::Array(patterns)) => match patterns.as_slice() {
                [] => "files".to_string(),
                [pattern] => pattern
                    .as_str()
                    .map(|pattern| truncate(pattern, 48))
                    .unwrap_or_else(|| "files".to_string()),
                [first, rest @ ..] => first
                    .as_str()
                    .map(|pattern| format!("{} +{}", truncate(pattern, 36), rest.len()))
                    .unwrap_or_else(|| format!("{} patterns", patterns.len())),
            },
            _ => "files".to_string(),
        };
        let path = view.str("path").unwrap_or(".");
        if path == "." {
            SemanticLine::new()
                .push_fixed("Find ")
                .push_flexible(selection)
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
