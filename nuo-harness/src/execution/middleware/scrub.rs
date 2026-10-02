//! Secret scrubbing middleware to prevent accidental credential leakage in tool outputs.

use async_trait::async_trait;
use nuo_contracts::ToolOutput;
use nuo_contracts::execution::{ExecutionEnvironment, ToolMiddleware};
use regex::Regex;
use std::sync::LazyLock;

#[allow(clippy::expect_used)]
static SECRET_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        // OpenAI API Key
        (
            Regex::new(r"sk-(?:proj-)?[a-zA-Z0-9_-]{20,}").expect("valid regex"),
            "[REDACTED_OPENAI_KEY]",
        ),
        // Anthropic API Key
        (
            Regex::new(r"sk-ant-[a-zA-Z0-9_-]{20,}").expect("valid regex"),
            "[REDACTED_ANTHROPIC_KEY]",
        ),
        // GitHub Personal Access Token
        (
            Regex::new(r"gh[pousr]_[A-Za-z0-9_]{36,255}").expect("valid regex"),
            "[REDACTED_GITHUB_TOKEN]",
        ),
        // AWS Access Key ID
        (
            Regex::new(r"(?:A3T[A-Z0-9]|AKIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASIA)[A-Z0-9]{16}")
                .expect("valid regex"),
            "[REDACTED_AWS_KEY]",
        ),
        // Bearer tokens
        (
            Regex::new(r"Bearer\s+[a-zA-Z0-9_\-\.]{20,}").expect("valid regex"),
            "Bearer [REDACTED_BEARER_TOKEN]",
        ),
        // Private Keys
        (
            Regex::new(r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----[\s\S]*?-----END (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----").expect("valid regex"),
            "[REDACTED_PRIVATE_KEY]",
        ),
    ]
});

/// Redacts sensitive keys, tokens, and credentials from tool output.
#[derive(Debug, Default, Clone)]
pub struct SecretScrubMiddleware;

impl SecretScrubMiddleware {
    pub fn scrub(input: &str) -> String {
        let mut scrubbed = input.to_string();
        for (pattern, replacement) in SECRET_PATTERNS.iter() {
            scrubbed = pattern.replace_all(&scrubbed, *replacement).to_string();
        }
        scrubbed
    }
}

#[async_trait]
impl ToolMiddleware for SecretScrubMiddleware {
    async fn post_execute(
        &self,
        _tool: &str,
        output: &mut ToolOutput,
        _env: &dyn ExecutionEnvironment,
    ) -> Result<(), String> {
        match output {
            ToolOutput::Text(t) => *t = Self::scrub(t),
            ToolOutput::Shell { stdout, stderr, .. } => {
                *stdout = Self::scrub(stdout);
                *stderr = Self::scrub(stderr);
            }
            ToolOutput::Code { text, .. } => *text = Self::scrub(text),
            ToolOutput::Error { message, detail } => {
                *message = Self::scrub(message);
                if let Some(d) = detail {
                    *d = Self::scrub(d);
                }
            }
            ToolOutput::Listing { entries } => {
                for e in entries.iter_mut() {
                    *e = Self::scrub(e);
                }
            }
            ToolOutput::Matches { lines, .. } => {
                for l in lines.iter_mut() {
                    *l = Self::scrub(l);
                }
            }
            ToolOutput::Patch {
                old, new, warnings, ..
            } => {
                *old = Self::scrub(old);
                *new = Self::scrub(new);
                for warning in warnings {
                    *warning = Self::scrub(warning);
                }
            }
            _ => {}
        }
        Ok(())
    }
}
