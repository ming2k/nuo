//! SiliconFlow (硅基流动) balance / usage fetcher.
//!
//! Query endpoint: `GET https://api.siliconflow.cn/v1/user/info`

use nuo_wire::{BalanceQuota, ProviderQuotaData, ProviderUsage, UsageMetric};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct SiliconFlowUserResponse {
    #[serde(default)]
    pub(crate) code: i64,
    #[serde(default)]
    pub(crate) data: Option<SiliconFlowUserData>,
    #[serde(default)]
    pub(crate) status: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SiliconFlowUserData {
    #[serde(default)]
    pub(crate) email: Option<String>,
    #[serde(default)]
    pub(crate) balance: Option<String>,
    #[serde(default, rename = "chargeBalance")]
    pub(crate) charge_balance: Option<String>,
    #[serde(default, rename = "totalBalance")]
    pub(crate) total_balance: Option<String>,
}

pub struct SiliconFlowUsageFetcher;

impl SiliconFlowUsageFetcher {
    pub fn matches(&self, provider: &str, base_url: &str) -> bool {
        provider == "siliconflow"
            || base_url.contains("siliconflow.cn")
            || base_url.contains("siliconflow.com")
    }

    pub async fn fetch_usage(
        &self,
        client: &nuo_provider_transport::http::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = if base_url.contains("siliconflow") {
            let base = base_url.trim_end_matches('/');
            let base = base.strip_suffix("/v1").unwrap_or(base);
            format!("{base}/v1/user/info")
        } else {
            "https://api.siliconflow.cn/v1/user/info".to_string()
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

        let body: SiliconFlowUserResponse = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse SiliconFlow user response: {e}"))?;

        parse_siliconflow_user(body)
    }
}

pub(crate) fn parse_siliconflow_user(
    body: SiliconFlowUserResponse,
) -> Result<ProviderUsage, String> {
    let data = body.data.ok_or_else(|| {
        format!(
            "No data returned by SiliconFlow user info API (code: {})",
            body.code
        )
    })?;

    let total = data
        .total_balance
        .clone()
        .unwrap_or_else(|| "0.00".to_string());
    let primary_balance = format!("¥{total}");

    let total_num = total.parse::<f64>().ok();
    let cash_num = data
        .charge_balance
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok());
    let voucher_num = data.balance.as_deref().and_then(|s| s.parse::<f64>().ok());

    let balance_quota = BalanceQuota {
        currency: "CNY".to_string(),
        total_balance: total_num,
        cash_balance: cash_num,
        voucher_balance: voucher_num,
        credit_limit: None,
        consumed_amount: None,
        display_primary: Some(primary_balance.clone()),
    };

    let mut metrics = Vec::new();
    if let Some(email) = data.email
        && !email.is_empty()
    {
        metrics.push(UsageMetric {
            label: "Account Email".to_string(),
            value: email,
            unit: None,
        });
    }
    metrics.push(UsageMetric {
        label: "Total Balance".to_string(),
        value: total,
        unit: Some("CNY".to_string()),
    });
    if let Some(charge) = data.charge_balance {
        metrics.push(UsageMetric {
            label: "Recharge Balance".to_string(),
            value: charge,
            unit: Some("CNY".to_string()),
        });
    }
    if let Some(gift) = data.balance {
        metrics.push(UsageMetric {
            label: "Gift Balance".to_string(),
            value: gift,
            unit: Some("CNY".to_string()),
        });
    }

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
    fn parses_siliconflow_user_json() {
        let json = r#"{
            "code": 20000,
            "message": "success",
            "status": true,
            "data": {
                "id": "sf-123",
                "email": "user@example.com",
                "balance": "5.00",
                "chargeBalance": "20.00",
                "totalBalance": "25.00"
            }
        }"#;

        let parsed: SiliconFlowUserResponse = serde_json::from_str(json).unwrap();
        let usage = parse_siliconflow_user(parsed).unwrap();

        assert_eq!(usage.plan.as_deref(), Some("Pay-as-you-go"));
        assert_eq!(usage.primary_balance.as_deref(), Some("¥25.00"));
        assert_eq!(usage.metrics.len(), 4);
        assert_eq!(usage.metrics[0].label, "Account Email");
        assert_eq!(usage.metrics[0].value, "user@example.com");

        if let Some(ProviderQuotaData::Balance(bal)) = usage.quota {
            assert_eq!(bal.currency, "CNY");
            assert_eq!(bal.total_balance, Some(25.0));
            assert_eq!(bal.cash_balance, Some(20.0));
            assert_eq!(bal.voucher_balance, Some(5.0));
        } else {
            panic!("Expected Balance quota data");
        }
    }
}
