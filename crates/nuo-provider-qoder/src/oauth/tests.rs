use super::*;

#[tokio::test]
async fn userinfo_resolves_the_uid_from_the_id_field() {
    let (addr, _hits, _keep) = spawn_scripted_server(vec![(
        200,
        r#"{"id":"9bdb6daa-be3f-45d6-8eee-a1f74dd18553","name":"Ming"}"#.to_string(),
    )])
    .await;
    let client = nuo_provider::transport::Http::control_plane().unwrap();
    let url = format!("http://{addr}/api/v1/userinfo");
    let uid = fetch_uid_at(&client, &url, "dt-token")
        .await
        .expect("userinfo resolves");
    assert_eq!(uid, "9bdb6daa-be3f-45d6-8eee-a1f74dd18553");
}

#[tokio::test]
async fn userinfo_without_an_id_fails_closed() {
    // An empty or absent `id` must not silently mint an empty uid: the
    // catalog's signature requires `Cosy-User`, so a missing uid is a hard
    // error rather than a request that will fail upstream.
    let (addr, _hits, _keep) =
        spawn_scripted_server(vec![(200, r#"{"name":"Ming"}"#.to_string())]).await;
    let client = nuo_provider::transport::Http::control_plane().unwrap();
    let url = format!("http://{addr}/api/v1/userinfo");
    assert!(fetch_uid_at(&client, &url, "dt-token").await.is_err());
}

#[test]
fn session_url_carries_the_full_pkce_contract() {
    let session = QoderDeviceSession::new(
        "0f8e2b1a-1111-4222-8333-444455556666",
        "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb",
    );
    let url = session.user_url();
    assert!(
        url.starts_with("https://qoder.com/device/selectAccounts?"),
        "{url}"
    );
    assert!(url.contains("challenge_method=S256"));
    assert!(url.contains("client_id=e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb"));
    assert!(url.contains("machine_id=0f8e2b1a"));
    assert!(url.contains(&format!("nonce={}", session.nonce)));
    // Challenge is unpadded base64url of SHA256(verifier).
    let expected = base64_url_no_pad(&sha2::Sha256::digest(session.verifier.as_bytes()));
    assert!(url.contains(&format!("challenge={expected}")));
}

#[test]
fn session_url_respects_custom_authorize_url() {
    let session = QoderDeviceSession::with_authorize_url(
        "0f8e2b1a-1111-4222-8333-444455556666",
        "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb",
        "https://qoder.cn/device/selectAccounts",
    );
    let url = session.user_url();
    assert!(
        url.starts_with("https://qoder.cn/device/selectAccounts?"),
        "{url}"
    );
    assert!(url.contains("client_id=e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb"));
}

#[test]
fn verifier_meets_the_length_floor() {
    let session = QoderDeviceSession::new("m", "c");
    assert!(session.verifier.len() >= 43);
    assert!(session.verifier.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn percent_encodes_query_values() {
    assert_eq!(percent("abc-_.~"), "abc-_.~");
    assert_eq!(percent("a b+c"), "a%20b%2Bc");
}

#[tokio::test]
async fn poll_surfaces_non_404_errors_immediately() {
    // The owned transport has no per-client base-URL override, so bind a
    // raw TCP server and hand its URL to the injectable endpoint.
    let (addr, hits, _keep) = spawn_scripted_server(vec![(500, "boom".to_string())]).await;
    let session = QoderDeviceSession::new("m", "c");
    let client = nuo_provider::transport::Http::control_plane().unwrap();
    let url = format!("http://{addr}/api/v1/deviceToken/poll");
    let result = poll_device_token_at(&client, &url, &session, |_| async {}, 5_000).await;
    assert!(matches!(
        result,
        Err(crate::oauth::AuthError::TokenEndpoint { status: 500, .. })
    ));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn poll_treats_404_as_pending_then_succeeds() {
    let (addr, hits, _keep) = spawn_scripted_server(vec![
        (404, "not found".to_string()),
        (
            200,
            r#"{"access_token":"dt-abc","refresh_token":"jrt-x","uid":"u1"}"#.to_string(),
        ),
    ])
    .await;
    let session = QoderDeviceSession::new("m", "c");
    let client = nuo_provider::transport::Http::control_plane().unwrap();
    let url = format!("http://{addr}/api/v1/deviceToken/poll");
    let result = poll_device_token_at(&client, &url, &session, |_| async {}, 5_000).await;
    let token = result.unwrap();
    assert_eq!(token.access_token.expose_secret(), "dt-abc");
    assert_eq!(token.uid.as_deref(), Some("u1"));
    // One pending poll + one success.
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn poll_treats_429_as_rate_limit_with_backoff() {
    // Two 429s then success: the flow must retry with backoff instead of
    // surfacing TokenEndpoint (the web page renders 429 from
    // /device/redirect as "Parameter invalid", which invites retry
    // storms — the poller breaks that loop itself).
    let (addr, hits, _keep) = spawn_scripted_server(vec![
        (429, "slow down".to_string()),
        (429, "slow down".to_string()),
        (
            200,
            r#"{"token":"dt-abc","refresh_token":"jrt-x"}"#.to_string(),
        ),
    ])
    .await;
    let session = QoderDeviceSession::new("m", "c");
    let client = nuo_provider::transport::Http::control_plane().unwrap();
    let url = format!("http://{addr}/api/v1/deviceToken/poll");
    let result = poll_device_token_at(&client, &url, &session, |_| async {}, 60_000).await;
    let token = result.unwrap();
    assert_eq!(token.access_token.expose_secret(), "dt-abc");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[test]
fn poll_accepts_1_1_57_token_field_name() {
    // qodercli 1.1.57 renamed the poll response field to `token`; the
    // 1.1.34 spellings must keep decoding too.
    for body in [
        r#"{"token":"dt-abc"}"#,
        r#"{"accessToken":"dt-abc"}"#,
        r#"{"device_token":"dt-abc"}"#,
    ] {
        let parsed: QoderDeviceToken = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.access_token.expose_secret(), "dt-abc");
    }
}

#[tokio::test]
async fn device_login_adopts_the_device_token_without_exchange() {
    // qodercli ≥1.1.34 uses refreshStrategy="device-token": the polled
    // `dt-` token IS the credential. Exchanging it as a `personal_token`
    // against /api/v1/jobToken/exchange is what produced the login-time
    // "token endpoint returned HTTP 400 BadRequest" regression.
    let (addr, hits, _keep) = spawn_scripted_server(vec![(
        200,
        r#"{"token":"dt-abc","refreshToken":"drt-xyz","uid":"u1","expireTime":1700000000000}"#
            .to_string(),
    )])
    .await;
    let session = QoderDeviceSession::new("m", "c");
    let client = nuo_provider::transport::Http::control_plane().unwrap();
    let url = format!("http://{addr}/api/v1/deviceToken/poll");
    let mut session = session;
    session.authorize_url = url.replace("/api/v1/deviceToken/poll", "/device/selectAccounts");
    // Poll directly (device_login wraps poll_device_token with prod URLs).
    let token = poll_device_token_at(&client, &url, &session, |_| async {}, 5_000)
        .await
        .unwrap();
    let login = TokenResponse {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        id_token: None,
        token_type: Some("Bearer".to_string()),
        expires_in: token.expire_time.map(|ms| {
            if ms > 1_000_000_000_000 {
                ms / 1000
            } else {
                ms
            }
        }),
        scope: None,
        qoder_uid: token.uid,
    };
    assert!(login.access_token.expose_secret().starts_with("dt-"));
    assert_eq!(
        login.refresh_token.as_ref().map(|r| r.expose_secret()),
        Some("drt-xyz")
    );
    assert_eq!(login.qoder_uid.as_deref(), Some("u1"));
    // Second-normalized expiry from the ms epoch value.
    assert_eq!(login.expires_in, Some(1_700_000_000));
    // Exactly one HTTP hit: no exchange round-trip.
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn device_refresh_posts_the_json_refresh_token() {
    let (addr, hits, _keep) = spawn_scripted_server(vec![(
        200,
        r#"{"token":"dt-new","refreshToken":"drt-new2","expireTime":1700000012000}"#.to_string(),
    )])
    .await;
    let client = nuo_provider::transport::Http::control_plane().unwrap();
    let url = format!("http://{addr}/api/v1/deviceToken/refresh");
    let token = refresh_device_token_at(&client, &url, "drt-old")
        .await
        .unwrap();
    assert_eq!(token.access_token.expose_secret(), "dt-new");
    assert_eq!(
        token.refresh_token.as_ref().map(|r| r.expose_secret()),
        Some("drt-new2")
    );
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn user_url_appends_machine_token_only_when_present() {
    let session = QoderDeviceSession::with_machine_token(
        "mid",
        "cid",
        "https://qoder.com/device/selectAccounts",
        Some("mt-1".to_string()),
    );
    assert!(session.user_url().ends_with("&machine_token=mt-1"));

    // Absent / blank tokens keep the 1.1.34 wire shape.
    let plain = QoderDeviceSession::with_machine_token(
        "mid",
        "cid",
        "https://qoder.com/device/selectAccounts",
        None,
    );
    assert!(!plain.user_url().contains("machine_token"));
    let blank = QoderDeviceSession::with_machine_token(
        "mid",
        "cid",
        "https://qoder.com/device/selectAccounts",
        Some("   ".to_string()),
    );
    assert!(!blank.user_url().contains("machine_token"));
}

#[tokio::test]
async fn exchange_maps_the_job_token_response() {
    let addr = server_single(
        200,
        r#"{"access_token":"jt-infer","refresh_token":"jrt-refresh","expires_in":86400,"token_type":"Bearer"}"#,
    )
    .await;
    let client = nuo_provider::transport::Http::control_plane().unwrap();
    let url = format!("http://{addr}/api/v1/jobToken/exchange");
    let result = exchange_inference_token_at(&client, &url, "pt-user-token").await;
    let token = result.unwrap();
    assert_eq!(token.access_token.expose_secret(), "jt-infer");
}

// ── test helpers ────────────────────────────────────────────────────────

/// Minimal HTTP test server serving a scripted list of (status, body)
/// responses, one per connection, repeating the last one indefinitely.
async fn spawn_scripted_server(
    script: Vec<(u16, String)>,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_task = Arc::clone(&hits);
    let script = Arc::new(tokio::sync::Mutex::new(script));
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0u8; 2048];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
            let step = {
                let mut script = script.lock().await;
                if script.len() > 1 {
                    script.remove(0)
                } else {
                    script[0].clone()
                }
            };
            hits_task.fetch_add(1, Ordering::SeqCst);
            let reason = if step.0 == 200 { "OK" } else { "ERR" };
            let payload = format!(
                "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                step.0,
                reason,
                step.1.len(),
                step.1
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, payload.as_bytes()).await;
            let _ = tokio::io::AsyncWriteExt::shutdown(&mut socket).await;
        }
    });
    (addr, hits, task)
}

async fn server_single(status: u16, body: &str) -> std::net::SocketAddr {
    spawn_scripted_server(vec![(status, body.to_string())])
        .await
        .0
}
