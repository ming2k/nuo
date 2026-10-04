//! The web tools' HTTP handle, on the owned transport (ADR-0200).
//!
//! Every web tool — search backends and the page reader — used to thread a
//! `reqwest::Client` through its signatures. They now share this handle, which
//! is the same transport the model path uses: platform trust roots, the
//! direct connections, content-encoding decoding, and **no automatic redirects**
//! (the SSRF guard re-validates every hop itself, so the transport must not
//! follow one behind its back).

use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use nuo_wire::WebConfig;
use netune::{
    Client, ClientConfig, Pool, RequestHead, Response, Target, TcpConnector, TlsConnector,
};

/// A response whose body has been read.
#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// Direct web client with public-address confinement.
pub struct WebHttp(Box<Client<TlsConnector<TcpConnector>>>);

impl WebHttp {
    /// Build the handle from resolved `[web]` behavior.
    pub fn new(config: &WebConfig) -> Result<Self, String> {
        let _timeout = Duration::from_secs(config.timeout_secs.max(1));
        let client_config = ClientConfig {
            user_agent: crate::client::MOZILLA_UA.to_string(),
            // The SSRF guard owns redirect policy: it must see every hop.
            max_redirects: 0,
            ..Default::default()
        };
        let connector = TlsConnector::platform(TcpConnector::strict_public())
            .map_err(|error| format!("Failed to build HTTP client: {error}"))?;
        Ok(Self(Box::new(Client::new(
            connector,
            Pool::default(),
            client_config,
        ))))
    }

    fn timeout(&self) -> Duration {
        // The deadline is stamped per request by the callers through
        // `WebRequest::timeout`; this is the fallback for the convenience
        // methods.
        Duration::from_secs(30)
    }

    /// Open a request and hand back the streaming response.
    ///
    /// `timeout` bounds connect + head + body read: these are tool calls, not
    /// model streams, so a whole-request deadline is the right shape.
    pub async fn open(&self, request: WebRequest, timeout: Duration) -> Result<Response, String> {
        let (target, path) =
            Target::from_url(&request.url).map_err(|error| format!("Invalid url: {error}"))?;
        let mut head = RequestHead::new(request.method, path);
        for (name, value) in request.headers.iter() {
            if let Ok(value) = value.to_str() {
                head = head.with_header(name.as_str(), value);
            }
        }
        let send = async {
            self.0
                .request(&target, head, request.body)
                .await
                .map_err(|error| error.to_string())
        };
        tokio::time::timeout(timeout, send)
            .await
            .map_err(|_| format!("Request to {} timed out", request.url))?
    }

    /// GET a URL and read the whole body as text.
    pub async fn get(&self, url: &str, headers: HeaderMap) -> Result<Reply, String> {
        self.read(WebRequest::get(url).headers(headers)).await
    }

    /// POST a JSON body and read the whole body as text.
    pub async fn post_json(
        &self,
        url: &str,
        headers: HeaderMap,
        body: &serde_json::Value,
    ) -> Result<Reply, String> {
        self.read(WebRequest::post_json(url, headers, body)).await
    }

    /// POST a form body and read the whole body as text.
    pub async fn post_form(
        &self,
        url: &str,
        headers: HeaderMap,
        form: &[(&str, &str)],
    ) -> Result<Reply, String> {
        self.read(WebRequest::post_form(url, headers, form)).await
    }

    async fn read(&self, request: WebRequest) -> Result<Reply, String> {
        let timeout = request.timeout;
        let mut response = self.open(request, timeout).await?;
        let status = response.head.status;
        let headers = response.head.headers.clone();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .body
            .next_chunk()
            .await
            .map_err(|error| format!("Failed to read body: {error}"))?
        {
            body.extend_from_slice(&chunk);
        }
        Ok(Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }

    /// Convenience for the deadline the web tools use.
    pub fn default_timeout(&self) -> Duration {
        self.timeout()
    }
}

/// A web-tool request under construction.
pub struct WebRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    pub timeout: Duration,
}

impl WebRequest {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: None,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::GET, url)
    }

    pub fn headers(mut self, headers: HeaderMap) -> Self {
        for (name, value) in headers.iter() {
            self.headers.insert(name.clone(), value.clone());
        }
        self
    }

    pub fn post_json(url: impl Into<String>, headers: HeaderMap, body: &serde_json::Value) -> Self {
        let mut request = Self::new(Method::POST, url).headers(headers);
        request.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        request.body = serde_json::to_vec(body).ok().map(Bytes::from);
        request
    }

    pub fn post_form(url: impl Into<String>, headers: HeaderMap, form: &[(&str, &str)]) -> Self {
        let mut request = Self::new(Method::POST, url).headers(headers);
        request.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        let mut encoded = String::new();
        for (index, (name, value)) in form.iter().enumerate() {
            if index > 0 {
                encoded.push('&');
            }
            encoded.push_str(&percent_encode(name));
            encoded.push('=');
            encoded.push_str(&percent_encode(value));
        }
        request.body = Some(Bytes::from(encoded));
        request
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Append percent-encoded query parameters to a URL.
pub fn with_query(url: &str, params: &[(&str, &str)]) -> String {
    let mut out = String::from(url);
    out.push(if out.contains('?') { '&' } else { '?' });
    for (index, (name, value)) in params.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(name));
        out.push('=');
        out.push_str(&percent_encode(value));
    }
    out
}

/// Resolve a `Location` value against the URL that produced it.
///
/// Deliberately minimal (scheme-relative, absolute-path, and same-directory
/// references); dot segments are left to the server. The SSRF guard re-validates
/// the resulting host on the next hop, which is the property that matters.
pub fn resolve_redirect(base: &str, location: &str) -> Option<String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Some(location.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    if let Some(rest) = location.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}"));
    }
    let (authority, base_path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if location.starts_with('/') {
        return Some(format!("{scheme}://{authority}{location}"));
    }
    let directory = base_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    Some(format!("{scheme}://{authority}{directory}/{location}"))
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parameters_are_encoded() {
        assert_eq!(
            with_query(
                "https://example.invalid/s",
                &[("q", "a b+c"), ("kl", "us-en")]
            ),
            "https://example.invalid/s?q=a+b%2Bc&kl=us-en"
        );
        assert_eq!(
            with_query("https://example.invalid/s?x=1", &[("q", "z")]),
            "https://example.invalid/s?x=1&q=z"
        );
    }

    #[test]
    fn a_form_request_sets_its_content_type_and_body() {
        let request = WebRequest::post_form(
            "https://example.invalid/s",
            HeaderMap::new(),
            &[("q", "hello"), ("kl", "us-en")],
        );
        assert_eq!(
            request.headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/x-www-form-urlencoded"
        );
        assert_eq!(&request.body.unwrap()[..], b"q=hello&kl=us-en");
    }
}
