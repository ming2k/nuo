//! System One discriminative primitives, typed questions/answers, and wire drivers.
//!
//! Provides machine-native, calibrated probabilistic decision and scoring support for
//! non-generative models such as TypeSafe Jev.

pub mod endpoint;
pub mod protocol;
pub mod types;

pub use endpoint::DiscriminativeEndpoint;
pub use protocol::DiscriminativeProtocol;
pub use types::{
    Answer, ChoiceAnswer, ChoiceQuestion, DiscriminativeRequest, DiscriminativeResponse,
    DiscriminativeUsage, NoulAnswer, NoulQuestion, Question, ScoreAnswer, ScoreQuestion,
};

use crate::client::WireClient;
use crate::error::{Result, WireError};

/// Evaluates a [`DiscriminativeRequest`] against `endpoint` using the shared [`WireClient`] transport pool.
pub async fn execute(
    client: &WireClient,
    endpoint: &DiscriminativeEndpoint,
    request: &DiscriminativeRequest,
) -> Result<DiscriminativeResponse> {
    let token = endpoint.resolve_api_key().await?;
    let (_url, _headers, _body) = protocol::build_request(endpoint, &token, request);

    #[cfg(feature = "netune")]
    {
        use netune::RequestHead;

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

        let response = client
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

        protocol::parse_response(endpoint, &json_val)
    }

    #[cfg(not(feature = "netune"))]
    {
        let _ = (client, endpoint, request);
        Err(WireError::Http(
            "No HTTP transport feature enabled (enable `netune`)".into(),
        ))
    }
}
