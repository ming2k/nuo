// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_tool::{ToolArgs, ToolSchema};
use serde::Deserialize;
use serde_json::json;

#[allow(dead_code)]
#[derive(ToolSchema)]
struct FileSearchArgs {
    #[tool(desc = "Directory to search within")]
    path: String,
    #[tool(desc = "Search query string")]
    query: String,
    #[tool(desc = "Maximum results to return")]
    limit: Option<usize>,
    #[tool(desc = "Include hidden files in search")]
    include_hidden: Option<bool>,
}

#[allow(dead_code)]
#[derive(Deserialize, ToolArgs)]
struct ModernToolArgs {
    /// Directory target path
    #[serde(rename = "target_dir")]
    dir: String,

    /// Query mode selection
    #[tool(choices = ["quick", "deep", "ast"])]
    mode: String,

    #[serde(skip)]
    internal_cache_key: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "snake_case")]
enum InspectionMode {
    OutlineSummary,
    FullAstTree,
    #[serde(rename = "custom_filter")]
    Custom,
}

fn assert_implements_tool_args<T: ToolArgs>() {
    let schema = T::parameters_schema();
    assert!(schema.is_object() || schema.is_string());
    assert!(!T::raw_schema().is_empty());
}

#[test]
fn test_tool_schema_derive_exported_via_nuo_tool() {
    let schema = FileSearchArgs::parameters_schema();

    assert_eq!(schema["type"], json!("object"));
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(schema["required"], json!(["path", "query"]));

    assert_eq!(schema["properties"]["path"]["type"], json!("string"));
    assert_eq!(
        schema["properties"]["path"]["description"],
        json!("Directory to search within")
    );

    assert_eq!(schema["properties"]["query"]["type"], json!("string"));
    assert_eq!(
        schema["properties"]["query"]["description"],
        json!("Search query string")
    );

    assert_eq!(schema["properties"]["limit"]["type"], json!("integer"));
    assert_eq!(
        schema["properties"]["limit"]["description"],
        json!("Maximum results to return")
    );

    assert_eq!(
        schema["properties"]["include_hidden"]["type"],
        json!("boolean")
    );
}

#[test]
fn test_modern_tool_args_trait_and_features() {
    assert_implements_tool_args::<ModernToolArgs>();
    assert_implements_tool_args::<InspectionMode>();

    let raw = ModernToolArgs::raw_schema();
    assert!(raw.contains("target_dir"));
    assert!(raw.contains("Directory target path"));
    assert!(!raw.contains("internal_cache_key"));

    let schema = ModernToolArgs::parameters_schema();
    assert_eq!(
        schema["properties"]["target_dir"]["description"],
        json!("Directory target path")
    );
    assert_eq!(
        schema["properties"]["mode"]["enum"],
        json!(["quick", "deep", "ast"])
    );
    assert!(schema["properties"].get("dir").is_none());
    assert!(schema["properties"].get("internal_cache_key").is_none());

    let enum_schema = InspectionMode::parameters_schema();
    assert_eq!(enum_schema["type"], json!("string"));
    assert_eq!(
        enum_schema["enum"],
        json!(["outline_summary", "full_ast_tree", "custom_filter"])
    );
}
