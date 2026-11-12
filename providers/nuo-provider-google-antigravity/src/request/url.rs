//! Wire URLs and per-model thinking budgets.

pub fn url(base_url: &str, model: &str) -> String {
    format!("{base_url}/models/{model}:generateContent")
}

/// The streaming streamGenerateContent URL (SSE framing via `alt=sse`).
pub fn stream_url(base_url: &str, model: &str) -> String {
    format!("{base_url}/models/{model}:streamGenerateContent?alt=sse")
}

/// The maximum `thinkingBudget` a Gemini 2.5 model accepts, in tokens — the
/// cap [`thinking_budget_wire`] buckets against. Gemini 2.5 Flash tops
/// out at `24576`; Pro at `32768`; Flash-Lite (floor `512`) also at `24576`.
/// Any other model id (Gemini 3.x, non-reasoning) returns `0`, signaling "no
/// budget surface" so [`resolve_thinking`] never constructs a `Budget` for it.
///
/// Kept free-form on the model id rather than a registry field because the cap
/// is an intrinsic property of the model family, not a capability the static
/// `Model` carries today, and it only matters for the 2.5 budget path.
pub fn max_thinking_budget(model: &str) -> u32 {
    if model.starts_with("gemini-2.5-pro") {
        32768
    } else if model.starts_with("gemini-2.5") {
        // 2.5-flash and 2.5-flash-lite share the 24576 cap.
        24576
    } else {
        0
    }
}
