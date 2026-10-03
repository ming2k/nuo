//! One-shot live smoke for the built-in `qianwen` provider: the standard
//! Chat Completions wire through the registry's own spec (URL, protocol,
//! reasoning ladder), driven against the real Token Plan endpoint. Run
//! manually with the plan key in the environment:
//!
//! `QIANWEN_API_KEY=sk-sp-… cargo run -p muta-providers --example qianwen_live_smoke`
//!
//! Hits the real API, so it is deliberately an example, not a test.
#![allow(clippy::expect_used)]

use async_trait::async_trait;
use nuo_host::SecretString;
use nuo_model_codec::{Effort, Message, Provider as _, ProviderStreamEvent, Role, Tool};

struct WeatherTool;

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }
    fn description(&self) -> &str {
        "Query current weather for a city"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        })
    }
    async fn execute(&self, _: &nuo_tool::context::ToolContext, _: serde_json::Value) -> Result<nuo_tool::output::ToolOutput, nuo_tool::error::ToolError> {
        Ok(nuo_tool::output::ToolOutput::success(""))
    }
}

#[tokio::main]
async fn main() {
    let key = std::env::var("QIANWEN_API_KEY").unwrap_or_else(|_| {
        eprintln!("set QIANWEN_API_KEY (the Token Plan key, sk-sp-…)");
        std::process::exit(2);
    });
    let secret = SecretString::new(key);

    // Resolve the model the same way the daemon does — through the model
    // registry (the capability source) — then build the standard
    // Chat Completions provider at the provider spec's root URL.
    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "qwen3.8-flash".to_string());
    let baseline = nuo_model_codec::model::resolve(&model);
    let spec = nuo_providers::model_provider_spec("qianwen")
        .unwrap_or_else(|| panic!("built-in qianwen spec is registered"));
    let url = format!("{}/chat/completions", spec.root_url.trim_end_matches('/'));
    println!(
        "model={model} window={} ladder={:?} url={url}",
        baseline.context_window,
        baseline
            .effort_levels
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>(),
    );

    let provider = nuo_providers::protocol::openai::chat_completions::OpenAiChatCompletionsProvider::
        with_base_url(secret.expose_secret().to_string(), model.clone(), &url)
        .with_id("qianwen-smoke".to_string())
        .with_model_capabilities(nuo_model_codec::ModelCapabilities::for_channel(baseline.id, None));

    // Round 1: plain streaming text.
    let stream = provider
        .stream_chat_events(vec![Message::new(Role::User, "Reply with exactly: OK")].into())
        .await
        .expect("stream opens");
    let mut text = String::new();
    let mut reasoning = 0usize;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use futures::StreamExt as _;
        let mut stream = stream;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderStreamEvent::TextDelta(d)) => text.push_str(&d),
                Ok(ProviderStreamEvent::ReasoningDelta(_)) => reasoning += 1,
                Ok(ProviderStreamEvent::Completed(meta)) => {
                    if let Some(usage) = &meta.usage {
                        println!(
                            "usage: prompt={} completion={} cache_read={}",
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            usage.cache_read_input_tokens
                        );
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("LIVE SMOKE FAIL — stream error: {error}");
                    std::process::exit(1);
                }
            }
        }
    })
    .await;
    println!(
        "round 1: text={text:?} reasoning_deltas={reasoning} {}",
        if text.contains("OK") { "PASS" } else { "CHECK" }
    );

    // Round 2: effort ladder — `none` must stream zero reasoning deltas, the
    // Qwen hybrid off switch verified upstream.
    let provider_off = nuo_providers::protocol::openai::chat_completions::OpenAiChatCompletionsProvider::
        with_base_url(secret.expose_secret().to_string(), model.clone(), &url)
        .with_id("qianwen-smoke".to_string())
        .with_reasoning_effort(Some(Effort::None));
    let stream = provider_off
        .stream_chat_events(vec![Message::new(Role::User, "Reply with exactly: OK")].into())
        .await
        .expect("off-stream opens");
    let mut reasoning_off = 0usize;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use futures::StreamExt as _;
        let mut stream = stream;
        while let Some(event) = stream.next().await {
            if let Ok(ProviderStreamEvent::ReasoningDelta(_)) = event {
                reasoning_off += 1;
            }
        }
    })
    .await;
    println!(
        "round 2 (effort=none): reasoning_deltas={reasoning_off} {}",
        if reasoning_off == 0 { "PASS" } else { "CHECK" }
    );

    // Round 3: native tool calls — the harness requires them.
    let request = nuo_model_codec::ModelRequest::with_tools(
        vec![Message::new(
            Role::User,
            "What is the weather in Beijing? Use the tool.",
        )],
        &[std::sync::Arc::new(WeatherTool) as std::sync::Arc<dyn Tool>],
    );
    let completion = provider.chat(request).await.expect("tool round completes");
    let calls = completion.message.tool_calls.clone().unwrap_or_default();
    println!(
        "round 3 (tools): calls={} {}",
        calls.len(),
        calls
            .first()
            .map(|c| format!("{}({})", c.name, c.arguments))
            .unwrap_or_else(|| "none".to_string())
    );

    println!("LIVE SMOKE DONE");
}
