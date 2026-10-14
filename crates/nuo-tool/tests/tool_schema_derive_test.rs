use nuo_tool::ToolSchema;
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

    assert_eq!(schema["properties"]["include_hidden"]["type"], json!("boolean"));
}
