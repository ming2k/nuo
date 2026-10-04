//! DeepSeek balance / usage fetcher.
//!
//! Query endpoint: `GET https://api.deepseek.com/user/balance`

use nuo_wire::{BalanceQuota, ProviderQuotaData, ProviderUsage, UsageMetric};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct DeepSeekBalanceResponse {
    #[serde(default)]
    pub(crate) is_available: bool,
    #[serde(default)]
    pub(crate) balance_infos: Vec<DeepSeekBalanceInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeepSeekBalanceInfo {
    #[serde(default)]
    pub(crate) currency: String,
    #[serde(default)]
    pub(crate) total_balance: String,
    #[serde(default)]
    pub(crate) granted_balance: String,
    #[serde(default)]
    pub(crate) topped_up_balance: String,
}

pub struct DeepSeekUsageFetcher;

impl DeepSeekUsageFetcher {
    pub fn matches(&self, provider: &str, base_url: &str) -> bool {
        provider == "deepseek" || base_url.contains("deepseek.com")
    }

    pub async fn fetch_usage(
        &self,
        client: &nuo_provider_transport::http::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = if base_url.contains("deepseek.com") {
            let base = base_url.trim_end_matches('/');
            let base = base.strip_suffix("/v1").unwrap_or(base);
            let base = base.strip_suffix("/beta").unwrap_or(base);
            format!("{base}/user/balance")
        } else {
            "https://api.deepseek.com/user/balance".to_string()
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

        let body: DeepSeekBalanceResponse = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse DeepSeek balance response: {e}"))?;

        parse_deepseek_balance(body)
    }
}

pub(crate) fn parse_deepseek_balance(
    body: DeepSeekBalanceResponse,
) -> Result<ProviderUsage, String> {
    let info = body
        .balance_infos
        .into_iter()
        .next()
        .ok_or_else(|| "No balance information returned by DeepSeek API".to_string())?;

    let currency = if info.currency.is_empty() {
        "CNY".to_string()
    } else {
        info.currency
    };

    let currency_symbol = if currency == "CNY" {
        "¥"
    } else if currency == "USD" {
        "$"
    } else {
        ""
    };
    let primary_balance = if !currency_symbol.is_empty() {
        format!("{currency_symbol}{}", info.total_balance)
    } else {
        format!("{} {currency}", info.total_balance)
    };

    let plan = if body.is_available {
        Some("Pay-as-you-go".to_string())
    } else {
        Some("Unavailable".to_string())
    };

    let total_num = info.total_balance.parse::<f64>().ok();
    let cash_num = info.topped_up_balance.parse::<f64>().ok();
    let voucher_num = info.granted_balance.parse::<f64>().ok();

    let balance_quota = BalanceQuota {
        currency: currency.clone(),
        total_balance: total_num,
        cash_balance: cash_num,
        voucher_balance: voucher_num,
        credit_limit: None,
        consumed_amount: None,
        display_primary: Some(primary_balance.clone()),
    };

    let mut metrics = Vec::new();
    metrics.push(UsageMetric {
        label: "Total Balance".to_string(),
        value: info.total_balance,
        unit: Some(currency.clone()),
    });
    metrics.push(UsageMetric {
        label: "Topped-up Balance".to_string(),
        value: info.topped_up_balance,
        unit: Some(currency.clone()),
    });
    metrics.push(UsageMetric {
        label: "Granted Balance".to_string(),
        value: info.granted_balance,
        unit: Some(currency.clone()),
    });
    metrics.push(UsageMetric {
        label: "Service Status".to_string(),
        value: if body.is_available {
            "Available".to_string()
        } else {
            "Unavailable".to_string()
        },
        unit: None,
    });

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();

    Ok(ProviderUsage {
        plan,
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
    fn parses_deepseek_balance_json() {
        let json = r#"{
            "is_available": true,
            "balance_infos": [
                {
                    "currency": "CNY",
                    "total_balance": "100.50",
                    "granted_balance": "0.00",
                    "topped_up_balance": "100.50"
                }
            ]
        }"#;

        let parsed: DeepSeekBalanceResponse = serde_json::from_str(json).unwrap();
        let usage = parse_deepseek_balance(parsed).unwrap();

        assert_eq!(usage.plan.as_deref(), Some("Pay-as-you-go"));
        assert_eq!(usage.primary_balance.as_deref(), Some("¥100.50"));
        assert_eq!(usage.metrics.len(), 4);
        assert_eq!(usage.metrics[0].label, "Total Balance");
        assert_eq!(usage.metrics[0].value, "100.50");
        assert_eq!(usage.metrics[0].unit.as_deref(), Some("CNY"));

        if let Some(ProviderQuotaData::Balance(bal)) = usage.quota {
            assert_eq!(bal.currency, "CNY");
            assert_eq!(bal.total_balance, Some(100.50));
            assert_eq!(bal.cash_balance, Some(100.50));
            assert_eq!(bal.voucher_balance, Some(0.0));
        } else {
            panic!("Expected Balance quota data");
        }
    }
}
