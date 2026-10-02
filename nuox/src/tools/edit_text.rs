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
        let Some(raw_path) = view.str("path") else {
            return SemanticLine::plain("Edit text");
        };
        let mut line = SemanticLine::new()
            .push_fixed("Edit ")
            .push_path(PathView::from_str(raw_path).maybe_base_dir(view.workspace_root));
        if let (Some(old), Some(new)) = (view.str("old_string"), view.str("new_string")) {
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
        let Some(raw_path) = view.str("path") else {
            return SemanticLine::plain("Write file");
        };
        let mut line = SemanticLine::new()
            .push_fixed("Write ")
            .push_path(PathView::from_str(raw_path).maybe_base_dir(view.workspace_root));
        if let Some(content) = view.str("content") {
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
