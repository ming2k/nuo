//! Baselines for case-sensitive model ids used by custom OpenAI-compatible
//! routes. Custom connections are declarations, not provider presets.

use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::effort_ladders;

pub const MODELS: &[Model] = &[
    Model {
        id: "GLM-5.2",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::GLM_5,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "Deepseek-v4-flash",
        family: "deepseek",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        effort_levels: effort_ladders::LOW_HIGH_MAX,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

#[cfg(test)]
mod tests {
    #[test]
    fn cased_third_party_ids_remain_exact() {
        let glm = nuo_provider::model::resolve("GLM-5.2");
        let lowercase = nuo_provider::model::resolve("glm-5.2");
        assert_eq!(glm.context_window, 200_000);
        assert_eq!(lowercase.context_window, 1_000_000);
    }
}
