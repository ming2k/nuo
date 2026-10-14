//! One-shot live smoke for the built-in `commandcode` provider: tests catalog fetch,
//! chat completions streaming, Anthropic messages streaming, and credit usage fetching.
//!
//! Run manually with the key:
//! `COMMANDCODE_API_KEY=user_… cargo run -p nuo-providers --example commandcode_live_smoke`

#![allow(clippy::expect_used)]

use futures::StreamExt as _;
use nuo_host::SecretString;
use nuo_model_codec::{Message, Provider, ProviderStreamEvent, Role};
use nuo_provider_catalog::{CatalogShape, RemoteCatalogRequest, list_models};
use nuo_provider_commandcode_plan::CommandCodeUsageFetcher;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let key = std::env::var("COMMANDCODE_API_KEY").unwrap_or_else(|_| {
        eprintln!("set COMMANDCODE_API_KEY (e.g. user_…)");
        std::process::exit(2);
    });

    println!("=== 1. Live Catalog Fetch ===");
    let secret = SecretString::new(key.clone());
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::OpenAi,
        base_url: "https://api.commandcode.ai/provider/v1",
        api_key: &secret,
        account_id: None,
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    match list_models(req).await {
        Ok(models) => {
            println!("Discovered {} models from Command Code!", models.len());
            for m in models.iter().take(8) {
                println!("  - {} (name: {:?}, protocol: {:?})", m.id, m.name, m.protocol);
            }
        }
        Err(e) => eprintln!("Catalog fetch failed: {e}"),
    }

    println!("\n=== 2. Chat Completions Streaming (deepseek/deepseek-v4-flash) ===");
    let provider_chat = nuo_provider_openai::OpenAiChatCompletionsProvider::
        with_base_url(
            secret.expose_secret().to_string(),
            "deepseek/deepseek-v4-flash".to_string(),
            "https://api.commandcode.ai/provider/v1/chat/completions",
        )
        .with_id("cmd-smoke-chat".to_string());

    let req_chat = nuo_model_codec::ModelRequest::new(vec![Message::new(
        Role::User,
        "Please reply with 'Hello from Command Code OpenAI!' and nothing else.",
    )]);
    match provider_chat.stream_chat_events(req_chat).await {
        Ok(mut stream) => {
            print!("Response: ");
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ProviderStreamEvent::TextDelta(delta)) => print!("{delta}"),
                    Ok(ProviderStreamEvent::ReasoningDelta(delta)) => eprint!("[thinking: {delta}]"),
                    Ok(ProviderStreamEvent::Completed(meta)) => {
                        println!("\n[Stream completed, usage: {:?}]", meta.usage);
                    }
                    Err(e) => eprintln!("\nStream error: {e}"),
                    _ => {}
                }
            }
        }
        Err(e) => eprintln!("Call failed: {e}"),
    }

    println!("\n=== 3. Anthropic Messages Streaming (claude-sonnet-5-5) ===");
    let cred_source: Arc<dyn nuo_model_codec::CredentialSource> =
        Arc::new(nuo_model_codec::auth::StaticCredentialSource::new(secret.clone()));
    let provider_anthropic = nuo_provider_anthropic::AnthropicMessagesProvider::with_credentials(
        cred_source,
        "claude-sonnet-5-5".to_string(),
        "https://api.commandcode.ai/provider/v1/messages",
        nuo_wire::ClientProfile::Native,
    )
    .with_id("cmd-smoke-anthropic".to_string());

    let req_anthropic = nuo_model_codec::ModelRequest::new(vec![Message::new(
        Role::User,
        "Please reply with 'Hello from Command Code Anthropic!' and nothing else.",
    )]);
    match provider_anthropic.stream_chat_events(req_anthropic).await {
        Ok(mut stream) => {
            print!("Response: ");
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ProviderStreamEvent::TextDelta(delta)) => print!("{delta}"),
                    Ok(ProviderStreamEvent::ReasoningDelta(delta)) => eprint!("[thinking: {delta}]"),
                    Ok(ProviderStreamEvent::Completed(meta)) => {
                        println!("\n[Stream completed, usage: {:?}]", meta.usage);
                    }
                    Err(e) => eprintln!("\nStream error: {e}"),
                    _ => {}
                }
            }
        }
        Err(e) => eprintln!("Call failed: {e}"),
    }

    println!("\n=== 4. Provider Usage / Credits ===");
    let http = nuo_provider_transport::http::Http::control_plane().unwrap();
    match CommandCodeUsageFetcher.fetch_usage(&http, "https://api.commandcode.ai/provider/v1", &key).await {
        Ok(usage) => {
            println!("Balance: {:?}", usage.primary_balance);
            for m in usage.metrics {
                println!("  - {}: {} {:?}", m.label, m.value, m.unit.unwrap_or_default());
            }
        }
        Err(e) => eprintln!("Usage fetch failed: {e}"),
    }
}
