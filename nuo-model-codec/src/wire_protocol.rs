//! Exact inference wire protocol used by a route.

/// The exact inference wire protocol used by a route. Provider dialects alter
/// authentication and envelopes without changing this protocol identity.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
)]
pub enum WireProtocol {
    #[default]
    #[serde(rename = "chat-completions", alias = "openai-chat-completions")]
    ChatCompletions,
    #[serde(rename = "responses", alias = "openai-responses")]
    Responses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "google-gemini", alias = "google-generate-content")]
    GoogleGemini,
}

impl WireProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat-completions",
            Self::Responses => "responses",
            Self::AnthropicMessages => "anthropic-messages",
            Self::GoogleGemini => "google-gemini",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "Chat Completions",
            Self::Responses => "Responses",
            Self::AnthropicMessages => "Anthropic Messages",
            Self::GoogleGemini => "Google Gemini",
        }
    }
}

impl std::fmt::Display for WireProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WireProtocol {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "chat-completions" | "openai-chat-completions" => Ok(Self::ChatCompletions),
            "responses" | "openai-responses" => Ok(Self::Responses),
            "anthropic-messages" => Ok(Self::AnthropicMessages),
            "google-gemini" | "google-generate-content" => Ok(Self::GoogleGemini),
            _ => Err(format!("unsupported inference protocol `{value}`")),
        }
    }
}
