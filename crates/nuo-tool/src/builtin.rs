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
    ReadText,
    WriteFile,
    EditText,
    ListDir,
    FindFiles,
    SearchText,
    ReadUrl,
    SearchWeb,
    ReadImage,
    AskUser,
    Todo,
    SpawnAgent,
    RecallMemory,
    CodeQuery,
    UseSkill,
    ListSkills,
}

impl BuiltinTool {
    /// Array of all native built-in tools.
    pub const ALL: &'static [BuiltinTool] = &[
        BuiltinTool::ExecuteCommand,
        BuiltinTool::ReadText,
        BuiltinTool::WriteFile,
        BuiltinTool::EditText,
        BuiltinTool::ListDir,
        BuiltinTool::FindFiles,
        BuiltinTool::SearchText,
        BuiltinTool::ReadUrl,
        BuiltinTool::SearchWeb,
        BuiltinTool::ReadImage,
        BuiltinTool::AskUser,
        BuiltinTool::Todo,
        BuiltinTool::SpawnAgent,
        BuiltinTool::RecallMemory,
        BuiltinTool::CodeQuery,
        BuiltinTool::UseSkill,
        BuiltinTool::ListSkills,
    ];

    /// The canonical wire and capability name of this tool.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ExecuteCommand => "execute_command",
            Self::ReadText => "read_text",
            Self::WriteFile => "write_file",
            Self::EditText => "edit_text",
            Self::ListDir => "list_dir",
            Self::FindFiles => "find_files",
            Self::SearchText => "search_text",
            Self::ReadUrl => "read_url",
            Self::SearchWeb => "search_web",
            Self::ReadImage => "read_image",
            Self::AskUser => "ask_user",
            Self::Todo => "todo",
            Self::SpawnAgent => "spawn_agent",
            Self::RecallMemory => "recall_memory",
            Self::CodeQuery => "code_query",
            Self::UseSkill => "use_skill",
            Self::ListSkills => "list_skills",
        }
    }

    /// Backwards-compatibility aliases (none for clean native built-ins).
    pub const fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Whether a requested tool name matches this tool.
    pub fn matches(&self, requested: &str) -> bool {
        self.as_str() == requested
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
        self.matches(*other)
    }
}

impl PartialEq<BuiltinTool> for &str {
    fn eq(&self, other: &BuiltinTool) -> bool {
        other.matches(*self)
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
        assert!(!BuiltinTool::ExecuteCommand.matches("run_command"));

        assert_eq!(BuiltinTool::from_name("execute_command"), Some(BuiltinTool::ExecuteCommand));
        assert_eq!(BuiltinTool::from_name("run_command"), None);
        assert_eq!(BuiltinTool::from_name("read_text"), Some(BuiltinTool::ReadText));
        assert_eq!(BuiltinTool::from_name("non_existent"), None);
    }

    #[test]
    fn test_builtin_tool_equality_with_strings() {
        assert_eq!(BuiltinTool::ExecuteCommand, "execute_command");
        assert_ne!(BuiltinTool::ExecuteCommand, "run_command");
        assert_eq!("execute_command", BuiltinTool::ExecuteCommand);
        assert_ne!("run_command", BuiltinTool::ExecuteCommand);
    }
}
