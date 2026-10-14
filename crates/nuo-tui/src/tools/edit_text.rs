//! Presenters for `edit_text` and `write_file`.
//!
//! `edit_text` renders a red/green line diff (old vs new) in the expanded body.
//! `write_file` renders a full-file insertion diff (all added lines in green,
//! Git/GitHub style) in the expanded body. Both default to expanded and show a
//! line-count suffix in the collapsed summary.

use super::diff::line_diff_counts;
use super::{ResultKind, ToolPresenter, ToolView};
use crate::components::inline_layout::SemanticLine;
use crate::components::path::PathView;

pub struct EditPresenter;

impl ToolPresenter for EditPresenter {
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        let raw_path = view
            .str("path")
            .or_else(|| view.str("file_path"))
            .or_else(|| view.str("filename"))
            .or_else(|| view.str("file"))
            .filter(|p| !p.trim().is_empty());
        let Some(raw_path) = raw_path else {
            return SemanticLine::plain("Edit text");
        };
        let mut line = SemanticLine::new()
            .push_fixed("Edit ")
            .push_path(PathView::from_str(raw_path).maybe_base_dir(view.workspace_root));
        let old = view
            .str("old_string")
            .or_else(|| view.str("old_str"))
            .or_else(|| view.str("old_text"))
            .or_else(|| view.str("old"));
        let new = view
            .str("new_string")
            .or_else(|| view.str("new_str"))
            .or_else(|| view.str("new_text"))
            .or_else(|| view.str("new"));
        if let (Some(old), Some(new)) = (old, new) {
            let (added, removed) = line_diff_counts(old, new);
            line = line.push_fixed(format!(" +{} -{}", added, removed));
        }
        line
    }

    fn summary(&self, view: &ToolView) -> String {
        self.render_summary(view).to_plain_text()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Diff
    }

    fn default_expanded(&self) -> bool {
        true
    }
}

pub struct WritePresenter;

impl ToolPresenter for WritePresenter {
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        let raw_path = view
            .str("path")
            .or_else(|| view.str("file_path"))
            .or_else(|| view.str("filename"))
            .or_else(|| view.str("file"))
            .filter(|p| !p.trim().is_empty());
        let Some(raw_path) = raw_path else {
            return SemanticLine::plain("Write file");
        };
        let mut line = SemanticLine::new()
            .push_fixed("Write ")
            .push_path(PathView::from_str(raw_path).maybe_base_dir(view.workspace_root));
        let content = view
            .str("content")
            .or_else(|| view.str("new_string"))
            .or_else(|| view.str("text"));
        if let Some(content) = content {
            let (added, _) = line_diff_counts("", content);
            line = line.push_fixed(format!(" +{}", added));
        }
        line
    }

    fn summary(&self, view: &ToolView) -> String {
        self.render_summary(view).to_plain_text()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Diff
    }

    fn default_expanded(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_presenter_defaults_and_diff_kind() {
        let presenter = EditPresenter;
        assert_eq!(presenter.result_kind(), ResultKind::Diff);
        assert!(presenter.default_expanded());
    }

    #[test]
    fn write_presenter_defaults_and_diff_kind() {
        let presenter = WritePresenter;
        assert_eq!(presenter.result_kind(), ResultKind::Diff);
        assert!(presenter.default_expanded());
    }
}
