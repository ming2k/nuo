//! Request-construction tests, split by surface.

use super::*;

use crate::response::{
    TERMINAL_THOUGHT_SIGNATURE_META_KEY, TEXT_THOUGHT_SIGNATURE_META_KEY,
    THOUGHT_SIGNATURE_ORIGIN_META_KEY, THOUGHT_SIGNATURES_META_KEY,
};

/// The wire identity these fixtures pretend minted their signatures — the
/// route the request is being built for. A signature that does not carry
/// this exact origin is foreign and must not be replayed.
const TEST_ORIGIN: &str = "gemini-test";

fn test_body_input<'a>(
    tool_specs: Option<&'a [nuo_provider::capability::ToolSpec]>,
    include_thoughts: bool,
    thinking: Option<GoogleThinking>,
) -> BodyInput<'a> {
    BodyInput {
        instructions: None,
        tool_specs,
        include_thoughts,
        thinking,
        signature_origin: TEST_ORIGIN,
    }
}

/// Stamp [`TEST_ORIGIN`] onto a sidecar built entry-by-entry in a test.
fn stamp_test_origin(meta: &mut serde_json::Map<String, Value>) {
    meta.insert(
        THOUGHT_SIGNATURE_ORIGIN_META_KEY.to_string(),
        json!(TEST_ORIGIN),
    );
}

/// Route capabilities with only the vision declaration varied.
fn caps_with_vision(vision: Option<bool>) -> nuo_provider::ModelCapabilities {
    nuo_provider::ModelCapabilities {
        family: "google".into(),
        context_window: 1_000_000,
        max_output_tokens: None,
        thinking: nuo_provider::ReasoningSupport::None,
        tool_call: true,
        vision,
        effort_levels: Vec::new(),
        effort_entities: Vec::new(),
        default_effort: None,
    }
}

fn image_message(text: &str) -> Message {
    Message::new(Role::User, text).with_images(vec![nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "aGk=".to_string(),
    }])
}

mod body;
mod replay;
mod schema;
mod signatures;
mod thinking;
