//! The `todo` tool (ADR-0215): one tool, two mutually exclusive modes.
//!
//! It implements the core [`Tool`] contract and mutates an injected
//! [`TodoToolContext`]. The agent owns the underlying state; this crate owns
//! the concrete tool implementation.
//!
//! - `items` present → full-replace reconcile (identity preserved).
//! - `key` + `status` present → surgical status update without re-sending
//!   the list.
//! - Both or neither → a guiding error naming the two modes.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use nuo_wire::{MAX_TODOS, TodoList, TodoStatus, Tool};

const TODO_DESCRIPTION: &str = "Update the task list. Two modes, mutually exclusive: \
(1) full replace — provide `items`, the full array ({content, status: \
'pending'|'in_progress'|'completed'|'cancelled'}), at most one item in_progress (extras are \
auto-demoted); (2) surgical update — provide `key` (1-based position or case-insensitive \
content substring) and `status` to mark progress without re-sending the whole list. Prefer \
the surgical mode when only marking progress on a single step.";

/// Shared handle injected into the todo tool so it mutates the live state
/// owned by the agent that dispatches it.
#[derive(Clone)]
pub struct TodoToolContext {
    todos: Arc<Mutex<TodoList>>,
    round_counter: Arc<Mutex<u64>>,
}

impl TodoToolContext {
    pub fn new(todos: Arc<Mutex<TodoList>>, round_counter: Arc<Mutex<u64>>) -> Self {
        Self {
            todos,
            round_counter,
        }
    }

    pub fn todos(&self) -> TodoList {
        self.todos.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_todos(&self, list: TodoList) {
        let mut guard = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        *guard = list;
    }

    pub fn current_round(&self) -> u64 {
        *self.round_counter.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The todo tool (ADR-0215). One model-visible tool owning the whole task-list
/// lifecycle: full-replace reconcile *and* surgical status edit, selected by
/// which parameters the call carries.
pub struct TodoTool {
    context: TodoToolContext,
}

impl TodoTool {
    pub fn new(context: TodoToolContext) -> Self {
        Self { context }
    }

    /// Full-replace mode (`items` present): the model sends the desired list;
    /// the tool reconciles it against the current list preserving identity
    /// (see `TodoList::reconcile`). This is the robust interface: the model
    /// never has to track ids.
    async fn replace(&self, arguments: &str) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct Arguments {
            items: Vec<TodoArgs>,
        }
        #[derive(serde::Deserialize)]
        struct TodoArgs {
            content: String,
            status: String,
        }

        let parsed: Arguments =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;
        if parsed.items.len() > MAX_TODOS {
            return Err(format!("Todo list is limited to {MAX_TODOS} items."));
        }

        let mut desired: Vec<(String, TodoStatus)> = Vec::with_capacity(parsed.items.len());
        for entry in parsed.items {
            if entry.content.trim().is_empty() {
                return Err("Todo item content cannot be empty.".to_string());
            }
            let status = TodoStatus::parse(&entry.status).ok_or_else(|| {
                format!(
                    "Unknown todo status '{}'. Use pending / in_progress / completed / cancelled.",
                    entry.status
                )
            })?;
            desired.push((entry.content, status));
        }

        // "At most one in_progress" is reconciled here rather than rejected.
        // The model's full-replace list frequently carries a leftover
        // in_progress from the previous turn (it forgot to flip it back); a
        // hard error there wastes a turn and shows the user a red message for
        // what is an obvious bookkeeping slip. Instead, keep the *last*
        // declared in_progress (the agent's current focus) and demote every
        // earlier one to pending. The rendered list still satisfies the ≤ one
        // in_progress invariant the panel relies on; the model is told via the
        // returned note so it can correct course.
        let in_progress_idx: Vec<usize> = desired
            .iter()
            .enumerate()
            .filter(|(_, (_, s))| *s == TodoStatus::InProgress)
            .map(|(i, _)| i)
            .collect();
        let mut demoted = 0;
        if in_progress_idx.len() > 1 {
            for &i in &in_progress_idx[..in_progress_idx.len() - 1] {
                desired[i].1 = TodoStatus::Pending;
                demoted += 1;
            }
        }

        let now = nuo_wire::todos::unix_now();
        let turn = self.context.current_round();
        let mut list = self.context.todos();
        let prev_ids: HashSet<u64> = list.items.iter().map(|i| i.id.0).collect();
        let prev_contents: HashSet<String> = list.items.iter().map(|i| i.content.clone()).collect();
        let changed = list.reconcile(&desired, now, turn);
        if changed {
            self.context.set_todos(list);
        }
        let current = self.context.todos();
        let rendered = current.render();

        let new_contents: Vec<&str> = current
            .items
            .iter()
            .filter(|i| !prev_ids.contains(&i.id.0))
            .map(|i| i.content.as_str())
            .filter(|c| !prev_contents.contains(*c))
            .collect();
        let identity_note = if new_contents.is_empty() {
            String::new()
        } else if new_contents.len() == prev_contents.len()
            && !prev_contents.is_empty()
            && current.items.len() == prev_contents.len()
        {
            format!(
                "\nNote: {} item(s) were rewritten with new identity (content changed). \
                 Their created_at reset.",
                new_contents.len()
            )
        } else {
            format!("\nNote: {} new item(s) created.", new_contents.len())
        };
        // Surface the in_progress reconciliation so the model sees its
        // bookkeeping slip was absorbed, not silently lost.
        let demotion_note = if demoted > 0 {
            format!(
                "\nNote: {demoted} item(s) had in_progress demoted to pending — at most one \
                 item may be in_progress at a time. The most recently declared one was kept."
            )
        } else {
            String::new()
        };
        Ok(format!(
            "Todo list updated:\n{rendered}{identity_note}{demotion_note}"
        ))
    }

    /// Surgical mode (`key` + `status` present): change the status of items
    /// matched by position or content substring, leaving everything else
    /// untouched. Lets the model mark a step done without re-emitting the
    /// entire list.
    async fn update(&self, value: &Value) -> Result<String, String> {
        let key = value
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'key'")?;
        let status_str = value
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'status'")?;
        let status = TodoStatus::parse(status_str).ok_or_else(|| {
            format!(
                "Unknown status '{status_str}'. Use pending / in_progress / completed / cancelled."
            )
        })?;

        let now = nuo_wire::todos::unix_now();
        let turn = self.context.current_round();
        let mut list = self.context.todos();
        if list.is_empty() {
            return Ok(
                "No todos to update. Provide `items` (full list) to create the list first."
                    .to_string(),
            );
        }
        let changed = list.update(key, status, now, turn);
        if changed == 0 {
            return Ok(format!(
                "No todo matched '{key}'. Current todos:\n{}",
                list.render()
            ));
        }
        if status == TodoStatus::InProgress {
            let in_progress: Vec<&str> = list
                .items
                .iter()
                .filter(|i| i.status == TodoStatus::InProgress)
                .map(|i| i.content.as_str())
                .collect();
            if in_progress.len() > 1 {
                return Ok(format!(
                    "Cannot set '{key}' to in_progress — multiple items are now in_progress \
                     ({}). At most one todo item may be in_progress at a time. \
                     Mark the others completed or cancelled first.\n\nCurrent todos:\n{}",
                    in_progress.join(", "),
                    list.render()
                ));
            }
        }
        self.context.set_todos(list);
        let rendered = self.context.todos().render();
        Ok(format!(
            "Updated {changed} item(s) to {status_str}.\n{rendered}",
            status_str = status_str
        ))
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        TODO_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "maxItems": MAX_TODOS,
                    "description": "Full-replace mode: the complete desired list. Mutually exclusive with key/status.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"]
                            }
                        },
                        "required": ["content", "status"],
                        "additionalProperties": false
                    }
                },
                "key": {
                    "type": "string",
                    "description": "Surgical mode: 1-based position or case-insensitive content substring. Mutually exclusive with items."
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "cancelled"],
                    "description": "Surgical mode: new status for the matched item(s)"
                }
            },
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let value: Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;
        let has_items = value.get("items").is_some();
        let has_key = value.get("key").is_some();

        match (has_items, has_key) {
            (true, false) => self.replace(arguments).await,
            (false, true) => self.update(&value).await,
            (true, true) => Err(
                "Provide either `items` (full-replace mode) or `key` + `status` (surgical \
                 update mode) — not both."
                    .to_string(),
            ),
            (false, false) => Err(
                "Provide either `items` (the full list, to replace it) or `key` + `status` \
                 (to surgically update matched items)."
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use nuo_wire::TodoList;

    fn ctx() -> (TodoToolContext, Arc<Mutex<TodoList>>) {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let turn = Arc::new(Mutex::new(5u64));
        (TodoToolContext::new(Arc::clone(&list), turn), list)
    }

    #[tokio::test]
    async fn todo_tool_reconciles_and_preserves_identity() {
        let (context, list) = ctx();
        let tool = TodoTool::new(context.clone());
        tool.call(r#"{"items":[{"content":"design","status":"pending"}]}"#)
            .await
            .unwrap();
        let first_id = list.lock().unwrap().items[0].id;

        tool.call(
            r#"{"items":[{"content":"design","status":"completed"},
                         {"content":"implement","status":"in_progress"}]}"#,
        )
        .await
        .unwrap();
        let guard = list.lock().unwrap();
        assert_eq!(guard.items[0].id, first_id);
        assert_eq!(guard.items[0].status, TodoStatus::Completed);
        assert_eq!(guard.items[1].content, "implement");
        assert_eq!(guard.updated_at_round, 5);
    }

    #[tokio::test]
    async fn todo_tool_auto_demotes_extra_in_progress() {
        // Full-replace must never reject on >1 in_progress — that would waste a
        // turn on a routine bookkeeping slip. Instead it keeps the last-declared
        // in_progress and demotes the earlier ones to pending, then commits.
        let (context, list) = ctx();
        let tool = TodoTool::new(context.clone());
        let body = tool
            .call(
                r#"{"items":[
                    {"content":"a","status":"in_progress"},
                    {"content":"b","status":"in_progress"}
                ]}"#,
            )
            .await
            .unwrap();
        // Earlier in_progress ("a") demoted to pending; later one ("b") kept.
        let guard = list.lock().unwrap();
        assert_eq!(guard.items[0].status, TodoStatus::Pending);
        assert_eq!(guard.items[1].status, TodoStatus::InProgress);
        // The model is told the demotion happened.
        assert!(body.contains("demoted"));
    }

    #[tokio::test]
    async fn todo_tool_surgical_update_matches_by_position() {
        let (context, list) = ctx();
        let tool = TodoTool::new(context.clone());
        tool.call(
            r#"{"items":[
                {"content":"a","status":"pending"},
                {"content":"b","status":"pending"}
            ]}"#,
        )
        .await
        .unwrap();
        tool.call(r#"{"key":"2","status":"completed"}"#)
            .await
            .unwrap();
        let guard = list.lock().unwrap();
        assert_eq!(guard.items[1].status, TodoStatus::Completed);
        assert_eq!(guard.items[0].status, TodoStatus::Pending);
    }

    #[tokio::test]
    async fn todo_tool_surgical_update_matches_by_content() {
        let (context, list) = ctx();
        let tool = TodoTool::new(context.clone());
        tool.call(r#"{"items":[{"content":"Write tests","status":"pending"}]}"#)
            .await
            .unwrap();
        let body = tool
            .call(r#"{"key":"tests","status":"completed"}"#)
            .await
            .unwrap();
        assert!(body.contains("Updated 1"));
        assert_eq!(list.lock().unwrap().items[0].status, TodoStatus::Completed);
    }

    #[tokio::test]
    async fn todo_tool_surgical_update_empty_list_returns_hint() {
        let (context, _) = ctx();
        let tool = TodoTool::new(context);
        let body = tool.call(r#"{"key":"1","status":"done"}"#).await.unwrap();
        assert!(body.contains("No todos"));
    }

    #[tokio::test]
    async fn todo_tool_surgical_update_rejects_second_in_progress() {
        let (context, list) = ctx();
        let tool = TodoTool::new(context.clone());
        tool.call(
            r#"{"items":[
                {"content":"a","status":"in_progress"},
                {"content":"b","status":"pending"}
            ]}"#,
        )
        .await
        .unwrap();
        let body = tool
            .call(r#"{"key":"b","status":"in_progress"}"#)
            .await
            .unwrap();
        assert!(body.contains("in_progress"));
        // The second item must NOT have been committed.
        assert_eq!(list.lock().unwrap().items[1].status, TodoStatus::Pending);
    }

    #[tokio::test]
    async fn todo_tool_rejects_both_modes() {
        let (context, _) = ctx();
        let tool = TodoTool::new(context);
        let body = tool
            .call(
                r#"{"items":[{"content":"a","status":"pending"}],
                   "key":"1","status":"completed"}"#,
            )
            .await
            .unwrap_err();
        assert!(body.contains("not both"));
    }

    #[tokio::test]
    async fn todo_tool_rejects_neither_mode() {
        let (context, _) = ctx();
        let tool = TodoTool::new(context);
        let body = tool.call(r#"{}"#).await.unwrap_err();
        assert!(body.contains("either"));
    }
}
