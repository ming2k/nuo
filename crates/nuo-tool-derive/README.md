# nuo-tool-derive

Compile-time procedural macro deriving JSON Schema specifications for `nuo-tool`.

## Overview

`nuo-tool-derive` provides the `#[derive(ToolSchema)]` procedural macro. It inspects Rust struct fields and doc comments / `#[tool(desc = "...")]` attributes to generate draft-07 JSON parameter schemas at compile time, eliminating schema-code drift without runtime reflection overhead.

## Usage

```rust
use nuo_tool::ToolSchema;
use serde::Deserialize;

#[derive(Deserialize, ToolSchema)]
pub struct SearchArgs {
    #[tool(desc = "Search query string")]
    pub query: String,

    #[tool(desc = "Maximum results to return")]
    pub limit: Option<usize>,
}
```

This generates `parameters_schema() -> serde_json::Value` matching the exact JSON Schema required by model tool calling.
