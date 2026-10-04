#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::provider::{MockProvider, ProviderDelta};
use nuo_agent::session::SessionEvent;
use tokio::sync::mpsc;

#[tokio::test]
async fn test_streaming_token_and_thinking_deltas_end_to_end() {
    let mock = MockProvider::new();

    // Push simulated streaming chunks: thinking chunk, then token deltas
    mock.push_stream_deltas(vec![
        ProviderDelta::thinking("Step 1: calculate 6 * 7. "),
        ProviderDelta::thinking("Step 2: obtain 42."),
        ProviderDelta::content("The "),
        ProviderDelta::content("answer "),
        ProviderDelta::content("is "),
        ProviderDelta::content("42."),
    ])
    .await;

    let agent = Agent::builder("agent://local/math-streamer")
        .provider(mock)
        .build()
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel(64);

    let handle =
        tokio::spawn(async move { agent.prompt_streaming("What is 6 * 7?", tx).await.unwrap() });

    let mut thinking_tokens = Vec::new();
    let mut content_tokens = Vec::new();
    let mut saw_round_started = false;
    let mut saw_done = false;

    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::RoundStarted { round } => {
                assert_eq!(round, 1);
                saw_round_started = true;
            }
            SessionEvent::ThinkingDelta { delta } => {
                thinking_tokens.push(delta);
            }
            SessionEvent::ContentDelta { delta } => {
                content_tokens.push(delta);
            }
            SessionEvent::Done { final_content, .. } => {
                assert_eq!(final_content, "The answer is 42.");
                saw_done = true;
            }
            _ => {}
        }
    }

    let final_answer = handle.await.unwrap();
    assert_eq!(final_answer, "The answer is 42.");
    assert!(saw_round_started);
    assert!(saw_done);

    // Verify thinking stream
    assert_eq!(
        thinking_tokens.join(""),
        "Step 1: calculate 6 * 7. Step 2: obtain 42."
    );
    // Verify content stream
    assert_eq!(content_tokens.join(""), "The answer is 42.");
}

#[tokio::test]
async fn test_streaming_tool_call_aggregation_and_execution_end_to_end() {
    let mock = MockProvider::new();

    // Round 1: Model streams a tool call split across multiple deltas
    mock.push_stream_deltas(vec![
        ProviderDelta::thinking("I should calculate 25 * 4 using the calculator tool."),
        ProviderDelta::tool_call_chunk(0, Some("calc_1".into()), Some("calculator".into()), None),
        ProviderDelta::tool_call_chunk(0, None, None, Some("{\"expression\":".into())),
        ProviderDelta::tool_call_chunk(0, None, None, Some(" \"25 * 4\"}".into())),
    ])
    .await;

    // Round 2: After tool execution observation, model returns final answer
    mock.push_stream_deltas(vec![
        ProviderDelta::content("25 * 4 is "),
        ProviderDelta::content("100."),
    ])
    .await;

    struct CalcTool;
    #[async_trait::async_trait]
    impl nuo_agent::tool::Tool for CalcTool {
        fn name(&self) -> &str {
            "calculator"
        }
        fn description(&self) -> &str {
            "evaluate arithmetic expressions"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "expression": { "type": "string" }
                },
                "required": ["expression"]
            })
        }
        async fn execute(
            &self,
            _ctx: &nuo_agent::tool::ToolContext,
            args: serde_json::Value,
        ) -> std::result::Result<nuo_agent::tool::ToolOutput, nuo_agent::tool::ToolError> {
            let expr = args["expression"].as_str().unwrap_or("");
            if expr == "25 * 4" {
                Ok(nuo_agent::tool::ToolOutput::success("100"))
            } else {
                Ok(nuo_agent::tool::ToolOutput::error("unknown expr"))
            }
        }
    }

    let agent = Agent::builder("agent://local/tool-streamer")
        .provider(mock)
        .tool(CalcTool)
        .build()
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .prompt_streaming("Calculate 25 * 4 please.", tx)
            .await
            .unwrap()
    });

    let mut tool_deltas = Vec::new();
    let mut tool_results = Vec::new();

    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ToolCallDelta {
                index,
                name,
                arguments_delta,
            } => {
                tool_deltas.push((index, name, arguments_delta));
            }
            SessionEvent::ToolCallFinished {
                call_id, output, ..
            } => {
                tool_results.push((call_id, output));
            }
            _ => {}
        }
    }

    let answer = handle.await.unwrap();
    assert_eq!(answer, "25 * 4 is 100.");
    assert!(!tool_deltas.is_empty());
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].0, "calc_1");
    assert_eq!(tool_results[0].1, "100");
}

struct CodecMockProvider;

#[async_trait::async_trait]
impl nuo_model_codec::capability::Provider for CodecMockProvider {
    async fn chat(
        &self,
        _request: nuo_model_codec::capability::ModelRequest,
    ) -> Result<nuo_model_codec::ProviderCompletion, nuo_wire::ProviderError> {
        unimplemented!()
    }
    async fn stream_chat(
        &self,
        _request: nuo_model_codec::capability::ModelRequest,
    ) -> Result<nuo_model_codec::capability::ProviderTextStream, nuo_wire::ProviderError> {
        unimplemented!()
    }
    async fn stream_chat_events(
        &self,
        _request: nuo_model_codec::capability::ModelRequest,
    ) -> Result<nuo_model_codec::capability::ProviderEventStream, nuo_wire::ProviderError> {
        use futures::StreamExt;
        let events = vec![
            Ok(nuo_model_codec::capability::ProviderStreamEvent::TextDelta("Hello from codec!".into())),
            Ok(nuo_model_codec::capability::ProviderStreamEvent::Completed(nuo_model_codec::ProviderCompletionMeta::default())),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

#[tokio::test]
async fn test_model_codec_adapter_streaming() {
    use nuo_agent::ModelCodecAdapter;
    let provider = ModelCodecAdapter::new(std::sync::Arc::new(CodecMockProvider));
    let agent = Agent::builder("agent://local/codec-streamer")
        .provider(provider)
        .build()
        .await
        .unwrap();

    let ans = agent.prompt("hi").await.unwrap();
    assert_eq!(ans, "Hello from codec!");
}
