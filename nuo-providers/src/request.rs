//! Request construction, without a third-party client (ADR-0200).
//!
//! The protocol adapters describe a request as `method + url + headers + body`.
//! That description used to be a `reqwest::RequestBuilder`; it is now this type,
//! so the *production* dependency graph contains no HTTP implementation at all —
//! execution goes through [`crate::Egress`], and only the differential oracle
//! (`reqwest-oracle`, dev/test builds) still links `reqwest`.
//!
//! The surface deliberately mirrors the four builder calls the adapters use
//! (`header`, `headers`, `json`, `timeout`) so the migration is a constructor
//! swap, not a rewrite.

use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use nuo_contracts::{ProviderError, ProviderErrorKind};

use crate::egress::RequestParts;

/// A request under construction.
#[derive(Debug, Clone)]
pub struct RequestBuilder {
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Bytes>,
    timeout: Option<Duration>,
    telemetry: nuo_contracts::TransportTelemetry,
}

impl RequestBuilder {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
            telemetry: nuo_contracts::TransportTelemetry::new(),
        }
    }

    /// Attach the attempt's telemetry handle (ADR-0232).
    ///
    /// The transport publishes this attempt's timings into `telemetry`, which
    /// belongs to whoever issued the attempt. A builder left without one carries
    /// a fresh empty handle, so its timings are simply unobserved rather than
    /// attributed somewhere unintended.
    pub fn with_telemetry(mut self, telemetry: nuo_contracts::TransportTelemetry) -> Self {
        self.telemetry = telemetry;
        self
    }

    /// Add a header. An invalid name or value is a programming error in the
    /// caller's own literal; it is dropped rather than panicking, exactly as the
    /// previous builder's infallible `header` did for static values.
    pub fn header<N, V>(mut self, name: N, value: V) -> Self
    where
        N: TryInto<HeaderName>,
        N::Error: std::fmt::Debug,
        V: TryInto<HeaderValue>,
        V::Error: std::fmt::Debug,
    {
        if let (Ok(name), Ok(value)) = (name.try_into(), value.try_into()) {
            self.headers.insert(name, value);
        }
        self
    }

    /// Merge a header map (vendor adapters build one up front).
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        for (name, value) in headers.iter() {
            self.headers.insert(name.clone(), value.clone());
        }
        self
    }

    /// Set a JSON body and its content type.
    pub fn json(mut self, value: &serde_json::Value) -> Self {
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        match serde_json::to_vec(value) {
            Ok(body) => self.body = Some(Bytes::from(body)),
            Err(_) => self.body = None,
        }
        self
    }

    /// Set a raw body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Stamp an overall deadline. Never applied to streaming requests by the
    /// callers; the transport honours it for the body read too.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Freeze into the transport-neutral request the [`crate::Egress`] seam
    /// takes.
    pub fn build(self, label: &'static str) -> Result<RequestParts, ProviderError> {
        if self.url.is_empty() {
            return Err(ProviderError::new(
                label,
                ProviderErrorKind::InvalidRequest,
                "request has no url",
            ));
        }
        Ok(RequestParts {
            label,
            method: self.method,
            url: self.url,
            headers: self.headers,
            body: self.body,
            timeout: self.timeout,
            telemetry: self.telemetry,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_body_sets_its_content_type_and_encodes() {
        let parts = RequestBuilder::new(Method::POST, "https://example.invalid/v1")
            .header("authorization", "Bearer x")
            .json(&serde_json::json!({ "a": 1 }))
            .build("Test")
            .expect("build");
        assert_eq!(
            parts.headers.get(http::header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
        assert_eq!(parts.headers.get("authorization").unwrap(), "Bearer x");
        assert_eq!(&parts.body.unwrap()[..], b"{\"a\":1}");
        assert!(parts.timeout.is_none());
    }

    #[test]
    fn a_header_map_merges_and_a_deadline_is_carried() {
        let mut map = HeaderMap::new();
        map.insert("x-a", HeaderValue::from_static("1"));
        let parts = RequestBuilder::new(Method::GET, "https://example.invalid/v1")
            .headers(map)
            .timeout(Duration::from_secs(3))
            .build("Test")
            .expect("build");
        assert_eq!(parts.headers.get("x-a").unwrap(), "1");
        assert_eq!(parts.timeout, Some(Duration::from_secs(3)));
        assert!(parts.body.is_none());
    }

    #[test]
    fn an_empty_url_is_refused_rather_than_sent() {
        let error = RequestBuilder::new(Method::GET, "")
            .build("Test")
            .expect_err("no url");
        assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    }
}
