//! The lightweight HTTP health and readiness probe side of the server's TCP port.
//!
//! The server's TCP listener speaks two protocols, split by peeking at the
//! request head (`classify` in `serve.rs`): WebSocket upgrades go to the
//! control plane; plain HTTP lands here.
//!
//! Deliberately lightweight and zero-dependency: operates directly on
//! `AsyncRead + AsyncWrite`, supports `/healthz`, `/readyz`, and CORS.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Request-head size cap; a peer that never finishes its headers is cut off.
const HEAD_CAP: usize = 16 * 1024;

/// Serve one plain-HTTP connection, then close it.
pub async fn serve<S>(
    stream: S,
    server_version: &str,
    server_id: &str,
    phase: &nuo_protocol::ServerPhase,
    persistence_health: Option<&nuo_protocol::PersistenceHealth>,
    expected_token: Option<&str>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    // Read the request head
    let mut head = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).await?;
        if n == 0 || head.len() > HEAD_CAP {
            return Ok(());
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        head.extend_from_slice(&line);
    }
    let head_str = String::from_utf8_lossy(&head);
    let mut lines = head_str.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = target.split(['?', '#']).next().unwrap_or("/");

    // Parse headers
    let auth_required = expected_token.is_some();
    let effective_health = persistence_health
        .cloned()
        .unwrap_or(nuo_protocol::PersistenceHealth::Healthy);
    let elapsed_ms = match phase {
        nuo_protocol::ServerPhase::Initializing {
            started_at_ms: Some(started),
            ..
        } => Some(crate::runtime::shutdown::unix_now_ms().saturating_sub(*started)),
        _ => None,
    };

    let response = match (method, path) {
        ("OPTIONS", _) => build_response(
            "204 No Content",
            "text/plain",
            &[],
            false,
            &[
                ("Access-Control-Allow-Origin", "*"),
                ("Access-Control-Allow-Methods", "GET, OPTIONS, HEAD"),
                (
                    "Access-Control-Allow-Headers",
                    "Authorization, Content-Type",
                ),
            ],
        ),
        ("GET" | "HEAD", "/healthz") => {
            let mut body = serde_json::json!({
                "version": server_version,
                "auth": auth_required,
                "server_id": server_id,
                "phase": phase,
                "persistence_health": effective_health,
            });
            if let Some(ms) = elapsed_ms {
                body["elapsed_ms"] = serde_json::json!(ms);
            }
            let body = body.to_string();
            build_response(
                "200 OK",
                "application/json",
                body.as_bytes(),
                method == "HEAD",
                &[("Access-Control-Allow-Origin", "*")],
            )
        }
        ("GET" | "HEAD", "/readyz") => {
            // ADR-0266 `[INV-SRVPHASE-08]`: `/readyz` reflects both global
            // lifecycle phase and durability writer health.
            let persistence_down = matches!(
                effective_health,
                nuo_protocol::PersistenceHealth::Down { .. }
            );
            let ready = phase.admits_work() && !persistence_down;
            let status = if ready {
                "200 OK"
            } else {
                "503 Service Unavailable"
            };
            let mut body = serde_json::json!({
                "ready": ready,
                "server_id": server_id,
                "phase": phase,
                "persistence_health": effective_health,
            });
            if let Some(ms) = elapsed_ms {
                body["elapsed_ms"] = serde_json::json!(ms);
            }
            let body = body.to_string();
            build_response(
                status,
                "application/json",
                body.as_bytes(),
                method == "HEAD",
                &[("Access-Control-Allow-Origin", "*")],
            )
        }
        ("GET" | "HEAD", _) => build_response(
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found",
            method == "HEAD",
            &[],
        ),
        _ => build_response(
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
            false,
            &[("Allow", "GET, HEAD, OPTIONS")],
        ),
    };
    writer.write_all(&response).await?;
    writer.flush().await
}

fn build_response(
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    extra: &[(&str, &str)],
) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let mut bytes = response.into_bytes();
    if !head_only {
        bytes.extend_from_slice(body);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn roundtrip_with_state(
        request: &str,
        token: Option<&str>,
        phase: nuo_protocol::ServerPhase,
        persistence_health: Option<nuo_protocol::PersistenceHealth>,
    ) -> String {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let token_owned = token.map(|s| s.to_string());
        let task = tokio::spawn(async move {
            serve(
                server,
                "0.0.0-test",
                "srv-test-1",
                &phase,
                persistence_health.as_ref(),
                token_owned.as_deref(),
            )
            .await
        });
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        client_writer.write_all(request.as_bytes()).await.unwrap();
        client_writer.shutdown().await.unwrap();
        let mut buf = Vec::new();
        client_reader.read_to_end(&mut buf).await.unwrap();
        task.await.unwrap().unwrap();
        String::from_utf8(buf).unwrap()
    }

    async fn roundtrip_with_token(request: &str, token: Option<&str>) -> String {
        roundtrip_with_state(request, token, nuo_protocol::ServerPhase::Ready, None).await
    }

    async fn roundtrip(request: &str) -> String {
        roundtrip_with_token(request, Some("secret123")).await
    }

    #[tokio::test]
    async fn readyz_returns_200_when_ready_and_503_when_initializing_draining_or_persistence_down()
    {
        let ready = roundtrip_with_state(
            "GET /readyz HTTP/1.1\r\n\r\n",
            None,
            nuo_protocol::ServerPhase::Ready,
            None,
        )
        .await;
        assert!(ready.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(ready.contains("\"ready\":true"));
        assert!(ready.contains("\"kind\":\"healthy\""));

        let init = roundtrip_with_state(
            "GET /readyz HTTP/1.1\r\n\r\n",
            None,
            nuo_protocol::ServerPhase::initializing_with(
                nuo_protocol::ServerInitStage::Storage,
                Some(nuo_protocol::StorageInitDetail::Migrating {
                    step: 1,
                    total: 2,
                    name: "0009_pure_catalog".into(),
                }),
                Some(1_700_000_000_000),
            ),
            None,
        )
        .await;
        assert!(init.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(init.contains("\"ready\":false"));
        assert!(init.contains("\"stage\":\"storage\""));
        assert!(init.contains("\"kind\":\"migrating\""));
        assert!(init.contains("\"elapsed_ms\":"));

        let draining = roundtrip_with_state(
            "GET /readyz HTTP/1.1\r\n\r\n",
            None,
            nuo_protocol::ServerPhase::Draining {
                reason: Some("SIGTERM".into()),
            },
            None,
        )
        .await;
        assert!(draining.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(draining.contains("\"ready\":false"));

        let down = roundtrip_with_state(
            "GET /readyz HTTP/1.1\r\n\r\n",
            None,
            nuo_protocol::ServerPhase::Ready,
            Some(nuo_protocol::PersistenceHealth::Down {
                attempt: 5,
                since_ms: 1_700_000_000_000,
                error: "disk full".into(),
            }),
        )
        .await;
        assert!(down.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(down.contains("\"ready\":false"));
        assert!(down.contains("\"kind\":\"down\""));
    }

    #[tokio::test]
    async fn root_does_not_serve_application_assets() {
        let response = roundtrip("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[tokio::test]
    async fn healthz_reports_version_and_auth_requirement() {
        let response = roundtrip("GET /healthz HTTP/1.1\r\n\r\n").await;
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains("\"version\":\"0.0.0-test\""));
        assert!(response.contains("\"auth\":true"));
    }

    #[tokio::test]
    async fn healthz_reports_no_auth_when_token_is_none() {
        let response = roundtrip_with_token("GET /healthz HTTP/1.1\r\n\r\n", None).await;
        assert!(response.contains("\"auth\":false"));
    }

    #[tokio::test]
    async fn cors_options_returns_no_content() {
        let response = roundtrip("OPTIONS /healthz HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Access-Control-Allow-Origin: *"));
    }

    #[tokio::test]
    async fn unknown_routes_are_not_found() {
        let response = roundtrip("GET /no/such/route?ws=x HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[tokio::test]
    async fn traversal_is_not_a_route() {
        let response = roundtrip("GET /../../etc/passwd HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[tokio::test]
    async fn head_omits_the_body() {
        let response = roundtrip("HEAD /healthz HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        let body_start = response.find("\r\n\r\n").unwrap() + 4;
        assert_eq!(&response[body_start..], "");
    }
}
