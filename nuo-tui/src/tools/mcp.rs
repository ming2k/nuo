//! Model Context Protocol (MCP) tool presenter.

use super::{ArgLayout, ResultKind, SemanticLine, ToolPresenter, ToolView};

pub struct McpPresenter;

pub fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let clean = name.strip_prefix("mcp__").unwrap_or(name);
    clean.split_once("__")
}

pub fn extract_prominent_arg(args: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    for key in &["path", "file", "url", "uri", "query", "command", "prompt", "text", "name", "id"] {
        if let Some(val) = args.get(*key) {
            if let Some(s) = val.as_str() {
                return Some(format!("{s:?}"));
            }
        }
    }
    for (k, v) in args {
        if let Some(s) = v.as_str() {
            return Some(format!("{k}: {s:?}"));
        } else if let Some(n) = v.as_i64() {
            return Some(format!("{k}: {n}"));
        } else if let Some(b) = v.as_bool() {
            return Some(format!("{k}: {b}"));
        }
    }
    if !args.is_empty() {
        Some(format!("{} args", args.len()))
    } else {
        None
    }
}

impl ToolPresenter for McpPresenter {
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        let (server, tool) = parse_mcp_tool_name(view.name).unwrap_or(("", view.name));
        let mut line = SemanticLine::new();
        line = line.push_fixed("⚡ ");
        if !server.is_empty() {
            line = line.push_fixed(server).push_dim(" · ").push_fixed(tool);
        } else {
            line = line.push_fixed(tool);
        }
        if let Some(prominent) = extract_prominent_arg(view.args) {
            line = line.push_fixed(" ").push_dim(prominent);
        }
        line
    }

    fn summary(&self, view: &ToolView) -> String {
        self.render_summary(view).to_plain_text()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Mcp
    }

    fn arg_layout(&self) -> ArgLayout {
        ArgLayout::KeyValue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_mcp_tool_name_splits_server_and_tool() {
        assert_eq!(parse_mcp_tool_name("mcp__github__create_issue"), Some(("github", "create_issue")));
        assert_eq!(parse_mcp_tool_name("github__create_issue"), Some(("github", "create_issue")));
        assert_eq!(parse_mcp_tool_name("plain_tool"), None);
    }

    #[test]
    fn extract_prominent_arg_prioritizes_high_signal_keys() {
        let mut map = serde_json::Map::new();
        map.insert("path".to_string(), json!("/foo/bar.rs"));
        map.insert("extra".to_string(), json!("ignore"));
        assert_eq!(extract_prominent_arg(&map), Some("\"/foo/bar.rs\"".to_string()));
    }

    #[test]
    fn extract_prominent_arg_falls_back_to_scalars_or_count() {
        let mut map = serde_json::Map::new();
        map.insert("count".to_string(), json!(42));
        assert_eq!(extract_prominent_arg(&map), Some("count: 42".to_string()));
    }

    #[test]
    fn mcp_presenter_renders_structured_summary() {
        let mut map = serde_json::Map::new();
        map.insert("query".to_string(), json!("hello"));
        let view = ToolView {
            name: "mcp__search__web",
            args: &map,
            profile: None,
            workspace_root: None,
        };
        assert_eq!(McpPresenter.summary(&view), "⚡ search · web \"hello\"");
    }
}
