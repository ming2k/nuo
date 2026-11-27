# nuo-tool-derive

Compile-time procedural macro deriving draft-07 JSON Schema specifications for `nuo-tool`.

## Overview

`nuo-tool-derive` provides the `#[derive(ToolArgs)]` procedural macro (with backward-compatible alias `#[derive(ToolSchema)]`). It implements the canonical `nuo_tool::ToolArgs` trait and inspects Rust struct fields or unit enums at compile time to generate draft-07 JSON parameter schemas without runtime reflection overhead.

## Key Industrial Features

- **Trait Contract**: Implements `::nuo_tool::ToolArgs`, enabling generic trait bounds (`T: ToolArgs`).
- **Doc-Comments First**: Automatically extracts standard Rust `///` doc comments into schema `description` fields.
- **Explicit Description Override**: Accepts `#[tool(description = "...")]` or `#[tool(desc = "...")]`.
- **Serde Interoperability**: Seamlessly respects `#[serde(rename = "...")]`, `#[serde(skip)]`, and container-level `#[serde(rename_all = "...")]`.
- **Enum & Choice Constraints**:
  - Direct derivation on unit enums generating `{"type": "string", "enum": [...]}`.
  - Field-level choice constraints via `#[tool(choices = ["a", "b"])]`.
- **Zero-Allocation Const Embedding**: Emits `const RAW_SCHEMA: &'static str` for zero-allocation access alongside parsed `parameters_schema() -> serde_json::Value`.

## Usage

### Struct Parameter Derivation

```rust
use nuo_tool::ToolArgs;
use serde::Deserialize;

#[derive(Deserialize, ToolArgs)]
pub struct SearchArgs {
    /// Directory to search within
    #[serde(rename = "target_path")]
    pub path: String,

    /// Query mode constraint
    #[tool(choices = ["fast", "deep", "ast"])]
    pub mode: String,

    /// Maximum results to return (optional)
    pub limit: Option<usize>,

    #[serde(skip)]
    pub internal_cache_key: Option<String>,
}
```

### Direct Unit Enum Derivation

```rust
use nuo_tool::ToolArgs;
use serde::Deserialize;

#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    OutlineSummary,
    FullAstTree,
    #[serde(rename = "custom_filter")]
    Custom,
}
```

Both generate:
- `pub fn parameters_schema() -> serde_json::Value`
- `pub fn raw_schema() -> &'static str`
- `impl nuo_tool::ToolArgs for ...`
