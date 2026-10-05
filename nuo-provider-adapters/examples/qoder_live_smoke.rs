//! One-shot live smoke: full Rust wire path (provider → pipeline → live
//! Qoder endpoint) with the local daemon's stored credentials. Run manually:
//! `cargo run -p nuo-provider-adapters --example qoder_live_smoke` — hits the real
//! API, so it is deliberately an example, not a test.

// Failing loudly on a broken pre-condition is the point of a smoke run.
#![allow(clippy::expect_used)]

use nuo_model_codec::{ClientPreset, Message, ResolvedAuth, Role, SecretString};
use nuo_provider_adapters::protocol::openai::chat_completions::OpenAiChatCompletionsProvider;
use nuo_provider_adapters::qoder::{QoderRequestIdentity, build_qoder_pipeline};
use std::sync::Arc;

#[derive(Debug)]
struct StoreSource {
    token: String,
    identity: QoderRequestIdentity,
}

impl nuo_model_codec::CredentialSource for StoreSource {
    fn resolve_auth(&self) -> futures::future::BoxFuture<'_, Result<ResolvedAuth, String>> {
        let token = self.token.clone();
        let identity = self.identity.clone();
        Box::pin(async move { Ok(ResolvedAuth::new(token).with_extension(identity)) })
    }
    fn force_refresh(&self) -> futures::future::BoxFuture<'_, Result<ResolvedAuth, String>> {
        Box::pin(async move { Err("no refresh in smoke".to_string()) })
    }
    fn is_oauth(&self) -> bool {
        true
    }
}

#[tokio::main]
async fn main() {
    let auth_toml =
        std::fs::read_to_string(std::path::Path::new(env!("HOME")).join(".local/state/muta/auth.toml"))
            .expect("auth store");
    // Scope the grab to the qoder connection's TOML section: the first
    // `access =` in the file may belong to another provider (e.g. ChatGPT's
    // JWT), which silently poisons the smoke run.
    let section = auth_toml
        .split("[tokens.qod]")
        .nth(1)
        .expect("[tokens.qod] section in auth store");
    let grab = |needle: &str| -> String {
        let line = section
            .lines()
            .find(|l| l.trim_start().starts_with(needle))
            .unwrap_or_else(|| panic!("missing {needle}"));
        line.split('"').nth(1).expect("quoted value").to_string()
    };
    let token = grab("access = ");
    let machine_key_hex = grab("machine_key_hex = ");
    let uid = grab("uid = ");
    println!("uid={uid} token={}… key={machine_key_hex}…", &token[..12.min(token.len())]);

    let provider = OpenAiChatCompletionsProvider::with_credentials(
        Arc::new(StoreSource {
            token,
            identity: QoderRequestIdentity {
                uid,
                machine_key_hex: SecretString::new(machine_key_hex),
                data_policy_agreed: true,
                organization_id: None,
                organization_tags: Vec::new(),
                infer_endpoint: None,
            },
        }),
        "qfmodel".to_string(),
        "https://api3.qoder.sh",
        ClientPreset::Native,
    )
    .with_dialect(nuo_model_codec::OpenAiChatDialect::Qoder)
    .with_pipeline(build_qoder_pipeline());

    use nuo_model_codec::Provider as _;
    let stream = provider
        .stream_chat_events(vec![Message::new(Role::User, "Reply with exactly: OK")].into())
        .await
        .expect("stream opens");
    let mut text = String::new();
    let mut count = 0usize;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use futures::StreamExt as _;
        let mut stream = stream;
        while let Some(event) = stream.next().await {
            match event {
                Ok(nuo_model_codec::ProviderStreamEvent::TextDelta(d)) => {
                    text.push_str(&d);
                    count += 1;
                }
                Ok(nuo_model_codec::ProviderStreamEvent::ReasoningDelta(d)) => {
                    count += 1;
                    let _ = d;
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
    println!("LIVE SMOKE PASS — events: {count}, content: {text:?}");
}
