//! The local-loopback OAuth callback server for desktop browser authorization.
//!
//! Features:
//! - Flexible port binding strategies: Fixed port, Dynamic OS port (0), or Preferred-with-Dynamic-fallback.
//! - Anti-CSRF PKCE `state` validation.
//! - Modern HTML success & error response pages.
//! - Manual code/URL injection support for seamless CLI paste integration.

use nuo_contracts::provider_auth::{OAuthConfig, PortMode};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

const MAX_CALLBACK_REQUEST_BYTES: usize = 8 * 1024;
const CALLBACK_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The outcome of an authorization attempt.
#[derive(Debug, Clone)]
pub enum CallbackOutcome {
    /// The authorization `code`, ready to exchange for tokens.
    Code(String),
    /// The authorization server denied the request, or a newer login superseded it.
    Failed(String),
}

/// The single in-flight callback we are waiting for.
struct Pending {
    state: String,
    tx: oneshot::Sender<CallbackOutcome>,
}

/// Owns the loopback listener, the actual bound port, and its pending-callback slot.
pub struct CallbackServer {
    bound_port: u16,
    pending: Arc<Mutex<Option<Pending>>>,
    _handle: tokio::task::JoinHandle<()>,
}

impl CallbackServer {
    /// Bind the loopback server according to the provider's [`PortMode`].
    pub async fn start_for(cfg: &OAuthConfig) -> Result<Self, std::io::Error> {
        let host = &cfg.oauth_host;
        let path = cfg.oauth_path.to_string();
        let label = cfg.provider_id.to_string();
        let pending = Arc::new(Mutex::new(None::<Pending>));
        let pending_for_task = Arc::clone(&pending);

        let listener = match cfg.port_mode {
            PortMode::Fixed(port) => tokio::net::TcpListener::bind((host.as_ref(), port)).await?,
            PortMode::Dynamic => tokio::net::TcpListener::bind((host.as_ref(), 0)).await?,
            PortMode::PreferredOrDynamic(preferred) => {
                match tokio::net::TcpListener::bind((host.as_ref(), preferred)).await {
                    Ok(l) => l,
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                        tracing::info!(
                            preferred_port = preferred,
                            provider = %label,
                            "preferred port is busy, falling back to dynamic port"
                        );
                        tokio::net::TcpListener::bind((host.as_ref(), 0)).await?
                    }
                    Err(e) => return Err(e),
                }
            }
        };

        let bound_port = listener.local_addr()?.port();
        tracing::debug!(
            provider = %label,
            bound_port = bound_port,
            "OAuth callback server listening"
        );

        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(error = %e, "{label} oauth callback accept failed");
                        continue;
                    }
                };
                let pending = Arc::clone(&pending_for_task);
                let path = path.clone();
                let label = label.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_one(stream, pending, &path, &label).await {
                        tracing::warn!(error = %e, "{label} oauth callback serve failed");
                    }
                });
            }
        });

        Ok(Self {
            bound_port,
            pending,
            _handle: handle,
        })
    }

    /// The actual port the loopback server bound to.
    pub fn bound_port(&self) -> u16 {
        self.bound_port
    }

    /// Register an in-flight callback expectation for `state` and return a
    /// receiver that resolves with the outcome. Supersedes any prior pending
    /// callback.
    pub fn wait_for_code(&self, state: String) -> oneshot::Receiver<CallbackOutcome> {
        let (tx, rx) = oneshot::channel();
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prior) = guard.take() {
            let _ = prior.tx.send(CallbackOutcome::Failed(
                "superseded by a newer authorization request".to_string(),
            ));
        }
        *guard = Some(Pending { state, tx });
        rx
    }

    /// Manually inject an authorization code or failure (e.g. from terminal paste).
    pub fn inject_outcome(&self, outcome: CallbackOutcome) -> bool {
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = guard.take() {
            let _ = p.tx.send(outcome);
            true
        } else {
            false
        }
    }
}

impl Drop for CallbackServer {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

/// Serve a single callback request. Parses `code`/`state`/`error`, resolves
/// the pending callback if the state matches, and replies with a clean HTML page.
async fn serve_one(
    mut stream: tokio::net::TcpStream,
    pending: Arc<Mutex<Option<Pending>>>,
    redirect_path: &str,
    label: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut request_bytes = Vec::with_capacity(1024);
    loop {
        let mut chunk = [0u8; 1024];
        let n = tokio::time::timeout(CALLBACK_READ_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| Error::new(ErrorKind::TimedOut, "OAuth callback request timed out"))??;
        if n == 0 {
            break;
        }
        request_bytes.extend_from_slice(&chunk[..n]);
        if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if request_bytes.len() >= MAX_CALLBACK_REQUEST_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "OAuth callback request headers are too large",
            )
            .into());
        }
    }
    if request_bytes.len() > MAX_CALLBACK_REQUEST_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "OAuth callback request headers are too large",
        )
        .into());
    }
    let request = std::str::from_utf8(&request_bytes).map_err(|_| {
        Error::new(
            ErrorKind::InvalidData,
            "OAuth callback request is not UTF-8",
        )
    })?;

    let request_line = request.lines().next().unwrap_or("");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or("");
    let path = request_parts.next().unwrap_or("/");
    let (pathname, query) = path.split_once('?').unwrap_or((path, ""));

    // Validate the HTTP envelope before inspecting OAuth parameters or
    // touching the one-shot pending state. Browser probes such as /favicon.ico
    // must be unable to cancel or complete a login.
    if method != "GET" {
        stream
            .write_all(
                b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET\r\nConnection: close\r\n\r\n",
            )
            .await?;
        stream.flush().await?;
        return Ok(());
    }
    if pathname != redirect_path {
        stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 9\r\n\r\nNot found")
            .await?;
        stream.flush().await?;
        return Ok(());
    }

    let params = match parse_query(query) {
        Ok(params) => params,
        Err(message) => {
            write_html_response(
                &mut stream,
                "400 Bad Request",
                &render_page(label, &message, false),
            )
            .await?;
            return Ok(());
        }
    };
    let code = params.get("code").cloned();
    let state = params.get("state").cloned();
    let error = params.get("error").cloned();
    let error_description = params.get("error_description").cloned();

    let (status, body) = {
        let mut guard = pending.lock().unwrap_or_else(|e| e.into_inner());
        let state_matches = guard
            .as_ref()
            .zip(state.as_deref())
            .is_some_and(|(pending, actual)| pending.state == actual);
        let (outcome, status, body) = match (error.as_deref(), code.as_deref()) {
            (Some(err), _) if state_matches => {
                let msg = error_description.unwrap_or_else(|| err.to_string());
                (
                    Some(CallbackOutcome::Failed(msg.clone())),
                    "400 Bad Request",
                    render_page(label, &msg, false),
                )
            }
            (Some(_), _) | (_, Some(_)) if !state_matches => {
                let msg = "invalid state - potential CSRF mismatch";
                (None, "400 Bad Request", render_page(label, msg, false))
            }
            (_, Some(code)) => (
                Some(CallbackOutcome::Code(code.to_string())),
                "200 OK",
                render_page(
                    label,
                    "Authorization successful! You may now close this tab and return to the terminal.",
                    true,
                ),
            ),
            _ => {
                let msg = "missing authorization code";
                (None, "400 Bad Request", render_page(label, msg, false))
            }
        };
        if let Some(outcome) = outcome
            && let Some(pending) = guard.take()
        {
            let _ = pending.tx.send(outcome);
        }
        (status, body)
    };

    write_html_response(&mut stream, status, &body).await?;
    Ok(())
}

async fn write_html_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    body: &str,
) -> Result<(), std::io::Error> {
    use tokio::io::AsyncWriteExt;

    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'\r\n\
         Cache-Control: no-store\r\n\
         Pragma: no-cache\r\n\
         Referrer-Policy: no-referrer\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\
         Content-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

fn parse_query(query: &str) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| "malformed authorization response".to_string())?;
        let key = decode(key)?;
        let value = decode(value)?;
        if map.insert(key, value).is_some() {
            return Err("duplicate authorization parameter".to_string());
        }
    }
    Ok(map)
}

fn decode(value: &str) -> Result<String, String> {
    let input = value.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        match input[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < input.len() => {
                let encoded = std::str::from_utf8(&input[index + 1..index + 3])
                    .map_err(|_| "invalid percent encoding".to_string())?;
                let byte = u8::from_str_radix(encoded, 16)
                    .map_err(|_| "invalid percent encoding".to_string())?;
                output.push(byte);
                index += 3;
            }
            b'%' => return Err("truncated percent encoding".to_string()),
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output).map_err(|_| "authorization response is not UTF-8".to_string())
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn render_page(label: &str, message: &str, ok: bool) -> String {
    let label = escape_html(label);
    let message = escape_html(message);
    let title = if ok {
        format!("{label} • Authorization Successful")
    } else {
        format!("{label} • Authorization Failed")
    };
    let accent_color = if ok { "#10B981" } else { "#EF4444" };
    let icon = if ok { "✓" } else { "✕" };

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{title}</title>
  <style>
    :root {{
      --bg: #090A0F;
      --card: #131722;
      --text: #F3F4F6;
      --muted: #9CA3AF;
      --accent: {accent_color};
    }}
    @media (prefers-color-scheme: light) {{
      :root {{
        --bg: #F8FAFC;
        --card: #FFFFFF;
        --text: #0F172A;
        --muted: #64748B;
        --accent: {accent_color};
      }}
    }}
    body {{
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      background: var(--bg);
      color: var(--text);
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    }}
    .card {{
      background: var(--card);
      border-radius: 16px;
      padding: 40px;
      max-width: 460px;
      margin: 20px;
      box-shadow: 0 20px 40px rgba(0,0,0,0.2);
      text-align: center;
      border: 1px solid rgba(255,255,255,0.08);
    }}
    .icon {{
      width: 56px;
      height: 56px;
      border-radius: 50%;
      background: var(--accent);
      color: #FFF;
      font-size: 28px;
      font-weight: bold;
      display: flex;
      align-items: center;
      justify-content: center;
      margin: 0 auto 20px;
    }}
    h1 {{
      font-size: 22px;
      margin: 0 0 12px;
      font-weight: 600;
    }}
    p {{
      color: var(--muted);
      font-size: 15px;
      line-height: 1.5;
      margin: 0;
    }}
  </style>
</head>
<body>
  <div class="card">
    <div class="icon">{icon}</div>
    <h1>{title}</h1>
    <p>{message}</p>
  </div>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn get(port: u16, target: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[test]
    fn parse_query_decodes_percent_and_plus() {
        let q = parse_query("code=abc&state=ST&error_description=bad+request%3A+denied").unwrap();
        assert_eq!(q.get("code").map(String::as_str), Some("abc"));
        assert_eq!(q.get("state").map(String::as_str), Some("ST"));
        assert_eq!(
            q.get("error_description").map(String::as_str),
            Some("bad request: denied")
        );
    }

    #[test]
    fn query_parser_rejects_duplicates_and_malformed_encoding() {
        assert!(parse_query("state=a&state=b").is_err());
        assert!(parse_query("state=%ZZ").is_err());
        assert!(parse_query("state=%F0%9F%94%90").is_ok());
    }

    #[test]
    fn rendered_page_escapes_provider_controlled_text() {
        let page = render_page("<provider>", "denied<script>alert(1)</script>", false);
        assert!(!page.contains("<provider>"));
        assert!(!page.contains("<script>"));
        assert!(page.contains("&lt;provider&gt;"));
    }

    #[tokio::test]
    async fn callback_server_drops_and_releases_port() {
        let cfg = OAuthConfig::builder("test")
            .oauth_host("127.0.0.1")
            .oauth_port(59923)
            .port_mode(PortMode::Fixed(59923))
            .oauth_path("/callback")
            .build();

        let server1 = CallbackServer::start_for(&cfg).await.expect("bind 1");
        let server2_err = CallbackServer::start_for(&cfg).await;
        assert!(
            server2_err.is_err(),
            "must fail while first server is alive"
        );

        drop(server1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let server3 = CallbackServer::start_for(&cfg)
            .await
            .expect("bind after drop");
        drop(server3);
    }

    #[tokio::test]
    async fn dynamic_port_binding_succeeds() {
        let cfg = OAuthConfig::builder("test_dynamic")
            .oauth_host("127.0.0.1")
            .port_mode(PortMode::Dynamic)
            .build();

        let server = CallbackServer::start_for(&cfg).await.expect("dynamic bind");
        assert!(server.bound_port() > 0);
        drop(server);
    }

    #[tokio::test]
    async fn unrelated_and_invalid_state_requests_do_not_consume_login() {
        let cfg = OAuthConfig::builder("strict_callback")
            .oauth_host("127.0.0.1")
            .port_mode(PortMode::Dynamic)
            .oauth_path("/auth/callback")
            .build();
        let server = CallbackServer::start_for(&cfg).await.unwrap();
        let mut outcome = server.wait_for_code("expected-state".to_string());

        assert!(
            get(server.bound_port(), "/favicon.ico")
                .await
                .starts_with("HTTP/1.1 404")
        );
        assert!(
            get(
                server.bound_port(),
                "/auth/callback?code=attacker&state=wrong"
            )
            .await
            .starts_with("HTTP/1.1 400")
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut outcome)
                .await
                .is_err(),
            "invalid requests must leave the real callback pending"
        );

        let success = get(
            server.bound_port(),
            "/auth/callback?code=real-code&state=expected-state",
        )
        .await;
        assert!(success.starts_with("HTTP/1.1 200"));
        assert!(success.contains("Cache-Control: no-store\r\n"));
        assert!(success.contains("Pragma: no-cache\r\n"));
        assert!(success.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(success.contains("Content-Security-Policy:"));
        assert!(
            matches!(outcome.await.unwrap(), CallbackOutcome::Code(code) if code == "real-code")
        );
    }

    #[tokio::test]
    async fn callback_missing_state_duplicate_params_and_invalid_encoding_are_rejected() {
        let cfg = OAuthConfig::builder("hardening_callback")
            .oauth_host("127.0.0.1")
            .port_mode(PortMode::Dynamic)
            .oauth_path("/callback")
            .build();
        let server = CallbackServer::start_for(&cfg).await.unwrap();
        let outcome = server.wait_for_code("expected-state".to_string());

        // 1. Missing state
        let resp = get(server.bound_port(), "/callback?code=mycode").await;
        assert!(resp.starts_with("HTTP/1.1 400"));
        assert!(resp.contains("Cache-Control: no-store"));

        // 2. Duplicate code parameter
        let resp = get(
            server.bound_port(),
            "/callback?code=c1&code=c2&state=expected-state",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 400"));

        // 3. Duplicate state parameter
        let resp = get(
            server.bound_port(),
            "/callback?code=mycode&state=s1&state=expected-state",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 400"));

        // 4. Invalid percent encoding in code or state
        let resp = get(
            server.bound_port(),
            "/callback?code=%ZZ&state=expected-state",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 400"));

        // 5. Truncated percent encoding
        let resp = get(
            server.bound_port(),
            "/callback?code=test%2&state=expected-state",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 400"));

        // 6. HTML injection in error description
        let resp = get(
            server.bound_port(),
            "/callback?error=access_denied&error_description=%3Cscript%3Ealert(1)%3C%2Fscript%3E&state=expected-state",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 400"));
        assert!(!resp.contains("<script>alert(1)</script>"));
        assert!(resp.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));

        // The outcome channel received the failure without crashing
        assert!(matches!(
            outcome.await.unwrap(),
            CallbackOutcome::Failed(msg) if msg.contains("<script>")
        ));
    }
}
