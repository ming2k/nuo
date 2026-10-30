//! OpenCode Go allowance fetcher (`[INV-PROV-08]` typed quota port).
//!
//! Query endpoint: `GET https://opencode.ai/zen/go/v1/usage`
//! (Wire specification: `docs/reference/openapi/opencode-go.yml` `/zen/go/v1/usage`).
//!
//! # What this endpoint is
//!
//! OpenCode Go is a *subscription*, but it is not unmetered: the plan meters
//! three rolling allowances — a 5-hour rolling window, a calendar week, and the
//! billing month. That endpoint is the only place those counters are published;
//! the console's dollar figure is a different (and, from here, unreachable)
//! surface.
//!
//! # Reporting discipline (ADR-0036 `[INV-QUOTA-01]`, ADR-0014 `[INV-LANE-05]`)
//!
//! This module is a **pure projection** of the control plane's answer: it
//! reports the windows, floors, and resets upstream published and never
//! recomputes a rate, extrapolates a remaining amount, or derives a dollar
//! figure from them. Upstream owns the metering arithmetic; the client renders.
//!
//! # Canonical shape only
//!
//! The deployed route answers
//! `{"usage": {"rolling": W, "weekly": W, "monthly": W}}` with each window
//! `{"status": "ok"|"rate-limited", "percent": 0..=100, "resetsAt": <RFC 3339>}`
//! (`percent` is *used*, floored).
//!
//! The pre-release proposal shape (`rollingUsage` / `weeklyUsage` /
//! `monthlyUsage` at the envelope root, `usagePercent` inside a window) was
//! never served in production and is deliberately **not** accepted here. A
//! retired spelling gets no parser: aliases accumulate into a second contract
//! that outlives its source, and a tolerant reader silently masks drift instead
//! of reporting it. A response this module cannot read is drift, and drift is
//! surfaced as an error (the `404`/shape-change path), not smoothed over.
use nuo_wire::{
    PeriodicQuota, ProviderQuotaData, ProviderUsage, QuotaWindowBucket, QuotaWindowKind,
    UsageMetric,
};
use serde::Deserialize;

/// The three allowances an OpenCode Go subscription meters, as upstream names
/// them. The ordering is the wire ordering and is preserved in the projection.
const WINDOWS: [(WindowName, QuotaWindowKind, &str); 3] = [
    (
        WindowName::Rolling,
        QuotaWindowKind::Rolling5Hour,
        "5-Hour Window",
    ),
    (WindowName::Weekly, QuotaWindowKind::Weekly, "Weekly Window"),
    (
        WindowName::Monthly,
        QuotaWindowKind::Monthly,
        "Monthly Window",
    ),
];

/// A window on the Go allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowName {
    Rolling,
    Weekly,
    Monthly,
}

/// One metered window as upstream reports it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GoWindow {
    /// `ok` while the window has room, `rate-limited` once it does not.
    #[serde(default)]
    status: Option<String>,
    /// Fraction of the window's allowance **already spent**, floored to a
    /// whole percent upstream. Absent when upstream publishes no reading.
    #[serde(default)]
    percent: Option<f64>,
    /// RFC 3339 instant at which the window resets.
    #[serde(default, rename = "resetsAt")]
    resets_at: Option<String>,
}

impl GoWindow {
    /// Whether upstream reports this window as refusing requests.
    ///
    /// The absolute judgement: `rate-limited` means spent, whatever `percent`
    /// says. Upstream floors `percent`, so a genuinely exhausted window can
    /// legitimately report `99` — reading the percentage alone would render a
    /// blocked window as having room left.
    fn is_rate_limited(&self) -> bool {
        self.status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case("rate-limited"))
    }

    /// The used percentage upstream actually stated, or `None` when it stated
    /// none.
    ///
    /// `rate-limited` is an absolute statement of exhaustion, so it reads as
    /// `100` whatever the floored `percent` says: upstream floors the number, so
    /// a genuinely blocked window can legitimately report `99`, and trusting the
    /// percentage alone would render a blocked window as having room left.
    ///
    /// The value is clamped because the upstream contract (`0..=100`) is a
    /// promise about *its* arithmetic, not a guarantee about bytes that reached
    /// us through a proxy; a mangled value must not become a negative badge.
    fn observed_used_percent(&self) -> Option<f64> {
        if self.is_rate_limited() {
            return Some(100.0);
        }
        self.percent
            .filter(|percent| percent.is_finite())
            .map(|percent| percent.clamp(0.0, 100.0))
    }

    /// The used fraction this window contributes, in `0.0..=1.0`.
    ///
    /// A window with no reading contributes nothing rather than pretending to be
    /// untouched.
    fn used_fraction(&self) -> f32 {
        self.observed_used_percent()
            .map_or(0.0, |used| (used / 100.0) as f32)
    }

    /// The remaining percentage to show a reader, in `0..=100`.
    ///
    /// `None` when upstream published no reading: an absent measurement is
    /// "not stated", never "100% left". Derived from the same truth
    /// [`Self::used_fraction`] uses, so the badge and the bar cannot disagree.
    fn remaining_percent(&self) -> Option<u32> {
        self.observed_used_percent()
            .map(|used| (100.0 - used).round().clamp(0.0, 100.0) as u32)
    }
}

/// The Go allowance envelope (`{"usage": {...}}`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GoUsageEnvelope {
    usage: GoUsageWindows,
}

#[derive(Debug, Clone, Deserialize)]
struct GoUsageWindows {
    #[serde(default)]
    rolling: Option<GoWindow>,
    #[serde(default)]
    weekly: Option<GoWindow>,
    #[serde(default)]
    monthly: Option<GoWindow>,
}

impl GoUsageWindows {
    fn get(&self, name: WindowName) -> Option<&GoWindow> {
        match name {
            WindowName::Rolling => self.rolling.as_ref(),
            WindowName::Weekly => self.weekly.as_ref(),
            WindowName::Monthly => self.monthly.as_ref(),
        }
    }
}

/// Reads the OpenCode Go allowance for one API key.
///
/// Declared as a spec's [`nuo_provider::QuotaPort::OpencodeGo`], so dispatch
/// reaches it through the provider's own declaration and never by matching a
/// provider id or base-URL substring (`[INV-PROV-08]`).
pub struct OpencodeGoUsageFetcher;

impl OpencodeGoUsageFetcher {
    pub async fn fetch_usage(
        &self,
        client: &nuo_provider::transport::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = usage_endpoint(base_url)?;

        let auth = format!("Bearer {api_key}");
        let resp = client
            .get(
                &endpoint,
                &[
                    ("authorization", auth.as_str()),
                    ("accept", "application/json"),
                ],
            )
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.is_success() {
            return Err(error_for_status(resp.status.as_u16(), &resp.body));
        }

        let envelope: GoUsageEnvelope = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse the OpenCode Go usage response: {e}"))?;

        Ok(parse_go_usage(envelope))
    }
}

/// Derive the usage endpoint from a connection's base URL.
///
/// The caller supplies the *connection's* `base_url`, which is whatever its
/// default channel reaches and therefore depends on the active model's wire
/// protocol: `https://opencode.ai/zen/go/v1/chat/completions` for an OpenAI
/// model, but `https://opencode.ai/inference/go/anthropic/v1/messages` for a
/// Claude model and `.../inference/go/google/v1beta` for a Gemini one (the
/// spec's `protocol_roots`). Anchoring on the origin is what keeps all three
/// collapsing onto the single allowance route, since the allowance is billed
/// per account, not per protocol.
///
/// A URL with no usable origin cannot name the surface at all; that is reported
/// rather than papered over with a hard-coded fallback host.
fn usage_endpoint(base_url: &str) -> Result<String, String> {
    // The allowance route lives directly under the Go version root.
    const GO_USAGE_PATH: &str = "/zen/go/v1/usage";

    let trimmed = base_url.trim();
    let (scheme, rest) = if let Some(rest) = trimmed.strip_prefix("https://") {
        ("https://", rest)
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        ("http://", rest)
    } else {
        // A URL with no scheme is not a connection root this client can use;
        // saying so beats silently prepending a guess.
        return Err(format!("`{base_url}` is not an absolute HTTP(S) URL"));
    };
    let host = rest.split('/').next().unwrap_or_default().trim();
    if host.is_empty() {
        return Err(format!("`{base_url}` carries no host"));
    }

    Ok(format!("{scheme}{host}{GO_USAGE_PATH}"))
}

/// Turn a non-success status into the reader's explanation.
///
/// `403` is an **entitlement** answer, not an authentication failure: the key
/// is valid but its workspace holds no Go subscription, so there is no
/// allowance to report. Upstream words it `EntitlementError`
/// (`"OpenCode Go subscription required."`). It is surfaced as an error — a
/// configured `opencode-go` connection that cannot read its own allowance is a
/// misconfiguration the operator must see, not a provider that "does not
/// support quota" — while its wording keeps it distinguishable from a rejected
/// key (`401`).
fn error_for_status(status: u16, body: &str) -> String {
    // Truncate by *chars*, never by bytes: an upstream error body is arbitrary
    // UTF-8, and a byte slice would panic on a multi-byte boundary right where
    // we are trying to report a failure.
    let detail = body.trim();
    let detail: String = detail.chars().take(240).collect();
    match status {
        403 => format!("OpenCode Go subscription required (HTTP 403 EntitlementError): {detail}"),
        401 => format!("OpenCode Go rejected the API key (HTTP 401): {detail}"),
        404 => format!(
            "OpenCode Go usage endpoint not found (HTTP 404) — the route has moved: {detail}"
        ),
        _ => format!("HTTP {status}: {detail}"),
    }
}

/// Project the metered windows into the generic usage model.
///
/// Infallible by construction: the envelope is deserialized and every window
/// field is optional, so an empty-but-valid envelope yields an empty-but-valid
/// projection. An envelope that names no window at all is still `Available` —
/// the key is valid and the allowance is simply nil — rather than an error.
fn parse_go_usage(envelope: GoUsageEnvelope) -> ProviderUsage {
    let mut buckets = Vec::with_capacity(WINDOWS.len());
    let mut metrics = Vec::with_capacity(WINDOWS.len());
    let mut lowest_remaining: Option<u32> = None;

    for (name, kind, label) in WINDOWS {
        let Some(window) = envelope.usage.get(name) else {
            continue;
        };

        let remaining = window.remaining_percent();
        lowest_remaining = match (lowest_remaining, remaining) {
            (Some(low), Some(remaining)) => Some(low.min(remaining)),
            (low, None) => low,
            (None, Some(remaining)) => Some(remaining),
        };

        // The bucket carries exactly what the surfaces render: the used fraction
        // (progress bar), the reset (countdown), and the window kind (ranking +
        // label). `used_amount` / `total_limit` / `unit` are deliberately left
        // empty: no surface reads them, and the plan meters a percentage rather
        // than an amount, so filling them would publish a unit upstream never
        // stated.
        buckets.push(QuotaWindowBucket {
            window: Some(kind),
            label: label.to_string(),
            group: None,
            used_fraction: window.used_fraction(),
            used_amount: None,
            total_limit: None,
            unit: None,
            reset_at_ms: window.resets_at.as_deref().and_then(epoch_ms_from_rfc3339),
            reset_time_str: window.resets_at.clone(),
        });

        // An unpublished reading is named as such: rendering it as a percentage
        // would assert a measurement upstream never made.
        let value = match window.remaining_percent() {
            Some(remaining) if window.is_rate_limited() => {
                format!(
                    "{remaining}% ({})",
                    window.status.as_deref().unwrap_or("rate-limited")
                )
            }
            Some(remaining) => format!("{remaining}%"),
            None => "not reported".to_string(),
        };
        metrics.push(UsageMetric {
            label: label.to_string(),
            value,
            unit: None,
        });
    }

    // The badge is the window that runs out first — the one fact an ambient
    // gauge is for. `None` when upstream published no readable window.
    let primary_balance = lowest_remaining.map(|remaining| format!("{remaining}%"));

    ProviderUsage {
        // The surface's own label ("OpenCode Go") already names the plan; the
        // allowance response carries no tier name, and echoing a generic
        // upstream service title in the plan badge is the defect recorded in the
        // changelog as "plan names that are not tier names".
        plan: None,
        description: Some(
            "OpenCode Go meters rolling allowances: 5-hour, weekly, and monthly.".to_string(),
        ),
        quota: Some(ProviderQuotaData::Periodic(PeriodicQuota { buckets })),
        primary_balance,
        metrics,
        updated_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .ok(),
    }
}

/// Parse an RFC 3339 instant into epoch milliseconds.
///
/// An unparseable instant yields `None` so the surfaces fall back to the raw
/// string (`reset_time_str`) instead of the bucket silently losing its reset.
fn epoch_ms_from_rfc3339(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .and_then(|dt| u64::try_from(dt.timestamp_millis()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(status: &str, percent: f64, resets_at: &str) -> GoWindow {
        GoWindow {
            status: Some(status.to_string()),
            percent: Some(percent),
            resets_at: Some(resets_at.to_string()),
        }
    }

    fn envelope(
        rolling: Option<GoWindow>,
        weekly: Option<GoWindow>,
        monthly: Option<GoWindow>,
    ) -> GoUsageEnvelope {
        GoUsageEnvelope {
            usage: GoUsageWindows {
                rolling,
                weekly,
                monthly,
            },
        }
    }

    #[test]
    fn parses_the_production_envelope_and_projects_every_window() {
        let body = r#"{
            "usage": {
                "rolling": {"status": "ok", "percent": 42, "resetsAt": "2026-10-24T17:00:00.000Z"},
                "weekly": {"status": "ok", "percent": 18, "resetsAt": "2026-10-26T00:00:00.000Z"},
                "monthly": {"status": "ok", "percent": 7, "resetsAt": "2026-11-01T00:00:00.000Z"}
            }
        }"#;
        let envelope: GoUsageEnvelope =
            serde_json::from_str(body).expect("canonical envelope parses");
        let usage = parse_go_usage(envelope);

        let Some(ProviderQuotaData::Periodic(periodic)) = usage.quota else {
            panic!("the Go allowance is periodic, not a balance");
        };
        assert_eq!(periodic.buckets.len(), 3);

        // Wire order is preserved: shortest window first, as the dashboard ranks.
        assert_eq!(
            periodic.buckets[0].window,
            Some(QuotaWindowKind::Rolling5Hour)
        );
        assert_eq!(periodic.buckets[1].window, Some(QuotaWindowKind::Weekly));
        assert_eq!(periodic.buckets[2].window, Some(QuotaWindowKind::Monthly));

        assert!((periodic.buckets[0].used_fraction - 0.42).abs() < 1e-6);
        assert!((periodic.buckets[1].used_fraction - 0.18).abs() < 1e-6);
        assert!((periodic.buckets[2].used_fraction - 0.07).abs() < 1e-6);

        // `percent` is *used*; the badge reports remaining. It names the window
        // that runs out first — the lowest remaining (58% here, the 5-hour
        // window at 42% used), which is the number that constrains the account.
        assert_eq!(usage.primary_balance.as_deref(), Some("58%"));
        assert_eq!(usage.metrics[0].value, "58%");
        assert_eq!(usage.metrics[1].value, "82%");
        assert_eq!(usage.metrics[2].value, "93%");

        // The bucket's reset is carried both ways: epoch for countdowns, raw for display.
        assert!(periodic.buckets[0].reset_at_ms.is_some());
        assert_eq!(
            periodic.buckets[0].reset_time_str.as_deref(),
            Some("2026-10-24T17:00:00.000Z")
        );
    }

    #[test]
    fn rate_limited_saturates_even_when_the_floored_percent_says_ninety_nine() {
        // Upstream floors `percent`, so an exhausted window can read `99`.
        // The status is the absolute judgement; the percentage alone would
        // render a blocked window as still having room.
        let usage = parse_go_usage(envelope(
            Some(window("rate-limited", 99.0, "2026-10-24T17:00:00Z")),
            Some(window("ok", 50.0, "2026-10-26T00:00:00Z")),
            None,
        ));

        let Some(ProviderQuotaData::Periodic(periodic)) = &usage.quota else {
            panic!("the Go allowance is periodic");
        };
        assert_eq!(periodic.buckets[0].used_fraction, 1.0);
        assert_eq!(usage.metrics[0].value, "0% (rate-limited)");
        // The exhausted window is the one that runs out first.
        assert_eq!(usage.primary_balance.as_deref(), Some("0%"));
    }

    #[test]
    fn an_absent_window_is_omitted_rather_than_reported_as_zero_used() {
        // Only the rolling window was published. The weekly and monthly rows
        // must not appear at all: rendering an unpublished window as "0% used"
        // would claim allowance upstream never stated.
        let usage = parse_go_usage(envelope(
            Some(window("ok", 10.0, "2026-10-24T17:00:00Z")),
            None,
            None,
        ));

        let Some(ProviderQuotaData::Periodic(periodic)) = &usage.quota else {
            panic!("the Go allowance is periodic");
        };
        assert_eq!(periodic.buckets.len(), 1);
        assert_eq!(usage.metrics.len(), 1);
    }

    #[test]
    fn an_empty_but_valid_envelope_is_available_not_an_error() {
        // A valid key whose plan publishes nothing is a real state. It must not
        // read as a failed query (the surfaces would show a red error row).
        let usage = parse_go_usage(envelope(None, None, None));
        assert!(usage.quota.is_some());
        assert_eq!(usage.primary_balance, None);

        let Some(ProviderQuotaData::Periodic(periodic)) = &usage.quota else {
            panic!("the Go allowance is periodic");
        };
        assert!(periodic.buckets.is_empty());
    }

    #[test]
    fn out_of_range_percentages_are_clamped_never_trusted_verbatim() {
        // Upstream contract is 0..=100, but a transport or proxy that mangles it
        // must not produce a negative remaining badge or a bar past full.
        let usage = parse_go_usage(envelope(
            Some(window("ok", 140.0, "2026-10-24T17:00:00Z")),
            Some(window("ok", -20.0, "2026-10-26T00:00:00Z")),
            None,
        ));

        let Some(ProviderQuotaData::Periodic(periodic)) = &usage.quota else {
            panic!("the Go allowance is periodic");
        };
        assert_eq!(periodic.buckets[0].used_fraction, 1.0);
        assert_eq!(periodic.buckets[1].used_fraction, 0.0);
        assert_eq!(usage.primary_balance.as_deref(), Some("0%"));
    }

    #[test]
    fn a_missing_reading_is_named_as_such_never_rendered_as_zero_used() {
        // A window carrying only a reset is a real upstream state. Reporting it
        // as "0% used" would assert a measurement that was never made; the
        // reading is named absent while the countdown still works.
        let window = GoWindow {
            status: None,
            percent: None,
            resets_at: Some("2026-10-24T17:00:00Z".to_string()),
        };
        let usage = parse_go_usage(envelope(Some(window), None, None));

        let Some(ProviderQuotaData::Periodic(periodic)) = &usage.quota else {
            panic!("the Go allowance is periodic");
        };
        assert_eq!(periodic.buckets[0].used_amount, None);
        assert_eq!(periodic.buckets[0].total_limit, None);
        assert_eq!(usage.metrics[0].value, "not reported");
        // No readable window ⇒ no badge, rather than a fabricated "100%".
        assert_eq!(usage.primary_balance, None);
        assert!(periodic.buckets[0].reset_at_ms.is_some());
    }

    #[test]
    fn a_status_without_a_percent_is_rate_limit_aware() {
        // `rate-limited` alone is a complete statement even with no percentage.
        let window = GoWindow {
            status: Some("rate-limited".to_string()),
            percent: None,
            resets_at: Some("2026-10-24T17:00:00Z".to_string()),
        };
        let usage = parse_go_usage(envelope(Some(window), None, None));

        let Some(ProviderQuotaData::Periodic(periodic)) = &usage.quota else {
            panic!("the Go allowance is periodic");
        };
        assert_eq!(periodic.buckets[0].used_fraction, 1.0);
        assert_eq!(usage.metrics[0].value, "0% (rate-limited)");
        assert_eq!(usage.primary_balance.as_deref(), Some("0%"));
    }

    #[test]
    fn an_unparseable_reset_keeps_the_raw_string_for_display() {
        let usage = parse_go_usage(envelope(Some(window("ok", 5.0, "tomorrow")), None, None));

        let Some(ProviderQuotaData::Periodic(periodic)) = &usage.quota else {
            panic!("the Go allowance is periodic");
        };
        assert_eq!(periodic.buckets[0].reset_at_ms, None);
        assert_eq!(
            periodic.buckets[0].reset_time_str.as_deref(),
            Some("tomorrow")
        );
    }

    #[test]
    fn the_retired_proposal_shape_is_rejected_not_aliased() {
        // `rollingUsage` at the envelope root was the pre-release proposal; the
        // deployed route never served it. Accepting it would mean shipping a
        // second contract with no producer — drift would then arrive as a
        // silently-empty allowance instead of a parse error.
        let retired = r#"{
            "rollingUsage": {"status": "ok", "usagePercent": 12, "resetsAt": "2026-08-16T20:00:00Z"}
        }"#;
        assert!(serde_json::from_str::<GoUsageEnvelope>(retired).is_err());

        // The envelope key is mandatory: a bare window object is not the route.
        let bare = r#"{"rolling": {"status": "ok", "percent": 12}}"#;
        assert!(serde_json::from_str::<GoUsageEnvelope>(bare).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored_so_upstream_may_add_them() {
        // Forward compatibility: upstream adding a field is not drift.
        let body = r#"{
            "useBalance": false,
            "usage": {"rolling": {"status": "ok", "percent": 1, "resetsAt": "2026-10-24T17:00:00Z", "future": 1}},
            "extra": {"nested": true}
        }"#;
        let envelope: GoUsageEnvelope = serde_json::from_str(body).expect("unknown fields ignored");
        let usage = parse_go_usage(envelope);
        assert_eq!(usage.metrics.len(), 1);
    }

    #[test]
    fn the_endpoint_is_derived_from_the_connection_url_not_a_host_constant() {
        // Every channel shape the connection may reach collapses to the same
        // route. The Anthropic and Google cases are the ones that matter: the
        // caller passes the *default channel's* base_url, which for a Claude
        // model is `inference/go/anthropic/v1/messages` and for a Gemini model
        // `inference/go/google/v1beta` — not the `/zen/go/v1` family at all.
        // The allowance is billed per account, so all of them must resolve.
        for base in [
            // OpenAI-protocol models (the connection default).
            "https://opencode.ai/zen/go/v1",
            "https://opencode.ai/zen/go/v1/",
            "https://opencode.ai/zen/go/v1/chat/completions",
            // Claude models (spec `protocol_roots`).
            "https://opencode.ai/inference/go/anthropic/v1/messages",
            // Gemini models (spec `protocol_roots`).
            "https://opencode.ai/inference/go/google/v1beta",
        ] {
            assert_eq!(
                usage_endpoint(base).as_deref(),
                Ok("https://opencode.ai/zen/go/v1/usage"),
                "base {base}"
            );
        }

        // A mirror origin is honoured: no hard-coded host to drift.
        assert_eq!(
            usage_endpoint("https://dev.opencode.ai/zen/go/v1/chat/completions").as_deref(),
            Ok("https://dev.opencode.ai/zen/go/v1/usage")
        );
        // The origin is the host, never a path prefix echoed back.
        assert_eq!(
            usage_endpoint("http://127.0.0.1:8080/inference/go/anthropic/v1/messages").as_deref(),
            Ok("http://127.0.0.1:8080/zen/go/v1/usage")
        );

        // A value that cannot name an origin is reported, not guessed at.
        assert!(usage_endpoint("opencode.ai/zen/go/v1").is_err());
        assert!(usage_endpoint("https://").is_err());
    }

    #[test]
    fn status_codes_are_reported_as_what_they_mean() {
        // 403 is an entitlement answer, not a rejected key: the operator must be
        // able to tell "no Go subscription" from "bad API key".
        let forbidden = error_for_status(403, r#"{"type":"error"}"#);
        assert!(forbidden.contains("subscription required"), "{forbidden}");
        assert!(forbidden.contains("403"), "{forbidden}");

        let unauthorized = error_for_status(401, r#"{"type":"error"}"#);
        assert!(
            unauthorized.contains("rejected the API key"),
            "{unauthorized}"
        );

        // A moved route is drift, and says so.
        let missing = error_for_status(404, "");
        assert!(missing.contains("has moved"), "{missing}");

        // An oversized body is truncated rather than echoed whole into the UI.
        let long = "x".repeat(1000);
        assert!(error_for_status(500, &long).len() < 400);
    }
}
