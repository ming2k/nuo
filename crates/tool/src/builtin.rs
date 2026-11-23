//! Canonical built-in tool enumeration for the Nuo cognitive agent system.
//!
//! Replaces raw strings ("stringly-typed" tool identifiers) with a strict,
//! compile-time guaranteed enumeration of native tools. This eliminates silent
//! misspellings, phantom tools, and registration drift across workspace crates.

use serde::{Deserialize, Serialize};

/// Strongly-typed enumeration of all official built-in tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinTool {
    ExecuteCommand,
    Read,
    WriteFile,
    EditText,
    FindFiles,
    SearchText,
    ReadUrl,
    SearchWeb,
    AskUser,
    Todo,
    SpawnAgent,
    RecallMemory,
    CodeQuery,
    ToolSearch,
    /// Load one skill's body into the conversation (`use_skill`).
    UseSkill,
    /// List the declared skills (`list_skills`).
    ListSkills,
}

impl BuiltinTool {
    /// Array of all native built-in tools.
    pub const ALL: &'static [BuiltinTool] = &[
        BuiltinTool::ExecuteCommand,
        BuiltinTool::Read,
        BuiltinTool::WriteFile,
        BuiltinTool::EditText,
        BuiltinTool::FindFiles,
        BuiltinTool::SearchText,
        BuiltinTool::ReadUrl,
        BuiltinTool::SearchWeb,
        BuiltinTool::AskUser,
        BuiltinTool::Todo,
        BuiltinTool::SpawnAgent,
        BuiltinTool::RecallMemory,
        BuiltinTool::CodeQuery,
        BuiltinTool::ToolSearch,
        BuiltinTool::UseSkill,
        BuiltinTool::ListSkills,
    ];

    /// The canonical wire and capability name of this tool.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ExecuteCommand => "execute_command",
            Self::Read => "read",
            Self::WriteFile => "write_file",
            Self::EditText => "edit_text",
            Self::FindFiles => "find_files",
            Self::SearchText => "search_text",
            Self::ReadUrl => "read_url",
            Self::SearchWeb => "search_web",
            Self::AskUser => "ask_user",
            Self::Todo => "todo",
            Self::SpawnAgent => "spawn_agent",
            Self::RecallMemory => "recall_memory",
            Self::CodeQuery => "code_query",
            Self::ToolSearch => "tool_search",
            Self::UseSkill => "use_skill",
            Self::ListSkills => "list_skills",
        }
    }

    /// Compatibility aliases used by models trained on other tool vocabularies.
    ///
    /// Aliases are dispatch identity, not versioning: each alias resolves to
    /// exactly one canonical capability, and `ALL` is asserted unique in tests.
    pub const fn aliases(&self) -> &'static [&'static str] {
        match self {
            Self::ExecuteCommand => &["bash", "shell", "run_command", "exec"],
            Self::Read => &["read_file", "read_text", "view"],
            Self::WriteFile => &["write", "create_file"],
            Self::EditText => &["edit", "str_replace", "replace_text"],
            Self::FindFiles => &["find", "glob"],
            Self::SearchText => &["grep", "rg", "ripgrep"],
            Self::ReadUrl => &["fetch", "webfetch", "open_url"],
            Self::SearchWeb => &["web_search"],
            Self::AskUser => &["question"],
            Self::Todo => &["todo_write", "todos"],
            Self::SpawnAgent => &["task", "subagent", "delegate"],
            Self::RecallMemory => &["memory", "recall"],
            Self::CodeQuery => &["outline", "symbols", "ast"],
            Self::ToolSearch => &["search_tools", "find_tools"],
            // Skill loading has no foreign spelling: `@name` in a request is the
            // user-facing mention grammar, not a tool alias (ADR-0230).
            Self::UseSkill | Self::ListSkills => &[],
        }
    }

    /// Whether a requested tool name matches this tool (canonical or alias).
    pub fn matches(&self, requested: &str) -> bool {
        self.as_str() == requested || self.aliases().contains(&requested)
    }

    /// Resolve a canonical tool name to its `BuiltinTool` variant.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|tool| tool.matches(name))
    }
}

impl std::fmt::Display for BuiltinTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl AsRef<str> for BuiltinTool {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for BuiltinTool {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::from_name(s).ok_or_else(|| format!("unknown builtin tool: {s}"))
    }
}

impl From<BuiltinTool> for String {
    fn from(tool: BuiltinTool) -> Self {
        tool.as_str().to_string()
    }
}

impl PartialEq<str> for BuiltinTool {
    fn eq(&self, other: &str) -> bool {
        self.matches(other)
    }
}

impl PartialEq<BuiltinTool> for str {
    fn eq(&self, other: &BuiltinTool) -> bool {
        other.matches(self)
    }
}

impl PartialEq<&str> for BuiltinTool {
    fn eq(&self, other: &&str) -> bool {
        self.matches(other)
    }
}

impl PartialEq<BuiltinTool> for &str {
    fn eq(&self, other: &BuiltinTool) -> bool {
        other.matches(self)
    }
}

impl PartialEq<String> for BuiltinTool {
    fn eq(&self, other: &String) -> bool {
        self.matches(other.as_str())
    }
}

impl PartialEq<BuiltinTool> for String {
    fn eq(&self, other: &BuiltinTool) -> bool {
        other.matches(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_tool_mapping() {
        assert_eq!(BuiltinTool::ExecuteCommand.as_str(), "execute_command");
        assert!(BuiltinTool::ExecuteCommand.matches("execute_command"));
        assert!(BuiltinTool::ExecuteCommand.matches("bash"));
        assert!(!BuiltinTool::ExecuteCommand.matches("unknown_command"));

        assert_eq!(
            BuiltinTool::from_name("execute_command"),
            Some(BuiltinTool::ExecuteCommand)
        );
        assert_eq!(
            BuiltinTool::from_name("run_command"),
            Some(BuiltinTool::ExecuteCommand)
        );
        assert_eq!(BuiltinTool::from_name("unknown_command"), None);
        assert_eq!(BuiltinTool::from_name("read"), Some(BuiltinTool::Read));
        assert_eq!(BuiltinTool::from_name("read_text"), Some(BuiltinTool::Read));
        assert_eq!(BuiltinTool::from_name("read_image"), None);
        assert_eq!(BuiltinTool::from_name("non_existent"), None);
    }

    #[test]
    fn test_builtin_tool_equality_with_strings() {
        assert_eq!(BuiltinTool::ExecuteCommand, "execute_command");
        assert_eq!(BuiltinTool::ExecuteCommand, "bash");
        assert_eq!("execute_command", BuiltinTool::ExecuteCommand);
        assert_eq!("bash", BuiltinTool::ExecuteCommand);
    }
}
