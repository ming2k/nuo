//! Presenters for orchestration / meta tools that act on session state rather
//! than the filesystem: `todo`, `subagent`, `use_skill`.

use super::{ResultKind, ToolPresenter, ToolView, truncate};
use serde_json::Value;

pub struct TodoPresenter;

impl ToolPresenter for TodoPresenter {
    fn summary(&self, view: &ToolView) -> String {
        if let Some(items) = view.args.get("items").and_then(Value::as_array) {
            let total = items.len();
            if total == 0 {
                return "Clear todos".to_string();
            }
            let mut completed = 0;
            let mut in_progress = None;

            for item in items {
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                let content = item.get("content").and_then(Value::as_str).unwrap_or("");
                match status {
                    "completed" => completed += 1,
                    "in_progress" if in_progress.is_none() && !content.is_empty() => {
                        in_progress = Some(content);
                    }
                    _ => {}
                }
            }

            if let Some(active) = in_progress {
                return format!(
                    "Todo: \"{}\" ({}/{} done)",
                    truncate(active, 32),
                    completed,
                    total
                );
            } else if completed == total {
                return format!("Todos: all {} completed", total);
            } else {
                return format!("Todos ({}/{} done)", completed, total);
            }
        }

        if let Some(content) = view.str("content") {
            let status = view.str("status").unwrap_or("updated");
            return format!("Todo: \"{}\" [{}]", truncate(content, 36), status);
        }

        "Update todo list".to_string()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Checklist
    }

    fn default_expanded(&self) -> bool {
        true
    }
}

pub struct SubagentPresenter;

impl ToolPresenter for SubagentPresenter {
    fn summary(&self, view: &ToolView) -> String {
        // The role badge `[explore]` / `[plan]` is drawn by the renderer in
        // front of this summary, so the summary itself carries only the task
        // description — repeating the role here would double it up.
        view.str("description")
            .map(|desc| truncate(desc, 56).to_string())
            .unwrap_or_else(|| "Run subagent".to_string())
    }
}

pub struct UseSkillPresenter;

impl ToolPresenter for UseSkillPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("name")
            .map(|name| format!("Use skill {}", name))
            .unwrap_or_else(|| "Use skill".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::TodoPresenter;
    use crate::tools::{ResultKind, ToolPresenter, ToolView};
    use serde_json::json;

    #[test]
    fn todo_presenter_formats_progress_summary() {
        let presenter = TodoPresenter;
        assert_eq!(presenter.result_kind(), ResultKind::Checklist);
        assert!(presenter.default_expanded());

        let args = json!({
            "items": [
                { "content": "Step 1", "status": "completed" },
                { "content": "Step 2", "status": "in_progress" },
                { "content": "Step 3", "status": "pending" }
            ]
        });
        let args_map = args.as_object().unwrap();
        let view = ToolView {
            name: "write_todos",
            args: args_map,
            profile: None,
            workspace_root: None,
        };
        assert_eq!(presenter.summary(&view), "Todo: \"Step 2\" (1/3 done)");

        let args_all_done = json!({
            "items": [
                { "content": "Step 1", "status": "completed" },
                { "content": "Step 2", "status": "completed" }
            ]
        });
        let args_done_map = args_all_done.as_object().unwrap();
        let view_done = ToolView {
            name: "write_todos",
            args: args_done_map,
            profile: None,
            workspace_root: None,
        };
        assert_eq!(presenter.summary(&view_done), "Todos: all 2 completed");
    }
}
