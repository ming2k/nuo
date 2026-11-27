//! Strongly-typed tool parameter schema contract and traits.

use serde_json::Value;

/// Contract for strongly-typed tool argument types providing draft-07 JSON Schemas.
///
/// Implemented automatically via `#[derive(ToolArgs)]` or `#[derive(ToolSchema)]`.
pub trait ToolArgs: Send + Sync {
    /// The draft-07 JSON Schema for this argument type, advertised to LLM models.
    fn parameters_schema() -> Value;

    /// Compile-time raw JSON schema string representation (zero heap allocation).
    fn raw_schema() -> &'static str;
}
