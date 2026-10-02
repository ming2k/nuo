//! Client identity simulation and fingerprinting profiles (Client Spoofing).
//!
//! Enables authenticating as authorized IDE extensions (e.g. GitHub Copilot),
//! specialized agent CLIs (Claude Code, Cursor), or applying custom attribution headers.

use std::collections::HashMap;

/// Client profile specifying User-Agent and fingerprint headers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ClientProfile {
    /// Transparent native SDK identity.
    #[default]
    Native,
    /// Simulates official VSCode GitHub Copilot plugin headers.
    Copilot,
    /// Simulates official Anthropic Claude Code CLI headers.
    ClaudeCode,
    /// Simulates Cursor IDE headers.
    Cursor,
    /// Simulates OpenCode relay headers.
    OpenCode,
    /// Arbitrary user-defined header fingerprint.
    Custom(HashMap<String, String>),
}

impl ClientProfile {
    /// Generates HTTP headers matching this client profile.
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Native => vec![(
                "user-agent",
                format!("nous-model-wire/{}", env!("CARGO_PKG_VERSION")),
            )],
            Self::Copilot => vec![
                ("user-agent", "GithubCopilot/1.250.0".to_string()),
                ("editor-version", "vscode/1.95.0".to_string()),
                ("editor-plugin-version", "copilot/1.250.0".to_string()),
                ("openai-organization", "github-copilot".to_string()),
                ("openai-intent", "conversation-panel".to_string()),
            ],
            Self::ClaudeCode => vec![
                (
                    "user-agent",
                    "claude-cli/0.2.29 (external, cli)".to_string(),
                ),
                ("x-app", "claude-code".to_string()),
            ],
            Self::Cursor => vec![
                ("user-agent", "Cursor/0.45.0".to_string()),
                ("x-cursor-client-version", "0.45.0".to_string()),
            ],
            Self::OpenCode => vec![
                ("user-agent", "opencode/0.1.0".to_string()),
                ("x-opencode-client", "cli".to_string()),
            ],
            Self::Custom(_) => Vec::new(),
        }
    }

    /// Primary User-Agent string for this profile.
    pub fn user_agent(&self) -> String {
        match self {
            Self::Native => format!("nous-model-wire/{}", env!("CARGO_PKG_VERSION")),
            Self::Copilot => "GithubCopilot/1.250.0".to_string(),
            Self::ClaudeCode => "claude-cli/0.2.29 (external, cli)".to_string(),
            Self::Cursor => "Cursor/0.45.0".to_string(),
            Self::OpenCode => "opencode/0.1.0".to_string(),
            Self::Custom(map) => map
                .get("user-agent")
                .cloned()
                .unwrap_or_else(|| format!("nous-model-wire/{}", env!("CARGO_PKG_VERSION"))),
        }
    }
}
