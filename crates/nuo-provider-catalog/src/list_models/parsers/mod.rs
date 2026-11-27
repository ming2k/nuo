//! Typed catalog parsers, one shape per wire dialect.

use nuo_protocol::CatalogShape;
use serde_json::Value;

use super::DiscoveredModel;

mod entry;
mod google;

use self::entry::parse_data_models;
use self::google::parse_google_models;

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

/// Return the typed catalog parser for the given standard catalog shape.
pub fn parser_for(protocol: CatalogShape) -> Box<dyn CatalogParser> {
    match protocol {
        CatalogShape::OpenAi => Box::new(OpenAiCatalogParser),
        CatalogShape::Anthropic => Box::new(AnthropicCatalogParser),
        CatalogShape::Google => Box::new(GoogleCatalogParser),
    }
}
