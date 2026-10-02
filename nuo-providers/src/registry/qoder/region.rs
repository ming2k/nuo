//! The Qoder inference-endpoint election (integration doc §3.1a).
//!
//! `api1/api2/api3.qoder.sh` are not interchangeable — `api2` is the
//! *security* cluster, the inference nodes are server-assigned. At startup the
//! official CLI syncs the role map from the center surface and caches it in
//! `~/.qoder/.cache/endpoint-cache.json`:
//!
//! 1. `GET https://center.qoder.sh/algo/api/v5/service/region/endpoints`
//!    with a **plain Bearer** (no COSY signature).
//! 2. The response body is QoderEncoding-encoded (same codec as the request
//!    body, [`super::super::wire::codec`]).
//! 3. Decoded, it is `{"inferNodes":[{"url":"https://api3.qoder.sh",…}], …}`.
//!
//! muta adopts `inferNodes[0].url` — and only after it passes the strict
//! allowlist below; a hostile or malformed region map must never redirect
//! muta's traffic. Anything that fails syncs degrades to the pinned
//! `MODEL_PROVIDER_SPEC.root_url` (ADR-0227: failure never diminishes a
//! connection).

use crate::http::{Http, Request};
use serde::Deserialize;

/// The center surface's election endpoint (international line; v5 is the
/// current shape, v3 the legacy alias — both verified live).
pub const REGION_ENDPOINTS_URL: &str =
    "https://center.qoder.sh/algo/api/v5/service/region/endpoints";

/// Only these host suffixes may be adopted as an inference endpoint. The
/// region map is server-issued, but the URL is consumed as a transport base —
/// validating here keeps a compromised center response from redirecting
/// inference to an arbitrary host.
fn is_allowed_infer_host(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#', ':'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    !host.is_empty() && (host == "qoder.sh" || host.ends_with(".qoder.sh"))
}

/// One role entry in the decoded region map.
#[derive(Debug, Deserialize)]
pub struct RegionNode {
    pub url: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub r#type: Option<String>,
}

/// The decoded region-endpoints payload (roles muta consumes; extra roles
/// such as `centerNodes`/`codebase` are ignored).
#[derive(Debug, Deserialize)]
pub struct RegionEndpoints {
    #[serde(default, alias = "inferNodes")]
    pub infer_nodes: Vec<RegionNode>,
}

/// Fetch and decode the region map, returning the elected inference endpoint
/// (e.g. `https://api3.qoder.sh`). `Err` carries the reason the map was not
/// adopted — the caller falls back to the pin.
pub async fn elect_infer_endpoint(
    client: &Http,
    bearer: &str,
) -> Result<String, String> {
    elect_infer_endpoint_at(client, REGION_ENDPOINTS_URL, bearer).await
}

/// Test-injectable variant with an explicit election endpoint.
pub async fn elect_infer_endpoint_at(
    client: &Http,
    endpoint: &str,
    bearer: &str,
) -> Result<String, String> {
    let request = Request::new(netune::Method::GET, endpoint)
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {bearer}"));
    let response = client
        .send(request)
        .await
        .map_err(|e| format!("region sync failed: {e}"))?;
    if !response.status.is_success() {
        return Err(format!(
            "region sync returned HTTP {}",
            response.status.as_u16()
        ));
    }
    // The body is QoderEncoding-encoded, exactly like the inference body
    // (verified live). A plain-JSON body fails the decode and is rejected —
    // shape drift is surfaced, not papered over.
    let raw = crate::qoder::wire_decode(&response.body)
        .ok_or_else(|| "region body is not QoderEncoding".to_string())?;
    let text = String::from_utf8(raw).map_err(|_| "region body is not UTF-8".to_string())?;
    let region: RegionEndpoints = serde_json::from_str(&text)
        .map_err(|e| format!("region payload malformed: {e}"))?;
    let elected = region
        .infer_nodes
        .first()
        .map(|node| node.url.trim().to_string())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "region payload has no inferNodes".to_string())?;
    if !is_allowed_infer_host(&elected) {
        return Err(format!("elected endpoint rejected by allowlist: {elected}"));
    }
    Ok(elected.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_region_payload(json: &str) -> String {
        crate::qoder::wire_encode(json.as_bytes())
    }

    #[tokio::test]
    async fn elects_the_first_infer_node() {
        let body = encode_region_payload(
            r#"{"inferNodes":[{"url":"https://api3.qoder.sh","type":"public"}]}"#,
        );
        let (addr, _keep) = spawn_scripted_server(vec![(200, body)]).await;
        let client = Http::control_plane().unwrap();
        let url = format!("http://{addr}/region/endpoints");
        let elected = elect_infer_endpoint_at(&client, &url, "dt-token")
            .await
            .unwrap();
        assert_eq!(elected, "https://api3.qoder.sh");
    }

    #[tokio::test]
    async fn non_success_is_a_soft_failure() {
        let (addr, _keep) = spawn_scripted_server(vec![(500, "boom".to_string())]).await;
        let client = Http::control_plane().unwrap();
        let url = format!("http://{addr}/region/endpoints");
        assert!(elect_infer_endpoint_at(&client, &url, "dt-token").await.is_err());
    }

    #[tokio::test]
    async fn plain_json_body_is_rejected_shape_drift() {
        let (addr, _keep) = spawn_scripted_server(vec![(
            200,
            r#"{"inferNodes":[{"url":"https://api3.qoder.sh"}]}"#.to_string(),
        )])
        .await;
        let client = Http::control_plane().unwrap();
        let url = format!("http://{addr}/region/endpoints");
        let error = elect_infer_endpoint_at(&client, &url, "dt-token")
            .await
            .unwrap_err();
        assert!(error.contains("QoderEncoding"), "{error}");
    }

    #[tokio::test]
    async fn allowlist_rejects_off_domain_urls() {
        for hostile in [
            "https://evil.example.com/algo",
            "http://api3.qoder.sh",
            "https://qoder.sh.evil.com",
            "not a url",
        ] {
            let body = encode_region_payload(&format!(
                r#"{{"inferNodes":[{{"url":"{hostile}"}}]}}"#
            ));
            let (addr, _keep) = spawn_scripted_server(vec![(200, body)]).await;
            let client = Http::control_plane().unwrap();
            let url = format!("http://{addr}/region/endpoints");
            let error = elect_infer_endpoint_at(&client, &url, "dt-token")
                .await
                .unwrap_err();
            assert!(error.contains("allowlist"), "{hostile}: {error}");
        }
    }

    #[tokio::test]
    async fn empty_infer_nodes_is_a_soft_failure() {
        let body = encode_region_payload(r#"{"inferNodes":[]}"#);
        let (addr, _keep) = spawn_scripted_server(vec![(200, body)]).await;
        let client = Http::control_plane().unwrap();
        let url = format!("http://{addr}/region/endpoints");
        assert!(elect_infer_endpoint_at(&client, &url, "dt-token").await.is_err());
    }

    /// Minimal HTTP test server serving a scripted list of (status, body)
    /// responses, one per connection, repeating the last one indefinitely.
    async fn spawn_scripted_server(
        script: Vec<(u16, String)>,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut script = script;
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 2048];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                let step = if script.len() > 1 {
                    script.remove(0)
                } else {
                    script[0].clone()
                };
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
        (addr, task)
    }
}
