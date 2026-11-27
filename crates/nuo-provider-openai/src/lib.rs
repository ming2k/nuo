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

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub(crate) mod cache;
pub mod chat_completions;
pub mod responses;
pub mod spec;

pub use chat_completions::{ChatCompletionsProvider, OpenAiChatCompletionsProvider};
pub use responses::{OpenAiResponsesProvider, ResponsesProvider};
pub use spec::*;
