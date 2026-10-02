use nuo_contracts::{CausalNode, Message, NodePayload, Role};
use serde_json::Value;
use std::collections::BTreeSet;

/// File operations (read vs modified) tracked across a sequence of messages or branch.
///
/// Under ADR-0275 §1, attempted, failed, and confirmed effects are tracked
/// separately; an unconfirmed intent never masquerades as a confirmed modification.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileOperations {
    pub read: BTreeSet<String>,
    pub modified: BTreeSet<String>,
    pub attempted: BTreeSet<String>,
    pub failed: BTreeSet<String>,
}

impl FileOperations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Extract file operations from tool calls in a message.
    pub fn extract_from_message(&mut self, message: &Message) {
        if message.role == Role::Assistant
            && let Some(ref calls) = message.tool_calls
        {
            for call in calls {
                let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
                let path = args.get("path").and_then(|v| v.as_str());

                match call.name.as_str() {
                    "read_text" | "read_file" | "find_files" | "list_dir" | "search_text" => {
                        if let Some(p) = path
                            && !p.is_empty()
                            && p != "."
                        {
                            self.attempted.insert(p.to_string());
                            self.read.insert(p.to_string());
                        }
                    }
                    "write_file" | "edit_text" => {
                        if let Some(p) = path
                            && !p.is_empty()
                        {
                            self.attempted.insert(p.to_string());
                            self.modified.insert(p.to_string());
                        }
                    }
                    _ => {}
                }
            }
        } else if message.role == Role::Tool
            && (message.content.starts_with("Error:")
                || message.content.starts_with("error:")
                || message.content.contains("Failed to"))
        {
            // Failed tool executions are recorded as failed (ADR-0275 §1)
            self.failed.insert(message.content.clone());
        }
    }

    /// Extract file operations from a Session IR CausalNode (ADR-0255).
    pub fn extract_from_node(&mut self, node: &CausalNode) {
        match &node.payload {
            NodePayload::Message { message } => {
                self.extract_from_message(message);
            }
            NodePayload::Compaction {
                read_files,
                modified_files,
                ..
            } => {
                for f in read_files {
                    self.read.insert(f.clone());
                }
                for f in modified_files {
                    self.modified.insert(f.clone());
                }
            }
            _ => {}
        }
    }

    /// Format file operations into a Markdown section.
    pub fn format_markdown(&self) -> String {
        if self.read.is_empty() && self.modified.is_empty() {
            return String::new();
        }

        let mut out = String::from("\n\n## File Operations\n");
        if !self.modified.is_empty() {
            out.push_str("### Modified Files\n");
            for f in &self.modified {
                out.push_str(&format!("- `{}`\n", f));
            }
        }
        if !self.read.is_empty() {
            out.push_str("### Read Files\n");
            for f in &self.read {
                out.push_str(&format!("- `{}`\n", f));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_contracts::ToolCall;

    #[test]
    fn extracts_reads_and_modifications() {
        let mut tracker = FileOperations::new();
        let mut msg1 = Message::new(Role::Assistant, "I will read a file");
        msg1.tool_calls = Some(vec![ToolCall::new(
            "call1",
            "read_text",
            r#"{"path":"src/main.rs"}"#,
        )]);
        tracker.extract_from_message(&msg1);

        let mut msg2 = Message::new(Role::Assistant, "I will edit a file");
        msg2.tool_calls = Some(vec![ToolCall::new(
            "call2",
            "edit_text",
            r#"{"path":"src/lib.rs"}"#,
        )]);
        tracker.extract_from_message(&msg2);

        assert!(tracker.read.contains("src/main.rs"));
        assert!(tracker.modified.contains("src/lib.rs"));

        let md = tracker.format_markdown();
        assert!(md.contains("`src/main.rs`"));
        assert!(md.contains("`src/lib.rs`"));
    }
}
