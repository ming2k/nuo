//! Typed catalog parsers, one shape per wire dialect.

use nuo_protocol::CatalogShape;
use serde_json::Value;

use super::DiscoveredModel;

mod codex;
mod copilot;
mod effort;
mod entry;
mod google;

use self::codex::parse_codex_models;
use self::entry::parse_data_models;
use self::google::parse_google_models;

#[cfg(test)]
pub(crate) use self::effort::deepseek_accepted_efforts;
pub(crate) use self::entry::{parse_models_with_dimensions, validate_catalog_shape};

#[cfg(test)]
pub(crate) fn parse_models(protocol: CatalogShape, json: &Value) -> Vec<DiscoveredModel> {
    parser_for(protocol).parse_json(json)
}

pub trait CatalogParser: Send + Sync {
    /// Parse raw JSON value into standardized discovered models.
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel>;
}

/// Strip parameter/execution-mode suffixes like `(Thinking)` or `(thinking)` from human labels.
///
/// Model identity and execution parameters (such as reasoning effort / thinking mode)
/// must remain strictly separated per ADR-0068 [INV-CATALOG-02]. Suffixes like
/// `(Thinking)` inside model names are redundant with orthogonal reasoning capabilities.
pub(crate) fn clean_display_label(raw: &str) -> String {
    nuo_protocol::clean_model_display_name(raw)
}

pub struct OpenAiCatalogParser;
impl CatalogParser for OpenAiCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        parse_data_models(json)
    }
}

pub struct AnthropicCatalogParser;
impl CatalogParser for AnthropicCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        parse_data_models(json)
    }
}

pub struct GoogleCatalogParser;
impl CatalogParser for GoogleCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        parse_google_models(json)
    }
}

pub struct GoogleCloudCodeCatalogParser;
impl CatalogParser for GoogleCloudCodeCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        nuo_provider_google_antigravity::catalog::parse_antigravity_catalog(json)
    }
}

pub struct CodexCatalogParser;
impl CatalogParser for CodexCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        parse_codex_models(json)
    }
}

/// Parser for Qoder's scene-keyed catalog (`{"<scene>": [{"key": …}]}`).
///
/// The response is a map from scene name to that scene's model entries. The
/// requested scene's array is the authoritative list; the parser reads the
/// scene the connection declared, falling back to `assistant` (the CLI's
/// default) when the response does not carry it.
pub struct SceneMapCatalogParser {
    /// The scene the connection requested.
    pub scene: String,
}

impl CatalogParser for SceneMapCatalogParser {
    fn parse_json(&self, json: &Value) -> Vec<DiscoveredModel> {
        nuo_provider_qoder::parse_scene_catalog(json, &self.scene)
    }
}

/// Return the typed catalog parser for the given catalog shape.
///
/// The shape is a parser, not a provider: many providers share one shape and
/// therefore one parser. `dimensions` supplies the request dimensions a
/// shape needs to select *which* catalog to read (Qoder's `scene`).
pub fn parser_for(protocol: CatalogShape) -> Box<dyn CatalogParser> {
    parser_for_with_dimensions(protocol, &[])
}

/// Return the typed catalog parser for a shape, given the request dimensions
/// the connection declared. Shapes that select a sub-catalog by a dimension
/// read it here; dimension-free shapes ignore the argument.
pub fn parser_for_with_dimensions(
    protocol: CatalogShape,
    dimensions: &[(&str, String)],
) -> Box<dyn CatalogParser> {
    match protocol {
        CatalogShape::OpenAi => Box::new(OpenAiCatalogParser),
        CatalogShape::Anthropic => Box::new(AnthropicCatalogParser),
        CatalogShape::Google => Box::new(GoogleCatalogParser),
        CatalogShape::GoogleCloudCode => Box::new(GoogleCloudCodeCatalogParser),
        CatalogShape::Codex => Box::new(CodexCatalogParser),
        CatalogShape::SceneMap => {
            let scene = dimensions
                .iter()
                .find(|(name, _)| *name == "scene")
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| "assistant".to_string());
            Box::new(SceneMapCatalogParser { scene })
        }
    }
}
