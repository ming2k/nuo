use futures::{SinkExt, StreamExt};
use nuo::serve::{AttachAction, Wire};
use nuo::serve_discovery::{self, Discovery};
use std::path::PathBuf;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let project_root = PathBuf::from(match std::env::args().nth(1) {
        Some(a) => a,
        None => {
            eprintln!("usage: attach_smoke <project_root>");
            std::process::exit(2);
        }
    });
    if let Err(e) = run(&project_root).await {
        eprintln!("attach_smoke: FAIL: {e}");
        std::process::exit(1);
    }
    println!("attach_smoke: OK");
}

async fn run(project_root: &std::path::Path) -> Result<(), String> {
    let _ = project_root;
    // ADR-0096: discovery is global (`daemon.json` under the instance dir),
    // not per-project — the pre-0096 `serve/<bucket>.json` layout is dead.
    let path = serve_discovery::global_discovery_path();
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("no discovery record at {}: {e}", path.display()))?;
    let info: Discovery = serde_json::from_slice(&bytes).map_err(|e| format!("corrupt: {e}"))?;
    println!(
        "attach_smoke: discovered pid={} port={}",
        info.pid, info.port
    );
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad url: {e}"))?;
    if let Some(token) = &info.token {
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| format!("{e}"))?,
        );
    }
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let (mut sink, mut source) = ws.split();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        // No declared project: the smoke run exercises the daemon's
        // cwd-fallback scope.
        project: None,
        posture: nuo_contracts::human_request::HumanChannelPosture::Interactive,
        protocol: None,
    })
    .map_err(|e| format!("{e}"))?;
    sink.send(WsMessage::Text(select.into()))
        .await
        .map_err(|e| format!("send: {e}"))?;
    let (session_id, n) = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(f) = source.next().await {
            if let Ok(WsMessage::Text(t)) = f
                && let Ok(Wire::Welcome {
                    session_id,
                    messages,
                    ..
                }) = serde_json::from_str::<Wire>(&t)
            {
                return Ok((session_id, messages.len()));
            }
        }
        Err("closed before welcome".to_string())
    })
    .await
    .map_err(|_| "timeout".to_string())??;
    println!("attach_smoke: welcome session={session_id} messages={n}");
    sink.send(WsMessage::Text(
        serde_json::json!({"type":"Request","Chat":{"text":"ping","images":[]}})
            .to_string()
            .into(),
    ))
    .await
    .map_err(|e| format!("{e}"))?;
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(f) = source.next().await {
            if let Ok(WsMessage::Text(t)) = f
                && let Ok(Wire::Response { .. }) = serde_json::from_str::<Wire>(&t)
            {
                return Ok(());
            }
        }
        Err("closed".to_string())
    })
    .await
    .map_err(|_| "timeout".to_string())?
}
