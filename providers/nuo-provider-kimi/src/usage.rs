//! Kimi (Moonshot) balance / usage fetcher.
//!
//! Query endpoint: `GET https://api.moonshot.cn/v1/users/me/balance`

use nuo_wire::{BalanceQuota, ProviderQuotaData, ProviderUsage, UsageMetric};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct KimiBalanceResponse {
    #[serde(default)]
    pub(crate) code: i64,
    #[serde(default)]
    pub(crate) data: Option<KimiBalanceData>,
    #[serde(default)]
    pub(crate) status: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KimiBalanceData {
    #[serde(default)]
    pub(crate) available_balance: f64,
    #[serde(default)]
    pub(crate) voucher_balance: f64,
    #[serde(default)]
    pub(crate) cash_balance: f64,
}

pub struct KimiUsageFetcher;

impl KimiUsageFetcher {
    pub fn matches(&self, provider: &str, base_url: &str) -> bool {
        provider == "kimi" || provider == "kimi-code" || base_url.contains("moonshot.cn") || base_url.contains("kimi.com")
    }

    pub async fn fetch_usage(
        &self,
        client: &nuo_provider_transport::http::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = if base_url.contains("moonshot.cn") {
            let base = base_url.trim_end_matches('/');
            let base = base.strip_suffix("/v1").unwrap_or(base);
            format!("{base}/v1/users/me/balance")
        } else {
            "https://api.moonshot.cn/v1/users/me/balance".to_string()
        };

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

        let body: KimiBalanceResponse = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse Kimi balance response: {e}"))?;

        parse_kimi_balance(body)
    }
}

pub(crate) fn parse_kimi_balance(body: KimiBalanceResponse) -> Result<ProviderUsage, String> {
    let data = body
        .data
        .ok_or_else(|| format!("No data returned by Kimi balance API (code: {})", body.code))?;

    let primary_balance = format!("¥{:.2}", data.available_balance);

    let balance_quota = BalanceQuota {
        currency: "CNY".to_string(),
        total_balance: Some(data.available_balance),
        cash_balance: Some(data.cash_balance),
        voucher_balance: Some(data.voucher_balance),
        credit_limit: None,
        consumed_amount: None,
        display_primary: Some(primary_balance.clone()),
    };

    let mut metrics = Vec::new();
    metrics.push(UsageMetric {
        label: "Available Balance".to_string(),
        value: format!("¥{:.2}", data.available_balance),
        unit: Some("CNY".to_string()),
    });
    metrics.push(UsageMetric {
        label: "Cash Balance".to_string(),
        value: format!("¥{:.2}", data.cash_balance),
        unit: Some("CNY".to_string()),
    });
    metrics.push(UsageMetric {
        label: "Voucher Balance".to_string(),
        value: format!("¥{:.2}", data.voucher_balance),
        unit: Some("CNY".to_string()),
    });

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();

    Ok(ProviderUsage {
        plan: if body.status {
            Some("Pay-as-you-go".to_string())
        } else {
            None
        },
        description: None,
        quota: Some(ProviderQuotaData::Balance(balance_quota)),
        primary_balance: Some(primary_balance),
        metrics,
        updated_at_ms: now_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kimi_balance_json() {
        let json = r#"{
            "code": 0,
            "data": {
                "available_balance": 25.8,
                "voucher_balance": 5.0,
                "cash_balance": 20.8
            },
            "status": true
        }"#;

        let parsed: KimiBalanceResponse = serde_json::from_str(json).unwrap();
        let usage = parse_kimi_balance(parsed).unwrap();

        assert_eq!(usage.plan.as_deref(), Some("Pay-as-you-go"));
        assert_eq!(usage.primary_balance.as_deref(), Some("¥25.80"));
        assert_eq!(usage.metrics.len(), 3);
        assert_eq!(usage.metrics[0].label, "Available Balance");
        assert_eq!(usage.metrics[0].value, "¥25.80");

        if let Some(ProviderQuotaData::Balance(bal)) = usage.quota {
            assert_eq!(bal.currency, "CNY");
            assert_eq!(bal.total_balance, Some(25.8));
            assert_eq!(bal.cash_balance, Some(20.8));
            assert_eq!(bal.voucher_balance, Some(5.0));
        } else {
            panic!("Expected Balance quota data");
        }
    }
}
