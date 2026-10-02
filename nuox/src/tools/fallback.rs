//! Generic presenter for unrecognized tools (e.g. MCP-provided tools).
//!
//! Unlike known tools — whose summary is a human verb phrase (`Read path`) —
//! an unknown tool has no natural verb, so the summary leads with the (cleaned)
//! tool name so the header still identifies it, optionally followed by its most
//! recognizable argument. The expanded body shows all arguments as `key: value`
//! since the header can't spell them out.

use super::{ArgLayout, ToolPresenter, ToolView};

pub struct FallbackPresenter;

impl ToolPresenter for FallbackPresenter {
    fn summary(&self, view: &ToolView) -> String {
        // When this step is a subagent run that announced its role
        // (`plan` / `verify` / …), lead with the role so the header
        // distinguishes subagent kinds. Falls back to the cleaned tool name
        // for ordinary unrecognized tools (MCP, etc.), where `profile` is None.
        let name = prettify_tool_name(view.name);
        let lead = view.profile.unwrap_or(&name).to_string();
        match ["path", "pattern", "command", "name", "url", "query"]
            .iter()
            .find_map(|key| view.str(key))
        {
            Some(value) => format!("{} {}", lead, value),
            None => lead,
        }
    }

    fn arg_layout(&self) -> ArgLayout {
        ArgLayout::KeyValue
    }
}

/// Turn a raw tool id into something readable for the header: strip the `mcp__`
/// prefix and render the remaining `server__tool` segments as `server / tool`.
fn prettify_tool_name(name: &str) -> String {
    name.strip_prefix("mcp__")
        .unwrap_or(name)
        .replace("__", " / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prettifies_mcp_names() {
        assert_eq!(
            prettify_tool_name("mcp__github__create_issue"),
            "github / create_issue"
        );
        assert_eq!(prettify_tool_name("custom_tool"), "custom_tool");
    }
}
