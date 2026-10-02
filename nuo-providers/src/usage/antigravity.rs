//! Google Antigravity CodeAssist quota and usage fetcher.
//!
//! Query endpoint: `POST https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary`

use nuo_contracts::async_trait;
use nuo_contracts::{
    PeriodicQuota, ProviderQuotaData, ProviderUsage, QuotaWindowBucket, QuotaWindowKind,
    UsageMetric,
};
use serde::{Deserialize, Serialize};

use super::ProviderUsageFetcher;
use crate::oauth::token::{
    ANTIGRAVITY_API_CLIENT_HEADER, ANTIGRAVITY_RETRIEVE_QUOTA_SUMMARY_URL, ANTIGRAVITY_USER_AGENT,
};

/// Individual Antigravity model/feature quota bucket (QuotaSummaryBucket in internal proto).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AntigravityQuotaBucket {
    #[serde(default, alias = "bucketId")]
    pub bucket_id: String,
    #[serde(default, alias = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default, alias = "remainingFraction")]
    pub remaining_fraction: Option<f32>,
    #[serde(default, alias = "remainingAmount")]
    pub remaining_amount: Option<i64>,
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default, alias = "resetTime")]
    pub reset_time: Option<String>,
}

/// Logical grouping of Antigravity quota buckets (QuotaSummaryGroup in internal proto).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AntigravityQuotaGroup {
    #[serde(default, alias = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub buckets: Vec<AntigravityQuotaBucket>,
}

/// Full Antigravity user quota summary (RetrieveUserQuotaSummaryResponse in internal proto).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AntigravityQuotaSummaryResponse {
    #[serde(default)]
    pub buckets: Vec<AntigravityQuotaBucket>,
    #[serde(default)]
    pub groups: Vec<AntigravityQuotaGroup>,
    #[serde(default)]
    pub description: Option<String>,
}

pub struct AntigravityUsageFetcher;

#[async_trait]
impl ProviderUsageFetcher for AntigravityUsageFetcher {
    fn matches(&self, provider: &str, base_url: &str) -> bool {
        provider == "google-antigravity" || base_url.contains("cloudcode-pa.googleapis.com")
    }

    async fn fetch_usage(
        &self,
        client: &crate::http::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = if base_url.contains("cloudcode-pa.googleapis.com") {
            let base = base_url.trim_end_matches('/');
            let base = base.strip_suffix("/v1internal").unwrap_or(base);
            format!("{base}/v1internal:retrieveUserQuotaSummary")
        } else {
            ANTIGRAVITY_RETRIEVE_QUOTA_SUMMARY_URL.to_string()
        };

        let req_body = serde_json::json!({
            "project": ""
        });

        let request = crate::http::Request::new(netune::Method::POST, &endpoint)
            .header("authorization", format!("Bearer {api_key}"))
            .header("user-agent", ANTIGRAVITY_USER_AGENT)
            .header("x-goog-api-client", ANTIGRAVITY_API_CLIENT_HEADER)
            .json(&req_body);
        let resp = client
            .send(request)
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.is_success() {
            return Err(format!("HTTP {}: {}", resp.status, resp.body));
        }

        let body: AntigravityQuotaSummaryResponse = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse Antigravity quota response: {e}"))?;

        parse_antigravity_quota(body)
    }
}

pub(crate) fn parse_antigravity_quota(
    body: AntigravityQuotaSummaryResponse,
) -> Result<ProviderUsage, String> {
    let (plan, description) = match body.description {
        Some(desc) => {
            let trimmed = desc.trim().to_string();
            if trimmed.len() <= 45
                && !trimmed.contains('\n')
                && !trimmed.contains("Within each group")
            {
                (Some(trimmed), None)
            } else {
                (Some("Antigravity Quota".to_string()), Some(trimmed))
            }
        }
        None => (Some("Google Antigravity".to_string()), None),
    };

    let mut all_grouped_buckets = Vec::new();
    for bucket in body.buckets {
        all_grouped_buckets.push((None::<String>, bucket));
    }
    for group in body.groups {
        let group_name = group
            .display_name
            .filter(|s| !s.trim().is_empty())
            .or_else(|| group.description.filter(|s| !s.trim().is_empty()));
        for bucket in group.buckets {
            all_grouped_buckets.push((group_name.clone(), bucket));
        }
    }

    if all_grouped_buckets.is_empty() {
        return Ok(ProviderUsage {
            plan: plan.or_else(|| Some("Antigravity Quota Active".to_string())),
            description,
            quota: Some(ProviderQuotaData::Periodic(PeriodicQuota::default())),
            primary_balance: Some("100%".to_string()),
            metrics: Vec::new(),
            updated_at_ms: now_epoch_ms(),
        });
    }

    let mut metrics = Vec::new();
    let mut quota_buckets = Vec::new();
    let mut min_remaining_fraction: Option<f32> = None;

    for (group_name, bucket) in all_grouped_buckets {
        let label = bucket
            .display_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(bucket.bucket_id);

        let window_kind = bucket.window.as_deref().map(|w| {
            let upper = w.to_ascii_uppercase();
            if upper.contains("5H") {
                QuotaWindowKind::Rolling5Hour
            } else if upper.contains("DAY") || upper.contains("24H") {
                QuotaWindowKind::Daily
            } else if upper.contains("WEEK") || upper.contains("7D") {
                QuotaWindowKind::Weekly
            } else if upper.contains("MONTH") || upper.contains("30D") {
                QuotaWindowKind::Monthly
            } else {
                QuotaWindowKind::Custom
            }
        });

        let reset_at_ms = bucket.reset_time.as_deref().and_then(|rt| {
            chrono::DateTime::parse_from_rfc3339(rt)
                .map(|dt| dt.timestamp_millis() as u64)
                .ok()
        });

        let used_fraction = if let Some(frac) = bucket.remaining_fraction {
            min_remaining_fraction = Some(min_remaining_fraction.map_or(frac, |m| m.min(frac)));
            (1.0 - frac).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let value_str = if let Some(frac) = bucket.remaining_fraction {
            let pct = (frac * 100.0).round() as u32;
            if let Some(reset) = bucket
                .reset_time
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                format!("{pct}% (Resets: {reset})")
            } else {
                format!("{pct}%")
            }
        } else if let Some(amt) = bucket.remaining_amount {
            if let Some(reset) = bucket
                .reset_time
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                format!("{amt} (Resets: {reset})")
            } else {
                format!("{amt}")
            }
        } else if bucket.disabled.unwrap_or(false) {
            "Disabled".to_string()
        } else {
            "Available".to_string()
        };

        let metric_label = match &group_name {
            Some(g) => format!("{g} · {label}"),
            None => label.clone(),
        };

        quota_buckets.push(QuotaWindowBucket {
            window: window_kind,
            label,
            group: group_name,
            used_fraction,
            used_amount: None,
            total_limit: None,
            unit: bucket.window.clone(),
            reset_at_ms,
            reset_time_str: bucket.reset_time.clone(),
        });

        metrics.push(UsageMetric {
            label: metric_label,
            value: value_str,
            unit: bucket.window,
        });
    }

    let primary_balance = min_remaining_fraction
        .map(|frac| {
            let pct = (frac * 100.0).round() as u32;
            format!("{pct}% Quota")
        })
        .or_else(|| Some("Active".to_string()));

    Ok(ProviderUsage {
        plan,
        description,
        quota: Some(ProviderQuotaData::Periodic(PeriodicQuota {
            buckets: quota_buckets,
        })),
        primary_balance,
        metrics,
        updated_at_ms: now_epoch_ms(),
    })
}

fn now_epoch_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_antigravity_quota_response_json() {
        let json = r#"{
            "description": "Google One AI Premium Tier",
            "groups": [
                {
                    "displayName": "Chat Models",
                    "buckets": [
                        {
                            "bucketId": "gemini-3.7-flash",
                            "displayName": "Gemini 3.7 Flash",
                            "window": "DAY",
                            "remainingFraction": 0.85,
                            "resetTime": "2026-09-01T12:00:00Z"
                        },
                        {
                            "bucketId": "gemini-3.1-pro",
                            "displayName": "Gemini 3.1 Pro",
                            "window": "5h",
                            "remainingFraction": 0.60
                        }
                    ]
                }
            ]
        }"#;

        let parsed: AntigravityQuotaSummaryResponse = serde_json::from_str(json).unwrap();
        let usage = parse_antigravity_quota(parsed).unwrap();

        assert_eq!(usage.plan.as_deref(), Some("Google One AI Premium Tier"));
        assert_eq!(usage.primary_balance.as_deref(), Some("60% Quota"));
        assert_eq!(usage.metrics.len(), 2);
        assert_eq!(usage.metrics[0].label, "Chat Models · Gemini 3.7 Flash");
        assert_eq!(usage.metrics[0].value, "85% (Resets: 2026-09-01T12:00:00Z)");
        assert_eq!(usage.metrics[0].unit.as_deref(), Some("DAY"));

        if let Some(ProviderQuotaData::Periodic(periodic)) = usage.quota {
            assert_eq!(periodic.buckets.len(), 2);
            assert_eq!(periodic.buckets[0].group.as_deref(), Some("Chat Models"));
            assert_eq!(periodic.buckets[0].window, Some(QuotaWindowKind::Daily));
            assert!((periodic.buckets[0].used_fraction - 0.15).abs() < 0.001);
            assert_eq!(
                periodic.buckets[1].window,
                Some(QuotaWindowKind::Rolling5Hour)
            );
            assert_eq!(periodic.buckets[1].group.as_deref(), Some("Chat Models"));
            assert!((periodic.buckets[1].used_fraction - 0.40).abs() < 0.001);
        } else {
            panic!("Expected Periodic quota data");
        }
    }

    #[test]
    fn parses_multi_group_antigravity_quota_with_long_policy_description() {
        let json = r#"{
            "description": "Within each group, models share a weekly limit and a 5-hour limit. Quota is consumed proportionally to the cost of the tokens.",
            "groups": [
                {
                    "displayName": "Claude Models",
                    "buckets": [
                        {
                            "bucketId": "claude-weekly",
                            "displayName": "Weekly Limit Remaining",
                            "window": "WEEKLY",
                            "remainingFraction": 0.01,
                            "resetTime": "2026-04-10T19:50:00Z"
                        },
                        {
                            "bucketId": "claude-5h",
                            "displayName": "Five Hour Limit Remaining",
                            "window": "5h",
                            "remainingFraction": 0.54,
                            "resetTime": "2026-04-07T15:00:00Z"
                        }
                    ]
                },
                {
                    "displayName": "Chat Models (Gemini)",
                    "buckets": [
                        {
                            "bucketId": "gemini-weekly",
                            "displayName": "Weekly Limit Remaining",
                            "window": "WEEKLY",
                            "remainingFraction": 1.0,
                            "resetTime": "2026-04-14T11:44:00Z"
                        },
                        {
                            "bucketId": "gemini-5h",
                            "displayName": "Five Hour Limit Remaining",
                            "window": "5h",
                            "remainingFraction": 1.0,
                            "resetTime": "2026-04-07T16:44:00Z"
                        }
                    ]
                }
            ]
        }"#;

        let parsed: AntigravityQuotaSummaryResponse = serde_json::from_str(json).unwrap();
        let usage = parse_antigravity_quota(parsed).unwrap();

        assert_eq!(usage.plan.as_deref(), Some("Antigravity Quota"));
        assert!(
            usage
                .description
                .as_deref()
                .unwrap()
                .starts_with("Within each group")
        );
        assert_eq!(usage.primary_balance.as_deref(), Some("1% Quota"));
        assert_eq!(usage.metrics.len(), 4);
        assert_eq!(
            usage.metrics[0].label,
            "Claude Models · Weekly Limit Remaining"
        );
        assert_eq!(
            usage.metrics[2].label,
            "Chat Models (Gemini) · Weekly Limit Remaining"
        );

        if let Some(ProviderQuotaData::Periodic(periodic)) = usage.quota {
            assert_eq!(periodic.buckets.len(), 4);
            assert_eq!(periodic.buckets[0].group.as_deref(), Some("Claude Models"));
            assert_eq!(
                periodic.buckets[2].group.as_deref(),
                Some("Chat Models (Gemini)")
            );
        } else {
            panic!("Expected Periodic quota data");
        }
    }
}
