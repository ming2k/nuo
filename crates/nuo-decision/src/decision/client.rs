//! Decision client executing System One evaluations over the shared wire transport.

use crate::decision::endpoint::DecisionEndpoint;
use crate::decision::types::{DecisionRequest, DecisionResponse};
use nuo_inference_wire::client::WireClient;
use nuo_inference_wire::error::{Result, WireError};

/// Specialized client for executing typed System One decision evaluations.
///
/// Transport sharing: the inner [`WireClient`] converges on the process's one
/// netune connection pool (per ADR-0084 S2 the pool stays in the wire crate;
/// this engine reuses it instead of fragmenting a second pool).
#[derive(Clone, Default)]
pub struct DecisionClient {
    wire: WireClient,
}

#[cfg(feature = "netune")]
use netune::RequestHead;

impl DecisionClient {
    /// Creates a new decision client with standard connection pooling.
    pub fn new() -> Self {
        Self {
            wire: WireClient::new(),
        }
    }

    /// Creates a decision client sharing an existing [`WireClient`] connection pool.
    pub fn with_wire(wire: WireClient) -> Self {
        Self { wire }
    }

    /// Evaluates a [`DecisionRequest`] against target [`DecisionEndpoint`].
    pub async fn decide(
        &self,
        endpoint: &DecisionEndpoint,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse> {
        let token = endpoint.resolve_api_key().await?;
        let (_url, _headers, _body) =
            crate::decision::protocol::build_request(endpoint, &token, request);

        #[cfg(feature = "netune")]
        {
            let url = _url;
            let headers = _headers;
            let body = _body;
            let (target, path) = netune::Target::from_url(&url)
                .map_err(|err| WireError::Http(format!("invalid target URL `{url}`: {err}")))?;

            let mut head = RequestHead::new(http::Method::POST, path);
            for (name, value) in headers.iter() {
                if let Ok(value_str) = value.to_str() {
                    head = head.with_header(name.as_str(), value_str);
                }
            }

            let body_bytes = serde_json::to_vec(&body)
                .map_err(|err| WireError::Protocol(format!("failed to serialize body: {err}")))?;

            let recorder =
                std::sync::Arc::new(std::sync::Mutex::new(netune_trace::Recorder::start(1024)));

            let response = self
                .wire
                .netune_client()
                .send(
                    &target,
                    recorder,
                    head,
                    Some(bytes::Bytes::from(body_bytes)),
                )
                .await
                .map_err(|err| WireError::Http(format!("Netune transport error: {err}")))?;

            let status = response.head.status;
            let mut body_reader = response.body;
            let mut full_body = Vec::new();
            while let Ok(Some(chunk)) = body_reader.next_chunk().await {
                full_body.extend_from_slice(&chunk);
            }

            if !status.is_success() {
                let error_text = String::from_utf8_lossy(&full_body).to_string();
                return Err(WireError::ApiError {
                    status: status.as_u16(),
                    message: error_text,
                });
            }

            let json_val: serde_json::Value = serde_json::from_slice(&full_body)
                .map_err(|err| WireError::Protocol(format!("invalid JSON response: {err}")))?;

            crate::decision::protocol::parse_response(endpoint, &json_val)
        }

        #[cfg(not(feature = "netune"))]
        {
            let _ = (endpoint, request);
            Err(WireError::Http(
                "No HTTP transport feature enabled (enable `netune`)".into(),
            ))
        }
    }
}
