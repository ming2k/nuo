//! OpenRouter balance / credit limit / rate limit fetcher.
//!
//! Query endpoint: `GET https://openrouter.ai/api/v1/auth/key`

use nuo_wire::async_trait;
use nuo_wire::{BalanceQuota, ProviderQuotaData, ProviderUsage, RateLimitSpec, UsageMetric};
use serde::Deserialize;

use super::ProviderUsageFetcher;

#[derive(Debug, Deserialize)]
pub(crate) struct OpenRouterKeyResponse {
    #[serde(default)]
    pub(crate) data: Option<OpenRouterKeyData>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenRouterKeyData {
    #[serde(default)]
    pub(crate) label: Option<String>,
    #[serde(default)]
    pub(crate) usage: f64,
    #[serde(default)]
    pub(crate) limit: Option<f64>,
    #[serde(default)]
    pub(crate) is_free_tier: bool,
    #[serde(default)]
    pub(crate) rate_limit: Option<OpenRouterRateLimit>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenRouterRateLimit {
    #[serde(default)]
    pub(crate) requests: i64,
    #[serde(default)]
    pub(crate) interval: String,
}

pub struct OpenRouterUsageFetcher;

#[async_trait]
impl ProviderUsageFetcher for OpenRouterUsageFetcher {
    fn matches(&self, provider: &str, base_url: &str) -> bool {
        provider == "openrouter" || base_url.contains("openrouter.ai")
    }

    async fn fetch_usage(
        &self,
        client: &crate::http::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = openrouter_key_endpoint(base_url);

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
            return Err(format!("HTTP {}: {}", resp.status, resp.body));
        }

        let body: OpenRouterKeyResponse = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse OpenRouter key response: {e}"))?;

        parse_openrouter_key(body)
    }
}

fn openrouter_key_endpoint(base_url: &str) -> String {
    if !base_url.contains("openrouter.ai") {
        return "https://openrouter.ai/api/v1/auth/key".to_string();
    }
    let base = base_url.trim_end_matches('/');
    let base = base
        .strip_suffix("/chat/completions")
        .or_else(|| base.strip_suffix("/responses"))
        .unwrap_or(base);
    let base = base.strip_suffix("/api/v1").unwrap_or(base);
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/api/v1/auth/key")
}

pub(crate) fn parse_openrouter_key(body: OpenRouterKeyResponse) -> Result<ProviderUsage, String> {
    let data = body
        .data
        .ok_or_else(|| "No data returned by OpenRouter auth/key API".to_string())?;

    let plan = if data.is_free_tier {
        Some("Free Tier".to_string())
    } else {
        Some("Pay-as-you-go".to_string())
    };

    let primary_balance = if let Some(limit) = data.limit {
        format!("${:.2} / ${:.2}", data.usage, limit)
    } else {
        format!("${:.2} used", data.usage)
    };

    let balance_quota = BalanceQuota {
        currency: "USD".to_string(),
        total_balance: data.limit.map(|lim| (lim - data.usage).max(0.0)),
        cash_balance: None,
        voucher_balance: None,
        credit_limit: data.limit,
        consumed_amount: Some(data.usage),
        display_primary: Some(primary_balance.clone()),
    };

    let mut rate_limits = Vec::new();
    if let Some(rl) = &data.rate_limit {
        rate_limits.push(RateLimitSpec {
            requests: rl.requests,
            interval: rl.interval.clone(),
        });
    }

    let mut metrics = Vec::new();
    if let Some(lbl) = data.label
        && !lbl.is_empty()
    {
        metrics.push(UsageMetric {
            label: "Key Label".to_string(),
            value: lbl,
            unit: None,
        });
    }
    metrics.push(UsageMetric {
        label: "Usage".to_string(),
        value: format!("${:.4}", data.usage),
        unit: Some("USD".to_string()),
    });
    if let Some(limit) = data.limit {
        metrics.push(UsageMetric {
            label: "Credit Limit".to_string(),
            value: format!("${:.2}", limit),
            unit: Some("USD".to_string()),
        });
    }
    if let Some(rl) = &data.rate_limit {
        metrics.push(UsageMetric {
            label: "Rate Limit".to_string(),
            value: format!("{} req / {}", rl.requests, rl.interval),
            unit: None,
        });
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();

    Ok(ProviderUsage {
        plan,
        description: None,
        quota: Some(ProviderQuotaData::Composite {
            balance: Some(balance_quota),
            periodic: None,
            rate_limits,
        }),
        primary_balance: Some(primary_balance),
        metrics,
        updated_at_ms: now_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openrouter_key_json() {
        let json = r#"{
            "data": {
                "label": "Work project",
                "usage": 1.2345,
                "limit": 10.0,
                "is_free_tier": false,
                "rate_limit": {
                    "requests": 200,
                    "interval": "10s"
                }
            }
        }"#;

        let parsed: OpenRouterKeyResponse = serde_json::from_str(json).unwrap();
        let usage = parse_openrouter_key(parsed).unwrap();

        assert_eq!(usage.plan.as_deref(), Some("Pay-as-you-go"));
        assert_eq!(usage.primary_balance.as_deref(), Some("$1.23 / $10.00"));
        assert_eq!(usage.metrics.len(), 4);
        assert_eq!(usage.metrics[0].label, "Key Label");
        assert_eq!(usage.metrics[0].value, "Work project");

        if let Some(ProviderQuotaData::Composite {
            balance,
            rate_limits,
            ..
        }) = usage.quota
        {
            let bal = balance.unwrap();
            assert_eq!(bal.currency, "USD");
            assert_eq!(bal.credit_limit, Some(10.0));
            assert_eq!(bal.consumed_amount, Some(1.2345));
            assert_eq!(rate_limits.len(), 1);
            assert_eq!(rate_limits[0].requests, 200);
            assert_eq!(rate_limits[0].interval, "10s");
        } else {
            panic!("Expected Composite quota data");
        }
    }

    #[test]
    fn derives_key_endpoint_from_inference_url_or_api_root() {
        for base_url in [
            "https://openrouter.ai/api/v1",
            "https://openrouter.ai/api/v1/chat/completions",
            "https://openrouter.ai/api/v1/responses",
        ] {
            assert_eq!(
                openrouter_key_endpoint(base_url),
                "https://openrouter.ai/api/v1/auth/key"
            );
        }
    }
}
