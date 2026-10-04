//! OpenAI wire protocols.
//!
//! Two surfaces live under the OpenAI umbrella, each in its own module:
//!
//! - [`chat_completions`] — the OpenAI-compatible **chat-completions** surface
//!   (`/v1/chat/completions`), served by OpenAI itself, OpenAI-compatible
//!   relays, and GitHub Copilot's chat channel ([`OpenAiChatCompletionsProvider`]).
//! - [`responses`] — the OpenAI **Responses API** surface, spoken by the
//!   ChatGPT Subscription backend and the Copilot Responses channel
//!   ([`OpenAiResponsesProvider`]).

pub(crate) mod cache;
pub mod chat_completions;
pub mod responses;

pub use chat_completions::{ChatCompletionsProvider, OpenAiChatCompletionsProvider};
pub use responses::{OpenAiResponsesProvider, ResponsesProvider};
