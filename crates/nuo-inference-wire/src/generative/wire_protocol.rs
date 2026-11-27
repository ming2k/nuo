//! Exact inference wire protocol used by a route.

/// The exact inference wire protocol used by a route. Provider dialects alter
/// authentication and envelopes without changing this protocol identity.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum WireProtocol {
    #[default]
    #[serde(rename = "openai-chat-completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "google-generate-content")]
    GoogleGenerateContent,
    #[serde(rename = "google-interactions")]
    GoogleInteractions,
}

impl WireProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai-chat-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
            Self::GoogleGenerateContent => "google-generate-content",
            Self::GoogleInteractions => "google-interactions",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "OpenAI Chat Completions",
            Self::OpenAiResponses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic Messages",
            Self::GoogleGenerateContent => "Google GenerateContent",
            Self::GoogleInteractions => "Google Interactions",
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
            "openai-chat-completions" => Ok(Self::OpenAiChatCompletions),
            "openai-responses" => Ok(Self::OpenAiResponses),
            "anthropic-messages" => Ok(Self::AnthropicMessages),
            "google-generate-content" => Ok(Self::GoogleGenerateContent),
            "google-interactions" => Ok(Self::GoogleInteractions),
            _ => Err(format!("unsupported inference protocol `{value}`")),
        }
    }
}
